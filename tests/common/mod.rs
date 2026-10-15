//! Test helpers: an unprivileged OpenSSH server on a random local port.

#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct TestServer {
    pub port: u16,
    pub dir: tempfile::TempDir,
    pub key: PathBuf,
    pub encrypted_key: PathBuf,
    pub rsa_key: PathBuf,
    pub ecdsa_key: PathBuf,
    child: Child,
}

pub fn keygen(path: &Path, kind: &str, pass: &str) {
    let mut cmd = Command::new("ssh-keygen");
    cmd.args(["-q", "-t", kind, "-N", pass, "-C", "peler-test", "-f"])
        .arg(path);
    if kind == "rsa" {
        cmd.args(["-b", "3072"]);
    }
    assert!(cmd.status().unwrap().success(), "ssh-keygen failed");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl TestServer {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        keygen(&d.join("host_ed25519"), "ed25519", "");
        let key = d.join("client_ed25519");
        keygen(&key, "ed25519", "");
        let encrypted_key = d.join("client_encrypted");
        keygen(&encrypted_key, "ed25519", "open sesame");
        let rsa_key = d.join("client_rsa");
        keygen(&rsa_key, "rsa", "");
        let ecdsa_key = d.join("client_ecdsa");
        keygen(&ecdsa_key, "ecdsa", "");
        let mut authorized = String::new();
        for k in [&key, &encrypted_key, &rsa_key, &ecdsa_key] {
            authorized += &std::fs::read_to_string(k.with_extension("pub")).unwrap();
        }
        std::fs::write(d.join("authorized_keys"), authorized).unwrap();

        let port = free_port();
        let config = format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {host}\nAuthorizedKeysFile {auth}\nPidFile {pid}\n\
             UsePAM no\nStrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             Subsystem sftp /usr/libexec/sftp-server\nLogLevel ERROR\nAcceptEnv LANG LC_*\n",
            host = d.join("host_ed25519").display(),
            auth = d.join("authorized_keys").display(),
            pid = d.join("sshd.pid").display(),
        );
        let cfg_path = d.join("sshd_config");
        std::fs::write(&cfg_path, config).unwrap();
        let child = Command::new("/usr/sbin/sshd")
            .args(["-D", "-e", "-f"])
            .arg(&cfg_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start sshd");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "sshd did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            port,
            dir,
            key,
            encrypted_key,
            rsa_key,
            ecdsa_key,
            child,
        }
    }

    pub fn host_public_key(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("host_ed25519.pub")).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn user() -> String {
    std::env::var("USER").unwrap()
}

/// Polls `f` until it returns `Some`, or panics after `secs` seconds.
pub fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
