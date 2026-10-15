//! Regression tests for review findings: host keys in the user's known_hosts, and reconnect.

mod common;

use std::time::Duration;

use alacritty_terminal::grid::Dimensions;

use common::{TestServer, user, wait_for};
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark};
use peler::settings::Settings;
use peler::ssh::session::{ConnectSpec, Connection, PaneStatus};
use peler::ssh::types::{HostKeyStatus, Prompt};
use peler::term::core::GridSize;
use peler::term::scheme::SCHEMES;
use peler::ui::tabs::{Env, Tab};

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn bookmark(server: &TestServer) -> Bookmark {
    Bookmark {
        host: "127.0.0.1".into(),
        port: server.port,
        username: user(),
        auth: Auth::Key {
            path: server.key.display().to_string(),
        },
        ..Default::default()
    }
}

/// Both scenarios share one process because the first one changes `HOME`.
#[test]
fn user_known_hosts_mismatch_and_reconnect() {
    let server = TestServer::start();
    let fake_home = tempfile::tempdir().unwrap();
    // SAFETY: set before any other thread in this test binary reads the environment.
    unsafe {
        std::env::set_var("PELER_SECRETS", "memory");
        std::env::set_var("HOME", fake_home.path());
    }
    let rt = runtime();
    let ctx = egui::Context::default();

    // ~/.ssh/known_hosts holds a different key for this host: must warn as "changed".
    let other = server.dir.path().join("other_host");
    common::keygen(&other, "ed25519", "");
    let other_pub: String = std::fs::read_to_string(other.with_extension("pub"))
        .unwrap()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::create_dir_all(fake_home.path().join(".ssh")).unwrap();
    std::fs::write(
        fake_home.path().join(".ssh/known_hosts"),
        format!("[127.0.0.1]:{} {other_pub}\n", server.port),
    )
    .unwrap();
    let app_known = server.dir.path().join("app_known_hosts");
    let spec = ConnectSpec {
        bookmark: bookmark(&server),
        keepalive_secs: 30,
        known_hosts: app_known.clone(),
    };
    let conn = Connection::start(rt.handle(), ctx.clone(), spec);
    let status = wait_for(15, "host key prompt", || {
        match conn.shared.lock().prompt.take() {
            Some(Prompt::HostKey { status, reply, .. }) => {
                let _ = reply.send(false);
                Some(status)
            }
            _ => None,
        }
    });
    assert_eq!(status, HostKeyStatus::Changed);
    drop(conn);

    // Trust the real key in the app's own file for the reconnect test.
    let real: String = server
        .host_public_key()
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(&app_known, format!("[127.0.0.1]:{} {real}\n", server.port)).unwrap();

    // Reconnecting a live tab: the old connection ending must not mark the new pane ended.
    let settings = Settings::default();
    let env = Env {
        rt: rt.handle(),
        ctx: &ctx,
        settings: &settings,
        scheme: &SCHEMES[0],
        known_hosts: app_known.clone(),
    };
    let mut tab = Tab::open(
        1,
        2,
        bookmark(&server),
        GridSize { cols: 80, rows: 24 },
        &env,
    );
    wait_for(15, "first shell", || {
        (*tab.panes[0].status.lock() == PaneStatus::Running).then_some(())
    });
    tab.reconnect(&env);
    wait_for(15, "shell after reconnect", || {
        (*tab.panes[0].status.lock() == PaneStatus::Running).then_some(())
    });
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        *tab.panes[0].status.lock(),
        PaneStatus::Running,
        "old connection must not end the new pane"
    );
    tab.panes[0].send(b"echo after-reconnect\r".to_vec());
    wait_for(10, "output after reconnect", || {
        let f = tab.panes[0].feeder.lock();
        let term = f.shared.term.lock();
        let text = term.bounds_to_string(
            alacritty_terminal::index::Point::new(
                term.topmost_line(),
                alacritty_terminal::index::Column(0),
            ),
            alacritty_terminal::index::Point::new(term.bottommost_line(), term.last_column()),
        );
        text.contains("after-reconnect\n").then_some(())
    });
}
