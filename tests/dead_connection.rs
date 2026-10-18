//! A connection whose server stops answering (as after turning a VPN on: the socket stays
//! open, nothing comes back) is noticed by the connection check and closed with a reason.

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use common::{TestServer, user, wait_for};
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark};
use peler::ssh::session::{ConnectSpec, Connection};
use peler::ssh::types::{ConnError, ConnState, ProbeReason, Prompt};

fn connect(rt: &tokio::runtime::Runtime, server: &TestServer) -> Connection {
    let spec = ConnectSpec {
        bookmark: Bookmark {
            host: "127.0.0.1".into(),
            port: server.port,
            username: user(),
            auth: Auth::Key {
                path: server.key.display().to_string(),
            },
            ..Default::default()
        },
        // No keepalive: only the check may notice the dead server.
        keepalive_secs: 0,
        jumps: Vec::new(),
        known_hosts: server.dir.path().join("kh"),
        save_secrets: false,
    };
    let conn = Connection::start(rt.handle(), egui::Context::default(), spec);
    wait_for(15, "host key prompt", || {
        match conn.shared.lock().prompt.take() {
            Some(Prompt::HostKey { reply, .. }) => {
                let _ = reply.send(true);
                Some(())
            }
            _ => None,
        }
    });
    wait_for(15, "connected", || {
        conn.state().is_connected().then_some(())
    });
    conn
}

/// The sshd processes serving connections (children of the listener, and theirs).
fn session_processes(server: &TestServer) -> Vec<String> {
    let pid = std::fs::read_to_string(server.dir.path().join("sshd.pid"))
        .expect("sshd pid file")
        .trim()
        .to_string();
    let mut all = Vec::new();
    let mut parents = vec![pid];
    while let Some(parent) = parents.pop() {
        let out = Command::new("pgrep")
            .args(["-P", &parent])
            .output()
            .unwrap();
        for child in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            all.push(child.to_string());
            parents.push(child.to_string());
        }
    }
    all
}

fn signal(pids: &[String], sig: &str) {
    for pid in pids {
        let _ = Command::new("kill").args([sig, pid]).status();
    }
}

#[test]
fn a_server_that_stops_answering_is_noticed() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = TestServer::start();
    let conn = connect(&rt, &server);

    // A healthy server answers the check right away: nothing changes.
    let received = conn.received();
    conn.probe(ProbeReason::NetworkChanged);
    wait_for(5, "answer", || (conn.received() > received).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(conn.state().is_connected());
    assert!(conn.shared.lock().checking_since.is_none());

    // The server freezes: the socket stays open but nothing comes back.
    let sessions = session_processes(&server);
    assert!(!sessions.is_empty(), "no sshd session process found");
    signal(&sessions, "-STOP");
    let started = Instant::now();
    conn.probe(ProbeReason::NetworkChanged);
    wait_for(5, "checking", || {
        conn.shared.lock().checking_since.is_some().then_some(())
    });
    let state = wait_for(15, "closed", || {
        let s = conn.state();
        s.is_closed().then_some(s)
    });
    signal(&sessions, "-CONT");
    assert_eq!(
        state,
        ConnState::Closed(Some(ConnError::NotResponding(ProbeReason::NetworkChanged)))
    );
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(7) && took < Duration::from_secs(12),
        "closed after {took:?}"
    );
    assert!(conn.shared.lock().checking_since.is_none());
}
