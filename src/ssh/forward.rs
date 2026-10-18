//! Port forwarding while a connection is open: local (`ssh -L`), remote (`ssh -R`) and dynamic
//! (`ssh -D`, a SOCKS5 proxy). Ports opened on this Mac only take connections from this Mac;
//! ports opened on the server listen on its loopback interface, as OpenSSH does by default.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::Context;
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use super::session::SshHandle;
use super::types::{ConnShared, ForwardStatus};
use crate::bookmarks::{Forward, ForwardKind};
use crate::i18n::tf;

/// Remote forwards: a port on the server → where its connections go from this Mac.
pub type RemoteForwards = Arc<Mutex<HashMap<u32, (String, u16)>>>;

/// Starts the connection's forwards; they stop when `cancel` fires (the connection ends).
pub fn start(
    handle: Arc<SshHandle>,
    forwards: &[Forward],
    remote: &RemoteForwards,
    cancel: &CancellationToken,
    shared: &Arc<Mutex<ConnShared>>,
    ctx: &Context,
) {
    shared.lock().forwards = forwards
        .iter()
        .map(|f| ForwardStatus {
            forward: f.clone(),
            listening: false,
            error: None,
        })
        .collect();
    for (index, f) in forwards.iter().cloned().enumerate() {
        let status = Status {
            shared: shared.clone(),
            ctx: ctx.clone(),
            index,
        };
        match f.kind {
            ForwardKind::Local | ForwardKind::Dynamic => {
                tokio::spawn(listen_here(handle.clone(), f, status, cancel.clone()));
            }
            ForwardKind::Remote => {
                tokio::spawn(listen_on_server(handle.clone(), f, status, remote.clone()));
            }
        }
    }
}

/// Where a forward reports how it is doing (shown in the tab).
#[derive(Clone)]
struct Status {
    shared: Arc<Mutex<ConnShared>>,
    ctx: Context,
    index: usize,
}

impl Status {
    fn set(&self, listening: bool, error: Option<String>) {
        if let Some(s) = self.shared.lock().forwards.get_mut(self.index) {
            s.listening = listening;
            s.error = error;
        }
        self.ctx.request_repaint();
    }
}

fn bind_error(port: u16, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::AddrInUse {
        tf(
            "port {} is already in use on this Mac",
            &[&port.to_string()],
        )
    } else {
        tf(
            "port {} could not be opened ({})",
            &[&port.to_string(), &e.to_string()],
        )
    }
}

/// A local or dynamic forward: listens on this Mac.
async fn listen_here(
    handle: Arc<SshHandle>,
    f: Forward,
    status: Status,
    cancel: CancellationToken,
) {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, f.listen_port)).await {
        Ok(l) => l,
        Err(e) => {
            status.set(false, Some(bind_error(f.listen_port, &e)));
            return;
        }
    };
    status.set(true, None);
    loop {
        let (tcp, peer) = tokio::select! {
            _ = cancel.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok(a) => a,
                // e.g. out of file descriptors: don't spin.
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            },
        };
        let (handle, f, status, cancel) =
            (handle.clone(), f.clone(), status.clone(), cancel.clone());
        tokio::spawn(async move {
            let result = match f.kind {
                ForwardKind::Dynamic => socks(&handle, tcp, peer, &cancel).await,
                _ => tunnel(&handle, tcp, peer, &f.host, f.port, &cancel).await,
            };
            // Still listening; the message says why the last connection failed.
            status.set(true, result.err());
        });
    }
}

/// Opens a channel from the server to `host:port`.
async fn open(
    handle: &SshHandle,
    host: &str,
    port: u16,
    peer: SocketAddr,
) -> Result<russh::Channel<russh::client::Msg>, String> {
    handle
        .channel_open_direct_tcpip(host, port as u32, peer.ip().to_string(), peer.port() as u32)
        .await
        .map_err(|e| {
            tf(
                "the server could not connect to {}:{} ({})",
                &[host, &port.to_string(), &e.to_string()],
            )
        })
}

async fn tunnel(
    handle: &SshHandle,
    tcp: TcpStream,
    peer: SocketAddr,
    host: &str,
    port: u16,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let channel = open(handle, host, port, peer).await?;
    pump(tcp, channel.into_stream(), cancel).await;
    Ok(())
}

/// Copies both ways until either side closes or the connection ends.
pub async fn pump<A, B>(mut a: A, mut b: B, cancel: &CancellationToken)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
    }
}

/// A SOCKS5 request (no authentication, CONNECT) that the server carries out.
async fn socks(
    handle: &SshHandle,
    mut tcp: TcpStream,
    peer: SocketAddr,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let Ok((host, port)) = socks_request(&mut tcp).await else {
        // Not a SOCKS5 client, or one wanting something else: nothing to report.
        return Ok(());
    };
    match open(handle, &host, port, peer).await {
        Ok(channel) => {
            if socks_reply(&mut tcp, 0).await.is_ok() {
                pump(tcp, channel.into_stream(), cancel).await;
            }
            Ok(())
        }
        Err(e) => {
            // "Host unreachable".
            let _ = socks_reply(&mut tcp, 4).await;
            Err(e)
        }
    }
}

/// Reads the client's greeting and CONNECT request: the address it wants.
async fn socks_request<S: AsyncRead + AsyncWrite + Unpin>(
    tcp: &mut S,
) -> std::io::Result<(String, u16)> {
    let bad = || std::io::Error::from(std::io::ErrorKind::InvalidData);
    let mut head = [0u8; 2];
    tcp.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(bad());
    }
    let mut methods = vec![0u8; head[1] as usize];
    tcp.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        tcp.write_all(&[5, 0xff]).await?;
        return Err(bad());
    }
    tcp.write_all(&[5, 0]).await?;
    let mut request = [0u8; 4];
    tcp.read_exact(&mut request).await?;
    if request[0] != 5 {
        return Err(bad());
    }
    if request[1] != 1 {
        // Only CONNECT: "command not supported".
        socks_reply(tcp, 7).await?;
        return Err(bad());
    }
    let host = match request[3] {
        1 => {
            let mut a = [0u8; 4];
            tcp.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let len = tcp.read_u8().await? as usize;
            let mut name = vec![0u8; len];
            tcp.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| bad())?
        }
        4 => {
            let mut a = [0u8; 16];
            tcp.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => {
            // "Address type not supported".
            socks_reply(tcp, 8).await?;
            return Err(bad());
        }
    };
    let port = tcp.read_u16().await?;
    Ok((host, port))
}

async fn socks_reply<S: AsyncWrite + Unpin>(tcp: &mut S, code: u8) -> std::io::Result<()> {
    tcp.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

/// A remote forward: the server listens and sends its connections here.
async fn listen_on_server(
    handle: Arc<SshHandle>,
    f: Forward,
    status: Status,
    remote: RemoteForwards,
) {
    remote
        .lock()
        .insert(f.listen_port as u32, (f.host.clone(), f.port));
    match handle
        .tcpip_forward("localhost", f.listen_port as u32)
        .await
    {
        Ok(_) => status.set(true, None),
        Err(e) => {
            remote.lock().remove(&(f.listen_port as u32));
            status.set(
                false,
                Some(tf(
                    "the server would not listen on port {} ({})",
                    &[&f.listen_port.to_string(), &e.to_string()],
                )),
            );
        }
    }
}

/// A connection the server received on a remote forward: connect to where it goes, then
/// accept the channel (or refuse it).
pub fn serve_forwarded(
    remote: &RemoteForwards,
    port: u32,
    channel: russh::Channel<russh::client::Msg>,
    reply: russh::client::ChannelOpenHandle,
    cancel: CancellationToken,
) {
    let Some((host, target_port)) = remote.lock().get(&port).cloned() else {
        tokio::spawn(reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited));
        return;
    };
    tokio::spawn(async move {
        match TcpStream::connect((host.as_str(), target_port)).await {
            Ok(tcp) => {
                reply.accept().await;
                pump(tcp, channel.into_stream(), &cancel).await;
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_socks5_requests() {
        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move { socks_request(&mut server).await.unwrap() });
        // Greeting (no auth), then CONNECT example.com:443.
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut answer = [0u8; 2];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer, [5, 0]);
        let mut request = vec![5, 1, 0, 3, 11];
        request.extend_from_slice(b"example.com");
        request.extend_from_slice(&443u16.to_be_bytes());
        client.write_all(&request).await.unwrap();
        assert_eq!(task.await.unwrap(), ("example.com".to_string(), 443));

        // An IPv4 address.
        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move { socks_request(&mut server).await.unwrap() });
        client.write_all(&[5, 1, 0]).await.unwrap();
        client.read_exact(&mut answer).await.unwrap();
        client
            .write_all(&[5, 1, 0, 1, 10, 0, 0, 7, 0x1f, 0x90])
            .await
            .unwrap();
        assert_eq!(task.await.unwrap(), ("10.0.0.7".to_string(), 8080));
    }
}
