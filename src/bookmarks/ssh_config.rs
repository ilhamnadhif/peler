//! Reader for OpenSSH client config files (`~/.ssh/config`).
//!
//! Every concrete `Host` alias becomes one entry. Options are resolved the way `ssh` does it:
//! blocks are scanned in file order and the first value obtained for each option wins, so
//! `Host *` defaults at the end of the file fill in what specific blocks leave unset.

use std::path::{Path, PathBuf};

use super::model::{Forward, ForwardKind};
use crate::paths;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SshConfigHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
    /// `ProxyJump`: the hops, as written (`none` leaves it empty).
    pub proxy_jump: Vec<String>,
    /// `IdentityAgent`: the agent's socket, empty for `SSH_AUTH_SOCK`.
    pub identity_agent: Option<String>,
    /// `LocalForward`, `RemoteForward` and `DynamicForward` (all of them apply, as in ssh).
    pub forwards: Vec<Forward>,
}

#[derive(Debug, Default)]
struct Block {
    patterns: Vec<String>,
    /// `Match` blocks are skipped: their conditions can't be evaluated statically.
    is_match: bool,
    options: Vec<(String, Vec<String>)>,
}

/// Largest config file read (anything bigger is not a hand-written config).
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Most files read through `Include` (globs that include each other can't multiply forever).
const MAX_INCLUDED_FILES: usize = 64;

/// Parses the user's config file at `path`, following `Include` directives.
pub fn parse_file(path: &Path) -> std::io::Result<Vec<SshConfigHost>> {
    let mut blocks = vec![Block::default()];
    let mut budget = Some(MAX_INCLUDED_FILES);
    read_into(path, &mut blocks, 0, &mut budget)?;
    Ok(resolve(&blocks))
}

/// Parses config text from an imported file. `Include` lines are ignored: a file from
/// elsewhere must not make the app read other local files.
pub fn parse_str(text: &str) -> Vec<SshConfigHost> {
    let mut blocks = vec![Block::default()];
    parse_text(text, &mut blocks, 0, &mut None);
    resolve(&blocks)
}

/// Reads one regular file of at most `MAX_FILE_BYTES` (never a device or FIFO).
fn read_config_text(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not a regular config file",
        ));
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES).read_to_string(&mut text)?;
    Ok(text)
}

/// `budget` is the number of files `Include` may still read; `None` disables `Include`.
fn read_into(
    path: &Path,
    blocks: &mut Vec<Block>,
    depth: usize,
    budget: &mut Option<usize>,
) -> std::io::Result<()> {
    let text = read_config_text(path)?;
    parse_text(&text, blocks, depth, budget);
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

fn parse_text(text: &str, blocks: &mut Vec<Block>, depth: usize, budget: &mut Option<usize>) {
    for line in text.lines() {
        let Some((key, args)) = split_line(line) else {
            continue;
        };
        match key.as_str() {
            "host" => blocks.push(Block {
                patterns: args,
                ..Default::default()
            }),
            "match" => blocks.push(Block {
                is_match: true,
                ..Default::default()
            }),
            "include" if depth < 8 && budget.is_some() => {
                for arg in args {
                    for file in expand_include(&arg) {
                        match budget {
                            Some(n) if *n > 0 => *n -= 1,
                            _ => return,
                        }
                        // Included files start in the context of the current block, as in ssh.
                        let _ = read_into(&file, blocks, depth + 1, budget);
                    }
                }
            }
            "include" => {}
            _ => {
                if let Some(block) = blocks.last_mut()
                    && !args.is_empty()
                {
                    block.options.push((key, args));
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
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .is_some_and(|n| glob_match(&name, &n.to_string_lossy()))
        })
        .collect();
    files.sort();
    files
}

/// `*` and `?` wildcard match, as used in `Host` patterns (linear time, no recursion).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    crate::ssh::known_hosts::wildcard_match(pattern, text)
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
            let mut h = SshConfigHost {
                alias: alias.clone(),
                ..Default::default()
            };
            let mut jump_seen = false;
            for block in blocks.iter().filter(|b| block_matches(b, &alias)) {
                for (key, args) in &block.options {
                    let value = &args[0];
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
                        "proxyjump" if !jump_seen => {
                            jump_seen = true;
                            if !value.eq_ignore_ascii_case("none") {
                                h.proxy_jump = value
                                    .split(',')
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty())
                                    .collect();
                            }
                        }
                        "identityagent" if h.identity_agent.is_none() => {
                            h.identity_agent = Some(match value.as_str() {
                                "none" => continue,
                                "SSH_AUTH_SOCK" => String::new(),
                                v => v.replace("%d", &paths::home_dir().to_string_lossy()),
                            });
                        }
                        "localforward" | "remoteforward" | "dynamicforward" => {
                            if let Some(f) = forward(key, args) {
                                h.forwards.push(f);
                            }
                        }
                        _ => {}
                    }
                }
            }
            h
        })
        .collect()
}

/// A `LocalForward`/`RemoteForward` (`[bind:]port host:port`) or `DynamicForward`
/// (`[bind:]port`). Unix socket forwards are not supported (`None`).
fn forward(key: &str, args: &[String]) -> Option<Forward> {
    let port_of = |s: &str| -> Option<u16> { s.rsplit(':').next()?.parse().ok() };
    let listen_port = port_of(args.first()?)?;
    if key == "dynamicforward" {
        return Some(Forward {
            kind: ForwardKind::Dynamic,
            listen_port,
            host: String::new(),
            port: 0,
        });
    }
    let target = args.get(1)?;
    let (host, port) = target.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some(Forward {
        kind: if key == "localforward" {
            ForwardKind::Local
        } else {
            ForwardKind::Remote
        },
        listen_port,
        host: host.to_string(),
        port: port.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_hosts_agents_and_forwards() {
        let cfg = r#"
Host bastion
    HostName bastion.example.com
    IdentityAgent "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"

Host app
    HostName 10.0.0.9
    ProxyJump bastion
    LocalForward 8080 localhost:80
    LocalForward 127.0.0.1:5433 [::1]:5432
    RemoteForward 9000 localhost:3000
    DynamicForward 1080

Host *
    ProxyJump none
"#;
        let hosts = parse_str(cfg);
        let bastion = &hosts[0];
        assert!(bastion.proxy_jump.is_empty());
        assert_eq!(
            bastion.identity_agent.as_deref(),
            Some("~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock")
        );
        let app = &hosts[1];
        assert_eq!(app.proxy_jump, ["bastion"]);
        let f = |kind, listen_port, host: &str, port| Forward {
            kind,
            listen_port,
            host: host.into(),
            port,
        };
        assert_eq!(
            app.forwards,
            [
                f(ForwardKind::Local, 8080, "localhost", 80),
                f(ForwardKind::Local, 5433, "::1", 5432),
                f(ForwardKind::Remote, 9000, "localhost", 3000),
                f(ForwardKind::Dynamic, 1080, "", 0),
            ]
        );
    }

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
        assert_eq!(
            db.user.as_deref(),
            Some("fallback"),
            "first obtained value wins"
        );
        assert_eq!(db.port, Some(22));
        assert_eq!(db.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    }

    #[test]
    fn glob_and_quotes() {
        assert!(glob_match("*.lan", "pi.LAN"));
        assert!(glob_match("web-??", "web-01"));
        assert!(!glob_match("web-??", "web-1"));
        assert_eq!(
            split_args(r#""/path with space/key" x"#),
            ["/path with space/key", "x"]
        );
    }

    #[test]
    fn include_reads_relative_files() {
        let dir = tempfile::tempdir().unwrap();
        let inc = dir.path().join("extra.conf");
        std::fs::write(&inc, "Host extra\n  HostName 1.2.3.4\n").unwrap();
        let main = dir.path().join("config");
        std::fs::write(
            &main,
            format!("Include {}\nHost main\n  User me\n", inc.display()),
        )
        .unwrap();
        let hosts = parse_file(&main).unwrap();
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "extra");
        assert_eq!(hosts[0].hostname.as_deref(), Some("1.2.3.4"));

        // Imported text never follows Include.
        let text = std::fs::read_to_string(&main).unwrap();
        assert_eq!(parse_str(&text).len(), 1);
    }

    #[test]
    fn include_loops_and_devices_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        // Every file includes every file: without a budget this is 3^8 reads.
        for name in ["a.conf", "b.conf", "c.conf"] {
            std::fs::write(
                dir.path().join(name),
                format!(
                    "Include {}/*.conf\nInclude /dev/zero\nHost {}\n",
                    dir.path().display(),
                    name.trim_end_matches(".conf")
                ),
            )
            .unwrap();
        }
        let start = std::time::Instant::now();
        let hosts = parse_file(&dir.path().join("a.conf")).unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        let mut names: Vec<_> = hosts.iter().map(|h| h.alias.clone()).collect();
        names.sort();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
