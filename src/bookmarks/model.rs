//! Bookmark and group data model.
//!
//! Secrets (passwords, key passphrases, imported key material) are never stored here; they live
//! in the macOS Keychain, keyed by bookmark id (see `crate::secrets`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_PORT: u16 = 22;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    /// Parent group; `None` means top level.
    #[serde(default)]
    pub parent: Option<Uuid>,
}

impl Group {
    pub fn new(name: impl Into<String>, parent: Option<Uuid>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            parent,
        }
    }
}

/// How the client authenticates. The matching secret (password or passphrase) is looked up in
/// the Keychain at connect time; when it is missing the user is prompted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Auth {
    #[default]
    Password,
    /// Private key file on disk. Only the path is stored and exported, never the key itself.
    Key { path: String },
    /// Private key material stored in the Keychain (used when importing from tools that embed
    /// key contents, such as Electerm), so no key is written to disk in plain text.
    KeychainKey,
}

impl Auth {
    pub fn uses_key(&self) -> bool {
        !matches!(self, Auth::Password)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub group: Option<Uuid>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub notes: String,
    /// Remote directory to `cd` into after login (also the initial remote SFTP directory).
    #[serde(default)]
    pub start_dir: String,
    /// Local directory the SFTP panel opens in (empty: the last one used).
    #[serde(default)]
    pub local_start_dir: String,
    /// Command sent to the shell right after login.
    #[serde(default)]
    pub startup_command: String,
    /// Also offer older algorithms (ssh-rsa, SHA-1 key exchange, CBC ciphers) for old servers.
    #[serde(default)]
    pub legacy_algorithms: bool,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Default for Bookmark {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            host: String::new(),
            port: DEFAULT_PORT,
            username: String::new(),
            auth: Auth::Password,
            group: None,
            tags: Vec::new(),
            notes: String::new(),
            start_dir: String::new(),
            local_start_dir: String::new(),
            startup_command: String::new(),
            legacy_algorithms: false,
        }
    }
}

impl Bookmark {
    /// Name shown in lists: the bookmark name, or `user@host` when it has none.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.address()
        } else {
            self.name.clone()
        }
    }

    /// `user@host[:port]`.
    pub fn address(&self) -> String {
        let mut s = String::new();
        if !self.username.is_empty() {
            s.push_str(&self.username);
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != DEFAULT_PORT {
            s.push_str(&format!(":{}", self.port));
        }
        s
    }

    /// Same server and login: used to match bookmarks across imports when ids differ.
    pub fn same_endpoint(&self, other: &Bookmark) -> bool {
        self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.username == other.username
    }

    /// Copy with a fresh id (the "Duplicate" action and "keep both" on import).
    pub fn duplicate(&self) -> Bookmark {
        Bookmark {
            id: Uuid::new_v4(),
            ..self.clone()
        }
    }
}
