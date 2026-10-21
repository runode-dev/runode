//! 跨网络的连接（见 `runode_protocol::remote` 的「跨网络」）：iroh 端点按这台 Mac 的 Ed25519 公钥
//! 寻址，手机连上来开一条双向流，这边原样转给本机回环地址上的 TLS 监听。流上跑的还是 TLS 1.3 和
//! 门禁，证书指纹、设备签名、限速和撤销都和局域网一样，这里不认人。
//!
//! iroh 是 async 的，自己一个线程跑一个单线程的 tokio 运行时，丢掉 `Tunnel` 时关掉端点、线程结束。
//!
//! shortcut: 经这里来的连接在监听方看来都来自 127.0.0.1，按来源的限速和名额由它们共用；有人拿着公钥
//! 硬试时再按 iroh 的对端公钥分开记

use std::{
    io,
    net::Ipv4Addr,
    sync::mpsc,
    thread::{self, JoinHandle},
};

use iroh::{
    Endpoint, SecretKey,
    endpoint::{Incoming, presets},
};
use runode_paths::Dirs;
use runode_protocol::remote::TUNNEL_ALPN;
use tokio::{io::AsyncWriteExt as _, net::TcpStream, sync::oneshot};

use crate::files::{no_home, random, read_optional, write_private};

/// 开着的 iroh 端点。
pub(crate) struct Tunnel {
    node: String,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Tunnel {
    /// 开端点，连上来的转给本机 `port` 上的监听。私钥第一次用时生成，存在 `remote_access_tunnel_key_file`；
    /// 调用方要拿着 `remote_access_lock_file` 的锁。
    pub(crate) fn start(dirs: &Dirs, port: u16) -> io::Result<Self> {
        let key = secret_key(dirs)?;
        let (ready, started) = mpsc::channel();
        let (stop, stopped) = oneshot::channel();
        let thread = thread::Builder::new().name("remote-tunnel".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => return drop(ready.send(Err(err))),
            };
            runtime.block_on(async move {
                let bound =
                    Endpoint::builder(presets::N0).secret_key(key).alpns(vec![TUNNEL_ALPN.to_vec()]).bind().await;
                let endpoint = match bound {
                    Ok(endpoint) => endpoint,
                    Err(err) => return drop(ready.send(Err(io::Error::other(err)))),
                };
                let _ = ready.send(Ok(endpoint.id().to_string()));
                tokio::select! {
                    () = accept(&endpoint, port) => {}
                    _ = stopped => {}
                }
                endpoint.close().await;
            });
        })?;
        let node = started.recv().map_err(|_| io::Error::other("the tunnel thread ended early"))??;
        tracing::info!("remote access is reachable across networks as {node}");
        Ok(Self { node, stop: Some(stop), thread: Some(thread) })
    }

    /// 手机拨号用的地址，见 `runode_protocol::remote::PairingUri::node`。
    pub(crate) fn node(&self) -> &str {
        &self.node
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn accept(endpoint: &Endpoint, port: u16) {
    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            if let Err(err) = forward(incoming, port).await {
                tracing::debug!("a tunnelled remote connection ended: {err}");
            }
        });
    }
}

/// 一条 iroh 连接上的第一条双向流和到本机监听的 TCP 连接之间两个方向搬字节，两边都说完为止。
async fn forward(incoming: Incoming, port: u16) -> io::Result<()> {
    let connection = incoming.await.map_err(io::Error::other)?;
    let (mut send, mut recv) = connection.accept_bi().await.map_err(io::Error::other)?;
    let mut tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    tcp.set_nodelay(true)?;
    let (mut from_listener, mut to_listener) = tcp.split();
    let up = async {
        let copied = tokio::io::copy(&mut recv, &mut to_listener).await;
        let _ = to_listener.shutdown().await;
        copied
    };
    let down = async {
        let copied = tokio::io::copy(&mut from_listener, &mut send).await;
        let _ = send.finish();
        copied
    };
    let (up, down) = tokio::join!(up, down);
    // 等手机读完最后的字节再关连接，不然它收不到 close_notify 和拒绝的回话。
    let _ = send.stopped().await;
    up.and(down).map(drop)
}

/// 读存着的私钥，还没有时生成一把存下。读到的长度不对时报错，不悄悄换掉：换了手机就拨不到了。
fn secret_key(dirs: &Dirs) -> io::Result<SecretKey> {
    let path = dirs.remote_access_tunnel_key_file().ok_or_else(no_home)?;
    let bytes: [u8; 32] = match read_optional(&path)? {
        Some(bytes) => bytes
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{} is not a key", path.display())))?,
        None => {
            let bytes = random::<32>()?;
            write_private(&path, &bytes)?;
            bytes
        }
    };
    Ok(SecretKey::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use iroh::EndpointAddr;
    use tokio::{io::AsyncReadExt as _, net::TcpListener};

    use super::*;

    /// 只走本机：不连 n0 的中继和 DNS，测试不碰外网。
    async fn local_endpoint(alpns: Vec<Vec<u8>>) -> Endpoint {
        Endpoint::builder(presets::Minimal).alpns(alpns).bind().await.unwrap()
    }

    /// 锁定转发两个方向原样搬字节：手机说完以后监听那边读到结尾，监听回的话手机读得完。
    #[tokio::test]
    async fn forwards_both_ways_until_both_sides_are_done() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = local_endpoint(vec![TUNNEL_ALPN.to_vec()]).await;
        let addr = server
            .bound_sockets()
            .into_iter()
            .filter(SocketAddr::is_ipv4)
            .fold(EndpointAddr::new(server.id()), |addr, socket| {
                addr.with_ip_addr((Ipv4Addr::LOCALHOST, socket.port()).into())
            });
        let accepting = tokio::spawn(async move {
            accept(&server, port).await;
        });
        let echo = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut got = Vec::new();
            tcp.read_to_end(&mut got).await.unwrap();
            tcp.write_all(&[got.as_slice(), b" back"].concat()).await.unwrap();
        });

        let phone = local_endpoint(Vec::new()).await;
        let connection = phone.connect(addr, TUNNEL_ALPN).await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"hello").await.unwrap();
        send.finish().unwrap();
        let reply = recv.read_to_end(1024).await.unwrap();
        assert_eq!(reply, b"hello back");
        echo.await.unwrap();
        accepting.abort();
    }
}
