//! One SSH connection per tab, with any number of shell channels (split panes) and an SFTP
//! channel on top of it.
//!
//! The UI never blocks on the network: `Connection::start` spawns a task that connects,
//! verifies the host key, authenticates, then serves commands (open shell, open SFTP,
//! disconnect). Questions for the user (unknown host key, password, passphrase) are posted to
//! `ConnShared::prompt` and answered through a oneshot channel.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::Context;
use parking_lot::Mutex;
use russh::client::{self, AuthResult, DisconnectReason, Handle, KeyboardInteractiveAuthResponse, Msg};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate, known_hosts};
use russh::{ChannelMsg, ChannelWriteHalf, Disconnect, MethodKind, Pty, cipher, kex, mac};
use russh_sftp::client::SftpSession;
use tokio::sync::{Notify, mpsc, oneshot};

use super::keys::{self, KeyLoadError};
use super::types::*;
use crate::bookmarks::{Auth, Bookmark};
use crate::i18n::{t, tf};
use crate::paths;
use crate::secrets::{self, SecretKind};
use crate::term::core::{self as tcore, Feeder};

pub type SshHandle = Handle<SshHandler>;

const TCP_TIMEOUT: Duration = Duration::from_secs(10);
/// Handshake time limit, not counting time spent waiting for the user to answer a prompt.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ATTEMPTS: usize = 3;

/// Everything needed to (re)connect.
#[derive(Clone)]
pub struct ConnectSpec {
    pub bookmark: Bookmark,
    pub keepalive_secs: u64,
    pub known_hosts: PathBuf,
}

/// Input for one terminal pane's channel.
#[derive(Debug)]
pub enum PaneInput {
    Data(Vec<u8>),
    Resize { cols: u32, rows: u32, px_w: u32, px_h: u32 },
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneStatus {
    Starting,
    Running,
    /// The shell ended (exit status when the server sent one).
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
        if let Err(mpsc::error::SendError(ConnCmd::OpenShell(pane))) = self.cmd_tx.send(ConnCmd::OpenShell(pane)) {
            fail_pane(&pane, t("Not connected."));
        }
    }

    /// Opens an SFTP session on this connection.
    pub fn open_sftp(&self) -> oneshot::Receiver<Result<SftpSession, String>> {
        let (tx, rx) = oneshot::channel();
        if let Err(mpsc::error::SendError(ConnCmd::OpenSftp(tx))) = self.cmd_tx.send(ConnCmd::OpenSftp(tx)) {
            let _ = tx.send(Err(t("Not connected.").into()));
        }
        rx
    }

    pub fn disconnect(&self) {
        let _ = self.cmd_tx.send(ConnCmd::Disconnect);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(ConnCmd::Disconnect);
    }
}

fn fail_pane(pane: &PaneStart, message: &str) {
    *pane.status.lock() = PaneStatus::Failed(message.to_string());
    let mut feeder = pane.feeder.lock();
    tcore::write_status(&mut feeder, message);
}

// ----- russh handler --------------------------------------------------------------------------

pub struct SshHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    shared: Arc<Mutex<ConnShared>>,
    ctx: Context,
    prompting: Arc<AtomicBool>,
    key_changed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    closed: Arc<Notify>,
}

impl SshHandler {
    fn check_files(&self, key: &russh::keys::PublicKey) -> Result<HostKeyCheck, anyhow::Error> {
        let check = |path: &std::path::Path| -> Option<HostKeyCheck> {
            match known_hosts::check_known_hosts_path(&self.host, self.port, key, path) {
                Ok(true) => Some(HostKeyCheck::Known),
                Ok(false) => None,
                Err(russh::keys::Error::KeyChanged { line }) => {
                    Some(HostKeyCheck::Changed { file: path.to_path_buf(), line })
                }
                Err(_) => None,
            }
        };
        if let Some(r) = check(&self.known_hosts) {
            return Ok(r);
        }
        // The user's own OpenSSH known_hosts is trusted read-only: a match there is enough.
        let user_file = paths::home_dir().join(".ssh/known_hosts");
        if let Some(HostKeyCheck::Known) = check(&user_file) {
            return Ok(HostKeyCheck::Known);
        }
        Ok(HostKeyCheck::Unknown)
    }
}

enum HostKeyCheck {
    Known,
    Unknown,
    Changed { file: PathBuf, line: usize },
}

impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, server_public_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        let check = self.check_files(&key)?;
        let status = match &check {
            HostKeyCheck::Known => return Ok(true),
            HostKeyCheck::Unknown => HostKeyStatus::Unknown,
            HostKeyCheck::Changed { .. } => HostKeyStatus::Changed,
        };
        let algorithm = key.algorithm().as_str().to_string();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let (host, port) = (self.host.clone(), self.port);
        let accepted = ask(&self.shared, &self.ctx, &self.prompting, |reply| Prompt::HostKey {
            host,
            port,
            algorithm,
            fingerprint,
            status,
            reply,
        })
        .await
        .unwrap_or(false);
        if !accepted {
            if status == HostKeyStatus::Changed {
                self.key_changed.store(true, Ordering::Relaxed);
            }
            return Ok(false);
        }
        if let HostKeyCheck::Changed { file, line } = &check {
            if *file == self.known_hosts {
                keys::forget_known_host_line(file, *line)?;
            }
        }
        paths::ensure_private_dir(self.known_hosts.parent().unwrap_or(paths::data_dir()))?;
        known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)?;
        Ok(true)
    }

    async fn disconnected(&mut self, reason: DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        let (message, result) = match reason {
            DisconnectReason::ReceivedDisconnect(info) => {
                let m = if info.message.trim().is_empty() { format!("{:?}", info.reason_code) } else { info.message.clone() };
                (m, Ok(()))
            }
            DisconnectReason::Error(e) => (format!("{e:#}"), Err(e)),
        };
        if !self.closing.load(Ordering::Relaxed) {
            let mut s = self.shared.lock();
            if s.state.is_connected() {
                s.state = ConnState::Closed(Some(ConnError::Disconnected(message)));
            }
        }
        self.closed.notify_waiters();
        self.closed.notify_one();
        self.ctx.request_repaint();
        result
    }
}

/// Posts a prompt and waits for the user's answer (`None` if the prompt was dismissed or the
/// tab went away).
async fn ask<T>(
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    prompting: &AtomicBool,
    make: impl FnOnce(oneshot::Sender<T>) -> Prompt,
) -> Option<T> {
    let (tx, rx) = oneshot::channel();
    shared.lock().prompt = Some(make(tx));
    prompting.store(true, Ordering::Relaxed);
    ctx.request_repaint();
    let answer = rx.await.ok();
    prompting.store(false, Ordering::Relaxed);
    shared.lock().prompt = None;
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
        ciphers.extend([cipher::AES_128_GCM, cipher::AES_256_CBC, cipher::AES_192_CBC, cipher::AES_128_CBC]);
        p.cipher = ciphers.into();
        let mut macs: Vec<mac::Name> = p.mac.to_vec();
        macs.extend([mac::HMAC_SHA1_ETM, mac::HMAC_SHA1]);
        p.mac = macs.into();
        cfg.preferred = p;
    }
    cfg
}

fn classify_russh(e: anyhow::Error, key_changed: bool, host: &str) -> ConnError {
    if let Some(re) = e.downcast_ref::<russh::Error>() {
        return match re {
            russh::Error::UnknownKey if key_changed => ConnError::HostKeyChanged,
            russh::Error::UnknownKey => ConnError::HostKeyRejected,
            russh::Error::Version => ConnError::Other(tf("{} did not answer as an SSH server.", &[host])),
            russh::Error::NoCommonAlgo { .. } => ConnError::Other(tf(
                "No common algorithm with the server ({}). Try enabling legacy algorithms in the bookmark.",
                &[&re.to_string()],
            )),
            russh::Error::Disconnect | russh::Error::HUP => {
                ConnError::Disconnected(t("the server closed the connection during the handshake").into())
            }
            russh::Error::IO(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => ConnError::Refused,
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

async fn tcp_connect(host: &str, port: u16) -> Result<tokio::net::TcpStream, ConnError> {
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| ConnError::HostNotFound(host.to_string()))?
        .collect();
    if addrs.is_empty() {
        return Err(ConnError::HostNotFound(host.to_string()));
    }
    let mut last = ConnError::Timeout;
    for addr in addrs {
        match tokio::time::timeout(TCP_TIMEOUT, tokio::net::TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                stream.set_nodelay(true).ok();
                return Ok(stream);
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => last = ConnError::Refused,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => last = ConnError::Timeout,
            Ok(Err(e)) => last = ConnError::Unreachable(e.to_string()),
            Err(_) => last = ConnError::Timeout,
        }
    }
    Err(last)
}

struct Flags {
    prompting: Arc<AtomicBool>,
    key_changed: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    closed: Arc<Notify>,
}

async fn establish(spec: &ConnectSpec, shared: &Arc<Mutex<ConnShared>>, ctx: &Context, flags: &Flags) -> Result<SshHandle, ConnError> {
    let b = &spec.bookmark;
    let host = b.host.trim().to_string();
    let stream = tcp_connect(&host, b.port).await?;
    let handler = SshHandler {
        host: host.clone(),
        port: b.port,
        known_hosts: spec.known_hosts.clone(),
        shared: shared.clone(),
        ctx: ctx.clone(),
        prompting: flags.prompting.clone(),
        key_changed: flags.key_changed.clone(),
        closing: flags.closing.clone(),
        closed: flags.closed.clone(),
    };
    let config = Arc::new(client_config(b.legacy_algorithms, spec.keepalive_secs));
    let mut handle = tokio::select! {
        r = client::connect_stream(config, stream, handler) => {
            r.map_err(|e| classify_russh(e, flags.key_changed.load(Ordering::Relaxed), &host))?
        }
        _ = handshake_watchdog(flags.prompting.clone(), HANDSHAKE_TIMEOUT) => return Err(ConnError::Timeout),
    };
    set_state(shared, ctx, ConnState::Authenticating);
    authenticate(&mut handle, spec, shared, ctx, &flags.prompting).await?;
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
    tokio::task::spawn_blocking(move || secrets::get(id, kind).ok().flatten()).await.ok().flatten()
}

async fn keychain_set(id: uuid::Uuid, kind: SecretKind, value: String) {
    let _ = tokio::task::spawn_blocking(move || secrets::set(id, kind, Some(&value))).await;
}

fn methods(r: &AuthResult) -> Vec<MethodKind> {
    match r {
        AuthResult::Success => Vec::new(),
        AuthResult::Failure { remaining_methods, .. } => remaining_methods.iter().cloned().collect(),
    }
}

fn method_list(m: &[MethodKind]) -> String {
    m.iter().map(|k| <&'static str>::from(k)).collect::<Vec<_>>().join(", ")
}

async fn authenticate(
    handle: &mut SshHandle,
    spec: &ConnectSpec,
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    prompting: &AtomicBool,
) -> Result<(), ConnError> {
    let b = &spec.bookmark;
    let user = if b.username.trim().is_empty() { local_user() } else { b.username.trim().to_string() };
    let other = |e: russh::Error| ConnError::Other(e.to_string());

    let key_text = match &b.auth {
        Auth::Password => None,
        Auth::Key { path } => {
            let path = paths::expand_tilde(path);
            Some((keys::read_key_file(&path).map_err(|e| ConnError::KeyFile(e.to_string()))?, path.display().to_string()))
        }
        Auth::KeychainKey => {
            let text = keychain_get(b.id, SecretKind::PrivateKey)
                .await
                .ok_or_else(|| ConnError::KeyFile(t("the private key is missing from the Keychain").into()))?;
            Some((text, "Keychain".to_string()))
        }
    };

    let mut fallback_methods: Option<Vec<MethodKind>> = None;
    if let Some((text, label)) = key_text {
        let mut passphrase = if keys::is_encrypted(&text) { keychain_get(b.id, SecretKind::Passphrase).await } else { None };
        let mut save = false;
        let mut attempts = 0;
        let key: PrivateKey = loop {
            match keys::decode_key(&text, passphrase.as_deref()) {
                Ok(k) => break k,
                Err(e @ (KeyLoadError::NeedsPassphrase | KeyLoadError::WrongPassphrase)) if attempts < MAX_ATTEMPTS => {
                    let retry = e == KeyLoadError::WrongPassphrase;
                    let message = label.clone();
                    let answer = ask(shared, ctx, prompting, |reply| Prompt::Secret {
                        kind: SecretPromptKind::Passphrase,
                        message,
                        echo: false,
                        can_save: true,
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
        let result = auth_publickey(handle, &user, key).await.map_err(other)?;
        if result.success() {
            if save {
                if let Some(p) = passphrase {
                    keychain_set(b.id, SecretKind::Passphrase, p).await;
                }
            }
            return Ok(());
        }
        let remaining = methods(&result);
        if !remaining.iter().any(|m| matches!(m, MethodKind::Password | MethodKind::KeyboardInteractive)) {
            return Err(ConnError::AuthFailed(tf(
                "the server rejected the key for user {} (server accepts: {})",
                &[&user, &method_list(&remaining)],
            )));
        }
        fallback_methods = Some(remaining);
    }

    // Password (and keyboard-interactive, which many PAM setups use for passwords).
    let mut stored = if fallback_methods.is_none() { keychain_get(b.id, SecretKind::Password).await } else { None };
    let mut attempts = 0;
    let mut retry = false;
    loop {
        let (password, save) = match stored.take() {
            Some(p) => (p, false),
            None => {
                let message = format!("{user}@{}", b.host);
                let answer = ask(shared, ctx, prompting, |reply| Prompt::Secret {
                    kind: SecretPromptKind::Password,
                    message,
                    echo: false,
                    can_save: true,
                    retry,
                    reply,
                })
                .await
                .flatten()
                .ok_or(ConnError::Cancelled)?;
                (answer.value, answer.save)
            }
        };
        let result = handle.authenticate_password(&user, &password).await.map_err(other)?;
        let mut ok = result.success();
        let remaining = methods(&result);
        if !ok && remaining.contains(&MethodKind::KeyboardInteractive) {
            ok = keyboard_interactive(handle, &user, &password, shared, ctx, prompting).await?;
        }
        if ok {
            if save {
                keychain_set(b.id, SecretKind::Password, password).await;
            }
            return Ok(());
        }
        if !ok && !remaining.iter().any(|m| matches!(m, MethodKind::Password | MethodKind::KeyboardInteractive)) {
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

async fn auth_publickey(handle: &mut SshHandle, user: &str, key: PrivateKey) -> Result<AuthResult, russh::Error> {
    let key = Arc::new(key);
    if key.algorithm().is_rsa() {
        let candidates: Vec<Option<HashAlg>> = match handle.best_supported_rsa_hash().await? {
            Some(h) => vec![h],
            None => vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256), None],
        };
        let mut last = None;
        for hash in candidates {
            let r = handle.authenticate_publickey(user, PrivateKeyWithHashAlg::new(key.clone(), hash)).await?;
            if r.success() {
                return Ok(r);
            }
            last = Some(r);
        }
        return Ok(last.expect("at least one candidate"));
    }
    handle.authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, None)).await
}

/// Keyboard-interactive login: password-like hidden prompts get the password, anything else
/// (e.g. a one-time code) is asked of the user.
async fn keyboard_interactive(
    handle: &mut SshHandle,
    user: &str,
    password: &str,
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
    prompting: &AtomicBool,
) -> Result<bool, ConnError> {
    let other = |e: russh::Error| ConnError::Other(e.to_string());
    let mut response = handle.authenticate_keyboard_interactive_start(user, None::<String>).await.map_err(other)?;
    let mut password_used = false;
    for _ in 0..8 {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
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
                    let answer = ask(shared, ctx, prompting, |reply| Prompt::Secret {
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
                response = handle.authenticate_keyboard_interactive_respond(answers).await.map_err(other)?;
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
        closing: Arc::new(AtomicBool::new(false)),
        closed: Arc::new(Notify::new()),
    };
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
            set_state(&shared, &ctx, ConnState::Closed(None));
            fail_all(queued, cmd_rx, t("Connection cancelled."));
            return;
        }
        Some(Err(e)) => {
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
                let reason = match &shared.lock().state {
                    ConnState::Closed(Some(e)) => e.message(),
                    _ => t("Connection closed.").to_string(),
                };
                fail_all(Vec::new(), cmd_rx, &reason);
                return;
            }
        }
    }
}

async fn close(handle: &SshHandle, flags: &Flags, shared: &Arc<Mutex<ConnShared>>, ctx: &Context) {
    flags.closing.store(true, Ordering::Relaxed);
    let _ = tokio::time::timeout(
        Duration::from_secs(3),
        handle.disconnect(Disconnect::ByApplication, "", "en"),
    )
    .await;
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
    let channel = handle.channel_open_session().await.map_err(|e| e.to_string())?;
    channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
    // Bigger write packets and a deeper write pipeline: with the defaults uploads are capped at
    // 16 x 32 KiB per round trip. More than 16 concurrent reads measured slower, so reads keep
    // the default depth.
    let config = russh_sftp::client::Config {
        max_packet_len: 256 * 1024,
        max_write_packet_len: 255 * 1024,
        max_concurrent_writes: 64,
        max_concurrent_reads: 16,
        request_timeout_secs: 60,
    };
    SftpSession::new_with_config(channel.into_stream(), config)
        .await
        .map_err(|e| tf("SFTP is not available on this server ({})", &[&e.to_string()]))
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
    let PaneStart { feeder, input_tx, mut input_rx, cols, rows, px_w, px_h, start_dir, startup_command, status } = pane;
    let open = async {
        let channel = handle.channel_open_session().await?;
        let modes = [(Pty::ECHO, 1), (Pty::ICRNL, 1), (Pty::IUTF8, 1), (Pty::TTY_OP_ISPEED, 38400), (Pty::TTY_OP_OSPEED, 38400)];
        channel.request_pty(false, "xterm-256color", cols, rows, px_w, px_h, &modes).await?;
        // Like OpenSSH's default `SendEnv LANG LC_*`: without a UTF-8 locale the remote shell
        // mangles non-ASCII text. Servers that don't accept it simply ignore the request.
        channel.set_env(false, "LANG", remote_lang()).await?;
        channel.request_shell(false).await?;
        Ok::<_, russh::Error>(channel)
    };
    let channel = match open.await {
        Ok(c) => c,
        Err(e) => {
            *status.lock() = PaneStatus::Failed(e.to_string());
            tcore::write_status(&mut feeder.lock(), &tf("Could not open a shell: {}", &[&e.to_string()]));
            ctx.request_repaint();
            return;
        }
    };
    *status.lock() = PaneStatus::Running;
    ctx.request_repaint();

    let (mut reader, writer) = channel.split();
    let writer: Arc<ChannelWriteHalf<Msg>> = Arc::new(writer);

    if !start_dir.trim().is_empty() {
        let _ = input_tx.send(PaneInput::Data(format!("{}\r", cd_command(&start_dir)).into_bytes()));
    }
    for line in startup_command.lines().filter(|l| !l.trim().is_empty()) {
        let _ = input_tx.send(PaneInput::Data(format!("{line}\r").into_bytes()));
    }

    // Writer: input, resizes and parser replies. Runs separately so a full remote window
    // never stops output from being read.
    let w = writer.clone();
    let write_task = tokio::spawn(async move {
        while let Some(input) = input_rx.recv().await {
            let r = match input {
                PaneInput::Data(bytes) => w.data_bytes(bytes).await,
                PaneInput::Resize { cols, rows, px_w, px_h } => w.window_change(cols, rows, px_w, px_h).await,
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
    loop {
        let deadline = feeder.lock().sync_deadline();
        let sleep = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            msg = reader.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let (reply, repaint) = feeder.lock().feed(&data);
                    if !reply.is_empty() {
                        let _ = input_tx.send(PaneInput::Data(reply));
                    }
                    if repaint {
                        ctx.request_repaint();
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
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
    let message = match exit_status {
        Some(code) => tf("[Session ended, exit code {}. Press Enter or ⌘R to reconnect.]", &[&code.to_string()]),
        None => t("[Connection closed. Press Enter or ⌘R to reconnect.]").to_string(),
    };
    tcore::write_status(&mut feeder.lock(), &message);
    *status.lock() = PaneStatus::Exited(exit_status);
    ctx.request_repaint();
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
}
