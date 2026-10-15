//! Password and keyboard-interactive login against an in-process russh server (an unprivileged
//! OpenSSH server cannot check passwords, so these flows are tested here).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::wait_for;
use eframe::egui;
use peler::bookmarks::{Auth, Bookmark};
use peler::secrets::{self, SecretKind};
use peler::ssh::session::{ConnectSpec, Connection};
use peler::ssh::types::{ConnError, ConnState, Prompt, SecretAnswer, SecretPromptKind};
use russh::server::{Auth as ServerAuth, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};

#[derive(Clone)]
struct TestServer {
    /// When false only keyboard-interactive is accepted (russh leaves enforcing this to us).
    password_allowed: bool,
}

impl russh::server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(
        &mut self,
        _user: &str,
        password: &str,
    ) -> Result<ServerAuth, Self::Error> {
        // Like OpenSSH: a failed attempt lists the methods that may still succeed.
        if !self.password_allowed {
            return Ok(ServerAuth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::KeyboardInteractive][..])),
                partial_success: false,
            });
        }
        Ok(if password == "secret" {
            ServerAuth::Accept
        } else {
            ServerAuth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                partial_success: false,
            }
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> Result<ServerAuth, Self::Error> {
        Ok(match response {
            None => ServerAuth::Partial {
                name: "".into(),
                instructions: "".into(),
                prompts: vec![
                    ("Password: ".into(), false),
                    ("Verification code: ".into(), true),
                ]
                .into(),
            },
            Some(r) => {
                let answers: Vec<String> = r
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .collect();
                if answers == ["secret", "123456"] {
                    ServerAuth::Accept
                } else {
                    ServerAuth::Reject {
                        proceed_with_methods: None,
                        partial_success: false,
                    }
                }
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, b"welcome-from-test-server\r\n".to_vec())?;
        Ok(())
    }
}

/// Starts a server offering `methods`; returns its port.
fn start_server(
    rt: &tokio::runtime::Runtime,
    dir: &std::path::Path,
    methods: &[MethodKind],
) -> u16 {
    let key_path = dir.join(format!("host_{}", uuid::Uuid::new_v4()));
    common::keygen(&key_path, "ed25519", "");
    let key =
        russh::keys::decode_secret_key(&std::fs::read_to_string(&key_path).unwrap(), None).unwrap();
    let config = Arc::new(russh::server::Config {
        methods: MethodSet::from(methods),
        keys: vec![key],
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let password_allowed = methods.contains(&MethodKind::Password);
    rt.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            tokio::spawn(async move {
                let handler = TestServer { password_allowed };
                if let Ok(session) = russh::server::run_stream(config, stream, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    port
}

fn spec(port: u16, id: uuid::Uuid, known: &std::path::Path) -> ConnectSpec {
    ConnectSpec {
        bookmark: Bookmark {
            id,
            host: "127.0.0.1".into(),
            port,
            username: "tester".into(),
            auth: Auth::Password,
            ..Default::default()
        },
        keepalive_secs: 30,
        known_hosts: known.to_path_buf(),
        save_secrets: true,
    }
}

/// Answers the next prompt; returns (kind, retry, echo) for secrets, or None for host keys.
fn answer(
    conn: &Connection,
    value: Option<&str>,
    save: bool,
) -> Option<(SecretPromptKind, bool, bool)> {
    wait_for(15, "a prompt", || {
        let mut s = conn.shared.lock();
        if let ConnState::Closed(e) = &s.state {
            panic!("connection closed while waiting for a prompt: {e:?}");
        }
        match s.prompt.take() {
            Some(Prompt::HostKey { reply, .. }) => {
                let _ = reply.send(true);
                Some(None)
            }
            Some(Prompt::Secret {
                kind,
                retry,
                echo,
                reply,
                ..
            }) => {
                let _ = reply.send(value.map(|v| SecretAnswer {
                    value: v.into(),
                    save,
                }));
                Some(Some((kind, retry, echo)))
            }
            None => None,
        }
    })
}

#[test]
fn password_and_keyboard_interactive_logins() {
    // SAFETY: set before other threads read the environment.
    unsafe { std::env::set_var("PELER_SECRETS", "memory") };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("known_hosts");
    let ctx = egui::Context::default();

    // --- password only: wrong, then right (saved), then no prompt next time -------------
    let port = start_server(&rt, dir.path(), &[MethodKind::Password]);
    let id = uuid::Uuid::new_v4();
    let conn = Connection::start(rt.handle(), ctx.clone(), spec(port, id, &known));
    assert_eq!(answer(&conn, None, false), None, "host key first");
    assert_eq!(
        answer(&conn, Some("wrong"), false),
        Some((SecretPromptKind::Password, false, false))
    );
    assert_eq!(
        answer(&conn, Some("secret"), true),
        Some((SecretPromptKind::Password, true, false))
    );
    wait_for(15, "connected", || {
        let st = conn.state();
        assert!(!st.is_closed(), "{st:?}");
        st.is_connected().then_some(())
    });
    assert_eq!(
        secrets::get(id, SecretKind::Password).unwrap().as_deref(),
        Some("secret")
    );
    drop(conn);

    let conn = Connection::start(rt.handle(), ctx.clone(), spec(port, id, &known));
    wait_for(15, "connected with saved password", || {
        conn.state().is_connected().then_some(())
    });
    assert!(conn.shared.lock().prompt.is_none());
    drop(conn);

    // Three wrong answers end with a clear error.
    let id2 = uuid::Uuid::new_v4();
    let conn = Connection::start(rt.handle(), ctx.clone(), spec(port, id2, &known));
    for _ in 0..3 {
        answer(&conn, Some("nope"), false);
    }
    let st = wait_for(15, "auth failure", || {
        conn.state().is_closed().then(|| conn.state())
    });
    assert!(
        matches!(st, ConnState::Closed(Some(ConnError::AuthFailed(_)))),
        "{st:?}"
    );

    // --- keyboard-interactive (PAM style): password filled in, one-time code asked -----
    let port = start_server(&rt, dir.path(), &[MethodKind::KeyboardInteractive]);
    let conn = Connection::start(
        rt.handle(),
        ctx.clone(),
        spec(port, uuid::Uuid::new_v4(), &known),
    );
    assert_eq!(answer(&conn, None, false), None, "host key first");
    assert_eq!(
        answer(&conn, Some("secret"), false),
        Some((SecretPromptKind::Password, false, false))
    );
    assert_eq!(
        answer(&conn, Some("123456"), false),
        Some((SecretPromptKind::Interactive, false, true))
    );
    wait_for(15, "connected via keyboard-interactive", || {
        conn.state().is_connected().then_some(())
    });
}
