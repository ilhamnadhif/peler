//! known_hosts handling, written for this app instead of using russh's helper so that:
//! - a malformed line is skipped on its own instead of hiding every line after it,
//! - hashed hosts (`|1|salt|hash`, the default on Debian/Ubuntu), host lists and wildcards match,
//! - `@revoked` keys are refused outright,
//! - a key of a *different type* than the recorded one is reported as a change, not as a new
//!   host (an attacker can simply offer another key type),
//! - entries are replaced by content under a process-wide lock, never by line number.

use std::io;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, KeyInit, Mac};
use parking_lot::Mutex;
use russh::keys::{HashAlg, PublicKey};
use sha1::Sha1;

use crate::paths;

/// A key recorded for a host, for display: algorithm and SHA-256 fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownKey {
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// The offered key is recorded for this host.
    Known,
    /// The offered key is explicitly revoked.
    Revoked,
    /// Nothing is recorded for this host.
    Unknown,
    /// Different keys are recorded for this host (same or other key type).
    Changed { known: Vec<KnownKey> },
}

/// `host` or `[host]:port`, lowercased, as written in known_hosts.
pub fn host_spec(host: &str, port: u16) -> String {
    let host = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

struct Line<'a> {
    marker: Option<&'a str>,
    hosts: &'a str,
    algorithm: &'a str,
    key: &'a str,
}

fn parse_line(line: &str) -> Option<Line<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut parts = line.split_whitespace();
    let mut first = parts.next()?;
    let marker = if first.starts_with('@') {
        let m = first;
        first = parts.next()?;
        Some(m)
    } else {
        None
    };
    let algorithm = parts.next()?;
    let key = parts.next()?;
    Some(Line {
        marker,
        hosts: first,
        algorithm,
        key,
    })
}

fn hashed_matches(pattern: &str, spec: &str) -> bool {
    // |1|base64(salt)|base64(HMAC-SHA1(salt, host))
    let mut it = pattern.trim_start_matches("|1|").splitn(2, '|');
    let (Some(salt), Some(hash)) = (it.next(), it.next()) else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (B64.decode(salt), B64.decode(hash)) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(spec.as_bytes());
    mac.verify_slice(&hash).is_ok()
}

/// `*` / `?` wildcard match without recursion (linear backtracking on the last `*`).
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi].eq_ignore_ascii_case(&t[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Whether a known_hosts host field (comma-separated patterns, hashes, negations) covers `spec`.
fn hosts_match(field: &str, spec: &str) -> bool {
    let mut matched = false;
    for pattern in field.split(',') {
        if pattern.starts_with("|1|") {
            matched |= hashed_matches(pattern, spec);
        } else if let Some(neg) = pattern.strip_prefix('!') {
            if wildcard_match(neg, spec) {
                return false;
            }
        } else if wildcard_match(pattern, spec) {
            matched = true;
        }
    }
    matched
}

fn display_key(algorithm: &str, key_b64: &str) -> KnownKey {
    let fingerprint = russh::keys::parse_public_key_base64(key_b64)
        .map(|k| k.fingerprint(HashAlg::Sha256).to_string())
        .unwrap_or_else(|_| "?".into());
    KnownKey {
        algorithm: algorithm.to_string(),
        fingerprint,
    }
}

/// The offered key as (algorithm, base64 blob).
pub fn key_parts(key: &PublicKey) -> (String, String) {
    let openssh = key.to_openssh().unwrap_or_default();
    let mut parts = openssh.split_whitespace();
    let algorithm = parts.next().unwrap_or_default().to_string();
    let blob = parts.next().unwrap_or_default().to_string();
    (algorithm, blob)
}

/// Checks the offered key against the app's own file (errors reading it are fatal) and the
/// user's OpenSSH file (read-only, errors ignored).
pub fn check(
    own: &Path,
    user: Option<&Path>,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> io::Result<Check> {
    let spec = host_spec(host, port);
    let (algorithm, blob) = key_parts(key);
    let mut texts = Vec::new();
    match std::fs::read_to_string(own) {
        Ok(t) => texts.push(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(user) = user
        && let Ok(t) = std::fs::read_to_string(user)
    {
        texts.push(t);
    }
    let mut known = Vec::new();
    let mut found = false;
    for text in &texts {
        for line in text.lines().filter_map(parse_line) {
            if !hosts_match(line.hosts, &spec) {
                continue;
            }
            match line.marker {
                Some("@revoked") if line.key == blob => return Ok(Check::Revoked),
                Some(_) => continue, // @cert-authority and others: not a plain host key
                None => {}
            }
            if line.algorithm == algorithm && line.key == blob {
                found = true;
            } else {
                known.push(display_key(line.algorithm, line.key));
            }
        }
    }
    if found {
        Ok(Check::Known)
    } else if known.is_empty() {
        Ok(Check::Unknown)
    } else {
        known.dedup();
        Ok(Check::Changed { known })
    }
}

/// Key types recorded for a host, so the client can ask the server for one of those first
/// (as OpenSSH does) instead of triggering a "changed key" warning for a new key type.
pub fn known_algorithms(own: &Path, user: Option<&Path>, host: &str, port: u16) -> Vec<String> {
    let spec = host_spec(host, port);
    let mut out = Vec::new();
    for path in std::iter::once(own).chain(user) {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines().filter_map(parse_line) {
            if line.marker.is_none()
                && hosts_match(line.hosts, &spec)
                && !out.iter().any(|a| a == line.algorithm)
            {
                out.push(line.algorithm.to_string());
            }
        }
    }
    out
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Records `key` for the host in the app's file, replacing any entry this app wrote for the
/// same host (by content, under a lock, so concurrent tabs can't lose each other's entries).
pub fn learn(own: &Path, host: &str, port: u16, key: &PublicKey) -> io::Result<()> {
    let _guard = WRITE_LOCK.lock();
    let spec = host_spec(host, port);
    let (algorithm, blob) = key_parts(key);
    let existing = match std::fs::read_to_string(own) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut out = String::new();
    for raw in existing.lines() {
        let replaced = parse_line(raw)
            .is_some_and(|l| l.marker.is_none() && l.hosts.eq_ignore_ascii_case(&spec));
        if !replaced {
            out.push_str(raw);
            out.push('\n');
        }
    }
    out.push_str(&format!("{spec} {algorithm} {blob}\n"));
    paths::write_private(own, out.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keygen(dir: &Path, name: &str, kind: &str) -> PublicKey {
        let path = dir.join(name);
        assert!(
            std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", kind, "-N", "", "-f"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let text = std::fs::read_to_string(path.with_extension("pub")).unwrap();
        russh::keys::parse_public_key_base64(text.split_whitespace().nth(1).unwrap()).unwrap()
    }

    #[test]
    fn known_unknown_changed_revoked_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let ed = keygen(dir.path(), "ed", "ed25519");
        let ed2 = keygen(dir.path(), "ed2", "ed25519");
        let ec = keygen(dir.path(), "ec", "ecdsa");
        let own = dir.path().join("own");
        let user = dir.path().join("user");

        assert_eq!(
            check(&own, None, "Web.Example", 22, &ed).unwrap(),
            Check::Unknown
        );
        learn(&own, "Web.Example", 22, &ed).unwrap();
        assert_eq!(
            check(&own, None, "web.example", 22, &ed).unwrap(),
            Check::Known,
            "case-insensitive"
        );
        assert!(matches!(
            check(&own, None, "web.example", 22, &ed2).unwrap(),
            Check::Changed { .. }
        ));
        // Another key type is a change too, not an unknown host.
        match check(&own, None, "web.example", 22, &ec).unwrap() {
            Check::Changed { known } => assert_eq!(known[0].algorithm, "ssh-ed25519"),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            known_algorithms(&own, None, "web.example", 22),
            ["ssh-ed25519"]
        );
        // Re-learning replaces the entry instead of piling up lines.
        learn(&own, "web.example", 22, &ed2).unwrap();
        assert_eq!(std::fs::read_to_string(&own).unwrap().lines().count(), 1);

        // A user file with a broken line, a hashed entry and a revoked key.
        let (alg, blob) = key_parts(&ec);
        let salt = [7u8; 20];
        let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt).unwrap();
        mac.update(b"[db.internal]:2222");
        let hashed = format!(
            "|1|{}|{}",
            B64.encode(salt),
            B64.encode(mac.finalize().into_bytes())
        );
        let (_, ed_blob) = key_parts(&ed);
        std::fs::write(
            &user,
            format!(
                "garbage line\nbroken ssh-ed25519 !!notbase64\n{hashed} {alg} {blob}\n@revoked * ssh-ed25519 {ed_blob}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            check(&own, Some(&user), "db.internal", 2222, &ec).unwrap(),
            Check::Known
        );
        assert_eq!(
            check(&own, Some(&user), "db.internal", 22, &ec).unwrap(),
            Check::Unknown,
            "port matters"
        );
        assert_eq!(
            check(&own, Some(&user), "anything", 22, &ed).unwrap(),
            Check::Revoked
        );
    }

    #[test]
    fn wildcards_without_recursion() {
        assert!(wildcard_match("*.example.com", "a.example.com"));
        assert!(wildcard_match("h?st", "host"));
        assert!(!wildcard_match("*.example.com", "example.com"));
        let long = "*".repeat(100_000);
        assert!(wildcard_match(&long, "x"));
        let evil = format!("{}b", "*a".repeat(30));
        assert!(!wildcard_match(&evil, &"a".repeat(60)));
    }
}
