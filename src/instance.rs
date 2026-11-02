//! One app instance per user. A second launch (for example `ternaryshell web-1` from a terminal while
//! the app is open) hands its arguments to the running instance and exits, so two processes
//! never edit the same bookmarks and settings files.
//!
//! The arguments travel over a Unix socket on macOS and Linux. On Windows the running instance
//! listens on a loopback port and writes the port with a random token into the private data
//! folder; a later launch has to present the token, so other users' programs can't send it
//! connections to open.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use eframe::egui;
use parking_lot::Mutex;

use crate::paths;

/// Largest message accepted from another launch.
const MAX_MESSAGE: u64 = 64 * 1024;

pub enum Instance {
    /// This process runs the app. `lock` is held for the process lifetime; `inbox` receives
    /// the arguments of later launches.
    Primary {
        lock: Option<File>,
        inbox: Option<Inbox>,
    },
    /// Another instance is running and took over the arguments.
    Forwarded,
}

fn lock_path() -> PathBuf {
    paths::data_dir().join("instance.lock")
}

/// The socket lives in the per-user temporary folder (private on macOS): socket paths are
/// limited to about 100 bytes, which a long home folder path can exceed. The name is derived
/// from the data folder, so separate data folders (tests) don't meet.
#[cfg(unix)]
fn socket_path() -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in paths::data_dir().as_os_str().as_encoded_bytes() {
        hash = (hash ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    std::env::temp_dir().join(format!("ternaryshell-{hash:016x}.sock"))
}

/// Becomes the primary instance, or forwards `args` to the running one.
pub fn acquire(args: &[String]) -> Instance {
    let _ = paths::ensure_private_dir(paths::data_dir());
    let lock = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path())
    {
        Ok(f) => f,
        // Without a lock file there is no way to coordinate: just run.
        Err(_) => {
            return Instance::Primary {
                lock: None,
                inbox: None,
            };
        }
    };
    match lock.try_lock() {
        Ok(()) => Instance::Primary {
            lock: Some(lock),
            inbox: listen(),
        },
        Err(TryLockError::WouldBlock) if forward(args) => Instance::Forwarded,
        // The other instance doesn't answer (still starting, or hung): better a second window
        // than a launch that does nothing.
        Err(_) => Instance::Primary {
            lock: None,
            inbox: None,
        },
    }
}

#[cfg(unix)]
fn listen() -> Option<Inbox> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok()?;
    // Only this user can connect (the socket's folder is private), so no token is needed.
    Some(serve(move || listener.accept().map(|(s, _)| s), |_| true))
}

/// Where the running instance on Windows leaves its port and token (in the private data folder).
#[cfg(windows)]
fn port_file() -> PathBuf {
    paths::data_dir().join("instance.port")
}

#[cfg(windows)]
const TOKEN_LEN: usize = 64;

#[cfg(windows)]
fn listen() -> Option<Inbox> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let mut raw = [0u8; TOKEN_LEN / 2];
    getrandom::fill(&mut raw).ok()?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    paths::write_private(&port_file(), format!("{port} {token}").as_bytes()).ok()?;
    Some(serve(
        move || listener.accept().map(|(s, _)| s),
        move |data: &mut Vec<u8>| {
            // The message starts with the token; what follows are the arguments.
            if data.len() < TOKEN_LEN || data[..TOKEN_LEN] != *token.as_bytes() {
                return false;
            }
            data.drain(..TOKEN_LEN);
            true
        },
    ))
}

/// Sends `args` to the running instance; true once it confirmed.
#[cfg(windows)]
fn forward(args: &[String]) -> bool {
    let Ok(json) = serde_json::to_vec(args) else {
        return false;
    };
    // The running instance may still be starting up and not listening yet.
    for _ in 0..15 {
        if let Some((port, token)) = std::fs::read_to_string(port_file()).ok().and_then(|s| {
            let (port, token) = s.trim().split_once(' ')?;
            Some((port.parse::<u16>().ok()?, token.to_string()))
        }) && let Ok(mut stream) = std::net::TcpStream::connect_timeout(
            &(std::net::Ipv4Addr::LOCALHOST, port).into(),
            Duration::from_secs(1),
        ) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
            let mut message = token.into_bytes();
            message.extend_from_slice(&json);
            let mut ack = [0u8; 1];
            return stream.write_all(&message).is_ok()
                && stream.shutdown(std::net::Shutdown::Write).is_ok()
                && stream.read_exact(&mut ack).is_ok();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Sends `args` to the running instance; true once it confirmed.
#[cfg(unix)]
fn forward(args: &[String]) -> bool {
    let Ok(message) = serde_json::to_vec(args) else {
        return false;
    };
    // Only a socket created by this user is trusted with the arguments.
    use std::os::unix::fs::MetadataExt;
    let Ok(me) = std::fs::metadata(paths::data_dir()).map(|m| m.uid()) else {
        return false;
    };
    let path = socket_path();
    // The running instance may still be starting up and not listening yet.
    for _ in 0..15 {
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.uid() != me) {
            return false;
        }
        if let Ok(mut stream) = UnixStream::connect(&path) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
            let mut ack = [0u8; 1];
            let ok = stream.write_all(&message).is_ok()
                && stream.shutdown(std::net::Shutdown::Write).is_ok()
                && stream.read_exact(&mut ack).is_ok();
            return ok;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Arguments of later launches. They are answered from the start, while the window is still
/// being set up (a launch that waits too long for an answer would open a second instance);
/// the app picks them up once it runs.
pub struct Inbox {
    rx: mpsc::Receiver<Vec<String>>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
}

impl Inbox {
    /// From now on each later launch brings the window to the front. Returns where their
    /// arguments arrive (including any that came before).
    pub fn attach(self, ctx: &egui::Context) -> mpsc::Receiver<Vec<String>> {
        *self.ctx.lock() = Some(ctx.clone());
        ctx.request_repaint();
        self.rx
    }
}

/// Answers later launches, each a connection `next` waits for; `accept` checks (and may trim)
/// each message before its arguments are taken.
fn serve<S>(
    mut next: impl FnMut() -> std::io::Result<S> + Send + 'static,
    accept: impl Fn(&mut Vec<u8>) -> bool + Send + 'static,
) -> Inbox
where
    S: Read + Write + StreamTimeout,
{
    let (tx, rx) = mpsc::channel();
    let slot: Arc<Mutex<Option<egui::Context>>> = Arc::default();
    let ctx = slot.clone();
    std::thread::Builder::new()
        .name("ternaryshell-instance".into())
        .spawn(move || {
            loop {
                let Ok(mut stream) = next() else {
                    // e.g. out of file descriptors: don't spin.
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut data = Vec::new();
                if (&mut stream)
                    .take(MAX_MESSAGE)
                    .read_to_end(&mut data)
                    .is_err()
                {
                    continue;
                }
                if !accept(&mut data) {
                    continue;
                }
                let args: Vec<String> = serde_json::from_slice(&data).unwrap_or_default();
                let _ = stream.write_all(b"1");
                if tx.send(args).is_err() {
                    return;
                }
                if let Some(ctx) = &*ctx.lock() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.request_repaint();
                }
            }
        })
        .ok();
    Inbox { rx, ctx: slot }
}

/// The read timeout of a stream, for both kinds of connection.
trait StreamTimeout {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

#[cfg(unix)]
impl StreamTimeout for UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
}

#[cfg(windows)]
impl StreamTimeout for std::net::TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        std::net::TcpStream::set_read_timeout(self, timeout)
    }
}

/// The bookmark names and addresses among the process arguments. Options are skipped,
/// including the value of macOS's `-NS…` / `-Apple…` options (e.g. `-NSDocumentRevisions…
/// YES` from Xcode) and the `-psn_…` serial number Finder used to pass.
pub fn targets(args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg.starts_with("-NS") || arg.starts_with("-Apple") {
            args.next();
        } else if !arg.starts_with('-') && !arg.trim().is_empty() {
            out.push(arg);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::targets;

    #[test]
    fn options_are_not_targets() {
        let args = [
            "-NSDocumentRevisionsDebugMode",
            "YES",
            "-psn_0_1234",
            "web-1",
            "-v",
            "root@db:2222",
        ];
        assert_eq!(
            targets(args.iter().map(|s| s.to_string())),
            ["web-1", "root@db:2222"]
        );
    }
}
