//! Reader for OpenSSH client config files (`~/.ssh/config`).
//!
//! Every concrete `Host` alias becomes one entry. Options are resolved the way `ssh` does it:
//! blocks are scanned in file order and the first value obtained for each option wins, so
//! `Host *` defaults at the end of the file fill in what specific blocks leave unset.

use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfigHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
}

#[derive(Debug, Default)]
struct Block {
    patterns: Vec<String>,
    /// `Match` blocks are skipped: their conditions can't be evaluated statically.
    is_match: bool,
    options: Vec<(String, String)>,
}

/// Parses the config file at `path`, following `Include` directives.
pub fn parse_file(path: &Path) -> std::io::Result<Vec<SshConfigHost>> {
    let mut blocks = vec![Block::default()];
    read_into(path, &mut blocks, 0)?;
    Ok(resolve(&blocks))
}

/// Parses config text; `Include` is resolved relative to `~/.ssh`.
pub fn parse_str(text: &str) -> Vec<SshConfigHost> {
    let mut blocks = vec![Block::default()];
    parse_text(text, &mut blocks, 0);
    resolve(&blocks)
}

fn read_into(path: &Path, blocks: &mut Vec<Block>, depth: usize) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    parse_text(&text, blocks, depth);
    Ok(())
}

fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Keyword is separated by whitespace and/or a single '='.
    let idx = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..idx].to_ascii_lowercase();
    let rest = line[idx..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
    Some((key, split_args(rest)))
}

/// Splits arguments on whitespace, honouring double quotes.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

fn parse_text(text: &str, blocks: &mut Vec<Block>, depth: usize) {
    for line in text.lines() {
        let Some((key, args)) = split_line(line) else { continue };
        match key.as_str() {
            "host" => blocks.push(Block { patterns: args, ..Default::default() }),
            "match" => blocks.push(Block { is_match: true, ..Default::default() }),
            "include" if depth < 8 => {
                for arg in args {
                    for file in expand_include(&arg) {
                        // Included files start in the context of the current block, as in ssh.
                        let _ = read_into(&file, blocks, depth + 1);
                    }
                }
            }
            _ => {
                if let (Some(block), Some(value)) = (blocks.last_mut(), args.into_iter().next()) {
                    block.options.push((key, value));
                }
            }
        }
    }
}

fn expand_include(arg: &str) -> Vec<PathBuf> {
    let path = if arg.starts_with('~') || arg.starts_with('/') {
        paths::expand_tilde(arg)
    } else {
        paths::home_dir().join(".ssh").join(arg)
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return vec![] };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name().is_some_and(|n| glob_match(&name, &n.to_string_lossy()))
        })
        .collect();
    files.sort();
    files
}

/// `*` and `?` wildcard match, as used in `Host` patterns.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && rec(&p[1..], &t[1..]),
            Some(c) => t.first().is_some_and(|tc| tc.eq_ignore_ascii_case(c)) && rec(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    rec(&p, &t)
}

fn block_matches(block: &Block, alias: &str) -> bool {
    if block.is_match {
        return false;
    }
    if block.patterns.is_empty() {
        return true; // options before the first Host apply to every host
    }
    let mut matched = false;
    for pat in &block.patterns {
        if let Some(neg) = pat.strip_prefix('!') {
            if glob_match(neg, alias) {
                return false;
            }
        } else if glob_match(pat, alias) {
            matched = true;
        }
    }
    matched
}

fn resolve(blocks: &[Block]) -> Vec<SshConfigHost> {
    let mut aliases: Vec<String> = Vec::new();
    for b in blocks.iter().filter(|b| !b.is_match) {
        for p in &b.patterns {
            if !p.contains(['*', '?', '!']) && !aliases.contains(p) {
                aliases.push(p.clone());
            }
        }
    }
    aliases
        .into_iter()
        .map(|alias| {
            let mut h = SshConfigHost { alias: alias.clone(), ..Default::default() };
            for block in blocks.iter().filter(|b| block_matches(b, &alias)) {
                for (key, value) in &block.options {
                    match key.as_str() {
                        "hostname" if h.hostname.is_none() => {
                            h.hostname = Some(value.replace("%h", &alias))
                        }
                        "port" if h.port.is_none() => h.port = value.parse().ok(),
                        "user" if h.user.is_none() => h.user = Some(value.clone()),
                        "identityfile" if h.identity_file.is_none() => {
                            let home = paths::home_dir();
                            let v = value.replace("%d", &home.to_string_lossy());
                            h.identity_file = Some(v);
                        }
                        _ => {}
                    }
                }
            }
            h
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_with_defaults_and_negation() {
        let cfg = r#"
# global
User fallback

Host web prod-web
    HostName 10.0.0.5
    Port 2222
    IdentityFile ~/.ssh/id_web

Host db
  HostName=db.internal
  User = postgres

Host *.lan !nas.lan
  User lanuser

Host *
  IdentityFile ~/.ssh/id_ed25519
  Port 22

Match host foo
  User nobody
"#;
        let hosts = parse_str(cfg);
        let names: Vec<_> = hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(names, ["web", "prod-web", "db"]);
        let web = &hosts[0];
        assert_eq!(web.hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(web.port, Some(2222));
        assert_eq!(web.user.as_deref(), Some("fallback"));
        assert_eq!(web.identity_file.as_deref(), Some("~/.ssh/id_web"));
        let db = &hosts[2];
        assert_eq!(db.user.as_deref(), Some("fallback"), "first obtained value wins");
        assert_eq!(db.port, Some(22));
        assert_eq!(db.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    }

    #[test]
    fn glob_and_quotes() {
        assert!(glob_match("*.lan", "pi.LAN"));
        assert!(glob_match("web-??", "web-01"));
        assert!(!glob_match("web-??", "web-1"));
        assert_eq!(split_args(r#""/path with space/key" x"#), ["/path with space/key", "x"]);
    }

    #[test]
    fn include_reads_relative_files() {
        let dir = tempfile::tempdir().unwrap();
        let inc = dir.path().join("extra.conf");
        std::fs::write(&inc, "Host extra\n  HostName 1.2.3.4\n").unwrap();
        let main = dir.path().join("config");
        std::fs::write(&main, format!("Include {}\nHost main\n  User me\n", inc.display()))
            .unwrap();
        let hosts = parse_file(&main).unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "extra");
        assert_eq!(hosts[0].hostname.as_deref(), Some("1.2.3.4"));
    }
}
