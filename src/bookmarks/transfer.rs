//! Bookmark export and import.
//!
//! Export writes one versioned JSON file. Credentials are left out unless the user opts in, and
//! then they are encrypted with a passphrase (see `crate::crypto`). Import never overwrites
//! silently: it builds an [`ImportPlan`] that the UI shows as a preview, with a per-bookmark
//! choice for every conflict, and only [`ImportPlan::apply`] changes the store.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::{Bookmark, Group};
use super::store::BookmarkStore;
use super::{electerm, ssh_config};
use crate::crypto::{self, EncryptedBlob};
use crate::i18n::{t, tf};
use crate::secrets::SecretSet;

pub const EXPORT_SCHEMA_VERSION: u32 = 1;
pub const EXPORT_APP: &str = "peler";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportFile {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    #[serde(default)]
    pub app: String,
    pub groups: Vec<Group>,
    pub bookmarks: Vec<Bookmark>,
    /// Encrypted `BTreeMap<Uuid, SecretSet>`; absent when credentials were not exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<EncryptedBlob>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportScope {
    All,
    /// A group and everything nested in it.
    Group(Uuid),
    Selected(Vec<Uuid>),
}

/// Builds an export. `secrets` provides each exported bookmark's credentials and is only called
/// when `passphrase` is given.
pub fn export(
    store: &BookmarkStore,
    scope: &ExportScope,
    passphrase: Option<&str>,
    mut secrets: impl FnMut(Uuid) -> SecretSet,
) -> Result<ExportFile> {
    let bookmarks: Vec<Bookmark> = match scope {
        ExportScope::All => store.bookmarks.clone(),
        ExportScope::Group(gid) => {
            let set = store.group_and_descendants(*gid);
            store.bookmarks.iter().filter(|b| b.group.is_some_and(|g| set.contains(&g))).cloned().collect()
        }
        ExportScope::Selected(ids) => {
            store.bookmarks.iter().filter(|b| ids.contains(&b.id)).cloned().collect()
        }
    };

    // Keep every group on the path to an exported bookmark so the tree survives the trip,
    // plus all sub-groups of an exported group (even empty ones).
    let mut keep: HashSet<Uuid> = match scope {
        ExportScope::All => store.groups.iter().map(|g| g.id).collect(),
        ExportScope::Group(gid) => store.group_and_descendants(*gid),
        ExportScope::Selected(_) => HashSet::new(),
    };
    let starts: Vec<Uuid> = bookmarks.iter().filter_map(|b| b.group).chain(keep.clone()).collect();
    for start in starts {
        let mut cur = Some(start);
        while let Some(gid) = cur {
            if !keep.insert(gid) && gid != start {
                break;
            }
            cur = store.group(gid).and_then(|g| g.parent);
        }
    }
    let groups: Vec<Group> = store.groups.iter().filter(|g| keep.contains(&g.id)).cloned().collect();

    let credentials = match passphrase {
        Some(pass) => {
            let map: BTreeMap<Uuid, SecretSet> = bookmarks
                .iter()
                .map(|b| (b.id, secrets(b.id)))
                .filter(|(_, s)| !s.is_empty())
                .collect();
            let plain = serde_json::to_vec(&map)?;
            Some(crypto::encrypt(pass, &plain)?)
        }
        None => None,
    };

    Ok(ExportFile {
        schema_version: EXPORT_SCHEMA_VERSION,
        exported_at: Utc::now(),
        app: EXPORT_APP.into(),
        groups,
        bookmarks,
        credentials,
    })
}

// ----- import ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Peler,
    Electerm,
    SshConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchedBy {
    Id,
    /// Same host, port and user.
    Endpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    New,
    /// Already present with the same content: nothing to do.
    Identical { existing: Uuid },
    Conflict { existing: Uuid, matched_by: MatchedBy },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Add,
    Skip,
    Overwrite,
    KeepBoth,
}

#[derive(Clone, Debug)]
pub struct ImportItem {
    pub incoming: Bookmark,
    /// Group path of the incoming bookmark, for display.
    pub group_path: String,
    pub secrets: SecretSet,
    pub status: ItemStatus,
    pub resolution: Resolution,
}

/// Parsed import file, before the user decides how to resolve conflicts.
#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub kind: SourceKind,
    /// Incoming groups in file order (ids as in the file).
    pub groups: Vec<Group>,
    pub items: Vec<ImportItem>,
    /// Encrypted credentials still waiting for the passphrase.
    pub encrypted_credentials: Option<EncryptedBlob>,
    pub warnings: Vec<String>,
}

/// A parsed but not yet planned import (format-specific readers produce this).
#[derive(Debug)]
pub struct Incoming {
    pub kind: SourceKind,
    pub groups: Vec<Group>,
    pub bookmarks: Vec<Bookmark>,
    pub secrets: HashMap<Uuid, SecretSet>,
    pub encrypted_credentials: Option<EncryptedBlob>,
    pub warnings: Vec<String>,
}

/// Detects the file format and parses it. `ssh_config_name` hints that a non-JSON file should
/// be read as an OpenSSH config.
pub fn read_import(data: &[u8]) -> Result<Incoming> {
    let text = std::str::from_utf8(data).map_err(|_| anyhow!("{}", t("The file is not UTF-8 text.")))?;
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(json_err) => {
            let hosts = ssh_config::parse_str(text);
            if !hosts.is_empty() {
                return Ok(from_ssh_config(&hosts));
            }
            bail!("{}", tf("The file is not valid JSON or an SSH config file: {}", &[&json_err.to_string()]));
        }
    };
    if value.get("schema_version").is_some() {
        return read_peler(value);
    }
    if electerm::looks_like_electerm(&value) {
        return electerm::read(&value);
    }
    bail!("{}", t("Unrecognized file format: expected a peler export, an Electerm bookmark export or an SSH config."))
}

fn read_peler(value: serde_json::Value) -> Result<Incoming> {
    let version = value.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0);
    if version > EXPORT_SCHEMA_VERSION as u64 {
        bail!(
            "{}",
            tf(
                "This file was exported by a newer version of peler (schema {}); this version reads up to schema {}. Please update the app.",
                &[&version.to_string(), &EXPORT_SCHEMA_VERSION.to_string()]
            )
        );
    }
    if version == 0 {
        bail!("Invalid schema_version in export file.");
    }
    let file: ExportFile = serde_json::from_value(value)
        .map_err(|e| anyhow!("{}", tf("The export file is damaged or incomplete: {}", &[&e.to_string()])))?;
    let mut warnings = Vec::new();
    let ids: HashSet<Uuid> = file.groups.iter().map(|g| g.id).collect();
    if file.bookmarks.iter().any(|b| b.group.is_some_and(|g| !ids.contains(&g))) {
        warnings.push(t("Some bookmarks refer to missing groups; they will be placed at the top level.").into());
    }
    Ok(Incoming {
        kind: SourceKind::Peler,
        groups: file.groups,
        bookmarks: file.bookmarks,
        secrets: HashMap::new(),
        encrypted_credentials: file.credentials,
        warnings,
    })
}

/// Converts `~/.ssh/config` hosts into bookmarks inside an "SSH config" group.
pub fn from_ssh_config(hosts: &[ssh_config::SshConfigHost]) -> Incoming {
    let group = Group::new("SSH config", None);
    let bookmarks = hosts
        .iter()
        .map(|h| Bookmark {
            // Stable id per alias so re-importing the same config matches by id.
            id: Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("ssh-config:{}", h.alias).as_bytes()),
            name: h.alias.clone(),
            host: h.hostname.clone().unwrap_or_else(|| h.alias.clone()),
            port: h.port.unwrap_or(super::DEFAULT_PORT),
            username: h.user.clone().unwrap_or_default(),
            auth: match &h.identity_file {
                Some(p) => super::Auth::Key { path: p.clone() },
                None => super::Auth::Password,
            },
            group: Some(group.id),
            ..Default::default()
        })
        .collect();
    Incoming {
        kind: SourceKind::SshConfig,
        groups: vec![group],
        bookmarks,
        secrets: HashMap::new(),
        encrypted_credentials: None,
        warnings: Vec::new(),
    }
}

impl ImportPlan {
    /// Compares incoming bookmarks with the store and proposes a default resolution: new ones
    /// are added, identical ones skipped, and conflicts skipped until the user decides.
    pub fn new(incoming: Incoming, store: &BookmarkStore) -> Self {
        let group_map = map_groups(&incoming.groups, store);
        let mut temp = store.clone();
        let names: HashMap<Uuid, String> = {
            for g in &incoming.groups {
                if !temp.groups.iter().any(|x| x.id == group_map[&g.id]) {
                    temp.groups.push(Group {
                        id: group_map[&g.id],
                        name: g.name.clone(),
                        parent: g.parent.and_then(|p| group_map.get(&p).copied()),
                    });
                }
            }
            incoming.groups.iter().map(|g| (g.id, temp.group_path(Some(group_map[&g.id])))).collect()
        };

        let mut items = Vec::new();
        let mut endpoint_taken = HashSet::new();
        for mut b in incoming.bookmarks {
            let group_path = b.group.and_then(|g| names.get(&g).cloned()).unwrap_or_default();
            b.group = b.group.and_then(|g| group_map.get(&g).copied());
            let secrets = incoming.secrets.get(&b.id).cloned().unwrap_or_default();
            let existing = store.bookmark(b.id).map(|e| (e, MatchedBy::Id)).or_else(|| {
                store
                    .bookmarks
                    .iter()
                    .find(|e| e.same_endpoint(&b) && !endpoint_taken.contains(&e.id))
                    .map(|e| (e, MatchedBy::Endpoint))
            });
            let status = match existing {
                Some((e, by)) => {
                    endpoint_taken.insert(e.id);
                    let same = Bookmark { id: e.id, ..b.clone() } == *e;
                    if same && secrets.is_empty() {
                        ItemStatus::Identical { existing: e.id }
                    } else {
                        ItemStatus::Conflict { existing: e.id, matched_by: by }
                    }
                }
                None => ItemStatus::New,
            };
            let resolution = match status {
                ItemStatus::New => Resolution::Add,
                _ => Resolution::Skip,
            };
            items.push(ImportItem { incoming: b, group_path, secrets, status, resolution });
        }

        ImportPlan {
            kind: incoming.kind,
            groups: incoming.groups,
            items,
            encrypted_credentials: incoming.encrypted_credentials,
            warnings: incoming.warnings,
        }
    }

    /// Decrypts the exported credentials and attaches them to the matching items.
    pub fn unlock_credentials(&mut self, passphrase: &str) -> Result<usize> {
        let Some(blob) = &self.encrypted_credentials else { return Ok(0) };
        let plain = crypto::decrypt(passphrase, blob)?;
        let map: BTreeMap<Uuid, SecretSet> = serde_json::from_slice(&plain)
            .map_err(|e| anyhow!("Decrypted credentials are malformed: {e}"))?;
        let mut n = 0;
        for item in &mut self.items {
            if let Some(s) = map.get(&item.incoming.id) {
                item.secrets = s.clone();
                n += 1;
                // Identical bookmark but new credentials: offer to update.
                if let ItemStatus::Identical { existing } = item.status {
                    item.status = ItemStatus::Conflict { existing, matched_by: MatchedBy::Id };
                }
            }
        }
        self.encrypted_credentials = None;
        Ok(n)
    }

    pub fn conflicts(&self) -> usize {
        self.items.iter().filter(|i| matches!(i.status, ItemStatus::Conflict { .. })).count()
    }

    /// Sets the same resolution on every conflicting item.
    pub fn resolve_all_conflicts(&mut self, r: Resolution) {
        for item in &mut self.items {
            if matches!(item.status, ItemStatus::Conflict { .. }) {
                item.resolution = r;
            }
        }
    }

    /// Applies the plan. Returns counts and the secrets the caller must store in the Keychain
    /// (keyed by the bookmark id they belong to after import).
    pub fn apply(&self, store: &mut BookmarkStore) -> ImportOutcome {
        let group_map = map_groups(&self.groups, store);
        for g in &self.groups {
            let id = group_map[&g.id];
            if store.group(id).is_none() {
                store.groups.push(Group {
                    id,
                    name: g.name.clone(),
                    parent: g.parent.and_then(|p| group_map.get(&p).copied()),
                });
            }
        }

        let mut out = ImportOutcome::default();
        for item in &self.items {
            let mut b = item.incoming.clone();
            let target_id = match (&item.status, item.resolution) {
                (_, Resolution::Skip) | (ItemStatus::Identical { .. }, _) => {
                    out.skipped += 1;
                    continue;
                }
                (ItemStatus::Conflict { existing, .. }, Resolution::Overwrite) => {
                    // Keep the existing id so its Keychain items stay attached.
                    b.id = *existing;
                    store.upsert_bookmark(b);
                    out.updated += 1;
                    *existing
                }
                (_, Resolution::KeepBoth | Resolution::Add | Resolution::Overwrite) => {
                    if store.bookmark(b.id).is_some() {
                        b.id = Uuid::new_v4();
                    }
                    let id = b.id;
                    store.bookmarks.push(b);
                    out.added += 1;
                    id
                }
            };
            if !item.secrets.is_empty() {
                out.secrets.push((target_id, item.secrets.clone()));
            }
        }
        store.sanitize();
        out
    }

    /// Counts for the preview: (added, changed, skipped).
    pub fn summary(&self) -> (usize, usize, usize) {
        let (mut add, mut change, mut skip) = (0, 0, 0);
        for i in &self.items {
            match (&i.status, i.resolution) {
                (ItemStatus::Identical { .. }, _) | (_, Resolution::Skip) => skip += 1,
                (ItemStatus::Conflict { .. }, Resolution::Overwrite) => change += 1,
                _ => add += 1,
            }
        }
        (add, change, skip)
    }
}

#[derive(Debug, Default)]
pub struct ImportOutcome {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    pub secrets: Vec<(Uuid, SecretSet)>,
}

/// Maps incoming group ids to local ids: same id if it exists locally, otherwise a local group
/// with the same name under the same (mapped) parent, otherwise the incoming id (or a fresh id
/// when that id is taken by a different group).
fn map_groups(groups: &[Group], store: &BookmarkStore) -> HashMap<Uuid, Uuid> {
    let mut map: HashMap<Uuid, Uuid> = HashMap::new();
    let incoming_ids: HashSet<Uuid> = groups.iter().map(|g| g.id).collect();
    // Resolve parents before children regardless of file order.
    let mut pending: Vec<&Group> = groups.iter().collect();
    let mut guard = 0;
    while !pending.is_empty() && guard <= groups.len() {
        guard += 1;
        pending.retain(|g| {
            let parent_ready = g.parent.is_none_or(|p| map.contains_key(&p) || !incoming_ids.contains(&p));
            if !parent_ready {
                return true;
            }
            let parent = g.parent.and_then(|p| map.get(&p).copied());
            let local = if store.group(g.id).is_some() {
                g.id
            } else if let Some(same) =
                store.groups.iter().find(|x| x.parent == parent && x.name == g.name)
            {
                same.id
            } else {
                g.id
            };
            map.insert(g.id, local);
            false
        });
    }
    for g in pending {
        map.insert(g.id, g.id); // cycle in the file: sanitize() will detach it
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bookmarks::Auth;

    fn sample_store() -> BookmarkStore {
        let mut s = BookmarkStore::default();
        let prod = s.add_group("Production", None);
        let db = s.add_group("Databases", Some(prod));
        let _empty = s.add_group("Empty", Some(prod));
        s.add_group("Staging", None);
        s.upsert_bookmark(Bookmark {
            name: "web-1".into(),
            host: "10.0.0.1".into(),
            username: "deploy".into(),
            group: Some(prod),
            tags: vec!["nginx".into()],
            notes: "front".into(),
            start_dir: "/var/www".into(),
            startup_command: "uptime".into(),
            ..Default::default()
        });
        s.upsert_bookmark(Bookmark {
            name: "pg".into(),
            host: "db.internal".into(),
            port: 2222,
            username: "postgres".into(),
            auth: Auth::Key { path: "~/.ssh/id_ed25519".into() },
            group: Some(db),
            ..Default::default()
        });
        s.upsert_bookmark(Bookmark { name: "loose".into(), host: "1.2.3.4".into(), ..Default::default() });
        s
    }

    fn secrets_for(store: &BookmarkStore) -> HashMap<Uuid, SecretSet> {
        let mut m = HashMap::new();
        m.insert(store.bookmarks[0].id, SecretSet { password: Some("hunter2".into()), ..Default::default() });
        m.insert(store.bookmarks[1].id, SecretSet { passphrase: Some("key pass".into()), ..Default::default() });
        m
    }

    #[test]
    fn round_trip_with_encrypted_credentials_is_identical() {
        let src = sample_store();
        let secrets = secrets_for(&src);
        let file = export(&src, &ExportScope::All, Some("correct horse"), |id| {
            secrets.get(&id).cloned().unwrap_or_default()
        })
        .unwrap();
        let json = serde_json::to_vec_pretty(&file).unwrap();
        let text = String::from_utf8(json.clone()).unwrap();
        assert!(!text.contains("hunter2") && !text.contains("key pass"), "secrets must be encrypted");

        let mut dst = BookmarkStore::default();
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &dst);
        assert!(plan.unlock_credentials("wrong").is_err());
        assert_eq!(plan.unlock_credentials("correct horse").unwrap(), 2);
        let out = plan.apply(&mut dst);
        assert_eq!((out.added, out.updated, out.skipped), (3, 0, 0));
        assert_eq!(dst, src, "store after import must equal the exported store");
        let got: HashMap<Uuid, SecretSet> = out.secrets.into_iter().collect();
        assert_eq!(got, secrets);

        // Importing the same file again changes nothing.
        let plan = ImportPlan::new(read_import(&json).unwrap(), &dst);
        assert_eq!(plan.summary(), (0, 0, 3));
        let before = dst.clone();
        plan.apply(&mut dst);
        assert_eq!(dst, before);
    }

    #[test]
    fn export_without_passphrase_has_no_credentials() {
        let src = sample_store();
        let file = export(&src, &ExportScope::All, None, |_| panic!("must not read secrets")).unwrap();
        assert!(file.credentials.is_none());
        let v = serde_json::to_value(&file).unwrap();
        for key in ["schema_version", "exported_at", "groups", "bookmarks"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn export_scopes() {
        let src = sample_store();
        let prod = src.groups[0].id;
        let db = src.groups[1].id;
        let f = export(&src, &ExportScope::Group(db), None, |_| SecretSet::default()).unwrap();
        assert_eq!(f.bookmarks.len(), 1);
        let gids: Vec<_> = f.groups.iter().map(|g| g.id).collect();
        assert_eq!(gids, [prod, db], "ancestors are kept so the path survives");

        let f = export(&src, &ExportScope::Group(prod), None, |_| SecretSet::default()).unwrap();
        assert_eq!(f.bookmarks.len(), 2);
        assert_eq!(f.groups.len(), 3, "includes the empty sub-group");

        let loose = src.bookmarks[2].id;
        let f = export(&src, &ExportScope::Selected(vec![loose]), None, |_| SecretSet::default()).unwrap();
        assert_eq!(f.bookmarks.len(), 1);
        assert!(f.groups.is_empty());
    }

    #[test]
    fn conflicts_by_id_and_endpoint() {
        let mut local = sample_store();
        let src = local.clone();
        // Changed copy of web-1 (same id) and a different-id bookmark to the same endpoint as pg.
        local.bookmarks[0].notes = "local edit".into();
        let pg_endpoint_twin =
            Bookmark { id: Uuid::new_v4(), notes: "twin".into(), ..src.bookmarks[1].clone() };
        let file = ExportFile {
            schema_version: 1,
            exported_at: Utc::now(),
            app: "peler".into(),
            groups: src.groups.clone(),
            bookmarks: vec![src.bookmarks[0].clone(), pg_endpoint_twin.clone()],
            credentials: None,
        };
        let json = serde_json::to_vec(&file).unwrap();
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &local);
        assert_eq!(plan.conflicts(), 2);
        assert!(matches!(plan.items[0].status, ItemStatus::Conflict { matched_by: MatchedBy::Id, .. }));
        assert!(matches!(plan.items[1].status, ItemStatus::Conflict { matched_by: MatchedBy::Endpoint, .. }));
        assert_eq!(plan.summary(), (0, 0, 2), "conflicts default to skip");

        plan.items[0].resolution = Resolution::Overwrite;
        plan.items[1].resolution = Resolution::KeepBoth;
        assert_eq!(plan.summary(), (1, 1, 0));
        let n = local.bookmarks.len();
        let out = plan.apply(&mut local);
        assert_eq!((out.added, out.updated), (1, 1));
        assert_eq!(local.bookmarks.len(), n + 1);
        assert_eq!(local.bookmarks[0].notes, "front");
        assert!(local.bookmark(pg_endpoint_twin.id).is_some());
    }

    #[test]
    fn rejects_newer_schema_and_garbage() {
        let err = read_import(br#"{"schema_version": 7, "groups": [], "bookmarks": []}"#).unwrap_err();
        assert!(err.to_string().contains("newer"), "{err}");
        let err = read_import(br#"{"schema_version": 1, "bookmarks": [{"name": 3}]}"#).unwrap_err();
        assert!(err.to_string().contains("damaged"), "{err}");
        assert!(read_import(b"\x00\xff").is_err());
        assert!(read_import(b"{\"hello\": 1}").is_err());
    }

    #[test]
    fn groups_merge_by_name() {
        let mut local = BookmarkStore::default();
        let prod = local.add_group("Production", None);
        let src = sample_store();
        let json = serde_json::to_vec(&export(&src, &ExportScope::All, None, |_| SecretSet::default()).unwrap()).unwrap();
        let plan = ImportPlan::new(read_import(&json).unwrap(), &local);
        plan.apply(&mut local);
        assert_eq!(local.child_groups(None).filter(|g| g.name == "Production").count(), 1);
        assert_eq!(local.bookmarks[0].group, Some(prod));
        assert_eq!(local.child_groups(Some(prod)).count(), 2);
    }

    #[test]
    fn ssh_config_import_is_idempotent() {
        let hosts = ssh_config::parse_str("Host a\n HostName 1.1.1.1\n User u\n IdentityFile ~/.ssh/k\n");
        let mut store = BookmarkStore::default();
        ImportPlan::new(from_ssh_config(&hosts), &store).apply(&mut store);
        assert_eq!(store.bookmarks.len(), 1);
        assert_eq!(store.bookmarks[0].auth, Auth::Key { path: "~/.ssh/k".into() });
        let plan = ImportPlan::new(from_ssh_config(&hosts), &store);
        assert_eq!(plan.summary(), (0, 0, 1));
    }
}
