//! Connection state shared between the SSH tasks and the UI.

use std::time::Instant;

use tokio::sync::oneshot;

use crate::i18n::{t, tf};

/// Why a connection failed or ended. Every error the user sees names its cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnError {
    HostNotFound(String),
    Refused,
    Timeout,
    Unreachable(String),
    HostKeyRejected,
    HostKeyChanged,
    /// The server's key is marked `@revoked` in a known_hosts file.
    HostKeyRevoked,
    AuthFailed(String),
    /// The user dismissed a password/passphrase prompt.
    Cancelled,
    KeyFile(String),
    Disconnected(String),
    Other(String),
}

impl ConnError {
    pub fn message(&self) -> String {
        match self {
            ConnError::HostNotFound(h) => tf("Host not found: {}", &[h]),
            ConnError::Refused => {
                t("Connection refused: nothing is listening on that port.").into()
            }
            ConnError::Timeout => t("Connection timed out.").into(),
            ConnError::Unreachable(e) => tf("Network unreachable: {}", &[e]),
            ConnError::HostKeyRejected => t("Host key not accepted.").into(),
            ConnError::HostKeyChanged => {
                t("The server's host key has changed. Connection refused to protect you.").into()
            }
            ConnError::HostKeyRevoked => {
                t("The server's host key is marked as revoked in known_hosts. Connection refused.")
                    .into()
            }
            ConnError::AuthFailed(m) => tf("Authentication failed: {}", &[m]),
            ConnError::Cancelled => t("Cancelled.").into(),
            ConnError::KeyFile(e) => tf("Private key error: {}", &[e]),
            ConnError::Disconnected(r) => tf("Disconnected: {}", &[r]),
            ConnError::Other(e) => e.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Authenticating,
    Connected,
    /// Ended; `None` when the user closed it.
    Closed(Option<ConnError>),
}

impl ConnState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnState::Connected)
    }

    pub fn is_closed(&self) -> bool {
        matches!(self, ConnState::Closed(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// Never seen before.
    Unknown,
    /// Known with a different key.
    Changed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretPromptKind {
    Password,
    Passphrase,
    /// A keyboard-interactive question from the server.
    Interactive,
}

/// A question the connection task needs answered by the user.
pub enum Prompt {
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        status: HostKeyStatus,
        /// For a changed key: the keys recorded before (algorithm and fingerprint).
        known: Vec<String>,
        reply: oneshot::Sender<bool>,
    },
    Secret {
        kind: SecretPromptKind,
        /// Text to show (e.g. the key path, or the server's question).
        message: String,
        /// Echo the answer (keyboard-interactive questions may ask for that).
        echo: bool,
        /// Offer "Save in Keychain".
        can_save: bool,
        /// Previous attempt failed.
        retry: bool,
        reply: oneshot::Sender<Option<SecretAnswer>>,
    },
}

#[derive(Clone, Debug)]
pub struct SecretAnswer {
    pub value: String,
    pub save: bool,
}

/// What the UI reads to draw a tab's connection status.
pub struct ConnShared {
    pub state: ConnState,
    pub prompt: Option<Prompt>,
    pub connected_at: Option<Instant>,
    /// Server identification and negotiated details, for the status line.
    pub info: String,
}

impl Default for ConnShared {
    fn default() -> Self {
        Self {
            state: ConnState::Connecting,
            prompt: None,
            connected_at: None,
            info: String::new(),
        }
    }
}
