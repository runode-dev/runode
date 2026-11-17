//! 经 iroh 按 `node`（对方端点的公钥）跨网络连别的电脑，ALPN 是 `TUNNEL_ALPN`。每台配对过的电脑用自己的一把
//! iroh 私钥（`TunnelKey`）建一个端点，连同一台电脑每次都是同一个公钥。端点只拨号：经 n0 的中继和 DNS 找对方，
//! 不发布自己的地址。端点多出 `MAX_ENDPOINTS` 个时关掉没有隧道连接在用、最久没用的；都在用时暂时多着，等有
//! 连接结束了再关。端点都跑在自己一个线程上的单线程 tokio 运行时里，运行时第一次要用时建，建好以后一直留着。
//!
//! iroh 的流是 async 的，TLS 和门禁是同步的：每条连接在回环地址上接一对 TCP，一头交给调用方当普通的
//! TCP 连接用（握手、门禁、搬字节都和局域网一样），另一头由运行时里的任务和 iroh 的双向流互相搬。

mod slots;

use std::{
    io,
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{Arc, Mutex, MutexGuard, Weak, mpsc},
    thread,
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointAddr, EndpointId, SecretKey,
    address_lookup::{DnsAddressLookup, PkarrResolver},
    endpoint::{Builder, Connection, RecvStream, SendStream, default_relay_mode, presets},
};
use runode_protocol::remote::TUNNEL_ALPN;
use tokio::{io::AsyncWriteExt as _, runtime::Handle, time::timeout};

use self::slots::Slots;
use super::{Cancel, DialError};

/// 跨网络连一台电脑用的 iroh 私钥（ed25519 的种子）。
pub(super) type TunnelKey = [u8; 32];

/// 两边都说完以后，最多等这么久让对面读完最后的字节再关 iroh 连接。
const LINGER: Duration = Duration::from_secs(5);

/// 最多同时留着几台电脑的端点。
const MAX_ENDPOINTS: usize = 8;

/// 跑端点的运行时和建好的端点。
struct Endpoints {
    handle: Handle,
    /// 建端点用的配置，私钥另加。
    builder: fn() -> Builder,
    table: Arc<Mutex<Table>>,
}

/// 建好的端点，挤出去的在 `handle` 上关掉。拿着它的锁时不丢 `InUse`：丢的时候要拿同一把锁。
struct Table {
    slots: Slots<Endpoint>,
    handle: Handle,
}

impl Table {
    fn trim(&mut self) {
        for endpoint in self.slots.trim() {
            close(&self.handle, endpoint);
        }
    }
}

/// 一条隧道连接在用某个端点（连着或正在连），连接结束时丢掉；没有连接在用的端点才会被挤掉。
struct InUse {
    count: Option<Arc<()>>,
    table: Weak<Mutex<Table>>,
}

impl Drop for InUse {
    fn drop(&mut self) {
        drop(self.count.take());
        if let Some(table) = self.table.upgrade() {
            lock(&table).trim();
        }
    }
}

/// 运行时起不来时不记下，下次要用时再起。建端点时一直拿着这把锁：同一把私钥同时要用时只建一个。先拿它再拿
/// `Table` 的锁。
static ENDPOINTS: Mutex<Option<Endpoints>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 在 `handle` 的运行时里关掉 `endpoint`，不等它关完。
fn close(handle: &Handle, endpoint: Endpoint) {
    handle.spawn(async move { endpoint.close().await });
}

/// 只拨号的端点：经 n0 的中继和 DNS 找对方，不发布自己的地址。
fn dial_only() -> Builder {
    Endpoint::builder(presets::Minimal)
        .relay_mode(default_relay_mode())
        .address_lookup(PkarrResolver::n0_dns())
        .address_lookup(DnsAddressLookup::n0_dns())
}

impl Endpoints {
    /// 起跑端点的运行时线程。
    fn start(builder: fn() -> Builder) -> io::Result<Self> {
        let (ready, started) = mpsc::channel();
        thread::Builder::new().name("remote-client-tunnel".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => return drop(ready.send(Err(err))),
            };
            let _ = ready.send(Ok(runtime.handle().clone()));
            runtime.block_on(std::future::pending::<()>());
        })?;
        let handle = started.recv().map_err(|_| io::Error::other("the tunnel thread ended early"))??;
        let table = Table { slots: Slots::new(MAX_ENDPOINTS), handle: handle.clone() };
        Ok(Self { handle, builder, table: Arc::new(Mutex::new(table)) })
    }

    /// 私钥 `key` 的端点和记着这一处在用的 `InUse`；还没有（或已经关了）时建一个，多出来的照 `Slots` 挤掉。
    /// 调用方拿着 `ENDPOINTS` 的锁。
    fn get(&self, key: &TunnelKey) -> io::Result<(Endpoint, InUse)> {
        let id = SecretKey::from_bytes(key).public();
        let found = {
            let mut table = lock(&self.table);
            table.slots.remove(|endpoint| endpoint.id() == id && endpoint.is_closed());
            table.slots.touch(|endpoint| endpoint.id() == id).map(|(endpoint, count)| (endpoint.clone(), count))
        };
        let (endpoint, count) = match found {
            Some(found) => found,
            None => {
                let endpoint = self.bind(SecretKey::from_bytes(key))?;
                let mut table = lock(&self.table);
                let count = table.slots.insert(endpoint.clone());
                table.trim();
                (endpoint, count)
            }
        };
        Ok((endpoint, InUse { count: Some(count), table: Arc::downgrade(&self.table) }))
    }

    /// 关掉公钥是 `id` 的端点，有连接在用也关。
    fn forget(&self, id: EndpointId) {
        if let Some(endpoint) = lock(&self.table).slots.remove(|endpoint| endpoint.id() == id) {
            close(&self.handle, endpoint);
        }
    }

    fn bind(&self, secret: SecretKey) -> io::Result<Endpoint> {
        let (bound, result) = mpsc::channel();
        let builder = self.builder;
        self.handle.spawn(async move {
            let _ = bound.send(builder().secret_key(secret).bind().await.map_err(io::Error::other));
        });
        result.recv().map_err(|_| io::Error::other("the tunnel thread ended early"))?
    }
}

/// 私钥 `key` 的端点、记着这一处在用的 `InUse` 和跑它的运行时。拿到 `ENDPOINTS` 的锁时 `stop` 已经取消了就
/// 不取也不建，报 `io::ErrorKind::Interrupted`：取消以后才拿到锁的，`forget` 可能已经关掉了这把私钥的端点。
fn endpoint(key: &TunnelKey, stop: Option<&Cancel>) -> io::Result<(Handle, Endpoint, InUse)> {
    let mut endpoints = lock(&ENDPOINTS);
    if stop.is_some_and(Cancel::is_cancelled) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let endpoints = match &mut *endpoints {
        Some(endpoints) => endpoints,
        none => none.insert(Endpoints::start(dial_only)?),
    };
    let (endpoint, in_use) = endpoints.get(key)?;
    Ok((endpoints.handle.clone(), endpoint, in_use))
}

/// 在后台把私钥 `key` 的端点建好，之后的 `open` 不用再等它。返回建它的线程，起不了线程时为 `None`。
pub(super) fn warm(key: TunnelKey) -> Option<thread::JoinHandle<()>> {
    let spawned = thread::Builder::new().name("remote-tunnel-warm".into()).spawn(move || {
        if let Err(err) = endpoint(&key, None) {
            tracing::debug!("cannot start iroh ahead of time: {err}");
        }
    });
    spawned.map_err(|err| tracing::debug!("cannot start iroh ahead of time: {err}")).ok()
}

/// 关掉私钥 `key` 的端点，连在它上面的连接跟着断；没建过时什么都不做。正在建时等它建完再关。
pub(super) fn forget(key: &TunnelKey) {
    if let Some(endpoints) = &*lock(&ENDPOINTS) {
        endpoints.forget(SecretKey::from_bytes(key).public());
    }
}

/// 用私钥 `key` 的端点经 iroh 连到 `node`，开一条双向流，返回接在它上面的本机 TCP 连接。最晚到 `until`；
/// `stop` 取消了时停下正在进行的 iroh 连接，返回 `DialError::Cancelled`。
pub(super) fn open(node: &str, key: &TunnelKey, until: Instant, stop: &Cancel) -> Result<TcpStream, DialError> {
    let id: EndpointId = node.parse().map_err(|_| DialError::Protocol(format!("{node} is not an iroh address")))?;
    let (handle, endpoint, in_use) = endpoint(key, Some(stop)).map_err(|err| {
        if stop.is_cancelled() {
            DialError::Cancelled
        } else {
            DialError::Unreachable { last: format!("cannot start iroh: {err}") }
        }
    })?;
    connect(&handle, &endpoint, id.into(), until, stop, in_use)
}

/// `open` 的主体：端点和对方的地址由调用方给。`keep` 留到这条连接结束（连不上、取消了，或者搬完字节）。
fn connect(
    handle: &Handle,
    endpoint: &Endpoint,
    addr: EndpointAddr,
    until: Instant,
    stop: &Cancel,
    keep: impl Send + 'static,
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
        let _keep = keep;
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

    use runode_protocol::remote::RejectReason;

    use super::{super::handshake, *};
    use crate::{
        limit::Source,
        listener::MAX_GATING_PER_SOURCE,
        tunnel::testing::{local_endpoint, tunnel_with},
    };

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
            connect(&handle, &client, addr, Instant::now() + Duration::from_secs(10), &Cancel::new(), ()).unwrap();
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
        let result = connect(&handle, &client, addr, Instant::now() + Duration::from_secs(20), &stop, ());
        let returned = Instant::now();
        let cancelled = canceller.join().unwrap();
        assert!(matches!(result, Err(DialError::Cancelled)), "{:?}", result.err());
        assert!(returned - cancelled < Duration::from_millis(100), "took {:?} after cancelling", returned - cancelled);
    }

    /// 只走本机的端点表：不连 n0 的中继和 DNS。
    fn local_endpoints() -> Endpoints {
        Endpoints::start(|| Endpoint::builder(presets::Minimal)).unwrap()
    }

    /// 等 `endpoint` 关掉，最多等几秒。
    fn closes(endpoint: &Endpoint) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !endpoint.is_closed() {
            if Instant::now() > deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
        true
    }

    fn len(endpoints: &Endpoints) -> usize {
        lock(&endpoints.table).slots.len()
    }

    /// 锁定公钥由私钥定、同一把私钥不另建；多出 `MAX_ENDPOINTS` 个时关掉的是没在用、最久没用的，在用的哪怕
    /// 最久没用也留着。
    #[test]
    fn only_an_idle_endpoint_is_closed_past_the_limit() {
        let endpoints = local_endpoints();
        let keys: Vec<TunnelKey> = (1..=MAX_ENDPOINTS as u8 + 1).map(|n| [n; 32]).collect();
        let (first, in_use) = endpoints.get(&keys[0]).unwrap();
        assert_eq!(first.id(), SecretKey::from_bytes(&keys[0]).public());
        let (second, _) = endpoints.get(&keys[1]).unwrap();
        let (third, _) = endpoints.get(&keys[2]).unwrap();
        for key in &keys[3..MAX_ENDPOINTS] {
            endpoints.get(key).unwrap();
        }
        assert_eq!(endpoints.get(&keys[1]).unwrap().0.id(), second.id());
        assert_eq!(len(&endpoints), MAX_ENDPOINTS);

        // 最久没用的是 first，但它在用；没在用的里面最久没用的是 third。
        endpoints.get(&keys[MAX_ENDPOINTS]).unwrap();
        assert_eq!(len(&endpoints), MAX_ENDPOINTS);
        assert!(closes(&third));
        assert!(!first.is_closed() && !second.is_closed());

        // 关掉了的那把再要时另建一个，同一个公钥；这回挤掉的是不用了的 first。
        drop(in_use);
        let (again, _) = endpoints.get(&keys[2]).unwrap();
        assert!(!again.is_closed());
        assert_eq!(again.id(), third.id());
        assert!(closes(&first));
    }

    /// 锁定端点都在用时暂时多出 `MAX_ENDPOINTS` 个，有一个不用了马上关掉它。
    #[test]
    fn endpoints_all_in_use_stay_until_one_is_done() {
        let endpoints = local_endpoints();
        let mut held: Vec<(Endpoint, InUse)> =
            (1..=MAX_ENDPOINTS as u8 + 1).map(|n| endpoints.get(&[n; 32]).unwrap()).collect();
        assert_eq!(len(&endpoints), MAX_ENDPOINTS + 1);
        assert!(held.iter().all(|(endpoint, _)| !endpoint.is_closed()));

        let (last, last_in_use) = held.pop().unwrap();
        drop(last_in_use);
        assert!(closes(&last));
        assert_eq!(len(&endpoints), MAX_ENDPOINTS);
        assert!(held.iter().all(|(endpoint, _)| !endpoint.is_closed()));
    }

    /// 锁定取消了以后才轮到建端点的 `open` 不再建：配对没成时 `forget` 关掉的端点不会被它用同一把私钥建回来。
    #[test]
    fn a_cancelled_open_builds_no_endpoint() {
        let key: TunnelKey = [201; 32];
        let node = SecretKey::from_bytes(&[9; 32]).public().to_string();
        let stop = Cancel::new();
        stop.cancel();
        let result = open(&node, &key, Instant::now() + Duration::from_secs(5), &stop);
        assert!(matches!(result, Err(DialError::Cancelled)), "{:?}", result.err());
        let id = SecretKey::from_bytes(&key).public();
        let built = lock(&ENDPOINTS)
            .as_ref()
            .is_some_and(|endpoints| lock(&endpoints.table).slots.remove(|endpoint| endpoint.id() == id).is_some());
        assert!(!built);
    }

    /// 锁定 `forget` 关掉在用的端点，别的不动。
    #[test]
    fn forgetting_closes_an_endpoint_in_use() {
        let endpoints = local_endpoints();
        let (one, _in_use) = endpoints.get(&[1; 32]).unwrap();
        let (two, _) = endpoints.get(&[2; 32]).unwrap();
        endpoints.forget(one.id());
        assert!(closes(&one));
        assert_eq!(len(&endpoints), 1);
        assert!(!two.is_closed());
    }

    /// 锁定同一把私钥前后建的两个端点连同一台电脑，那边看到的是同一个公钥；换一把私钥是另一个。只走本机。
    #[test]
    fn the_same_key_reaches_the_other_side_as_the_same_peer() {
        let (ready, started) = mpsc::channel();
        let (seen, peers) = mpsc::channel();
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async move {
                let server =
                    Endpoint::builder(presets::Minimal).alpns(vec![TUNNEL_ALPN.to_vec()]).bind().await.unwrap();
                let addr = server
                    .bound_sockets()
                    .into_iter()
                    .filter(SocketAddr::is_ipv4)
                    .fold(EndpointAddr::new(server.id()), |addr, socket| {
                        addr.with_ip_addr((Ipv4Addr::LOCALHOST, socket.port()).into())
                    });
                ready.send(addr).unwrap();
                for _ in 0..3 {
                    let connection = server.accept().await.unwrap().await.unwrap();
                    seen.send(connection.remote_id()).unwrap();
                    let (mut send, mut recv) = connection.accept_bi().await.unwrap();
                    let got = recv.read_to_end(1024).await.unwrap();
                    send.write_all(&got).await.unwrap();
                    send.finish().unwrap();
                    let _ = timeout(Duration::from_secs(10), connection.closed()).await;
                }
            });
        });
        let addr = started.recv().unwrap();

        let endpoints = local_endpoints();
        let dial = |endpoints: &Endpoints, key: &TunnelKey| {
            let (endpoint, in_use) = endpoints.get(key).unwrap();
            let mut tcp = connect(
                &endpoints.handle,
                &endpoint,
                addr.clone(),
                Instant::now() + Duration::from_secs(10),
                &Cancel::new(),
                in_use,
            )
            .unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            tcp.write_all(b"hi").unwrap();
            tcp.shutdown(Shutdown::Write).unwrap();
            let mut reply = Vec::new();
            tcp.read_to_end(&mut reply).unwrap();
            assert_eq!(reply, b"hi");
            endpoint
        };
        let key = [7; 32];
        let first = dial(&endpoints, &key);
        let first_peer = peers.recv_timeout(Duration::from_secs(10)).unwrap();
        // 像重启以后一样，关掉端点再用同一把私钥另建一个。
        endpoints.forget(first.id());
        assert!(closes(&first));
        dial(&endpoints, &key);
        let second_peer = peers.recv_timeout(Duration::from_secs(10)).unwrap();
        dial(&endpoints, &[8; 32]);
        let other_peer = peers.recv_timeout(Duration::from_secs(10)).unwrap();

        assert_eq!(first_peer, SecretKey::from_bytes(&key).public());
        assert_eq!(second_peer, first_peer);
        assert_ne!(other_peer, first_peer);
        server.join().unwrap();
    }

    /// 只听回环地址的监听，挂着只走本机的隧道，和两个经隧道连它的端点。
    struct Gatekeeper {
        root: std::path::PathBuf,
        dirs: runode_paths::Dirs,
        listener: crate::Listener,
        fingerprint: [u8; runode_protocol::remote::FINGERPRINT_LEN],
        handle: Handle,
        tunnel: EndpointAddr,
        a: Endpoint,
        b: Endpoint,
    }

    impl Gatekeeper {
        fn start(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("rra-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let dirs = runode_paths::Dirs::from_vars(|_| Some(root.clone().into()));
            let none: crate::Connect = std::sync::Arc::new(|| Err(io::Error::other("no host")));
            let listener = crate::Listener::start(crate::Options {
                dirs: dirs.clone(),
                port: 0,
                bind: crate::Bind::Loopback,
                host_name: Some("测试的电脑".into()),
                advertise: false,
                tunnel: false,
                connect: none.clone(),
                connect_screen: none.clone(),
                connect_speech: none.clone(),
                connect_media: none,
                connect_project: None,
            })
            .unwrap();
            let fingerprint = crate::listener_status(&dirs).unwrap().unwrap().fingerprint.0.try_into().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let handle = runtime.handle().clone();
            let (tunnel, a, b) = runtime.block_on(async {
                let (tunnel, _accepting) =
                    tunnel_with(listener.port(), listener.tunnel_ports(), listener.rate_limiter()).await;
                (tunnel, local_endpoint(Vec::new()).await, local_endpoint(Vec::new()).await)
            });
            thread::spawn(move || runtime.block_on(std::future::pending::<()>()));
            Self { root, dirs, listener, fingerprint, handle, tunnel, a, b }
        }

        /// 从 `from` 经隧道连上监听，返回本机这一头的 TCP。
        fn dial(&self, from: &Endpoint) -> Result<TcpStream, DialError> {
            let until = Instant::now() + Duration::from_secs(10);
            connect(&self.handle, from, self.tunnel.clone(), until, &Cancel::new(), ())
        }

        /// 从 `from` 经隧道连上监听，握完手读到 `RemoteChallenge`。
        fn open(&self, from: &Endpoint) -> Result<handshake::Challenged, DialError> {
            handshake::open(self.dial(from)?, &self.fingerprint, Instant::now() + Duration::from_secs(10))
        }
    }

    impl Drop for Gatekeeper {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// 锁定一个隧道对端硬试口令被挡住时，别的隧道对端照样配得上：门禁按对端公钥算失败次数，不按 127.0.0.1。
    #[test]
    fn a_tunnel_peer_locked_out_does_not_lock_out_others() {
        let gate = Gatekeeper::start("tunnel-lockout");
        let ticket = crate::PairingTicket::begin(&gate.dirs, Duration::from_secs(60)).unwrap();
        let key = handshake::load_key(&handshake::generate_key().unwrap()).unwrap();
        let wrong = [0; runode_protocol::remote::SECRET_LEN];
        for _ in 0..crate::limit::MAX_FAILURES {
            let tried = gate.open(&gate.a).and_then(|challenged| challenged.pair(&wrong, "a", &key));
            assert!(matches!(tried, Err(DialError::Rejected(RejectReason::PairingInvalid))), "{:?}", tried.err());
        }
        let paired = gate.open(&gate.b).and_then(|challenged| challenged.pair(&ticket.secret(), "b", &key));
        assert!(paired.is_ok(), "{:?}", paired.err());
        let again = gate.open(&gate.a);
        assert!(matches!(again, Err(DialError::Unreachable { .. })), "{:?}", again.err().map(|err| err.to_string()));
    }

    /// 锁定一个隧道对端占满自己门禁阶段的名额时，别的隧道对端照样进得了门禁。
    #[test]
    fn a_slow_tunnel_peer_does_not_take_another_ones_place() {
        let gate = Gatekeeper::start("tunnel-slow");
        // 只发 TLS 记录的第一个字节，监听方一直等着握手。
        let slow: Vec<TcpStream> = (0..MAX_GATING_PER_SOURCE)
            .map(|_| {
                let mut tcp = gate.dial(&gate.a).unwrap();
                tcp.write_all(&[0x16]).unwrap();
                tcp
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        while gate.listener.gating_sources().len() < MAX_GATING_PER_SOURCE {
            assert!(Instant::now() < deadline, "the listener sees {:?}", gate.listener.gating_sources());
            thread::sleep(Duration::from_millis(10));
        }
        let challenged = gate.open(&gate.b);
        assert!(challenged.is_ok(), "{:?}", challenged.err());
        let sources = gate.listener.gating_sources();
        let from_a = sources.iter().filter(|&&source| source == Source::Tunnel(gate.a.id())).count();
        assert_eq!(from_a, MAX_GATING_PER_SOURCE, "{sources:?}");
        assert!(sources.contains(&Source::Tunnel(gate.b.id())), "{sources:?}");
        drop(slow);
    }
}
