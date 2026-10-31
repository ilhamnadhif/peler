//! Application data locations and private (0600, atomic) file writes.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub const APP_NAME: &str = "peler";

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Data directory: `~/Library/Application Support/peler`, or `PELER_DATA_DIR` when set in a
/// debug build (tests and the scripted UI checks; release builds ignore it).
pub fn data_dir() -> &'static Path {
    DATA_DIR.get_or_init(|| {
        if cfg!(debug_assertions)
            && let Some(dir) = std::env::var_os("PELER_DATA_DIR")
        {
            return PathBuf::from(dir);
        }
        dirs::data_dir()
            .unwrap_or_else(|| home_dir().join("Library/Application Support"))
            .join(APP_NAME)
    })
}

/// Where crash reports go: `~/Library/Logs/Peler` on macOS, `%LOCALAPPDATA%\Peler\Logs` on
/// Windows, `~/.local/state/peler/logs` on Linux.
pub fn logs_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_dir().join("Library/Logs/Peler")
    } else if cfg!(windows) {
        dirs::data_local_dir()
            .unwrap_or_else(home_dir)
            .join("Peler")
            .join("Logs")
    } else {
        dirs::state_dir()
            .unwrap_or_else(|| home_dir().join(".local/state"))
            .join(APP_NAME)
            .join("logs")
    }
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn bookmarks_file() -> PathBuf {
    data_dir().join("bookmarks.json")
}

pub fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn known_hosts_file() -> PathBuf {
    data_dir().join("known_hosts")
}

/// Creates the directory with mode 0700 if it does not exist yet. (On Windows the data folders
/// in the user profile are private to the user already.)
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes a file atomically with mode 0600: write to a temp file, `fsync`, then `rename`.
/// The previous file is never left half-written.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        ensure_private_dir(dir)?;
    }
    // Unique per write, so two writers (threads, or a second app instance) never share a
    // temporary file; `create_new` refuses to follow anything already at that name.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut f = options.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        #[cfg(unix)]
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        // Replaces an existing file on Windows too.
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Replaces a leading `~` with the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("~\\").filter(|_| cfg!(windows)))
    {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

/// Inverse of `expand_tilde`, for display and for exports that are portable across machines.
pub fn contract_tilde(path: &Path) -> String {
    let home = home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => {
            let rest = rest.to_string_lossy();
            // Forward slashes on Windows too, so exported paths read the same everywhere.
            #[cfg(windows)]
            let rest = rest.replace('\\', "/");
            format!("~/{rest}")
        }
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_private_sets_0600_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/file.json");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        // No temp file left behind.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn tilde_round_trip() {
        let p = expand_tilde("~/.ssh/id_ed25519");
        assert!(p.is_absolute());
        assert_eq!(contract_tilde(&p), "~/.ssh/id_ed25519");
        assert_eq!(expand_tilde("/etc/hosts"), PathBuf::from("/etc/hosts"));
    }
}
