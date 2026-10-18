//! Host-key edge cases: a revoked key is refused without asking, and abandoning a connection
//! while its host-key prompt is up ends it cleanly.

mod common;

use common::{TestServer, user, wait_for};
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark};
use peler::ssh::session::{ConnectSpec, Connection};
use peler::ssh::types::{ConnError, ConnState, Prompt};

fn spec(server: &TestServer, known: &std::path::Path) -> ConnectSpec {
    ConnectSpec {
        bookmark: Bookmark {
            host: "127.0.0.1".into(),
            port: server.port,
            username: user(),
            auth: Auth::Key {
                path: server.key.display().to_string(),
            },
            ..Default::default()
        },
        keepalive_secs: 30,
        jumps: Vec::new(),
        known_hosts: known.to_path_buf(),
        save_secrets: false,
    }
}

#[test]
fn revoked_key_and_cancelled_prompt() {
    let server = TestServer::start();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let ctx = egui::Context::default();

    // Revoked: refused outright, no prompt.
    let real: String = server
        .host_public_key()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let revoked = server.dir.path().join("revoked_known_hosts");
    std::fs::write(&revoked, format!("@revoked * {real}\n")).unwrap();
    let conn = Connection::start(rt.handle(), ctx.clone(), spec(&server, &revoked));
    let state = wait_for(15, "revoked key refused", || {
        assert!(
            conn.shared.lock().prompt.is_none(),
            "a revoked key must not be offered"
        );
        match conn.state() {
            ConnState::Closed(e) => Some(e),
            _ => None,
        }
    });
    assert_eq!(state, Some(ConnError::HostKeyRevoked));

    // Unknown host, and the tab is closed while the prompt is up.
    let fresh = server.dir.path().join("fresh_known_hosts");
    let conn = Connection::start(rt.handle(), ctx.clone(), spec(&server, &fresh));
    wait_for(15, "host key prompt", || {
        matches!(conn.shared.lock().prompt, Some(Prompt::HostKey { .. })).then_some(())
    });
    conn.disconnect();
    wait_for(10, "connection ends", || {
        (conn.state() == ConnState::Closed(None)).then_some(())
    });
    assert!(conn.shared.lock().prompt.is_none());
    assert!(!fresh.exists(), "nothing learned for an unanswered prompt");
}
