//! One SSH connection per tab, with any number of shell channels (split panes) and an SFTP
//! channel on top of it.
//!
//! The UI never blocks on the network: `Connection::start` spawns a task that connects,
//! verifies the host key, authenticates, then serves commands (open shell, open SFTP,
//! disconnect). Questions for the user (unknown host key, password, passphrase) are posted to
//! `ConnShared::prompt` and answered through a oneshot channel.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use eframe::egui::Context;
use parking_lot::Mutex;
use russh::client::{
    self, AuthResult, DisconnectReason, Handle, KeyboardInteractiveAuthResponse, Msg,
};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, ChannelWriteHalf, Disconnect, MethodKind, Pty, Sig, cipher, kex, mac};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use super::keys::{self, KeyLoadError};
use super::known_hosts::{self, Check};
use super::types::*;
use crate::bookmarks::{Auth, Bookmark};
use crate::i18n::{t, tf};
use crate::paths;
use crate::secrets::{self, SecretKind};
use crate::term::core::{self as tcore, Feeder};

pub type SshHandle = Handle<SshHandler>;

/// Time limit for connecting to one address (shorter per address when there are several).
const TCP_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshake time limit, not counting time spent waiting for the user to answer a prompt.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// Authentication time limit, again not counting time spent in prompts.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: usize = 3;

/// Everything needed to (re)connect.
#[derive(Clone)]
pub struct ConnectSpec {
    pub bookmark: Bookmark,
    pub keepalive_secs: u64,
    pub known_hosts: PathBuf,
    /// Offer "Save in Keychain" in prompts: only for bookmarks that are actually stored, so an
    /// address typed on the command line leaves nothing behind in the Keychain.
    pub save_secrets: bool,
}

/// Input for one terminal pane's channel.
#[derive(Debug)]
pub enum PaneInput {
    Data(Vec<u8>),
    Resize {
        cols: u32,
        rows: u32,
        px_w: u32,
        px_h: u32,
    },
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneStatus {
    Starting,
    Running,
    /// The shell ended (exit status when the server sent one; 128 + n for signal n).
    Exited(Option<u32>),
    /// The channel could not be opened or the connection dropped.
    Failed(String),
}

impl PaneStatus {
    pub fn is_live(&self) -> bool {
        matches!(self, PaneStatus::Starting | PaneStatus::Running)
    }
}

/// What a new shell channel needs; created by the UI for each pane.
pub struct PaneStart {
    pub feeder: Arc<Mutex<Feeder>>,
    pub input_tx: mpsc::UnboundedSender<PaneInput>,
    pub input_rx: mpsc::UnboundedReceiver<PaneInput>,
    pub cols: u32,
    pub rows: u32,
    pub px_w: u32,
    pub px_h: u32,
    pub start_dir: String,
    pub startup_command: String,
    pub status: Arc<Mutex<PaneStatus>>,
    /// The pane's current connection generation; a task whose `generation` is no longer
    /// current (the pane reconnected) must not touch the terminal or status any more.
    pub generation: Arc<AtomicU64>,
    pub gen_id: u64,
}

impl PaneStart {
    fn is_current(&self) -> bool {
        self.generation.load(Ordering::Relaxed) == self.gen_id
    }
}

enum ConnCmd {
    OpenShell(PaneStart),
    OpenSftp(oneshot::Sender<Result<SftpSession, String>>),
    Disconnect,
}

pub struct Connection {
    pub shared: Arc<Mutex<ConnShared>>,
    cmd_tx: mpsc::UnboundedSender<ConnCmd>,
}

impl Connection {
    /// Starts connecting in the background on runtime `rt`.
    pub fn start(rt: &tokio::runtime::Handle, ctx: Context, spec: ConnectSpec) -> Self {
        let shared = Arc::new(Mutex::new(ConnShared::default()));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let task_shared = shared.clone();
        rt.spawn(async move {
            run_connection(spec, task_shared, ctx, cmd_rx).await;
        });
        Self { shared, cmd_tx }
    }

    pub fn state(&self) -> ConnState {
        self.shared.lock().state.clone()
    }

    /// Opens a shell channel once the connection is up (queued until then).
    pub fn open_shell(&self, pane: PaneStart) {
        if let Err(mpsc::error::SendError(ConnCmd::OpenShell(pane))) =
            self.cmd_tx.send(ConnCmd::OpenShell(pane))
        {
            fail_pane(&pane, t("Not connected."));
        }
    }

    /// Opens an SFTP session on this connection.
    pub fn open_sftp(&self) -> oneshot::Receiver<Result<SftpSession, String>> {
        let (tx, rx) = oneshot::channel();
        if let Err(mpsc::error::SendError(ConnCmd::OpenSftp(tx))) =
            self.cmd_tx.send(ConnCmd::OpenSftp(tx))
        {
            let _ = tx.send(Err(t("Not connected.").into()));
        }
        rx
    }

    pub fn disconnect(&self) {
        // Dropping a pending prompt's reply sender wakes the task waiting on it.
        self.shared.lock().prompt = None;
        let _ = self.cmd_tx.send(ConnCmd::Disconnect);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn fail_pane(pane: &PaneStart, message: &str) {
    if !pane.is_current() {
        return;
    }
    *pane.status.lock() = PaneStatus::Failed(message.to_string());
    let mut feeder = pane.feeder.lock();
    tcore::end_session(&mut feeder, message);
}

// ----- a TCP stream that dies with its connection -----------------------------------------------

/// russh runs the session on its own task. If the connection is abandoned mid-handshake (tab
/// closed, Disconnect while a prompt is up) that task would keep the socket open and could
/// still post prompts; this wrapper fails every read and write once `cancel` fires.
struct CancellableStream {
    inner: TcpStream,
    cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl CancellableStream {
    fn new(inner: TcpStream, cancel: &CancellationToken) -> Self {
        Self {
            inner,
            cancelled: Box::pin(cancel.clone().cancelled_owned()),
        }
    }

    fn check(&mut self, cx: &mut std::task::Context<'_>) -> std::io::Result<()> {
        match self.cancelled.as_mut().poll(cx) {
            Poll::Ready(()) => Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "connection closed by the app",
            )),
            Poll::Pending => Ok(()),
        }
    }
}

impl AsyncRead for CancellableStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for CancellableStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// ----- russh handler --------------------------------------------------------------------------

pub struct SshHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    shared: Arc<Mutex<ConnShared>>,
    ctx: Context,
    flags: Flags,
}

impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        // The user's own OpenSSH known_hosts is consulted read-only: a match is trusted, and a
        // different key there is reported as a changed key (never as merely "unknown").
        let user_file = paths::home_dir().join(".ssh/known_hosts");
        let check = known_hosts::check(
            &self.known_hosts,
            Some(&user_file),
            &self.host,
            self.port,
            &key,
        )
        .map_err(|e| {
            anyhow::anyhow!(tf(
                "Could not read {} ({})",
                &[&self.known_hosts.display().to_string(), &e.to_string()],
            ))
        })?;
        let (status, known) = match check {
            Check::Known => return Ok(true),
            Check::Revoked => {
                self.flags.key_revoked.store(true, Ordering::Relaxed);
                return Ok(false);
            }
            Check::Unknown => (HostKeyStatus::Unknown, Vec::new()),
            Check::Changed { known } => (
                HostKeyStatus::Changed,
                known
                    .into_iter()
                    .map(|k| format!("{}  {}", k.algorithm, k.fingerprint))
                    .collect(),
            ),
        };
        let algorithm = key.algorithm().as_str().to_string();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let (host, port) = (self.host.clone(), self.port);
        let accepted = ask(&self.shared, &self.ctx, &self.flags, |reply| {
            Prompt::HostKey {
                host,
                port,
                algorithm,
                fingerprint,
                status,
                known,
                reply,
            }
        })
        .await
        .unwrap_or(false);
        if !accepted {
            if status == HostKeyStatus::Changed {
                self.flags.key_changed.store(true, Ordering::Relaxed);
            }
            return Ok(false);
        }
        known_hosts::learn(&self.known_hosts, &self.host, self.port, &key).map_err(|e| {
            anyhow::anyhow!(tf(
                "Could not save the host key in {} ({})",
                &[&self.known_hosts.display().to_string(), &e.to_string()],
            ))
        })?;
        Ok(true)
    }

    async fn disconnected(
        &mut self,
        reason: DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        let (message, result) = match reason {
            DisconnectReason::ReceivedDisconnect(info) => {
                let m = if info.message.trim().is_empty() {
                    format!("{:?}", info.reason_code)
                } else {
                    info.message.clone()
                };
                (m, Ok(()))
            }
            DisconnectReason::Error(e) => (format!("{e:#}"), Err(e)),
        };
        // Kept for whoever reports the end: the connected loop, or a failed handshake/login
        // ("Too many authentication failures" and the like).
        self.flags.reason.lock().get_or_insert(message);
        self.flags.closed.notify_waiters();
        self.flags.closed.notify_one();
        self.ctx.request_repaint();
        result
    }
}

/// Posts a prompt and waits for the user's answer (`None` if the prompt was dismissed, the tab
/// went away or the connection was abandoned).
async fn ask<T>(
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    flags: &Flags,
    make: impl FnOnce(oneshot::Sender<T>) -> Prompt,
) -> Option<T> {
    if flags.cancel.is_cancelled() {
        return None;
    }
    let (tx, rx) = oneshot::channel();
    shared.lock().prompt = Some(make(tx));
    flags.prompting.store(true, Ordering::Relaxed);
    ctx.request_repaint();
    let answer = tokio::select! {
        a = rx => a.ok(),
        _ = flags.cancel.cancelled() => None,
    };
    flags.prompting.store(false, Ordering::Relaxed);
    shared.lock().prompt = None;
    ctx.request_repaint();
    answer
}

// ----- connecting -----------------------------------------------------------------------------

pub fn client_config(legacy: bool, keepalive_secs: u64) -> client::Config {
    let mut cfg = client::Config {
        keepalive_interval: (keepalive_secs > 0).then(|| Duration::from_secs(keepalive_secs)),
        keepalive_max: 3,
        inactivity_timeout: None,
        nodelay: true,
        // A large window keeps transfers fast on high-latency links and avoids a connection
        // drop during client-initiated rekeys with several busy channels.
        window_size: 16 * 1024 * 1024,
        ..Default::default()
    };
    // AES-GCM uses the CPU's AES instructions: about 3.5x faster than ChaCha20 on Apple Silicon.
    cfg.preferred.cipher = vec![
        cipher::AES_256_GCM,
        cipher::AES_128_GCM,
        cipher::CHACHA20_POLY1305,
        cipher::AES_256_CTR,
        cipher::AES_192_CTR,
        cipher::AES_128_CTR,
    ]
    .into();
    if legacy {
        let mut p = cfg.preferred.clone();
        let mut kexes: Vec<kex::Name> = p.kex.to_vec();
        kexes.extend([
            kex::ECDH_SHA2_NISTP256,
            kex::ECDH_SHA2_NISTP384,
            kex::ECDH_SHA2_NISTP521,
            kex::DH_G14_SHA1,
            kex::DH_GEX_SHA1,
            kex::DH_G1_SHA1,
        ]);
        p.kex = kexes.into();
        let mut ciphers: Vec<cipher::Name> = p.cipher.to_vec();
        ciphers.extend([
            cipher::AES_128_GCM,
            cipher::AES_256_CBC,
            cipher::AES_192_CBC,
            cipher::AES_128_CBC,
        ]);
        p.cipher = ciphers.into();
        let mut macs: Vec<mac::Name> = p.mac.to_vec();
        macs.extend([mac::HMAC_SHA1_ETM, mac::HMAC_SHA1]);
        p.mac = macs.into();
        cfg.preferred = p;
    } else {
        // SHA-1 RSA host-key signatures (`ssh-rsa`) only with legacy algorithms, like OpenSSH.
        let keys: Vec<Algorithm> = cfg
            .preferred
            .key
            .iter()
            .filter(|a| **a != Algorithm::Rsa { hash: None })
            .cloned()
            .collect();
        cfg.preferred.key = keys.into();
    }
    cfg
}

/// Asks for host-key types already recorded for the host first (as OpenSSH does), so a server
/// with several keys presents the one we know instead of triggering a "changed key" warning.
fn prefer_known_key_types(cfg: &mut client::Config, known: &[String]) {
    let known: Vec<Algorithm> = known
        .iter()
        .filter_map(|a| Algorithm::new(a).ok())
        .collect();
    if known.is_empty() {
        return;
    }
    let is_known = |a: &Algorithm| {
        known
            .iter()
            .any(|k| k == a || (k.clone().is_rsa() && a.clone().is_rsa()))
    };
    let (mut first, rest): (Vec<Algorithm>, Vec<Algorithm>) =
        cfg.preferred.key.iter().cloned().partition(|a| is_known(a));
    first.extend(rest);
    cfg.preferred.key = first.into();
}

fn classify_russh(e: anyhow::Error, flags: &Flags, host: &str) -> ConnError {
    let reason = flags.reason.lock().clone();
    if let Some(re) = e.downcast_ref::<russh::Error>() {
        return match re {
            russh::Error::UnknownKey if flags.key_revoked.load(Ordering::Relaxed) => {
                ConnError::HostKeyRevoked
            }
            russh::Error::UnknownKey if flags.key_changed.load(Ordering::Relaxed) => {
                ConnError::HostKeyChanged
            }
            russh::Error::UnknownKey => ConnError::HostKeyRejected,
            russh::Error::Version => {
                ConnError::Other(tf("{} did not answer as an SSH server.", &[host]))
            }
            russh::Error::NoCommonAlgo { .. } => ConnError::Other(tf(
                "No common algorithm with the server ({}). Try enabling legacy algorithms in the bookmark.",
                &[&re.to_string()],
            )),
            russh::Error::Disconnect | russh::Error::HUP => {
                ConnError::Disconnected(reason.unwrap_or_else(|| {
                    t("the server closed the connection during the handshake").into()
                }))
            }
            russh::Error::IO(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
                ConnError::Refused
            }
            russh::Error::IO(io) => ConnError::Unreachable(io.to_string()),
            _ => ConnError::Other(re.to_string()),
        };
    }
    ConnError::Other(format!("{e:#}"))
}

/// Waits until `limit` of non-prompt time has passed.
async fn handshake_watchdog(prompting: Arc<AtomicBool>, limit: Duration) {
    let mut spent = Duration::ZERO;
    let step = Duration::from_millis(250);
    while spent < limit {
        tokio::time::sleep(step).await;
        if !prompting.load(Ordering::Relaxed) {
            spent += step;
        }
    }
}

/// The host as given, without IPv6 brackets; `None` when it can't be a host name or address.
pub fn clean_host(host: &str) -> Option<String> {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let valid = !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%'));
    valid.then(|| host.to_string())
}

async fn tcp_connect(host: &str, port: u16) -> Result<TcpStream, ConnError> {
    let lookup = tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host, port)));
    let addrs: Vec<std::net::SocketAddr> = match lookup.await {
        Ok(Ok(addrs)) => addrs.collect(),
        Ok(Err(_)) => return Err(ConnError::HostNotFound(host.to_string())),
        Err(_) => {
            return Err(ConnError::Other(tf("Looking up {} timed out.", &[host])));
        }
    };
    if addrs.is_empty() {
        return Err(ConnError::HostNotFound(host.to_string()));
    }
    // With several addresses (IPv6 + IPv4) one unreachable address must not use up the whole
    // time limit before the next is tried.
    let per_address = (TCP_TIMEOUT / addrs.len() as u32).max(Duration::from_secs(3));
    let mut last = ConnError::Timeout;
    for addr in addrs {
        match tokio::time::timeout(per_address, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                stream.set_nodelay(true).ok();
                return Ok(stream);
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                last = ConnError::Refused
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => last = ConnError::Timeout,
            Ok(Err(e)) => last = ConnError::Unreachable(e.to_string()),
            Err(_) => last = ConnError::Timeout,
        }
    }
    Err(last)
}

#[derive(Clone)]
struct Flags {
    prompting: Arc<AtomicBool>,
    key_changed: Arc<AtomicBool>,
    key_revoked: Arc<AtomicBool>,
    closed: Arc<Notify>,
    /// Why the transport ended, as reported by russh.
    reason: Arc<Mutex<Option<String>>>,
    /// Fires when the connection task ends: the transport and any pending prompt die with it.
    cancel: CancellationToken,
}

async fn establish(
    spec: &ConnectSpec,
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    flags: &Flags,
) -> Result<SshHandle, ConnError> {
    let b = &spec.bookmark;
    let host =
        clean_host(&b.host).ok_or_else(|| ConnError::HostNotFound(b.host.trim().to_string()))?;
    let stream = tcp_connect(&host, b.port).await?;
    let stream = CancellableStream::new(stream, &flags.cancel);
    let handler = SshHandler {
        host: host.clone(),
        port: b.port,
        known_hosts: spec.known_hosts.clone(),
        shared: shared.clone(),
        ctx: ctx.clone(),
        flags: flags.clone(),
    };
    let mut config = client_config(b.legacy_algorithms, spec.keepalive_secs);
    let user_file = paths::home_dir().join(".ssh/known_hosts");
    let known_types =
        known_hosts::known_algorithms(&spec.known_hosts, Some(&user_file), &host, b.port);
    prefer_known_key_types(&mut config, &known_types);
    let mut handle = tokio::select! {
        r = client::connect_stream(Arc::new(config), stream, handler) => {
            r.map_err(|e| classify_russh(e, flags, &host))?
        }
        _ = handshake_watchdog(flags.prompting.clone(), HANDSHAKE_TIMEOUT) => return Err(ConnError::Timeout),
    };
    set_state(shared, ctx, ConnState::Authenticating);
    let auth = tokio::select! {
        r = authenticate(&mut handle, spec, shared, ctx, flags) => r,
        _ = handshake_watchdog(flags.prompting.clone(), AUTH_TIMEOUT) => Err(ConnError::Timeout),
    };
    if let Err(e) = auth {
        // A login the server cut short says why ("Too many authentication failures", …).
        let reason = flags.reason.lock().clone();
        return Err(match (e, reason) {
            (ConnError::Other(_), Some(r)) => ConnError::Disconnected(r),
            (e, _) => e,
        });
    }
    Ok(handle)
}

fn set_state(shared: &Arc<Mutex<ConnShared>>, ctx: &Context, state: ConnState) {
    let mut s = shared.lock();
    if state.is_connected() {
        s.connected_at = Some(Instant::now());
    }
    s.state = state;
    drop(s);
    ctx.request_repaint();
}

fn local_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".into())
}

async fn keychain_get(id: uuid::Uuid, kind: SecretKind) -> Option<String> {
    tokio::task::spawn_blocking(move || secrets::get(id, kind).ok().flatten())
        .await
        .ok()
        .flatten()
}

async fn keychain_set(id: uuid::Uuid, kind: SecretKind, value: String) {
    let _ = tokio::task::spawn_blocking(move || secrets::set(id, kind, Some(&value))).await;
}

fn methods(r: &AuthResult) -> Vec<MethodKind> {
    match r {
        AuthResult::Success => Vec::new(),
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods.iter().cloned().collect(),
    }
}

fn method_list(m: &[MethodKind]) -> String {
    m.iter()
        .map(<&'static str>::from)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn authenticate(
    handle: &mut SshHandle,
    spec: &ConnectSpec,
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    flags: &Flags,
) -> Result<(), ConnError> {
    let b = &spec.bookmark;
    let user = if b.username.trim().is_empty() {
        local_user()
    } else {
        b.username.trim().to_string()
    };
    let other = |e: russh::Error| ConnError::Other(e.to_string());

    let key_text = match &b.auth {
        Auth::Password => None,
        Auth::Key { path } => {
            let path = paths::expand_tilde(path);
            Some((
                keys::read_key_file(&path).map_err(|e| ConnError::KeyFile(e.to_string()))?,
                path.display().to_string(),
            ))
        }
        Auth::KeychainKey => {
            let text = keychain_get(b.id, SecretKind::PrivateKey)
                .await
                .ok_or_else(|| {
                    ConnError::KeyFile(t("the private key is missing from the Keychain").into())
                })?;
            Some((text, "Keychain".to_string()))
        }
    };

    let mut fallback_methods: Option<Vec<MethodKind>> = None;
    if let Some((text, label)) = key_text {
        let mut passphrase = if keys::is_encrypted(&text) {
            keychain_get(b.id, SecretKind::Passphrase).await
        } else {
            None
        };
        let mut save = false;
        let mut attempts = 0;
        let key: PrivateKey = loop {
            match keys::decode_key(&text, passphrase.as_deref()) {
                Ok(k) => break k,
                Err(e @ (KeyLoadError::NeedsPassphrase | KeyLoadError::WrongPassphrase))
                    if attempts < MAX_ATTEMPTS =>
                {
                    let retry = e == KeyLoadError::WrongPassphrase;
                    let message = label.clone();
                    let answer = ask(shared, ctx, flags, |reply| Prompt::Secret {
                        kind: SecretPromptKind::Passphrase,
                        message,
                        echo: false,
                        can_save: spec.save_secrets,
                        retry,
                        reply,
                    })
                    .await
                    .flatten()
                    .ok_or(ConnError::Cancelled)?;
                    passphrase = Some(answer.value);
                    save = answer.save;
                    attempts += 1;
                }
                Err(e) => return Err(ConnError::KeyFile(e.to_string())),
            }
        };
        let result = auth_publickey(handle, &user, key, b.legacy_algorithms).await?;
        if result.success() {
            if save && let Some(p) = passphrase {
                keychain_set(b.id, SecretKind::Passphrase, p).await;
            }
            return Ok(());
        }
        let remaining = methods(&result);
        if !remaining
            .iter()
            .any(|m| matches!(m, MethodKind::Password | MethodKind::KeyboardInteractive))
        {
            return Err(ConnError::AuthFailed(tf(
                "the server rejected the key for user {} (server accepts: {})",
                &[&user, &method_list(&remaining)],
            )));
        }
        fallback_methods = Some(remaining);
    }

    // Password (and keyboard-interactive, which many PAM setups use for passwords).
    let mut stored = if fallback_methods.is_none() {
        keychain_get(b.id, SecretKind::Password).await
    } else {
        None
    };
    let mut attempts = 0;
    let mut retry = false;
    loop {
        let (password, save) = match stored.take() {
            Some(p) => (p, false),
            None => {
                let message = format!("{user}@{}", b.host);
                // A password asked for after a rejected key is not saved: key bookmarks never
                // read a saved password.
                let can_save = spec.save_secrets && fallback_methods.is_none();
                let answer = ask(shared, ctx, flags, |reply| Prompt::Secret {
                    kind: SecretPromptKind::Password,
                    message,
                    echo: false,
                    can_save,
                    retry,
                    reply,
                })
                .await
                .flatten()
                .ok_or(ConnError::Cancelled)?;
                (answer.value, answer.save)
            }
        };
        let result = handle
            .authenticate_password(&user, &password)
            .await
            .map_err(other)?;
        let mut ok = result.success();
        let remaining = methods(&result);
        if !ok && remaining.contains(&MethodKind::KeyboardInteractive) {
            ok = keyboard_interactive(handle, &user, &password, shared, ctx, flags).await?;
        }
        if ok {
            if save {
                keychain_set(b.id, SecretKind::Password, password).await;
            }
            return Ok(());
        }
        // Servers list the methods that may still succeed; an empty list says nothing, so
        // only give up when password-style methods are clearly not offered.
        if !ok
            && !remaining.is_empty()
            && !remaining
                .iter()
                .any(|m| matches!(m, MethodKind::Password | MethodKind::KeyboardInteractive))
        {
            return Err(ConnError::AuthFailed(tf(
                "password login is not allowed for {} (server accepts: {})",
                &[&user, &method_list(&remaining)],
            )));
        }
        attempts += 1;
        retry = true;
        if attempts >= MAX_ATTEMPTS {
            return Err(ConnError::AuthFailed(tf("wrong password for {}", &[&user])));
        }
    }
}

async fn auth_publickey(
    handle: &mut SshHandle,
    user: &str,
    key: PrivateKey,
    legacy: bool,
) -> Result<AuthResult, ConnError> {
    let other = |e: russh::Error| ConnError::Other(e.to_string());
    let key = Arc::new(key);
    if key.algorithm().is_rsa() {
        // SHA-1 signatures (`ssh-rsa`) only with legacy algorithms enabled, like OpenSSH.
        let sha1_only = || {
            ConnError::AuthFailed(t(
                "the server only accepts RSA keys with SHA-1 signatures. Enable legacy algorithms in the bookmark to allow it.",
            )
            .into())
        };
        let candidates: Vec<Option<HashAlg>> =
            match handle.best_supported_rsa_hash().await.map_err(other)? {
                Some(None) if !legacy => return Err(sha1_only()),
                Some(h) => vec![h],
                None if legacy => vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256), None],
                None => vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256)],
            };
        let mut last = None;
        for hash in candidates {
            let r = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key.clone(), hash))
                .await
                .map_err(other)?;
            if r.success() {
                return Ok(r);
            }
            last = Some(r);
        }
        return last.ok_or_else(sha1_only);
    }
    handle
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, None))
        .await
        .map_err(other)
}

/// Keyboard-interactive login: password-like hidden prompts get the password, anything else
/// (e.g. a one-time code) is asked of the user.
async fn keyboard_interactive(
    handle: &mut SshHandle,
    user: &str,
    password: &str,
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    flags: &Flags,
) -> Result<bool, ConnError> {
    let other = |e: russh::Error| ConnError::Other(e.to_string());
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None::<String>)
        .await
        .map_err(other)?;
    let mut password_used = false;
    for _ in 0..8 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let mut answers = Vec::new();
                for p in prompts {
                    if !p.echo && p.prompt.to_lowercase().contains("password") && !password_used {
                        answers.push(password.to_string());
                        password_used = true;
                        continue;
                    }
                    let message = [name.as_str(), instructions.as_str(), p.prompt.as_str()]
                        .iter()
                        .filter(|s| !s.trim().is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    let echo = p.echo;
                    let answer = ask(shared, ctx, flags, |reply| Prompt::Secret {
                        kind: SecretPromptKind::Interactive,
                        message,
                        echo,
                        can_save: false,
                        retry: false,
                        reply,
                    })
                    .await
                    .flatten()
                    .ok_or(ConnError::Cancelled)?;
                    answers.push(answer.value);
                }
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(other)?;
            }
        }
    }
    Ok(false)
}

async fn run_connection(
    spec: ConnectSpec,
    shared: Arc<Mutex<ConnShared>>,
    ctx: Context,
    mut cmd_rx: mpsc::UnboundedReceiver<ConnCmd>,
) {
    let flags = Flags {
        prompting: Arc::new(AtomicBool::new(false)),
        key_changed: Arc::new(AtomicBool::new(false)),
        key_revoked: Arc::new(AtomicBool::new(false)),
        closed: Arc::new(Notify::new()),
        reason: Arc::new(Mutex::new(None)),
        cancel: CancellationToken::new(),
    };
    // However this task ends, the socket and any prompt still waiting go with it.
    let _cancel_on_exit = flags.cancel.clone().drop_guard();
    set_state(&shared, &ctx, ConnState::Connecting);

    // A Disconnect sent while connecting aborts the attempt; other commands wait.
    let pending: Mutex<Vec<ConnCmd>> = Mutex::new(Vec::new());
    let early_disconnect = async {
        loop {
            match cmd_rx.recv().await {
                Some(ConnCmd::Disconnect) | None => return,
                Some(other) => pending.lock().push(other),
            }
        }
    };
    let result = tokio::select! {
        r = establish(&spec, &shared, &ctx, &flags) => Some(r),
        _ = early_disconnect => None,
    };
    let queued: Vec<ConnCmd> = std::mem::take(&mut *pending.lock());

    let handle = match result {
        None => {
            flags.cancel.cancel();
            set_state(&shared, &ctx, ConnState::Closed(None));
            fail_all(queued, cmd_rx, t("Connection cancelled."));
            return;
        }
        Some(Err(e)) => {
            flags.cancel.cancel();
            let message = e.message();
            set_state(&shared, &ctx, ConnState::Closed(Some(e)));
            fail_all(queued, cmd_rx, &message);
            return;
        }
        Some(Ok(h)) => Arc::new(h),
    };
    set_state(&shared, &ctx, ConnState::Connected);

    for cmd in queued {
        if handle_cmd(cmd, &handle, &ctx) {
            close(&handle, &flags, &shared, &ctx).await;
            return;
        }
    }
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let stop = match cmd {
                    Some(cmd) => handle_cmd(cmd, &handle, &ctx),
                    None => true,
                };
                if stop {
                    close(&handle, &flags, &shared, &ctx).await;
                    return;
                }
            }
            _ = flags.closed.notified() => {
                // Transport ended; shells notice on their own. Refuse further commands.
                let reason = flags.reason.lock().clone().unwrap_or_else(|| t("Connection closed.").into());
                set_state(&shared, &ctx, ConnState::Closed(Some(ConnError::Disconnected(reason.clone()))));
                fail_all(Vec::new(), cmd_rx, &tf("Disconnected: {}", &[&reason]));
                return;
            }
        }
    }
}

async fn close(handle: &SshHandle, flags: &Flags, shared: &Arc<Mutex<ConnShared>>, ctx: &Context) {
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        handle.disconnect(Disconnect::ByApplication, "", "en"),
    )
    .await;
    flags.cancel.cancel();
    set_state(shared, ctx, ConnState::Closed(None));
}

/// Fails every queued and future command (the connection is gone).
fn fail_all(queued: Vec<ConnCmd>, mut rx: mpsc::UnboundedReceiver<ConnCmd>, reason: &str) {
    rx.close();
    let rest = std::iter::from_fn(|| rx.try_recv().ok());
    for cmd in queued.into_iter().chain(rest) {
        match cmd {
            ConnCmd::OpenShell(p) => fail_pane(&p, reason),
            ConnCmd::OpenSftp(tx) => {
                let _ = tx.send(Err(reason.to_string()));
            }
            ConnCmd::Disconnect => {}
        }
    }
}

/// Returns true when the connection should close.
fn handle_cmd(cmd: ConnCmd, handle: &Arc<SshHandle>, ctx: &Context) -> bool {
    match cmd {
        ConnCmd::OpenShell(pane) => {
            let handle = handle.clone();
            let ctx = ctx.clone();
            tokio::spawn(run_shell(handle, pane, ctx));
            false
        }
        ConnCmd::OpenSftp(reply) => {
            let handle = handle.clone();
            tokio::spawn(async move {
                let _ = reply.send(open_sftp(&handle).await);
            });
            false
        }
        ConnCmd::Disconnect => true,
    }
}

async fn open_sftp(handle: &SshHandle) -> Result<SftpSession, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| e.to_string())?;
    // Bigger write packets and a deeper write pipeline: with the defaults uploads are capped at
    // 16 x 32 KiB per round trip. More than 16 concurrent reads measured slower, so reads keep
    // the default depth.
    let config = russh_sftp::client::Config {
        max_packet_len: 256 * 1024,
        max_write_packet_len: 255 * 1024,
        // 16 x 255 KiB in flight: fast on high-latency links without queueing so much that
        // requests time out on a slow uplink or browsing waits behind an upload.
        max_concurrent_writes: 16,
        max_concurrent_reads: 16,
        request_timeout_secs: 60,
    };
    SftpSession::new_with_config(channel.into_stream(), config)
        .await
        .map_err(|e| {
            tf(
                "SFTP is not available on this server ({})",
                &[&e.to_string()],
            )
        })
}

// ----- shells ---------------------------------------------------------------------------------

/// Shell-quotes a directory for `cd`, keeping a leading `~/` expandable.
pub fn cd_command(dir: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let dir = dir.trim();
    if dir == "~" {
        return "cd ~".into();
    }
    match dir.strip_prefix("~/") {
        Some(rest) => format!("cd ~/{}", quote(rest)),
        None => format!("cd {}", quote(dir)),
    }
}

/// The locale to request on the server: the local `LANG` when it is UTF-8, else en_US.UTF-8.
fn remote_lang() -> String {
    std::env::var("LANG")
        .ok()
        .filter(|l| l.to_ascii_uppercase().replace('-', "").contains("UTF8"))
        .unwrap_or_else(|| "en_US.UTF-8".to_string())
}

async fn run_shell(handle: Arc<SshHandle>, pane: PaneStart, ctx: Context) {
    let PaneStart {
        feeder,
        input_tx,
        mut input_rx,
        cols,
        rows,
        px_w,
        px_h,
        start_dir,
        startup_command,
        status,
        generation,
        gen_id,
    } = pane;
    let current = move || generation.load(Ordering::Relaxed) == gen_id;
    let open = async {
        let channel = handle.channel_open_session().await?;
        let modes = [
            (Pty::ECHO, 1),
            (Pty::ICRNL, 1),
            (Pty::IUTF8, 1),
            (Pty::TTY_OP_ISPEED, 38400),
            (Pty::TTY_OP_OSPEED, 38400),
        ];
        channel
            .request_pty(false, "xterm-256color", cols, rows, px_w, px_h, &modes)
            .await?;
        // Like OpenSSH's default `SendEnv LANG LC_*`: without a UTF-8 locale the remote shell
        // mangles non-ASCII text. Servers that don't accept it simply ignore the request.
        channel.set_env(false, "LANG", remote_lang()).await?;
        channel.request_shell(false).await?;
        Ok::<_, russh::Error>(channel)
    };
    let channel = match open.await {
        Ok(c) => c,
        Err(e) => {
            if current() {
                let message = tf("Could not open a shell: {}", &[&e.to_string()]);
                *status.lock() = PaneStatus::Failed(message.clone());
                tcore::end_session(&mut feeder.lock(), &message);
                ctx.request_repaint();
            }
            return;
        }
    };
    if !current() {
        let _ = channel.close().await;
        return;
    }
    *status.lock() = PaneStatus::Running;
    ctx.request_repaint();

    let (mut reader, writer) = channel.split();
    let writer: Arc<ChannelWriteHalf<Msg>> = Arc::new(writer);

    // Start folder and startup commands go out before anything typed while connecting.
    let mut setup = String::new();
    if !start_dir.trim().is_empty() {
        setup.push_str(&format!("{}\r", cd_command(&start_dir)));
    }
    for line in startup_command.lines().filter(|l| !l.trim().is_empty()) {
        setup.push_str(&format!("{line}\r"));
    }
    if !setup.is_empty() {
        let _ = writer.data_bytes(setup.into_bytes()).await;
    }

    // Writer: input, resizes and parser replies. Runs separately so a full remote window
    // never stops output from being read.
    let w = writer.clone();
    let write_task = tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            let r = match input {
                PaneInput::Data(bytes) => w.data_bytes(bytes).await,
                PaneInput::Resize {
                    cols,
                    rows,
                    px_w,
                    px_h,
                } => w.window_change(cols, rows, px_w, px_h).await,
                PaneInput::Close => {
                    let _ = w.eof().await;
                    let _ = w.close().await;
                    break;
                }
            };
            if r.is_err() {
                break;
            }
        }
    });

    let mut exit_status = None;
    let mut exit_signal: Option<String> = None;
    loop {
        let deadline = feeder.lock().sync_deadline();
        let sleep = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
                None => std::future::pending::<()>().await,
            }
        };
        if !current() {
            // The pane reconnected; this old channel must not write into it any more.
            write_task.abort();
            let _ = writer.close().await;
            return;
        }
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if !current() {
                        continue;
                    }
                    let (reply, repaint) = feeder.lock().feed(&data);
                    if !reply.is_empty() {
                        let _ = input_tx.send(PaneInput::Data(reply));
                    }
                    if repaint {
                        ctx.request_repaint();
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    exit_signal = Some(signal_label(&signal_name));
                    exit_status = Some(128 + signal_number(&signal_name));
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            _ = sleep => {
                let reply = feeder.lock().flush_sync();
                if !reply.is_empty() {
                    let _ = input_tx.send(PaneInput::Data(reply));
                }
                ctx.request_repaint();
            }
        }
    }
    write_task.abort();
    if !current() {
        return;
    }
    let message = match (&exit_signal, exit_status) {
        (Some(sig), _) => tf(
            "[Session ended by signal {}. Press Enter or ⌘R to reconnect.]",
            &[sig],
        ),
        (None, Some(code)) => tf(
            "[Session ended, exit code {}. Press Enter or ⌘R to reconnect.]",
            &[&code.to_string()],
        ),
        (None, None) => t("[Connection closed. Press Enter or ⌘R to reconnect.]").to_string(),
    };
    tcore::end_session(&mut feeder.lock(), &message);
    *status.lock() = PaneStatus::Exited(exit_status);
    ctx.request_repaint();
}

fn signal_label(sig: &Sig) -> String {
    match sig {
        Sig::Custom(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// The usual Linux signal number, for a shell-style `128 + n` exit status.
fn signal_number(sig: &Sig) -> u32 {
    match sig {
        Sig::HUP => 1,
        Sig::INT => 2,
        Sig::QUIT => 3,
        Sig::ILL => 4,
        Sig::ABRT => 6,
        Sig::FPE => 8,
        Sig::KILL => 9,
        Sig::USR1 => 10,
        Sig::SEGV => 11,
        Sig::PIPE => 13,
        Sig::ALRM => 14,
        Sig::TERM => 15,
        Sig::Custom(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_quoting() {
        assert_eq!(cd_command("/var/www"), "cd '/var/www'");
        assert_eq!(cd_command("~/my dir"), "cd ~/'my dir'");
        assert_eq!(cd_command("~"), "cd ~");
        assert_eq!(cd_command("/it's"), r"cd '/it'\''s'");
    }

    #[test]
    fn hosts() {
        assert_eq!(clean_host(" [::1] ").as_deref(), Some("::1"));
        assert_eq!(clean_host("fe80::1%en0").as_deref(), Some("fe80::1%en0"));
        assert_eq!(
            clean_host("web-01.example.com").as_deref(),
            Some("web-01.example.com")
        );
        assert_eq!(clean_host("-oProxyCommand=x"), None);
        assert_eq!(clean_host("a b"), None);
        assert_eq!(clean_host(""), None);
    }

    #[test]
    fn known_key_types_go_first() {
        let mut cfg = client_config(false, 0);
        assert!(!cfg.preferred.key.contains(&Algorithm::Rsa { hash: None }));
        prefer_known_key_types(&mut cfg, &["ssh-rsa".into()]);
        assert!(cfg.preferred.key[0].clone().is_rsa() && cfg.preferred.key[1].clone().is_rsa());
        assert!(!cfg.preferred.key[2].clone().is_rsa());
    }
}
