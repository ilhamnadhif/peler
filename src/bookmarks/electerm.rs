//! Reader for Electerm exports, checked against Electerm 5.6.10 (`bookmark-toolbar.jsx`
//! `handleDownload`).
//!
//! Accepted shapes: the bookmark export `{bookmarkGroups, bookmarks}`, the "export all data"
//! object (same keys plus `profiles`, used to resolve `authType: "profiles"`), and a bare array
//! of bookmarks. Groups nest through `bookmarkGroupIds`: a group is a root when no other group
//! lists it. Electerm writes passwords, passphrases and inline keys in plain text; they are moved
//! into the import's secrets (and from there into the Keychain), never into the bookmark file.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use serde_json::Value;
use uuid::Uuid;

use super::model::{Auth, Bookmark, DEFAULT_PORT, Group};
use super::transfer::{Incoming, SourceKind};
use crate::i18n::{t, tf};
use crate::secrets::SecretSet;

const DEFAULT_GROUP: &str = "default";

pub fn looks_like_electerm(value: &Value) -> bool {
    match value {
        Value::Array(items) => items
            .iter()
            .any(|b| b.get("host").is_some() || b.get("title").is_some()),
        Value::Object(map) => {
            map.get("bookmarks").is_some_and(Value::is_array) && !map.contains_key("schema_version")
        }
        _ => false,
    }
}

/// Stable ids derived from Electerm ids, so importing the same file twice matches by id.
fn bookmark_uuid(id: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("electerm:bookmark:{id}").as_bytes(),
    )
}

fn group_uuid(id: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("electerm:group:{id}").as_bytes(),
    )
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

fn ids(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn port(v: &Value) -> u16 {
    match v.get("port") {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(DEFAULT_PORT),
        Some(Value::String(t)) => t.trim().parse().unwrap_or(DEFAULT_PORT),
        _ => DEFAULT_PORT,
    }
}

/// Undoes the obfuscation Electerm applied to `password` when `passwordEncrypted` is set
/// (`pass-enc.js`: each UTF-16 unit shifted by its index + 1).
pub fn legacy_decode(text: &str) -> String {
    let units: Vec<u16> = text
        .encode_utf16()
        .enumerate()
        .map(|(i, c)| ((c as u32 + 65536 - ((i as u32 + 1) % 65536)) % 65536) as u16)
        .collect();
    String::from_utf16_lossy(&units)
}

pub fn read(value: &Value) -> Result<Incoming> {
    let (raw_groups, raw_bookmarks, profiles): (Vec<Value>, Vec<Value>, Vec<Value>) = match value {
        Value::Array(items) => (Vec::new(), items.clone(), Vec::new()),
        Value::Object(map) => {
            let arr = |k: &str| {
                map.get(k)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            (arr("bookmarkGroups"), arr("bookmarks"), arr("profiles"))
        }
        _ => bail!("Not an Electerm export."),
    };
    let profiles: HashMap<&str, &Value> = profiles
        .iter()
        .filter_map(|p| Some((p.get("id")?.as_str()?, p)))
        .collect();

    // --- groups: build parent links from bookmarkGroupIds (first parent wins) -----------------
    let mut by_id: HashMap<String, &Value> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for g in &raw_groups {
        let id = s(g, "id").to_string();
        if !id.is_empty() && !by_id.contains_key(&id) {
            by_id.insert(id.clone(), g);
            order.push(id);
        }
    }
    let mut parent_of: HashMap<String, String> = HashMap::new();
    for id in &order {
        for child in ids(by_id[id], "bookmarkGroupIds") {
            if child != *id
                && child != DEFAULT_GROUP
                && by_id.contains_key(&child)
                && !parent_of.contains_key(&child)
            {
                parent_of.insert(child, id.clone());
            }
        }
    }
    // Break cycles by detaching the group where the walk loops back.
    for id in &order {
        let mut seen = HashSet::from([id.clone()]);
        let mut cur = parent_of.get(id).cloned();
        while let Some(p) = cur {
            if !seen.insert(p.clone()) {
                parent_of.remove(id);
                break;
            }
            cur = parent_of.get(&p).cloned();
        }
    }

    // Depth-first order: roots in file order, children in bookmarkGroupIds order. Electerm's
    // "default" group maps to the top level.
    let mut tree_order: Vec<String> = Vec::new();
    fn visit(
        id: &str,
        by_id: &HashMap<String, &Value>,
        parent_of: &HashMap<String, String>,
        out: &mut Vec<String>,
    ) {
        if out.iter().any(|x| x == id) {
            return;
        }
        out.push(id.to_string());
        for child in ids(by_id[id], "bookmarkGroupIds") {
            if parent_of.get(&child).is_some_and(|p| p == id) {
                visit(&child, by_id, parent_of, out);
            }
        }
    }
    for id in &order {
        if !parent_of.contains_key(id) {
            visit(id, &by_id, &parent_of, &mut tree_order);
        }
    }

    let local_group = |id: &str| -> Option<Uuid> {
        (id != DEFAULT_GROUP && by_id.contains_key(id)).then(|| group_uuid(id))
    };
    let groups: Vec<Group> = tree_order
        .iter()
        .filter(|id| id.as_str() != DEFAULT_GROUP)
        .map(|id| {
            let title = s(by_id[id], "title");
            Group {
                id: group_uuid(id),
                name: if title.is_empty() {
                    id.clone()
                } else {
                    title.to_string()
                },
                parent: parent_of.get(id).and_then(|p| local_group(p)),
            }
        })
        .collect();

    // --- bookmarks: place each in the first group that lists it ------------------------------
    let mut group_of: HashMap<String, String> = HashMap::new();
    let mut listed_order: Vec<String> = Vec::new();
    for gid in &tree_order {
        for bid in ids(by_id[gid], "bookmarkIds") {
            if !group_of.contains_key(&bid) {
                group_of.insert(bid.clone(), gid.clone());
                listed_order.push(bid);
            }
        }
    }
    let mut raw_by_id: HashMap<String, &Value> = HashMap::new();
    let mut file_order: Vec<String> = Vec::new();
    for (i, b) in raw_bookmarks.iter().enumerate() {
        if !b.is_object() {
            continue;
        }
        let mut id = s(b, "id").to_string();
        if id.is_empty() {
            id = format!("__index_{i}");
        }
        if raw_by_id.insert(id.clone(), b).is_none() {
            file_order.push(id);
        }
    }
    let ordered: Vec<String> = listed_order
        .into_iter()
        .filter(|id| raw_by_id.contains_key(id))
        .chain(
            file_order
                .iter()
                .filter(|id| !group_of.contains_key(*id))
                .cloned(),
        )
        .collect();

    let mut bookmarks = Vec::new();
    let mut secrets = HashMap::new();
    let mut skipped_types: Vec<String> = Vec::new();
    let mut dropped_features = HashSet::new();
    for eid in ordered {
        let b = raw_by_id[&eid];
        let kind = s(b, "type");
        let kind = if kind.is_empty() { "ssh" } else { kind };
        if kind != "ssh" {
            skipped_types.push(kind.to_string());
            continue;
        }
        let host = s(b, "host");
        if host.is_empty() {
            skipped_types.push("ssh (no host)".into());
            continue;
        }

        // Credentials can come from a saved profile.
        let profile = (s(b, "authType") == "profiles")
            .then(|| profiles.get(s(b, "profile")).copied())
            .flatten();
        let field = |key: &str| -> String {
            let own = s(b, key);
            if !own.is_empty() {
                return own.to_string();
            }
            profile.map(|p| s(p, key).to_string()).unwrap_or_default()
        };

        let mut secret = SecretSet::default();
        let mut password = field("password");
        let encrypted_flag = b
            .get("passwordEncrypted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if encrypted_flag && !s(b, "password").is_empty() {
            password = legacy_decode(s(b, "password"));
        }
        let key_text = field("privateKey");
        let key_path = field("privateKeyPath");
        let passphrase = field("passphrase");
        let auth = if !key_text.is_empty() {
            secret.private_key = Some(if key_text.ends_with('\n') {
                key_text
            } else {
                key_text + "\n"
            });
            Auth::KeychainKey
        } else if !key_path.is_empty() {
            Auth::Key { path: key_path }
        } else {
            Auth::Password
        };
        if !password.is_empty() && matches!(auth, Auth::Password) {
            secret.password = Some(password);
        }
        if !passphrase.is_empty() && auth.uses_key() {
            secret.passphrase = Some(passphrase);
        }

        let mut start_dir = s(b, "startDirectory").to_string();
        if start_dir.is_empty() {
            start_dir = s(b, "startDirectoryRemote").to_string();
        }
        let startup_command = b
            .get("runScripts")
            .and_then(Value::as_array)
            .map(|scripts| {
                scripts
                    .iter()
                    .map(|r| s(r, "script"))
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        for (key, label) in [
            ("connectionHoppings", t("jump hosts")),
            ("sshTunnels", t("port forwarding")),
        ] {
            if b.get(key)
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
            {
                dropped_features.insert(label);
            }
        }
        if !s(b, "proxy").is_empty() {
            dropped_features.insert(t("proxies"));
        }

        let id = bookmark_uuid(&eid);
        let username = {
            let u = field("username");
            if u.is_empty() { field("user") } else { u }
        };
        bookmarks.push(Bookmark {
            id,
            name: s(b, "title").to_string(),
            host: host.to_string(),
            port: port(b),
            username,
            auth,
            group: group_of.get(&eid).and_then(|g| local_group(g)),
            tags: Vec::new(),
            notes: s(b, "description").to_string(),
            start_dir,
            startup_command,
            legacy_algorithms: false,
        });
        if !secret.is_empty() {
            secrets.insert(id, secret);
        }
    }

    let mut warnings = Vec::new();
    if !skipped_types.is_empty() {
        skipped_types.sort();
        skipped_types.dedup();
        warnings.push(tf(
            "Skipped bookmarks that are not SSH connections: {}.",
            &[&skipped_types.join(", ")],
        ));
    }
    if !dropped_features.is_empty() {
        let mut f: Vec<_> = dropped_features.into_iter().collect();
        f.sort();
        warnings.push(tf(
            "These Electerm settings are not supported and were left out: {}.",
            &[&f.join(", ")],
        ));
    }
    if !secrets.is_empty() {
        warnings.push(
            t("Passwords and keys from the file will be stored in the macOS Keychain.").into(),
        );
    }
    if bookmarks.is_empty() {
        bail!("{}", t("The Electerm file contains no SSH bookmarks."));
    }

    Ok(Incoming {
        kind: SourceKind::Electerm,
        groups,
        bookmarks,
        secrets,
        encrypted_credentials: None,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bookmarks::BookmarkStore;
    use crate::bookmarks::transfer::{ImportPlan, read_import};

    const SAMPLE: &str = include_str!("../../tests/fixtures/electerm-sample.json");

    #[test]
    fn sample_export_maps_groups_bookmarks_and_secrets() {
        let incoming = read_import(SAMPLE.as_bytes()).unwrap();
        assert_eq!(incoming.kind, SourceKind::Electerm);
        let names: Vec<_> = incoming.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["Production", "Databases"],
            "'default' maps to the top level"
        );
        assert_eq!(incoming.groups[1].parent, Some(incoming.groups[0].id));

        let bm: Vec<_> = incoming.bookmarks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            bm,
            [
                "web-01 (password)",
                "pg-primary (inline key + passphrase)",
                "pg-replica (key file path)"
            ]
        );
        let web = &incoming.bookmarks[0];
        assert_eq!(web.group, Some(incoming.groups[0].id));
        assert_eq!(web.start_dir, "/var/www/site");
        assert_eq!(web.startup_command, "sudo -i");
        assert_eq!(web.username, "deploy");
        assert_eq!(
            incoming.secrets[&web.id].password.as_deref(),
            Some("Sup3r$ecret-w3b")
        );

        let pg = &incoming.bookmarks[1];
        assert_eq!(pg.auth, Auth::KeychainKey);
        assert_eq!(pg.port, 2222);
        assert_eq!(pg.group, Some(incoming.groups[1].id));
        let pg_secret = &incoming.secrets[&pg.id];
        assert!(
            pg_secret
                .private_key
                .as_deref()
                .unwrap()
                .starts_with("-----BEGIN OPENSSH")
        );
        assert_eq!(pg_secret.passphrase.as_deref(), Some("k3y-passphrase"));
        assert!(pg_secret.password.is_none());

        assert_eq!(
            incoming.bookmarks[2].auth,
            Auth::Key {
                path: "~/.ssh/db_ed25519".into()
            }
        );
        assert!(
            incoming
                .warnings
                .iter()
                .any(|w| w.contains("local") && w.contains("telnet"))
        );
        assert!(incoming.warnings.iter().any(|w| w.contains("jump hosts")));
    }

    #[test]
    fn reimport_is_identical_and_bare_arrays_work() {
        let mut store = BookmarkStore::default();
        let plan = ImportPlan::new(read_import(SAMPLE.as_bytes()).unwrap(), &store);
        let out = plan.apply(&mut store);
        assert_eq!(out.added, 3);
        assert_eq!(out.secrets.len(), 2);
        // Second import: same ids, nothing new. Secrets make them "conflicts" (update offered).
        let plan = ImportPlan::new(read_import(SAMPLE.as_bytes()).unwrap(), &store);
        assert_eq!(plan.summary().0, 0);

        let arr = r#"[{"title":"a","host":"h1","port":"2200","username":"u"},{"host":"h2"}]"#;
        let inc = read_import(arr.as_bytes()).unwrap();
        assert_eq!(inc.bookmarks.len(), 2);
        assert_eq!(inc.bookmarks[0].port, 2200);
        assert_eq!(inc.bookmarks[1].display_name(), "h2");
    }

    #[test]
    fn profiles_and_legacy_passwords() {
        let enc: String = "pw!"
            .encode_utf16()
            .enumerate()
            .map(|(i, c)| char::from_u32(c as u32 + i as u32 + 1).unwrap())
            .collect();
        let data = serde_json::json!({
            "bookmarks": [
                {"id": "a", "host": "h", "authType": "profiles", "profile": "p1"},
                {"id": "b", "host": "h2", "password": enc, "passwordEncrypted": true}
            ],
            "bookmarkGroups": [],
            "profiles": [{"id": "p1", "username": "prof-user", "password": "prof-pw"}]
        });
        let inc = read(&data).unwrap();
        assert_eq!(inc.bookmarks[0].username, "prof-user");
        assert_eq!(
            inc.secrets[&inc.bookmarks[0].id].password.as_deref(),
            Some("prof-pw")
        );
        assert_eq!(
            inc.secrets[&inc.bookmarks[1].id].password.as_deref(),
            Some("pw!")
        );
        assert_eq!(legacy_decode(""), "");
    }

    #[test]
    fn cyclic_groups_do_not_hang() {
        let data = serde_json::json!({
            "bookmarkGroups": [
                {"id": "x", "title": "X", "bookmarkGroupIds": ["y"], "bookmarkIds": ["b"]},
                {"id": "y", "title": "Y", "bookmarkGroupIds": ["x"]}
            ],
            "bookmarks": [{"id": "b", "host": "h"}]
        });
        let inc = read(&data).unwrap();
        assert_eq!(inc.groups.len(), 2);
        let mut store = BookmarkStore::default();
        ImportPlan::new(inc, &store).apply(&mut store);
        assert_eq!(store.groups.len(), 2);
        assert!(store.groups.iter().any(|g| g.parent.is_none()));
    }
}
