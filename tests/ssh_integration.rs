//! End-to-end tests of the SSH layer against a real (unprivileged) OpenSSH server.

mod common;

use std::sync::Arc;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use common::{TestServer, user, wait_for};
use eframe::egui;
use parking_lot::Mutex;
use peler::bookmarks::{Auth, Bookmark};
use peler::ssh::session::{ConnectSpec, Connection, PaneInput, PaneStart, PaneStatus};
use peler::ssh::types::{
    ConnError, ConnState, HostKeyStatus, Prompt, SecretAnswer, SecretPromptKind,
};
use peler::term::core::{Feeder, GridSize, new_terminal, term_config};
use peler::term::scheme::SCHEMES;
use tokio::sync::mpsc;

fn runtime() -> tokio::runtime::Runtime {
    // SAFETY: tests in this file set the same value before any other thread reads it.
    unsafe { std::env::set_var("PELER_SECRETS", "memory") };
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn spec(server: &TestServer, auth: Auth, known_hosts: &std::path::Path) -> ConnectSpec {
    ConnectSpec {
        bookmark: Bookmark {
            name: "test".into(),
            host: "127.0.0.1".into(),
            port: server.port,
            username: user(),
            auth,
            ..Default::default()
        },
        keepalive_secs: 30,
        known_hosts: known_hosts.to_path_buf(),
    }
}

/// Answers prompts as they appear; returns what was asked.
fn answer_prompt(conn: &Connection, host_key: bool, secret: Option<&str>) -> String {
    wait_for(15, "a prompt", || {
        let mut s = conn.shared.lock();
        match s.prompt.take() {
            Some(Prompt::HostKey {
                status,
                fingerprint,
                reply,
                ..
            }) => {
                assert!(fingerprint.starts_with("SHA256:"));
                let _ = reply.send(host_key);
                Some(format!("hostkey:{status:?}"))
            }
            Some(Prompt::Secret {
                kind, reply, retry, ..
            }) => {
                let _ = reply.send(secret.map(|v| SecretAnswer {
                    value: v.into(),
                    save: true,
                }));
                Some(format!("secret:{kind:?}:{retry}"))
            }
            None => None,
        }
    })
}

fn wait_state(conn: &Connection, pred: impl Fn(&ConnState) -> bool) -> ConnState {
    wait_for(15, "connection state", || {
        let st = conn.state();
        pred(&st).then_some(st)
    })
}

struct TestPane {
    feeder: Arc<Mutex<Feeder>>,
    input: mpsc::UnboundedSender<PaneInput>,
    status: Arc<Mutex<PaneStatus>>,
}

fn open_pane(conn: &Connection, start_dir: &str, startup: &str) -> TestPane {
    let (shared, events) = new_terminal(
        term_config(1000, Default::default(), false),
        GridSize {
            cols: 100,
            rows: 30,
        },
        (&SCHEMES[0]).into(),
    );
    let feeder = Arc::new(Mutex::new(Feeder::new(shared, events)));
    let (tx, rx) = mpsc::unbounded_channel();
    let status = Arc::new(Mutex::new(PaneStatus::Starting));
    conn.open_shell(PaneStart {
        feeder: feeder.clone(),
        input_tx: tx.clone(),
        input_rx: rx,
        cols: 100,
        rows: 30,
        px_w: 800,
        px_h: 480,
        start_dir: start_dir.into(),
        startup_command: startup.into(),
        status: status.clone(),
    });
    TestPane {
        feeder,
        input: tx,
        status,
    }
}

fn screen_text(p: &TestPane) -> String {
    let f = p.feeder.lock();
    let term = f.shared.term.lock();
    let mut out = String::new();
    for l in -(term.grid().history_size() as i32)..term.screen_lines() as i32 {
        let row = &term.grid()[Line(l)];
        let line: String = (0..term.columns()).map(|c| row[Column(c)].c).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[test]
fn key_auth_shell_known_hosts_and_errors() {
    let rt = runtime();
    let server = TestServer::start();
    let known = server.dir.path().join("peler_known_hosts");
    let ctx = egui::Context::default();

    // First connection: unknown host key is confirmed, key auth succeeds.
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(
            &server,
            Auth::Key {
                path: server.key.display().to_string(),
            },
            &known,
        ),
    );
    assert_eq!(answer_prompt(&conn, true, None), "hostkey:Unknown");
    wait_state(&conn, ConnState::is_connected);
    assert!(
        std::fs::read_to_string(&known)
            .unwrap()
            .contains(&format!("[127.0.0.1]:{}", server.port))
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&known).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "known_hosts must be private");
    }

    // Shell with start directory and startup command; then interactive input.
    let tmp = server.dir.path().display().to_string();
    let pane = open_pane(&conn, &tmp, "echo startup-$((40+2))");
    wait_for(10, "startup output", || {
        screen_text(&pane).contains("startup-42").then_some(())
    });
    pane.input
        .send(PaneInput::Data(
            b"pwd; printf 'x%.0s' 1 2 3; echo; tput cols\r".to_vec(),
        ))
        .unwrap();
    wait_for(10, "pwd output", || {
        let t = screen_text(&pane);
        (t.contains(&tmp.replace("/var/", "/private/var/")) || t.contains(&tmp)).then_some(())
    });
    wait_for(10, "tput cols", || {
        screen_text(&pane)
            .lines()
            .any(|l| l.trim() == "100")
            .then_some(())
    });
    pane.input
        .send(PaneInput::Resize {
            cols: 120,
            rows: 40,
            px_w: 960,
            px_h: 640,
        })
        .unwrap();
    pane.input
        .send(PaneInput::Data(b"stty size\r".to_vec()))
        .unwrap();
    wait_for(10, "stty size after resize", || {
        screen_text(&pane).contains("40 120").then_some(())
    });

    // Second pane on the same connection.
    let pane2 = open_pane(&conn, "", "echo second-pane");
    wait_for(10, "second pane", || {
        screen_text(&pane2).contains("second-pane").then_some(())
    });

    // Exit ends the pane with status 0.
    pane.input
        .send(PaneInput::Data(b"exit 3\r".to_vec()))
        .unwrap();
    wait_for(10, "pane exit", || {
        matches!(*pane.status.lock(), PaneStatus::Exited(Some(3))).then_some(())
    });
    conn.disconnect();
    wait_state(&conn, |s| *s == ConnState::Closed(None));

    // Second connection: host is known now, so no prompt; RSA key.
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(
            &server,
            Auth::Key {
                path: server.rsa_key.display().to_string(),
            },
            &known,
        ),
    );
    wait_state(&conn, ConnState::is_connected);
    assert!(conn.shared.lock().prompt.is_none());
    drop(conn);

    // ECDSA key.
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(
            &server,
            Auth::Key {
                path: server.ecdsa_key.display().to_string(),
            },
            &known,
        ),
    );
    wait_state(&conn, ConnState::is_connected);
    drop(conn);

    // Encrypted key: wrong passphrase, then right one.
    let s = spec(
        &server,
        Auth::Key {
            path: server.encrypted_key.display().to_string(),
        },
        &known,
    );
    let id = s.bookmark.id;
    let conn = Connection::start(rt.handle(), ctx.clone(), s.clone());
    assert_eq!(
        answer_prompt(&conn, true, Some("wrong")),
        "secret:Passphrase:false"
    );
    assert_eq!(
        answer_prompt(&conn, true, Some("open sesame")),
        "secret:Passphrase:true"
    );
    wait_state(&conn, ConnState::is_connected);
    drop(conn);
    // The passphrase was saved ("save: true"), so the next connection does not ask.
    assert_eq!(
        peler::secrets::get(id, peler::secrets::SecretKind::Passphrase)
            .unwrap()
            .as_deref(),
        Some("open sesame")
    );
    let conn = Connection::start(rt.handle(), ctx.clone(), s);
    wait_state(&conn, ConnState::is_connected);
    drop(conn);

    // Password auth is disabled on this server: clear message naming the allowed methods.
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(&server, Auth::Password, &known),
    );
    assert_eq!(
        answer_prompt(&conn, true, Some("pw")),
        "secret:Password:false"
    );
    let st = wait_state(&conn, ConnState::is_closed);
    match st {
        ConnState::Closed(Some(ConnError::AuthFailed(m))) => {
            assert!(m.contains("publickey"), "{m}")
        }
        other => panic!("unexpected {other:?}"),
    }

    // Cancelled prompt.
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(&server, Auth::Password, &known),
    );
    answer_prompt(&conn, true, None);
    assert_eq!(
        wait_state(&conn, ConnState::is_closed),
        ConnState::Closed(Some(ConnError::Cancelled))
    );

    // Changed host key: replace the stored key with another one → warned, rejected.
    let other = server.dir.path().join("other_host");
    common::keygen(&other, "ed25519", "");
    let other_pub = std::fs::read_to_string(other.with_extension("pub")).unwrap();
    let fake: String = other_pub
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(&known, format!("[127.0.0.1]:{} {fake}\n", server.port)).unwrap();
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(
            &server,
            Auth::Key {
                path: server.key.display().to_string(),
            },
            &known,
        ),
    );
    assert_eq!(answer_prompt(&conn, false, None), "hostkey:Changed");
    assert_eq!(
        wait_state(&conn, ConnState::is_closed),
        ConnState::Closed(Some(ConnError::HostKeyChanged))
    );
    // Accepting the changed key replaces the stored entry.
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(
            &server,
            Auth::Key {
                path: server.key.display().to_string(),
            },
            &known,
        ),
    );
    assert_eq!(answer_prompt(&conn, true, None), "hostkey:Changed");
    wait_state(&conn, ConnState::is_connected);
    let stored = std::fs::read_to_string(&known).unwrap();
    let real: String = server
        .host_public_key()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        stored.contains(&real) && !stored.contains(&fake),
        "{stored}"
    );
    drop(conn);
    let _ = HostKeyStatus::Unknown;
    let _ = SecretPromptKind::Password;
}

#[test]
fn network_errors_are_classified() {
    let rt = runtime();
    let ctx = egui::Context::default();
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("kh");
    let mk = |host: &str, port: u16| ConnectSpec {
        bookmark: Bookmark {
            host: host.into(),
            port,
            username: "x".into(),
            ..Default::default()
        },
        keepalive_secs: 0,
        known_hosts: known.clone(),
    };
    // A port with nothing listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let conn = Connection::start(rt.handle(), ctx.clone(), mk("127.0.0.1", port));
    assert_eq!(
        wait_state(&conn, ConnState::is_closed),
        ConnState::Closed(Some(ConnError::Refused))
    );

    let conn = Connection::start(rt.handle(), ctx.clone(), mk("no-such-host.invalid", 22));
    assert_eq!(
        wait_state(&conn, ConnState::is_closed),
        ConnState::Closed(Some(ConnError::HostNotFound("no-such-host.invalid".into())))
    );

    // Something that is not an SSH server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            use std::io::Write;
            let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }
    });
    let conn = Connection::start(rt.handle(), ctx, mk("127.0.0.1", port));
    match wait_state(&conn, ConnState::is_closed) {
        ConnState::Closed(Some(e)) => assert!(!matches!(e, ConnError::Timeout), "{e:?}"),
        other => panic!("{other:?}"),
    }
}
