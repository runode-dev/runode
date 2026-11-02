//! 经 iroh 按 `node`（对方端点的公钥）跨网络连别的电脑，ALPN 是 `TUNNEL_ALPN`。整个进程共用一个 iroh 端点，
//! 第一次要用时建，跑在自己一个线程上的单线程 tokio 运行时里，建好以后一直留着。
//!
//! iroh 的流是 async 的，TLS 和门禁是同步的：每条连接在回环地址上接一对 TCP，一头交给调用方当普通的
//! TCP 连接用（握手、门禁、搬字节都和局域网一样），另一头由运行时里的任务和 iroh 的双向流互相搬。

use std::{
    io,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use runode_protocol::remote::TUNNEL_ALPN;
use tokio::{io::AsyncWriteExt as _, runtime::Handle, time::timeout};

use super::{Cancel, DialError};

/// 两边都说完以后，最多等这么久让对面读完最后的字节再关 iroh 连接。
const LINGER: Duration = Duration::from_secs(5);

/// 建好的端点和跑它的运行时。
#[derive(Clone)]
struct Shared {
    handle: Handle,
    endpoint: Endpoint,
}

/// 建不了时不记下，下次要用时再建。
static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn shared() -> io::Result<Shared> {
    let mut shared = SHARED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(shared) = &*shared {
        return Ok(shared.clone());
    }
    let (ready, started) = mpsc::channel();
    thread::Builder::new().name("remote-client-tunnel".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(err) => return drop(ready.send(Err(err))),
        };
        let handle = runtime.handle().clone();
        runtime.block_on(async move {
            match Endpoint::builder(presets::N0).bind().await {
                Ok(endpoint) => {
                    let _ = ready.send(Ok(Shared { handle, endpoint }));
                    std::future::pending::<()>().await;
                }
                Err(err) => drop(ready.send(Err(io::Error::other(err)))),
            }
        });
    })?;
    let built = started.recv().map_err(|_| io::Error::other("the tunnel thread ended early"))??;
    *shared = Some(built.clone());
    Ok(built)
}

/// 经 iroh 连到 `node`，开一条双向流，返回接在它上面的本机 TCP 连接。最晚到 `until`；`stop` 取消了时停下正在
/// 进行的 iroh 连接，返回 `DialError::Cancelled`。
pub(super) fn open(node: &str, until: Instant, stop: &Cancel) -> Result<TcpStream, DialError> {
    let id: EndpointId = node.parse().map_err(|_| DialError::Protocol(format!("{node} is not an iroh address")))?;
    let shared = shared().map_err(|err| DialError::Unreachable { last: format!("cannot start iroh: {err}") })?;
    connect(&shared.handle, &shared.endpoint, id.into(), until, stop)
}

/// `open` 的主体：端点和对方的地址由调用方给。
fn connect(
    handle: &Handle,
    endpoint: &Endpoint,
    addr: EndpointAddr,
    until: Instant,
    stop: &Cancel,
) -> Result<TcpStream, DialError> {
    let unreachable = |err: io::Error| DialError::Unreachable { last: format!("across networks: {err}") };
    let (ours, theirs) = loopback_pair().map_err(unreachable)?;
    let theirs = {
        theirs.set_nonblocking(true).map_err(unreachable)?;
        let _guard = handle.enter();
        tokio::net::TcpStream::from_std(theirs).map_err(unreachable)?
    };
    let left = until.saturating_duration_since(Instant::now());
    let (opened, result) = mpsc::channel();
    let woken = opened.clone();
    let endpoint = endpoint.clone();
    let task = handle.spawn(async move {
        let streams = timeout(left, async {
            let connection = endpoint.connect(addr, TUNNEL_ALPN).await.map_err(io::Error::other)?;
            let (send, recv) = connection.open_bi().await.map_err(io::Error::other)?;
            io::Result::Ok((connection, send, recv))
        });
        match streams.await {
            Ok(Ok((connection, send, recv))) => {
                let _ = opened.send(Ok(()));
                forward(connection, send, recv, theirs).await;
            }
            Ok(Err(err)) => drop(opened.send(Err(err))),
            Err(_) => drop(opened.send(Err(io::Error::new(io::ErrorKind::TimedOut, "timed out")))),
        }
    });
    // 连上以前取消了才停下任务；连上以后撤销，接着搬字节的任务由 `ours` 关掉时收尾。
    let _stopped = {
        let abort = task.abort_handle();
        stop.on_cancel(move || {
            abort.abort();
            let _ = woken.send(Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")));
        })
    };
    match result.recv_timeout(left + Duration::from_secs(1)) {
        Ok(Ok(())) => {
            ours.set_nodelay(true).map_err(unreachable)?;
            Ok(ours)
        }
        Ok(Err(_)) if stop.is_cancelled() => Err(DialError::Cancelled),
        Ok(Err(err)) => Err(unreachable(err)),
        Err(_) => Err(unreachable(io::Error::new(io::ErrorKind::TimedOut, "timed out"))),
    }
}

/// 回环地址上连好的一对 TCP。只认自己连上去的那条：同一时刻别的程序连进这个端口的不要。
fn loopback_pair() -> io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let ours = TcpStream::connect(listener.local_addr()?)?;
    let me = ours.local_addr()?;
    loop {
        let (theirs, peer) = listener.accept()?;
        if peer == me {
            return Ok((ours, theirs));
        }
    }
}

/// 在本机 TCP 和 iroh 的双向流之间搬字节，直到两个方向都说完，再关掉 iroh 连接。
async fn forward(connection: Connection, mut send: SendStream, mut recv: RecvStream, local: tokio::net::TcpStream) {
    let (mut from_local, mut to_local) = local.into_split();
    let up = async {
        let copied = tokio::io::copy(&mut from_local, &mut send).await;
        let _ = send.finish();
        copied
    };
    let down = async {
        let copied = tokio::io::copy(&mut recv, &mut to_local).await;
        let _ = to_local.shutdown().await;
        copied
    };
    let (up, down) = tokio::join!(up, down);
    if let Err(err) = up.and(down) {
        tracing::debug!("a remote connection across networks ended: {err}");
    }
    // 等对面读完最后的字节（含 close_notify）再关，它不读也不久等。
    let _ = timeout(LINGER, send.stopped()).await;
    connection.close(0u32.into(), b"");
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::{Shutdown, SocketAddr},
    };

    use super::*;

    /// 锁定接出来的本机 TCP 两个方向都原样搬：这边说完以后对面读到结尾，对面回的话这边读得完，之后 iroh
    /// 连接关掉。只走本机，不连 n0 的中继和 DNS。
    #[test]
    fn a_tunnelled_connection_carries_both_ways() {
        let (ready, started) = mpsc::channel();
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let handle = runtime.handle().clone();
            runtime.block_on(async move {
                let client = Endpoint::builder(presets::Minimal).bind().await.unwrap();
                let server =
                    Endpoint::builder(presets::Minimal).alpns(vec![TUNNEL_ALPN.to_vec()]).bind().await.unwrap();
                let addr = server
                    .bound_sockets()
                    .into_iter()
                    .filter(SocketAddr::is_ipv4)
                    .fold(EndpointAddr::new(server.id()), |addr, socket| {
                        addr.with_ip_addr((Ipv4Addr::LOCALHOST, socket.port()).into())
                    });
                ready.send((handle, client, addr)).unwrap();
                let connection = server.accept().await.unwrap().await.unwrap();
                let (mut send, mut recv) = connection.accept_bi().await.unwrap();
                let got = recv.read_to_end(1024).await.unwrap();
                send.write_all(&[got.as_slice(), b" back"].concat()).await.unwrap();
                send.finish().unwrap();
                timeout(Duration::from_secs(10), connection.closed()).await.unwrap();
            });
        });
        let (handle, client, addr) = started.recv().unwrap();

        let mut tcp =
            connect(&handle, &client, addr, Instant::now() + Duration::from_secs(10), &Cancel::new()).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        tcp.write_all(b"hello").unwrap();
        tcp.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        tcp.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"hello back");
        drop(tcp);
        server.join().unwrap();
    }

    /// 对面一声不吭（UDP 收了不回）时 iroh 一直在重发；取消后马上放手，不等到截止时刻。只走本机。
    #[test]
    fn cancelling_stops_a_tunnel_that_hangs() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let handle = runtime.handle().clone();
        let (client, server) = runtime.block_on(async {
            let client = Endpoint::builder(presets::Minimal).bind().await.unwrap();
            let server = Endpoint::builder(presets::Minimal).bind().await.unwrap();
            (client, server)
        });
        thread::spawn(move || runtime.block_on(std::future::pending::<()>()));
        let silent = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = EndpointAddr::new(server.id()).with_ip_addr(silent.local_addr().unwrap());

        let stop = Cancel::new();
        let canceller = {
            let stop = stop.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(300));
                let cancelled = Instant::now();
                stop.cancel();
                cancelled
            })
        };
        let result = connect(&handle, &client, addr, Instant::now() + Duration::from_secs(20), &stop);
        let returned = Instant::now();
        let cancelled = canceller.join().unwrap();
        assert!(matches!(result, Err(DialError::Cancelled)), "{:?}", result.err());
        assert!(returned - cancelled < Duration::from_millis(100), "took {:?} after cancelling", returned - cancelled);
    }
}
