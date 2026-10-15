//! File entries shown in the SFTP panel (local and remote), plus formatting helpers.

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<i64>,
    /// Unix permission bits (lower 12 bits), if known.
    pub mode: Option<u32>,
}

impl FileEntry {
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
}

/// Directories first, then by the chosen key (name ties broken case-insensitively).
pub fn sort_entries(entries: &mut [FileEntry], key: SortKey, descending: bool) {
    entries.sort_by(|a, b| {
        let dirs = b.is_dir.cmp(&a.is_dir);
        if dirs.is_ne() {
            return dirs;
        }
        let name = || {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.name.cmp(&b.name))
        };
        let ord = match key {
            SortKey::Name => name(),
            SortKey::Size => a.size.cmp(&b.size).then_with(name),
            SortKey::Modified => a.modified.cmp(&b.modified).then_with(name),
        };
        if descending { ord.reverse() } else { ord }
    });
}

/// Lists a local directory (not recursive). Unreadable entries are skipped.
pub fn list_local(dir: &Path) -> std::io::Result<Vec<FileEntry>> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let Ok(link_meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let is_symlink = link_meta.file_type().is_symlink();
        // Follow symlinks for type and size so linked folders can be opened.
        let meta = if is_symlink {
            entry.path().metadata().unwrap_or(link_meta)
        } else {
            link_meta
        };
        out.push(FileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            is_symlink,
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok().and_then(unix_secs),
            mode: Some(meta.permissions().mode() & 0o7777),
        });
    }
    Ok(out)
}

pub fn unix_secs(t: SystemTime) -> Option<i64> {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// "1.4 MB" style sizes (decimal units, like Finder).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1000.0;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

/// `rwxr-xr-x` from mode bits, with setuid/setgid/sticky shown like `ls`.
pub fn mode_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (bit, c) in bits {
        s.push(if mode & bit != 0 { c } else { '-' });
    }
    let mut chars: Vec<char> = s.chars().collect();
    let special = |chars: &mut Vec<char>, idx: usize, flag: bool, lower: char, upper: char| {
        if flag {
            chars[idx] = if chars[idx] == 'x' { lower } else { upper };
        }
    };
    special(&mut chars, 2, mode & 0o4000 != 0, 's', 'S');
    special(&mut chars, 5, mode & 0o2000 != 0, 's', 'S');
    special(&mut chars, 8, mode & 0o1000 != 0, 't', 'T');
    chars.into_iter().collect()
}

/// Parses an octal mode like "755" or "0644".
pub fn parse_mode(text: &str) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() || t.len() > 4 {
        return None;
    }
    u32::from_str_radix(t, 8).ok().filter(|m| *m <= 0o7777)
}

pub fn format_time(secs: i64) -> String {
    DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// Joins a remote (always '/'-separated) path.
pub fn remote_join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "." {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent of a remote path ("/" stays "/").
pub fn remote_parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None if path.starts_with('/') => "/".into(),
        Some(i) => trimmed[..i].to_string(),
        None => ".".into(),
    }
}

/// Final component of a remote path.
pub fn remote_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(25_000_000), "25 MB");
        assert_eq!(mode_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_string(0o4755), "rwsr-xr-x");
        assert_eq!(mode_string(0o1777), "rwxrwxrwt");
        assert_eq!(mode_string(0o644), "rw-r--r--");
        assert_eq!(parse_mode("0644"), Some(0o644));
        assert_eq!(parse_mode("9"), None);
        assert_eq!(parse_mode("77777"), None);
    }

    #[test]
    fn remote_paths() {
        assert_eq!(remote_join("/home/u", "f"), "/home/u/f");
        assert_eq!(remote_join("/", "etc"), "/etc");
        assert_eq!(remote_parent("/home/u"), "/home");
        assert_eq!(remote_parent("/home"), "/");
        assert_eq!(remote_parent("/"), "/");
        assert_eq!(remote_name("/a/b/c.txt"), "c.txt");
        assert_eq!(remote_name("/a/b/"), "b");
    }

    #[test]
    fn sorting_and_local_listing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("zdir")).unwrap();
        std::fs::write(dir.path().join("B.txt"), "12345").unwrap();
        std::fs::write(dir.path().join("a.txt"), "1").unwrap();
        let mut entries = list_local(dir.path()).unwrap();
        sort_entries(&mut entries, SortKey::Name, false);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", "a.txt", "B.txt"]);
        sort_entries(&mut entries, SortKey::Size, true);
        assert_eq!(entries[1].name, "B.txt");
        assert_eq!(entries[1].size, 5);
    }
}
