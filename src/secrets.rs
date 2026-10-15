//! Credentials in the macOS Keychain.
//!
//! Each bookmark can have up to three secrets, stored as separate generic-password items under
//! service `peler`, account `<bookmark-id>/<kind>`. Nothing secret is ever written to the
//! bookmark file.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SERVICE: &str = "peler";

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

// ----- storage backends -----------------------------------------------------------------------

enum Backend {
    Keychain,
    /// Process-local store used by tests (`PELER_SECRETS=memory`, debug builds only).
    Memory(Mutex<HashMap<String, String>>),
}

fn backend() -> &'static Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    BACKEND.get_or_init(|| {
        // The in-memory store is for tests only; release builds always use the Keychain.
        if cfg!(test)
            || (cfg!(debug_assertions)
                && std::env::var("PELER_SECRETS").is_ok_and(|v| v == "memory"))
        {
            Backend::Memory(Mutex::new(HashMap::new()))
        } else {
            Backend::Keychain
        }
    })
}

fn entry(id: Uuid, kind: SecretKind) -> anyhow::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &account(id, kind)).map_err(|e| anyhow::anyhow!("Keychain: {e}"))
}

/// Reads one secret. A missing item is `Ok(None)`. Blocking: keep off the UI thread when the
/// Keychain might prompt.
pub fn get(id: Uuid, kind: SecretKind) -> anyhow::Result<Option<String>> {
    match backend() {
        Backend::Memory(m) => Ok(m.lock().get(&account(id, kind)).cloned()),
        Backend::Keychain => match entry(id, kind)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("Keychain: {e}")),
        },
    }
}

/// Stores (or replaces) one secret; `None` or an empty value deletes it.
pub fn set(id: Uuid, kind: SecretKind, value: Option<&str>) -> anyhow::Result<()> {
    let value = value.filter(|v| !v.is_empty());
    match backend() {
        Backend::Memory(m) => {
            let mut m = m.lock();
            match value {
                Some(v) => m.insert(account(id, kind), v.to_string()),
                None => m.remove(&account(id, kind)),
            };
            Ok(())
        }
        Backend::Keychain => {
            let e = entry(id, kind)?;
            let res = match value {
                Some(v) => e.set_password(v),
                None => match e.delete_credential() {
                    Err(keyring::Error::NoEntry) => Ok(()),
                    other => other,
                },
            };
            res.map_err(|e| anyhow::anyhow!("Keychain: {e}"))
        }
    }
}

pub fn has(id: Uuid, kind: SecretKind) -> bool {
    matches!(get(id, kind), Ok(Some(_)))
}

/// Like `load_set`, but a Keychain error is reported instead of read as "nothing saved".
pub fn try_load_set(id: Uuid) -> anyhow::Result<SecretSet> {
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

/// Stores the secrets present in `set` (absent ones are left untouched).
pub fn store_set(id: Uuid, set: &SecretSet) -> anyhow::Result<()> {
    for kind in SecretKind::ALL {
        if let Some(v) = set.get(kind) {
            self::set(id, kind, Some(v))?;
        }
    }
    Ok(())
}

/// Removes every secret of a bookmark (used when the bookmark is deleted).
pub fn delete_all(id: Uuid) {
    for kind in SecretKind::ALL {
        let _ = set(id, kind, None);
    }
}

/// Copies all secrets from one bookmark to another (the "Duplicate" action).
pub fn copy_all(from: Uuid, to: Uuid) {
    let s = load_set(from);
    let _ = store_set(to, &s);
}

#[cfg(test)]
mod tests {
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
}
