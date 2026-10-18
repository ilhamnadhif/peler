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
    /// The keys of an SSH agent (the macOS one, 1Password, Secretive, a YubiKey…); the agent
    /// signs, Peler never sees the keys. `socket` is the agent's socket, empty for the one in
    /// `SSH_AUTH_SOCK`.
    Agent {
        #[serde(default)]
        socket: String,
    },
}

impl Auth {
    /// A private key Peler reads itself, which may have a passphrase.
    pub fn uses_key(&self) -> bool {
        matches!(self, Auth::Key { .. } | Auth::KeychainKey)
    }
}

/// A port forwarded through the connection while it is open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    pub kind: ForwardKind,
    /// The port that is listened on: on this Mac (local, dynamic) or on the server (remote).
    pub listen_port: u16,
    /// Where connections are sent: a host as the server sees it (local) or as this Mac sees it
    /// (remote). Not used for dynamic forwarding.
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// `ssh -L`: a port on this Mac reaches `host:port` from the server.
    Local,
    /// `ssh -R`: a port on the server reaches `host:port` from this Mac.
    Remote,
    /// `ssh -D`: a SOCKS proxy on this Mac whose connections leave from the server.
    Dynamic,
}

impl Forward {
    /// `-L 8080:localhost:80` style, for lists and tooltips.
    pub fn describe(&self) -> String {
        match self.kind {
            ForwardKind::Local => format!("L {} → {}:{}", self.listen_port, self.host, self.port),
            ForwardKind::Remote => format!("R {} → {}:{}", self.listen_port, self.host, self.port),
            ForwardKind::Dynamic => format!("D {} (SOCKS)", self.listen_port),
        }
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
    /// Reach the server through another saved connection (`ssh -J`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<Forward>,
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
            jump: None,
            forwards: Vec::new(),
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

    /// `user@host[:port]` (IPv6 addresses in brackets).
    pub fn address(&self) -> String {
        let mut s = String::new();
        if !self.username.is_empty() {
            s.push_str(&self.username);
            s.push('@');
        }
        if self.host.contains(':') && !self.host.starts_with('[') {
            s.push_str(&format!("[{}]", self.host));
        } else {
            s.push_str(&self.host);
        }
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

/// An address typed by the user: `[ssh://][user@]host[:port]`, where an IPv6 host with a port
/// is written in brackets (`[::1]:2222`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

/// Parses a typed address; the error says what is wrong with it.
pub fn parse_target(input: &str) -> Result<Target, String> {
    use crate::i18n::t;
    let input = input.trim();
    let input = input
        .strip_prefix("ssh://")
        .unwrap_or(input)
        .trim_end_matches('/');
    let (user, rest) = match input.rsplit_once('@') {
        Some((u, r)) => (
            Some(u.trim().to_string()).filter(|u| !u.is_empty()),
            r.trim(),
        ),
        None => (None, input),
    };
    let bad_port = || t("Port must be a number between 1 and 65535.").to_string();
    let parse_port = |p: &str| {
        p.parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(bad_port)
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let (host, after) = inner
            .split_once(']')
            .ok_or_else(|| t("Missing \"]\" after the IPv6 address.").to_string())?;
        let port = match after {
            "" => None,
            p => Some(parse_port(p.strip_prefix(':').ok_or_else(bad_port)?)?),
        };
        (host.to_string(), port)
    } else {
        match rest.matches(':').count() {
            0 => (rest.to_string(), None),
            1 => {
                let (h, p) = rest.split_once(':').unwrap_or((rest, ""));
                (h.to_string(), Some(parse_port(p)?))
            }
            // Several colons: a bare IPv6 address (a port needs the brackets).
            _ => (rest.to_string(), None),
        }
    };
    if host.is_empty() {
        return Err(t("Host is required.").into());
    }
    if crate::ssh::session::clean_host(&host).is_none() {
        return Err(t("The host name contains characters that are not allowed.").into());
    }
    Ok(Target { user, host, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(user: Option<&str>, host: &str, port: Option<u16>) -> Target {
        Target {
            user: user.map(str::to_string),
            host: host.into(),
            port,
        }
    }

    #[test]
    fn typed_addresses() {
        assert_eq!(
            parse_target("web.example.com"),
            Ok(target(None, "web.example.com", None))
        );
        assert_eq!(
            parse_target(" root@10.0.0.1:2222 "),
            Ok(target(Some("root"), "10.0.0.1", Some(2222)))
        );
        assert_eq!(
            parse_target("ssh://deploy@host/"),
            Ok(target(Some("deploy"), "host", None))
        );
        assert_eq!(
            parse_target("[::1]:2299"),
            Ok(target(None, "::1", Some(2299)))
        );
        assert_eq!(
            parse_target("admin@fe80::1"),
            Ok(target(Some("admin"), "fe80::1", None))
        );
        assert!(parse_target("host:abc").is_err());
        assert!(parse_target("host:0").is_err());
        assert!(parse_target("[::1").is_err());
        assert!(parse_target("bad host").is_err());
        assert!(parse_target("user@").is_err());
        let b = Bookmark {
            host: "::1".into(),
            port: 2299,
            username: "me".into(),
            ..Default::default()
        };
        assert_eq!(b.address(), "me@[::1]:2299");
    }
}
