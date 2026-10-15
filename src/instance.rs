//! One app instance per user. A second launch (for example `peler web-1` from a terminal while
//! the app is open) hands its arguments to the running instance and exits, so two processes
//! never edit the same bookmarks and settings files.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui;

use crate::paths;

/// Largest message accepted from another launch.
const MAX_MESSAGE: u64 = 64 * 1024;

pub enum Instance {
    /// This process runs the app. `lock` is held for the process lifetime; `listener` receives
    /// the arguments of later launches.
    Primary {
        lock: Option<File>,
        listener: Option<UnixListener>,
    },
    /// Another instance is running and took over the arguments.
    Forwarded,
}

fn lock_path() -> PathBuf {
    paths::data_dir().join("instance.lock")
}

fn socket_path() -> PathBuf {
    paths::data_dir().join("instance.sock")
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
                listener: None,
            };
        }
    };
    match lock.try_lock() {
        Ok(()) => {
            let path = socket_path();
            let _ = std::fs::remove_file(&path);
            Instance::Primary {
                lock: Some(lock),
                listener: UnixListener::bind(&path).ok(),
            }
        }
        Err(TryLockError::WouldBlock) if forward(args) => Instance::Forwarded,
        // The other instance doesn't answer (still starting, or hung): better a second window
        // than a launch that does nothing.
        Err(_) => Instance::Primary {
            lock: None,
            listener: None,
        },
    }
}

/// Sends `args` to the running instance; true once it confirmed.
fn forward(args: &[String]) -> bool {
    let Ok(message) = serde_json::to_vec(args) else {
        return false;
    };
    // The running instance may still be starting up and not listening yet.
    for _ in 0..15 {
        if let Ok(mut stream) = UnixStream::connect(socket_path()) {
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

/// Serves later launches: their arguments arrive on the returned channel, and the window is
/// brought to the front.
pub fn serve(listener: UnixListener, ctx: egui::Context) -> mpsc::Receiver<Vec<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("peler-instance".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut data = Vec::new();
                if (&mut stream)
                    .take(MAX_MESSAGE)
                    .read_to_end(&mut data)
                    .is_err()
                {
                    continue;
                }
                let args: Vec<String> = serde_json::from_slice(&data).unwrap_or_default();
                let _ = stream.write_all(b"1");
                if tx.send(args).is_err() {
                    return;
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.request_repaint();
            }
        })
        .ok();
    rx
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
