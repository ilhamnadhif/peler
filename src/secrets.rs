//! Credentials in the macOS Keychain.
//!
//! Each bookmark can have up to three secrets, stored as separate generic-password items under
//! service `peler`, account `<bookmark-id>/<kind>`. Nothing secret is ever written to the
//! bookmark file.

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
    pub const ALL: [SecretKind; 3] =
        [SecretKind::Password, SecretKind::Passphrase, SecretKind::PrivateKey];

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
