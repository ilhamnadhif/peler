//! Credentials in the system's secret store: the Keychain on macOS, Credential Manager on
//! Windows, the Secret Service (GNOME Keyring, KWallet, KeePassXC …) on Linux. Nothing secret is
//! ever written to the bookmark file.
//!
//! Each bookmark can have up to three secrets, addressed as `<bookmark-id>/<kind>` under service
//! `ternaryshell`. On Windows and Linux each is an item of its own. On macOS they all share one
//! Keychain item, the [`Vault`]: macOS asks for permission for each item an app reads, and asks
//! again after every update of an app without a Developer ID signature, so one item per secret
//! meant a password prompt for every connection opened after an update.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::i18n::tf;

pub const SERVICE: &str = "ternaryshell";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    Passphrase,
    PrivateKey,
}

impl SecretKind {
    pub const ALL: [SecretKind; 3] = [
        SecretKind::Password,
        SecretKind::Passphrase,
        SecretKind::PrivateKey,
    ];

    fn suffix(self) -> &'static str {
        match self {
            SecretKind::Password => "password",
            SecretKind::Passphrase => "passphrase",
            SecretKind::PrivateKey => "private-key",
        }
    }
}

/// The secrets belonging to one bookmark (used for export/import and the editor).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
}

impl SecretSet {
    pub fn is_empty(&self) -> bool {
        self.password.is_none() && self.passphrase.is_none() && self.private_key.is_none()
    }

    pub fn get(&self, kind: SecretKind) -> Option<&String> {
        match kind {
            SecretKind::Password => self.password.as_ref(),
            SecretKind::Passphrase => self.passphrase.as_ref(),
            SecretKind::PrivateKey => self.private_key.as_ref(),
        }
    }

    pub fn set(&mut self, kind: SecretKind, value: Option<String>) {
        match kind {
            SecretKind::Password => self.password = value,
            SecretKind::Passphrase => self.passphrase = value,
            SecretKind::PrivateKey => self.private_key = value,
        }
    }
}

impl std::fmt::Display for SecretKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.suffix())
    }
}

fn account(id: Uuid, kind: SecretKind) -> String {
    format!("{id}/{}", kind.suffix())
}

// ----- stores ---------------------------------------------------------------------------------

/// Items by account name.
trait Store: Send + Sync {
    /// The item's value; `None` when there is no such item.
    fn read(&self, account: &str) -> Result<Option<String>>;
    /// Stores `value` under `account`; `None` deletes the item (a missing item is fine).
    fn write(&self, account: &str, value: Option<&str>) -> Result<()>;
}

fn store_error(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{}: {e}", crate::platform::SECRET_STORE)
}

/// The system's secret store, under service `ternaryshell`.
struct System;

/// The login Keychain, through Security.framework. Each build of an app without a Developer ID
/// signature is a new app to it: a build may read an item, or delete it, only once macOS was
/// told to trust that build for the item ("Always Allow"; "Allow" lets one read through).
/// Writing goes through the item's reference without reading it, which macOS allows any build
/// without asking.
#[cfg(target_os = "macos")]
impl Store for System {
    fn read(&self, account: &str) -> Result<Option<String>> {
        use security_framework::os::macos::passwords::find_generic_password;
        match find_generic_password(None, SERVICE, account) {
            Ok((data, _)) => String::from_utf8(data.to_owned())
                .map(Some)
                .map_err(store_error),
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }

    fn write(&self, account: &str, value: Option<&str>) -> Result<()> {
        use security_framework::item::{ItemClass, ItemSearchOptions, Reference, SearchResult};
        let mut search = ItemSearchOptions::new();
        search
            .class(ItemClass::generic_password())
            .service(SERVICE)
            .account(account);
        let Some(value) = value else {
            // Fails for an item of an older build that macOS wasn't told to trust.
            return match search.delete() {
                Err(e) if e.code() != ITEM_NOT_FOUND => Err(store_error(e)),
                _ => Ok(()),
            };
        };
        match search.load_refs(true).search() {
            Ok(found) => {
                for result in found {
                    if let SearchResult::Ref(Reference::KeychainItem(mut item)) = result {
                        return item.set_password(value.as_bytes()).map_err(store_error);
                    }
                }
            }
            Err(e) if e.code() == ITEM_NOT_FOUND => {}
            Err(e) => return Err(store_error(e)),
        }
        security_framework::os::macos::keychain::SecKeychain::default()
            .and_then(|k| k.add_generic_password(SERVICE, account, value.as_bytes()))
            .map(|_| ())
            .map_err(store_error)
    }
}

/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const ITEM_NOT_FOUND: i32 = -25300;

#[cfg(not(target_os = "macos"))]
impl Store for System {
    fn read(&self, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(store_error)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }

    fn write(&self, account: &str, value: Option<&str>) -> Result<()> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(store_error)?;
        let result = match value {
            Some(v) => entry.set_password(v),
            None => match entry.delete_credential() {
                Err(keyring::Error::NoEntry) => Ok(()),
                other => other,
            },
        };
        result.map_err(store_error)
    }
}

/// Process-local store used by tests (`TERNARYSHELL_SECRETS=memory`, debug builds only).
#[derive(Default)]
struct Memory(Mutex<HashMap<String, String>>);

impl Store for Memory {
    fn read(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().get(account).cloned())
    }

    fn write(&self, account: &str, value: Option<&str>) -> Result<()> {
        let mut items = self.0.lock();
        match value {
            Some(v) => items.insert(account.to_string(), v.to_string()),
            None => items.remove(account),
        };
        Ok(())
    }
}

enum Backend {
    /// One item per secret.
    Items(Box<dyn Store>),
    /// Every secret in one item (macOS).
    Vault(Mutex<Vault>),
}

fn backend() -> &'static Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    BACKEND.get_or_init(|| {
        // The in-memory store is for tests only; release builds always use the system's.
        if cfg!(test)
            || (cfg!(debug_assertions)
                && std::env::var("TERNARYSHELL_SECRETS").is_ok_and(|v| v == "memory"))
        {
            Backend::Items(Box::new(Memory::default()))
        } else if cfg!(target_os = "macos") {
            Backend::Vault(Mutex::new(Vault::new(Box::new(System))))
        } else {
            // Neither Credential Manager nor the Secret Service asks for each item, and a
            // Credential Manager item holds at most 2.5 KB: too little to share.
            Backend::Items(Box::new(System))
        }
    })
}

/// Changes several secrets at once (`None` removes one).
fn write_all(changes: &[(String, Option<&str>)]) -> Result<()> {
    match backend() {
        Backend::Items(store) => {
            for (account, value) in changes {
                store.write(account, *value)?;
            }
            Ok(())
        }
        Backend::Vault(vault) => vault.lock().write(changes),
    }
}

// ----- the vault (macOS) ----------------------------------------------------------------------

/// The account of the vault's own item.
const VAULT: &str = "vault";
const VAULT_VERSION: u32 = 1;

/// What the vault's item holds (with `version`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Contents {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    /// Accounts whose item from before the vault is still there but no longer counts: this
    /// build couldn't delete it. It is never read again.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    retired: BTreeSet<String>,
}

#[derive(Serialize)]
struct VaultFile<'a> {
    version: u32,
    #[serde(flatten)]
    contents: &'a Contents,
}

/// Every secret in one item, read once per run and kept in memory: after an update macOS asks
/// for permission once, however many connections are then opened. Secrets that earlier versions
/// kept in items of their own move into the vault when first used (macOS asks one last time for
/// each); those items are deleted when macOS lets this build, and are never read again either
/// way.
struct Vault {
    store: Box<dyn Store>,
    /// The vault's contents, once its item was read.
    contents: Option<Contents>,
    /// Accounts known to have no item of their own (any more), in this run.
    no_own_item: HashSet<String>,
}

impl Vault {
    fn new(store: Box<dyn Store>) -> Self {
        Self {
            store,
            contents: None,
            no_own_item: HashSet::new(),
        }
    }

    /// The contents, read on first use. A vault that can't be read (permission denied, damaged,
    /// from a newer version) is tried again next time, and never overwritten meanwhile.
    fn contents(&mut self) -> Result<&Contents> {
        if self.contents.is_none() {
            let contents = match self.store.read(VAULT)? {
                None => Contents::default(),
                Some(text) => parse_vault(&text)?,
            };
            self.contents = Some(contents);
        }
        Ok(self.contents.get_or_insert_default())
    }

    fn read(&mut self, account: &str) -> Result<Option<String>> {
        let contents = self.contents()?;
        if let Some(value) = contents.secrets.get(account) {
            return Ok(Some(value.clone()));
        }
        if contents.retired.contains(account) || self.no_own_item.contains(account) {
            return Ok(None);
        }
        // Saved by an earlier version, in an item of its own: it moves into the vault.
        let Some(value) = self.store.read(account)? else {
            self.no_own_item.insert(account.to_string());
            return Ok(None);
        };
        let mut next = self.contents()?.clone();
        next.secrets.insert(account.to_string(), value.clone());
        // The old item goes once its secret is safely in the vault (if macOS lets this build).
        if self.save(next).is_ok() && self.store.write(account, None).is_ok() {
            self.no_own_item.insert(account.to_string());
        }
        Ok(Some(value))
    }

    /// Applies `changes` (`None` removes a secret) in one write of the vault.
    fn write(&mut self, changes: &[(String, Option<&str>)]) -> Result<()> {
        let mut next = self.contents()?.clone();
        for (account, value) in changes {
            match value {
                Some(v) => next.secrets.insert(account.clone(), v.to_string()),
                None => next.secrets.remove(account),
            };
            // A secret an earlier version kept in an item of its own must not come back: the
            // item is deleted, or (when macOS doesn't let this build) retired.
            if !self.no_own_item.contains(account) {
                if self.store.write(account, None).is_ok() {
                    self.no_own_item.insert(account.clone());
                    next.retired.remove(account);
                } else {
                    next.retired.insert(account.clone());
                }
            }
        }
        if self.contents.as_ref() != Some(&next) {
            self.save(next)?;
        }
        Ok(())
    }

    /// Writes the vault. Its item is never deleted (a build macOS doesn't trust couldn't): an
    /// empty vault is written as one.
    fn save(&mut self, contents: Contents) -> Result<()> {
        let file = VaultFile {
            version: VAULT_VERSION,
            contents: &contents,
        };
        self.store
            .write(VAULT, Some(&serde_json::to_string(&file)?))?;
        self.contents = Some(contents);
        Ok(())
    }
}

fn parse_vault(text: &str) -> Result<Contents> {
    let store = crate::platform::SECRET_STORE;
    let damaged = || {
        anyhow!(tf(
            "{}: the saved passwords and keys are damaged.",
            &[store]
        ))
    };
    // The version first: a newer vault may not read as this version's format.
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version: Version = serde_json::from_str(text).map_err(|_| damaged())?;
    if version.version > VAULT_VERSION {
        bail!(tf(
            "{}: the saved passwords and keys were written by a newer version of TernaryShell. Please update the app.",
            &[store],
        ));
    }
    serde_json::from_str(text).map_err(|_| damaged())
}

// ----- secrets of a bookmark ------------------------------------------------------------------

/// Reads one secret. A missing item is `Ok(None)`. Blocking: keep off the UI thread when the
/// Keychain might prompt.
pub fn get(id: Uuid, kind: SecretKind) -> Result<Option<String>> {
    let account = account(id, kind);
    match backend() {
        Backend::Items(store) => store.read(&account),
        Backend::Vault(vault) => vault.lock().read(&account),
    }
}

/// Stores (or replaces) one secret; `None` or an empty value deletes it.
pub fn set(id: Uuid, kind: SecretKind, value: Option<&str>) -> Result<()> {
    write_all(&[(account(id, kind), value.filter(|v| !v.is_empty()))])
}

pub fn has(id: Uuid, kind: SecretKind) -> bool {
    matches!(get(id, kind), Ok(Some(_)))
}

/// Like `load_set`, but a Keychain error is reported instead of read as "nothing saved".
pub fn try_load_set(id: Uuid) -> Result<SecretSet> {
    let mut s = SecretSet::default();
    for kind in SecretKind::ALL {
        s.set(kind, get(id, kind)?);
    }
    Ok(s)
}

pub fn load_set(id: Uuid) -> SecretSet {
    let mut s = SecretSet::default();
    for kind in SecretKind::ALL {
        s.set(kind, get(id, kind).ok().flatten());
    }
    s
}

/// Stores the secrets present in `set` (absent ones are left untouched; empty ones are
/// deleted).
pub fn store_set(id: Uuid, set: &SecretSet) -> Result<()> {
    let changes: Vec<(String, Option<&str>)> = SecretKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let value = set.get(kind)?;
            Some((
                account(id, kind),
                Some(value.as_str()).filter(|v| !v.is_empty()),
            ))
        })
        .collect();
    write_all(&changes)
}

/// Removes every secret of a bookmark (used when the bookmark is deleted).
pub fn delete_all(id: Uuid) {
    let changes = SecretKind::ALL.map(|kind| (account(id, kind), None));
    let _ = write_all(&changes);
}

/// Copies all secrets from one bookmark to another (the "Duplicate" action).
pub fn copy_all(from: Uuid, to: Uuid) {
    let s = load_set(from);
    let _ = store_set(to, &s);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn memory_backend_round_trip() {
        let id = Uuid::new_v4();
        assert_eq!(get(id, SecretKind::Password).unwrap(), None);
        set(id, SecretKind::Password, Some("pw")).unwrap();
        store_set(
            id,
            &SecretSet {
                passphrase: Some("pp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let other = Uuid::new_v4();
        copy_all(id, other);
        assert_eq!(
            load_set(other),
            SecretSet {
                password: Some("pw".into()),
                passphrase: Some("pp".into()),
                private_key: None
            }
        );
        set(id, SecretKind::Password, Some("")).unwrap();
        assert!(!has(id, SecretKind::Password));
        delete_all(other);
        assert!(load_set(other).is_empty());
    }

    /// The login Keychain as macOS treats the builds of an app without a Developer ID signature
    /// (as measured): each build is a new app. A build may read an item, or delete it, only when
    /// trusted for it: it made the item, or the user chose "Always Allow". Otherwise reading asks
    /// for the login password (what `asked` counts) and deleting fails without asking. Writing
    /// through the item's reference works for any build, without asking.
    #[derive(Clone)]
    struct Keychain {
        state: Arc<KeychainState>,
        build: u32,
    }

    #[derive(Default)]
    struct KeychainState {
        items: Mutex<BTreeMap<String, Item>>,
        asked: Mutex<Vec<String>>,
        /// The user's answer: "Always Allow", or "Allow" (Enter: this read only).
        always: AtomicBool,
        deny: AtomicBool,
    }

    struct Item {
        value: String,
        trusted: HashSet<u32>,
    }

    impl Keychain {
        fn new(always_allow: bool) -> Self {
            let state = KeychainState::default();
            state.always.store(always_allow, Ordering::Relaxed);
            Self {
                state: Arc::new(state),
                build: 1,
            }
        }

        /// With items an earlier build (0) saved, one per secret.
        fn with_old_items(always_allow: bool, items: &[(&str, &str)]) -> Self {
            let keychain = Self::new(always_allow);
            for (account, value) in items {
                keychain.state.items.lock().insert(
                    account.to_string(),
                    Item {
                        value: value.to_string(),
                        trusted: HashSet::from([0]),
                    },
                );
            }
            keychain
        }

        /// The app is opened (again), as the same build.
        fn launch(&self) -> Vault {
            self.state.asked.lock().clear();
            Vault::new(Box::new(self.clone()))
        }

        /// The app is updated to a new build, and opened.
        fn update(&mut self) -> Vault {
            self.build += 1;
            self.launch()
        }

        fn asked(&self) -> Vec<String> {
            self.state.asked.lock().clone()
        }

        fn value(&self, account: &str) -> Option<String> {
            self.state
                .items
                .lock()
                .get(account)
                .map(|i| i.value.clone())
        }
    }

    impl Store for Keychain {
        fn read(&self, account: &str) -> Result<Option<String>> {
            let mut items = self.state.items.lock();
            let Some(item) = items.get_mut(account) else {
                return Ok(None);
            };
            if !item.trusted.contains(&self.build) {
                if self.state.deny.load(Ordering::Relaxed) {
                    bail!("denied");
                }
                self.state.asked.lock().push(account.to_string());
                if self.state.always.load(Ordering::Relaxed) {
                    item.trusted.insert(self.build);
                }
            }
            Ok(Some(item.value.clone()))
        }

        fn write(&self, account: &str, value: Option<&str>) -> Result<()> {
            let mut items = self.state.items.lock();
            match (value, items.get_mut(account)) {
                (Some(v), Some(item)) => item.value = v.to_string(),
                (Some(v), None) => {
                    let item = Item {
                        value: v.to_string(),
                        trusted: HashSet::from([self.build]),
                    };
                    items.insert(account.to_string(), item);
                }
                (None, Some(item)) if !item.trusted.contains(&self.build) => {
                    bail!("Invalid attempt to change the owner of this item. (-25244)")
                }
                (None, _) => {
                    items.remove(account);
                }
            }
            Ok(())
        }
    }

    fn put(vault: &mut Vault, account: &str, value: &str) {
        vault.write(&[(account.to_string(), Some(value))]).unwrap();
    }

    fn forget(vault: &mut Vault, account: &str) {
        vault.write(&[(account.to_string(), None)]).unwrap();
    }

    #[test]
    fn one_permission_per_update_however_many_connections() {
        for always in [true, false] {
            let mut keychain = Keychain::new(always);
            let mut vault = keychain.launch();
            put(&mut vault, "a/password", "pw-a");
            put(&mut vault, "b/password", "pw-b");
            put(&mut vault, "c/private-key", "KEY");
            assert!(keychain.asked().is_empty(), "saving asks nothing");

            // After an update: every connection, saved or not, and saving, for one question.
            let mut vault = keychain.update();
            for account in [
                "a/password",
                "b/password",
                "c/private-key",
                "c/passphrase",
                "d/password",
            ] {
                vault.read(account).unwrap();
            }
            assert_eq!(vault.read("b/password").unwrap().as_deref(), Some("pw-b"));
            put(&mut vault, "d/password", "pw-d");
            forget(&mut vault, "a/password");
            assert_eq!(keychain.asked(), ["vault"]);

            // Opened again: nothing more after "Always Allow"; "Allow" let one read through.
            let mut vault = keychain.launch();
            assert_eq!(vault.read("d/password").unwrap().as_deref(), Some("pw-d"));
            assert_eq!(vault.read("a/password").unwrap(), None);
            let expected: &[&str] = if always { &[] } else { &["vault"] };
            assert_eq!(keychain.asked(), expected);
        }
    }

    #[test]
    fn secrets_from_earlier_versions_move_into_the_vault() {
        for always in [true, false] {
            let mut keychain =
                Keychain::with_old_items(always, &[("x/password", "pw"), ("y/private-key", "KEY")]);
            let mut vault = keychain.launch();
            assert_eq!(vault.read("x/password").unwrap().as_deref(), Some("pw"));
            assert_eq!(vault.read("x/password").unwrap().as_deref(), Some("pw"));
            assert_eq!(vault.read("y/passphrase").unwrap(), None);
            assert_eq!(keychain.asked(), ["x/password"], "one last time");
            // Deleted when macOS lets this build ("Always Allow"), left otherwise.
            assert_eq!(keychain.value("x/password").is_some(), !always);

            // The next update: the vault, and the secret that wasn't used before, once.
            let mut vault = keychain.update();
            assert_eq!(vault.read("x/password").unwrap().as_deref(), Some("pw"));
            assert_eq!(vault.read("y/private-key").unwrap().as_deref(), Some("KEY"));
            assert_eq!(keychain.asked(), ["vault", "y/private-key"]);
            let mut vault = keychain.update();
            vault.read("x/password").unwrap();
            vault.read("y/private-key").unwrap();
            assert_eq!(keychain.asked(), ["vault"]);
        }
    }

    /// Old items this build may not delete (it was only allowed once, or never asked), replaced
    /// or removed in the editor: saving works, and they never come back.
    #[test]
    fn old_items_that_cannot_be_deleted_never_come_back() {
        let mut keychain =
            Keychain::with_old_items(false, &[("z/password", "old"), ("w/password", "old")]);
        let mut vault = keychain.launch();
        forget(&mut vault, "z/password");
        put(&mut vault, "w/password", "new");
        assert!(keychain.value("z/password").is_some() && keychain.value("w/password").is_some());
        assert!(keychain.asked().is_empty());

        let mut vault = keychain.update();
        assert_eq!(vault.read("z/password").unwrap(), None, "never read again");
        assert_eq!(vault.read("w/password").unwrap().as_deref(), Some("new"));
        forget(&mut vault, "w/password");
        assert_eq!(keychain.asked(), ["vault"]);
        let mut vault = keychain.update();
        assert_eq!(vault.read("z/password").unwrap(), None);
        assert_eq!(vault.read("w/password").unwrap(), None);
        assert_eq!(keychain.asked(), ["vault"]);
        // An empty vault stays (an untrusted build couldn't delete it).
        assert!(keychain.value("vault").is_some());
    }

    #[test]
    fn a_vault_that_cannot_be_read_is_never_overwritten() {
        let mut keychain = Keychain::new(true);
        put(&mut keychain.launch(), "a/password", "pw");
        let saved = keychain.value("vault").unwrap();

        // Permission denied: nothing is read as "no secrets", nothing is written.
        keychain.state.deny.store(true, Ordering::Relaxed);
        let mut vault = keychain.update();
        assert!(vault.read("a/password").is_err());
        assert!(vault.write(&[("b/password".into(), Some("x"))]).is_err());
        assert_eq!(keychain.value("vault"), Some(saved));
        // Allowed the next time it asks.
        keychain.state.deny.store(false, Ordering::Relaxed);
        assert_eq!(vault.read("a/password").unwrap().as_deref(), Some("pw"));

        // Damaged, or written by a newer version.
        for text in ["garbage", r#"{"version": 99, "secrets": {}, "more": 1}"#] {
            let keychain = Keychain::with_old_items(true, &[("vault", text)]);
            let mut vault = keychain.launch();
            assert!(vault.read("a/password").is_err(), "{text}");
            assert!(vault.write(&[("a/password".into(), Some("x"))]).is_err());
            assert_eq!(keychain.value("vault").as_deref(), Some(text));
        }
    }

    /// What earlier 1.4.4 vaults hold still reads, and what this version writes reads back.
    #[test]
    fn vault_format() {
        let contents = parse_vault(r#"{"version":1,"secrets":{"a/password":"pw"}}"#).unwrap();
        assert_eq!(contents.secrets["a/password"], "pw");
        assert!(contents.retired.is_empty());
        let keychain = Keychain::with_old_items(false, &[("r/password", "old")]);
        let mut vault = keychain.launch();
        forget(&mut vault, "r/password");
        let text = keychain.value("vault").unwrap();
        assert_eq!(
            text,
            r#"{"version":1,"secrets":{},"retired":["r/password"]}"#
        );
        assert_eq!(parse_vault(&text).unwrap().retired.len(), 1);
    }
}
