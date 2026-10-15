//! The built-in editor's load and save against a real OpenSSH server.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use common::{TestServer, user, wait_for};
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark};
use peler::sftp::edit::{self, LoadError, SaveError};
use peler::sftp::remote;
use peler::ssh::session::{ConnectSpec, Connection};
use peler::ssh::types::Prompt;

fn connect(rt: &tokio::runtime::Runtime, server: &TestServer) -> (Connection, remote::Sftp) {
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
        keepalive_secs: 30,
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
    let sftp = rt.block_on(conn.open_sftp()).unwrap().unwrap();
    (conn, Arc::new(sftp))
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn load_edit_save() {
    let server = TestServer::start();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (_conn, sftp) = connect(&rt, &server);
    let dir = server.dir.path().join("edit");
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("app.yml");
    std::fs::write(&file, "port: 80\r\nhost: a\r\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let path = file.display().to_string();

    // Load: CRLF is normalized for editing and restored on save; the mode is kept.
    let loaded = rt.block_on(edit::load(&sftp, &path)).unwrap();
    assert_eq!(loaded.text, "port: 80\nhost: a\n");
    assert!(loaded.crlf);
    let new_text = loaded.text.replace("80", "8080");
    let data = edit::encode(&new_text, loaded.crlf, loaded.bom);
    let stamp = rt
        .block_on(edit::save(&sftp, &path, &data, Some(loaded.stamp)))
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"port: 8080\r\nhost: a\r\n");
    assert_eq!(mode(&file), 0o640);
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers.len(), 1, "no temporary files left: {leftovers:?}");

    // Someone else changes the file: saving with the old stamp is refused, overwriting works.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&file, "changed elsewhere\n").unwrap();
    assert_eq!(
        rt.block_on(edit::save(&sftp, &path, b"mine\n", Some(stamp))),
        Err(SaveError::Changed)
    );
    rt.block_on(edit::save(&sftp, &path, b"mine\n", None))
        .unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine\n");

    // A symlink stays a symlink; the file it points to is changed.
    let link = dir.join("link.yml");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let link_path = link.display().to_string();
    rt.block_on(edit::save(&sftp, &link_path, b"via link\n", None))
        .unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "via link\n");

    // A folder where files can't be created: written in place.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let saved = rt.block_on(edit::save(&sftp, &path, b"in place\n", None));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    saved.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "in place\n");

    // Binary and oversized files are not opened.
    let bin = dir.join("tool.bin");
    std::fs::write(&bin, b"\x7fELF\0\0\x01").unwrap();
    assert_eq!(
        rt.block_on(edit::load(&sftp, &bin.display().to_string()))
            .unwrap_err(),
        LoadError::NotText
    );
    let big = dir.join("big.log");
    std::fs::write(&big, vec![b'x'; edit::MAX_EDIT_BYTES as usize + 1]).unwrap();
    assert!(matches!(
        rt.block_on(edit::load(&sftp, &big.display().to_string())),
        Err(LoadError::TooLarge(_))
    ));
}
