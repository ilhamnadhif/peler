//! Jump hosts, port forwarding and SSH agent logins against a real OpenSSH server.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{TestServer, user, wait_for};
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark, Forward, ForwardKind};
use peler::ssh::session::{ConnectSpec, Connection};
use peler::ssh::types::Prompt;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn bookmark(server: &TestServer, name: &str) -> Bookmark {
    Bookmark {
        name: name.into(),
        host: "127.0.0.1".into(),
        port: server.port,
        username: user(),
        auth: Auth::Key {
            path: server.key.display().to_string(),
        },
        ..Default::default()
    }
}

/// Connects, trusting the host key (asked once: every hop is the same test server).
fn connect(
    rt: &tokio::runtime::Runtime,
    server: &TestServer,
    target: Bookmark,
    jumps: Vec<Bookmark>,
) -> Connection {
    let spec = ConnectSpec {
        bookmark: target,
        jumps,
        keepalive_secs: 30,
        known_hosts: server.dir.path().join("kh"),
        save_secrets: false,
    };
    let conn = Connection::start(rt.handle(), egui::Context::default(), spec);
    wait_for(20, "connected", || {
        if let Some(Prompt::HostKey { reply, .. }) = conn.shared.lock().prompt.take() {
            let _ = reply.send(true);
        }
        let state = conn.state();
        assert!(!state.is_closed(), "connection failed: {state:?}");
        state.is_connected().then_some(())
    });
    conn
}

/// A TCP server on this Mac that answers every connection with what it receives.
fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 1024];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 || stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn echo_through(mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(b"ping through ssh").unwrap();
    let mut back = [0u8; 16];
    stream.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"ping through ssh");
}

fn wait_listening(conn: &Connection, index: usize) {
    wait_for(10, "forward listening", || {
        let s = conn.shared.lock();
        let f = s.forwards.get(index)?;
        assert!(
            f.listening || f.error.is_none(),
            "forward failed: {:?}",
            f.error
        );
        f.listening.then_some(())
    });
}

#[test]
fn through_a_jump_host() {
    let rt = runtime();
    let server = TestServer::start();
    let jump = bookmark(&server, "bastion");
    // Two jumps in a row, then the server: three SSH connections inside each other.
    let conn = connect(
        &rt,
        &server,
        bookmark(&server, "target"),
        vec![jump.clone(), jump],
    );
    let sftp = rt.block_on(conn.open_sftp()).unwrap().unwrap();
    let home = rt.block_on(sftp.canonicalize(".")).unwrap();
    assert!(home.starts_with('/'));
}

#[test]
fn local_remote_and_socks_forwards() {
    let rt = runtime();
    let server = TestServer::start();
    let echo = echo_server();
    let (local, socks, remote, taken) = (
        free_port(),
        free_port(),
        free_port(),
        TcpListener::bind("127.0.0.1:0").unwrap(),
    );
    let taken_port = taken.local_addr().unwrap().port();
    let mut target = bookmark(&server, "forwards");
    target.forwards = vec![
        Forward {
            kind: ForwardKind::Local,
            listen_port: local,
            host: "127.0.0.1".into(),
            port: echo,
        },
        Forward {
            kind: ForwardKind::Dynamic,
            listen_port: socks,
            host: String::new(),
            port: 0,
        },
        Forward {
            kind: ForwardKind::Remote,
            listen_port: remote,
            host: "127.0.0.1".into(),
            port: echo,
        },
        Forward {
            kind: ForwardKind::Local,
            listen_port: taken_port,
            host: "127.0.0.1".into(),
            port: echo,
        },
    ];
    let conn = connect(&rt, &server, target, Vec::new());

    // -L: a port here reaches the echo server from the server's side.
    wait_listening(&conn, 0);
    echo_through(TcpStream::connect(("127.0.0.1", local)).unwrap());

    // -D: a SOCKS5 CONNECT to the echo server.
    wait_listening(&conn, 1);
    let mut s = TcpStream::connect(("127.0.0.1", socks)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&[5, 1, 0]).unwrap();
    let mut answer = [0u8; 2];
    s.read_exact(&mut answer).unwrap();
    assert_eq!(answer, [5, 0]);
    let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
    request.extend_from_slice(&echo.to_be_bytes());
    s.write_all(&request).unwrap();
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).unwrap();
    assert_eq!(reply[1], 0, "SOCKS reply {reply:?}");
    echo_through(s);

    // -R: the server listens; its connections come back to the echo server here.
    wait_listening(&conn, 2);
    echo_through(TcpStream::connect(("127.0.0.1", remote)).unwrap());

    // A port already in use is reported, and the others keep working.
    let error = wait_for(10, "error", || conn.shared.lock().forwards[3].error.clone());
    assert!(error.contains(&taken_port.to_string()), "{error}");
    assert!(!conn.shared.lock().forwards[3].listening);
    drop(taken);

    // The forwards close with the connection.
    conn.disconnect();
    wait_for(10, "closed", || conn.state().is_closed().then_some(()));
    wait_for(10, "local port released", || {
        TcpStream::connect(("127.0.0.1", local))
            .is_err()
            .then_some(())
    });
}

struct Agent {
    child: Child,
    socket: std::path::PathBuf,
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_agent(dir: &std::path::Path, key: &std::path::Path) -> Agent {
    let socket = dir.join("agent.sock");
    let child = Command::new("ssh-agent")
        .args(["-D", "-a"])
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ssh-agent");
    let agent = Agent { child, socket };
    wait_for(10, "agent socket", || agent.socket.exists().then_some(()));
    let ok = Command::new("ssh-add")
        .arg(key)
        .env("SSH_AUTH_SOCK", &agent.socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "ssh-add failed");
    agent
}

#[test]
fn logs_in_with_an_agent() {
    let rt = runtime();
    let server = TestServer::start();
    let agent = start_agent(server.dir.path(), &server.key);
    let mut target = bookmark(&server, "agent");
    target.auth = Auth::Agent {
        socket: agent.socket.display().to_string(),
    };
    let conn = connect(&rt, &server, target, Vec::new());
    assert!(conn.state().is_connected());

    // An RSA key in the agent, signed with SHA-2 by the agent.
    let rsa_agent_dir = tempfile::tempdir().unwrap();
    let rsa_agent = start_agent(rsa_agent_dir.path(), &server.rsa_key);
    let mut target = bookmark(&server, "agent-rsa");
    target.auth = Auth::Agent {
        socket: rsa_agent.socket.display().to_string(),
    };
    let conn = connect(&rt, &server, target, Vec::new());
    assert!(conn.state().is_connected());
}
