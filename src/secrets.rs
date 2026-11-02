//! Credentials in the system's secret store: the Keychain on macOS, Credential Manager on
//! Windows, the Secret Service (GNOME Keyring, KWallet, KeePassXC …) on Linux. Nothing secret is
//! ever written to the bookmark file.
//!
//! Each bookmark can have up to three secrets, addressed as `<bookmark-id>/<kind>` under service
//! `ternaryshell`. On Windows and Linux each is an item of its own. On macOS they all share one
//! Keychain item, the [`Vault`]: macOS asks for permission for each item an app reads, and asks
//! again after every update of an app without a Developer ID signature, so one item per secret
//! meant a password prompt for every connection opened after an update.

use std::collections::{BTreeMap, HashMap, HashSet};
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
            // Deleting doesn't read the item, so macOS doesn't ask for permission.
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

#[derive(Deserialize)]
struct VaultFile {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct VaultFileRef<'a> {
    version: u32,
    secrets: &'a BTreeMap<String, String>,
}

/// Every secret in one item, read once per run and kept in memory: after an update macOS asks
/// for permission once, however many connections are then opened. Secrets that earlier versions
/// kept in items of their own move into the vault when first used (macOS asks one last time for
/// each), and those items are deleted.
struct Vault {
    store: Box<dyn Store>,
    /// The secrets by account, once the vault's item was read.
    secrets: Option<BTreeMap<String, String>>,
    /// Accounts known to have no item of their own (any more), in this run.
    no_own_item: HashSet<String>,
}

impl Vault {
    fn new(store: Box<dyn Store>) -> Self {
        Self {
            store,
            secrets: None,
            no_own_item: HashSet::new(),
        }
    }

    /// The secrets, read on first use. A vault that can't be read (permission denied, damaged,
    /// from a newer version) is tried again next time, and never overwritten meanwhile.
    fn secrets(&mut self) -> Result<&BTreeMap<String, String>> {
        if self.secrets.is_none() {
            let secrets = match self.store.read(VAULT)? {
                None => BTreeMap::new(),
                Some(text) => parse_vault(&text)?,
            };
            self.secrets = Some(secrets);
        }
        Ok(self.secrets.get_or_insert_default())
    }

    fn read(&mut self, account: &str) -> Result<Option<String>> {
        if let Some(value) = self.secrets()?.get(account) {
            return Ok(Some(value.clone()));
        }
        if self.no_own_item.contains(account) {
            return Ok(None);
        }
        // Saved by an earlier version, in an item of its own: it moves into the vault.
        let Some(value) = self.store.read(account)? else {
            self.no_own_item.insert(account.to_string());
            return Ok(None);
        };
        let mut secrets = self.secrets()?.clone();
        secrets.insert(account.to_string(), value.clone());
        // The old item goes only once its secret is safely in the vault.
        if self.save(secrets).is_ok() && self.store.write(account, None).is_ok() {
            self.no_own_item.insert(account.to_string());
        }
        Ok(Some(value))
    }

    /// Applies `changes` (`None` removes a secret) in one write of the vault.
    fn write(&mut self, changes: &[(String, Option<&str>)]) -> Result<()> {
        let mut secrets = self.secrets()?.clone();
        for (account, value) in changes {
            match value {
                Some(v) => secrets.insert(account.clone(), v.to_string()),
                None => secrets.remove(account),
            };
        }
        if self.secrets.as_ref() != Some(&secrets) {
            self.save(secrets)?;
        }
        // A secret an earlier version kept in an item of its own must not come back.
        for (account, _) in changes {
            if !self.no_own_item.contains(account) {
                self.store.write(account, None)?;
                self.no_own_item.insert(account.clone());
            }
        }
        Ok(())
    }

    fn save(&mut self, secrets: BTreeMap<String, String>) -> Result<()> {
        if secrets.is_empty() {
            self.store.write(VAULT, None)?;
        } else {
            let file = VaultFileRef {
                version: VAULT_VERSION,
                secrets: &secrets,
            };
            self.store
                .write(VAULT, Some(&serde_json::to_string(&file)?))?;
        }
        self.secrets = Some(secrets);
        Ok(())
    }
}

fn parse_vault(text: &str) -> Result<BTreeMap<String, String>> {
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
    let file: VaultFile = serde_json::from_str(text).map_err(|_| damaged())?;
    Ok(file.secrets)
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

    /// The Keychain as macOS presents it to a freshly updated app: it asks for permission for
    /// each item that exists and is read. Writing and deleting don't ask.
    #[derive(Clone, Default)]
    struct Keychain(Arc<KeychainState>);

    #[derive(Default)]
    struct KeychainState {
        items: Mutex<BTreeMap<String, String>>,
        asked: Mutex<Vec<String>>,
        deny: AtomicBool,
    }

    impl Keychain {
        fn with(items: &[(&str, &str)]) -> Self {
            let k = Self::default();
            for (account, value) in items {
                k.0.items
                    .lock()
                    .insert(account.to_string(), value.to_string());
            }
            k
        }

        /// The app is updated and opened again: a new run that macOS asks about anew.
        fn run(&self) -> Vault {
            self.0.asked.lock().clear();
            Vault::new(Box::new(self.clone()))
        }

        fn asked(&self) -> Vec<String> {
            self.0.asked.lock().clone()
        }

        fn item(&self, account: &str) -> Option<String> {
            self.0.items.lock().get(account).cloned()
        }
    }

    impl Store for Keychain {
        fn read(&self, account: &str) -> Result<Option<String>> {
            let Some(value) = self.item(account) else {
                return Ok(None);
            };
            if self.0.deny.load(Ordering::Relaxed) {
                bail!("denied");
            }
            self.0.asked.lock().push(account.to_string());
            Ok(Some(value))
        }

        fn write(&self, account: &str, value: Option<&str>) -> Result<()> {
            let mut items = self.0.items.lock();
            match value {
                Some(v) => items.insert(account.to_string(), v.to_string()),
                None => items.remove(account),
            };
            Ok(())
        }
    }

    fn put(vault: &mut Vault, account: &str, value: &str) {
        vault.write(&[(account.to_string(), Some(value))]).unwrap();
    }

    #[test]
    fn one_permission_per_update_however_many_connections() {
        let keychain = Keychain::default();
        let mut vault = keychain.run();
        put(&mut vault, "a/password", "pw-a");
        put(&mut vault, "b/password", "pw-b");
        put(&mut vault, "c/private-key", "KEY");
        assert!(keychain.asked().is_empty(), "saving asks nothing");

        // After an update: every connection, saved or not, without asking again.
        let mut vault = keychain.run();
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
        assert_eq!(keychain.asked(), ["vault"]);
    }

    #[test]
    fn secrets_from_earlier_versions_move_into_the_vault() {
        let keychain = Keychain::with(&[("x/password", "pw"), ("y/private-key", "KEY")]);
        let mut vault = keychain.run();
        assert_eq!(vault.read("x/password").unwrap().as_deref(), Some("pw"));
        assert_eq!(vault.read("x/password").unwrap().as_deref(), Some("pw"));
        assert_eq!(vault.read("y/passphrase").unwrap(), None);
        assert_eq!(keychain.asked(), ["x/password"], "one last time");
        assert_eq!(keychain.item("x/password"), None, "the old item is gone");
        assert!(keychain.item("vault").is_some());

        // The next update: the vault, and the secret that wasn't used before, once.
        let mut vault = keychain.run();
        assert_eq!(vault.read("x/password").unwrap().as_deref(), Some("pw"));
        assert_eq!(vault.read("y/private-key").unwrap().as_deref(), Some("KEY"));
        assert_eq!(keychain.asked(), ["vault", "y/private-key"]);
        let mut vault = keychain.run();
        vault.read("x/password").unwrap();
        vault.read("y/private-key").unwrap();
        assert_eq!(keychain.asked(), ["vault"]);
    }

    #[test]
    fn forgotten_secrets_stay_forgotten() {
        // An old item that is replaced or removed before it was ever read.
        let keychain = Keychain::with(&[("z/password", "old"), ("w/password", "old")]);
        let mut vault = keychain.run();
        vault.write(&[("z/password".into(), None)]).unwrap();
        put(&mut vault, "w/password", "new");
        assert_eq!(keychain.item("z/password"), None);
        assert_eq!(keychain.item("w/password"), None);
        let mut vault = keychain.run();
        assert_eq!(vault.read("z/password").unwrap(), None);
        assert_eq!(vault.read("w/password").unwrap().as_deref(), Some("new"));

        // The last secret removed: the vault's item goes too.
        vault.write(&[("w/password".into(), None)]).unwrap();
        assert_eq!(keychain.item("vault"), None);
        assert_eq!(keychain.run().read("w/password").unwrap(), None);
    }

    #[test]
    fn a_vault_that_cannot_be_read_is_never_overwritten() {
        let keychain = Keychain::default();
        put(&mut keychain.run(), "a/password", "pw");
        let saved = keychain.item("vault").unwrap();

        // Permission denied: nothing is read as "no secrets", nothing is written.
        keychain.0.deny.store(true, Ordering::Relaxed);
        let mut vault = keychain.run();
        assert!(vault.read("a/password").is_err());
        assert!(vault.write(&[("b/password".into(), Some("x"))]).is_err());
        assert_eq!(keychain.item("vault").as_ref(), Some(&saved));
        // Allowed the next time it asks.
        keychain.0.deny.store(false, Ordering::Relaxed);
        assert_eq!(vault.read("a/password").unwrap().as_deref(), Some("pw"));

        // Damaged, or written by a newer version.
        for text in ["garbage", r#"{"version": 99, "secrets": {}, "more": 1}"#] {
            let keychain = Keychain::with(&[("vault", text)]);
            let mut vault = keychain.run();
            assert!(vault.read("a/password").is_err(), "{text}");
            assert!(vault.write(&[("a/password".into(), Some("x"))]).is_err());
            assert_eq!(keychain.item("vault").as_deref(), Some(text));
        }
    }
}
