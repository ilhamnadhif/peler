//! SFTP over a real OpenSSH server: listing, file operations, transfers with verification.

mod common;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use common::{TestServer, user, wait_for};
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark};
use peler::sftp::entry::remote_join;
use peler::sftp::remote;
use peler::sftp::transfer::{TransferState, Transfers};
use peler::ssh::session::{ConnectSpec, Connection};
use peler::ssh::types::{ConnState, Prompt};

fn runtime() -> tokio::runtime::Runtime {
    // SAFETY: set before any other thread reads the environment.
    unsafe { std::env::set_var("PELER_SECRETS", "memory") };
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

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
    let rx = conn.open_sftp();
    let sftp = rt.block_on(rx).unwrap().unwrap();
    (conn, Arc::new(sftp))
}

fn random_file(path: &Path, size: usize) {
    let mut f = std::fs::File::create(path).unwrap();
    let mut state: u64 = 0x9e3779b97f4a7c15 ^ size as u64;
    let mut buf = vec![0u8; 1 << 20];
    let mut left = size;
    while left > 0 {
        for chunk in buf.chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let bytes = state.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        let n = left.min(buf.len());
        f.write_all(&buf[..n]).unwrap();
        left -= n;
    }
}

fn same_contents(a: &Path, b: &Path) -> bool {
    let (mut fa, mut fb) = (
        std::fs::File::open(a).unwrap(),
        std::fs::File::open(b).unwrap(),
    );
    if fa.metadata().unwrap().len() != fb.metadata().unwrap().len() {
        return false;
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = fa.read(&mut ba).unwrap();
        if n == 0 {
            return true;
        }
        fb.read_exact(&mut bb[..n]).unwrap();
        if ba[..n] != bb[..n] {
            return false;
        }
    }
}

fn wait_transfer(t: &peler::sftp::transfer::Transfer, secs: u64) -> TransferState {
    wait_for(secs, "transfer", || t.is_finished().then(|| t.state()))
}

#[test]
fn file_operations_and_transfers() {
    let rt = runtime();
    let server = TestServer::start();
    let (conn, sftp) = connect(&rt, &server);
    let base = server.dir.path().join("remote");
    std::fs::create_dir(&base).unwrap();
    let base = base.to_str().unwrap().to_string();
    let local = server.dir.path().join("local");
    std::fs::create_dir(&local).unwrap();

    rt.block_on(async {
        let home = remote::home(&sftp).await.unwrap();
        assert!(home.starts_with('/'));
        remote::mkdir(&sftp, &remote_join(&base, "dir"))
            .await
            .unwrap();
        assert!(
            remote::mkdir(&sftp, &remote_join(&base, "dir"))
                .await
                .is_err()
        );
        {
            use tokio::io::AsyncWriteExt;
            let mut f = sftp.create(remote_join(&base, "a.txt")).await.unwrap();
            f.write_all(b"hello").await.unwrap();
            f.close().await.unwrap();
        }
        remote::rename(
            &sftp,
            &remote_join(&base, "a.txt"),
            &remote_join(&base, "b.txt"),
        )
        .await
        .unwrap();
        remote::chmod(&sftp, &remote_join(&base, "b.txt"), 0o640)
            .await
            .unwrap();
        let entries = remote::list(&sftp, &base).await.unwrap();
        let b = entries.iter().find(|e| e.name == "b.txt").unwrap();
        assert_eq!(b.mode, Some(0o640));
        assert_eq!(b.size, 5);
        assert!(entries.iter().any(|e| e.name == "dir" && e.is_dir));
        let err = remote::list(&sftp, "/definitely/not/here")
            .await
            .unwrap_err();
        assert!(err.contains("no such file"), "{err}");
    });

    let mut transfers = Transfers::default();

    // Single file round trip (40 MB).
    let src = local.join("big.bin");
    random_file(&src, 40 * 1024 * 1024);
    let started = Instant::now();
    let up = transfers.upload(rt.handle(), sftp.clone(), src.clone(), &base, || {});
    assert_eq!(wait_transfer(&up, 120), TransferState::Done);
    let up_secs = started.elapsed().as_secs_f64();
    let back = local.join("back");
    std::fs::create_dir(&back).unwrap();
    let started = Instant::now();
    let down = transfers.download(
        rt.handle(),
        sftp.clone(),
        remote_join(&base, "big.bin"),
        &back,
        || {},
    );
    assert_eq!(wait_transfer(&down, 120), TransferState::Done);
    let down_secs = started.elapsed().as_secs_f64();
    assert!(
        same_contents(&src, &back.join("big.bin")),
        "round trip must be byte-identical"
    );
    eprintln!(
        "40 MB: upload {:.0} MB/s, download {:.0} MB/s",
        40.0 / up_secs,
        40.0 / down_secs
    );
    assert_eq!(up.fraction(), 1.0);

    // Folder round trip with nesting, an empty folder and an executable bit.
    let tree = local.join("tree");
    std::fs::create_dir_all(tree.join("sub/deeper")).unwrap();
    std::fs::create_dir_all(tree.join("empty")).unwrap();
    std::fs::write(tree.join("one.txt"), "1").unwrap();
    std::fs::write(tree.join("sub/two.txt"), "22").unwrap();
    random_file(&tree.join("sub/deeper/three.bin"), 300_000);
    std::fs::write(tree.join("run.sh"), "#!/bin/sh\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tree.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let up = transfers.upload(rt.handle(), sftp.clone(), tree.clone(), &base, || {});
    assert_eq!(wait_transfer(&up, 60), TransferState::Done);
    assert_eq!(up.files_done.load(std::sync::atomic::Ordering::Relaxed), 4);
    let back2 = local.join("back2");
    std::fs::create_dir(&back2).unwrap();
    let down = transfers.download(
        rt.handle(),
        sftp.clone(),
        remote_join(&base, "tree"),
        &back2,
        || {},
    );
    assert_eq!(wait_transfer(&down, 60), TransferState::Done);
    for rel in ["one.txt", "sub/two.txt", "sub/deeper/three.bin", "run.sh"] {
        assert!(
            same_contents(&tree.join(rel), &back2.join("tree").join(rel)),
            "{rel}"
        );
    }
    assert!(back2.join("tree/empty").is_dir());
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(back2.join("tree/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "executable bit preserved");
    }

    // Cancelling removes the partial file.
    let huge = local.join("huge.bin");
    random_file(&huge, 200 * 1024 * 1024);
    let up = transfers.upload(rt.handle(), sftp.clone(), huge, &base, || {});
    wait_for(30, "transfer progress", || {
        (up.done.load(std::sync::atomic::Ordering::Relaxed) > 0).then_some(())
    });
    up.cancel();
    assert_eq!(wait_transfer(&up, 30), TransferState::Cancelled);
    assert!(!rt.block_on(remote::exists(&sftp, &remote_join(&base, "huge.bin"))));

    // Recursive delete.
    rt.block_on(async {
        remote::remove(&sftp, &remote_join(&base, "tree"))
            .await
            .unwrap();
        assert!(!remote::exists(&sftp, &remote_join(&base, "tree")).await);
    });

    // Errors are reported, not hidden.
    let missing = transfers.download(
        rt.handle(),
        sftp.clone(),
        remote_join(&base, "nope.bin"),
        &back,
        || {},
    );
    match wait_transfer(&missing, 30) {
        TransferState::Failed(msg) => assert!(msg.contains("no such file"), "{msg}"),
        other => panic!("{other:?}"),
    }
    drop(conn);
    let _ = ConnState::Connecting;
}

/// The release criterion: a 1 GB file round trip without corruption.
/// Run with `cargo test --release --test sftp_integration -- --ignored`.
#[test]
#[ignore]
fn one_gigabyte_round_trip() {
    let rt = runtime();
    let server = TestServer::start();
    let (_conn, sftp) = connect(&rt, &server);
    let dir = server.dir.path();
    let remote_dir = dir.join("r");
    std::fs::create_dir(&remote_dir).unwrap();
    let src = dir.join("gig.bin");
    random_file(&src, 1024 * 1024 * 1024);
    let mut transfers = Transfers::default();
    let t0 = Instant::now();
    let up = transfers.upload(
        rt.handle(),
        sftp.clone(),
        src.clone(),
        remote_dir.to_str().unwrap(),
        || {},
    );
    assert_eq!(wait_transfer(&up, 1800), TransferState::Done);
    let up_s = t0.elapsed().as_secs_f64();
    let back = dir.join("back");
    std::fs::create_dir(&back).unwrap();
    let t0 = Instant::now();
    let down = transfers.download(
        rt.handle(),
        sftp,
        remote_join(remote_dir.to_str().unwrap(), "gig.bin"),
        &back,
        || {},
    );
    assert_eq!(wait_transfer(&down, 1800), TransferState::Done);
    let down_s = t0.elapsed().as_secs_f64();
    let sha = |p: &Path| {
        let out = std::process::Command::new("shasum")
            .args(["-a", "256"])
            .arg(p)
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    };
    let (a, b, c) = (
        sha(&src),
        sha(&remote_dir.join("gig.bin")),
        sha(&back.join("gig.bin")),
    );
    eprintln!(
        "1 GiB: upload {:.0} MB/s, download {:.0} MB/s; sha256 {a}",
        1024.0 / up_s,
        1024.0 / down_s
    );
    assert_eq!(a, b, "uploaded copy differs");
    assert_eq!(a, c, "downloaded copy differs");
}
