//! 跨网络的连接（`runode_protocol::remote` 的「跨网络」）：iroh 端点按这台 Mac 的 Ed25519 公钥
//! 寻址，手机连上来开一条双向流，这边原样转给本机回环地址上的 TLS 监听。流上跑的还是 TLS 1.3 和
//! 门禁，证书指纹、设备签名、限速和撤销都和局域网一样，这里不验证身份；经这里来的连接在门禁上按对端的
//! iroh 公钥算来源（`TunnelPorts`），不按 127.0.0.1。
//!
//! iroh 是 async 的，自己一个线程跑一个单线程的 tokio 运行时，丢掉 `Tunnel` 时关掉端点、线程结束。
//!
//! 对端开出第一条流之前碰不到监听方的名额，所以这里自己分两段占名额：握手时的 `MAX_HANDSHAKES`，
//! 握完手认出公钥后的转发名额（`room_for`）。
//!
//! shortcut: 握手阶段的名额所有对端共用，换着公钥拖住握手的仍能占满，新连接进不来；`node` 只经配对的
//! 二维码和 `RemoteChallenge` 给出去，碰到有人拿它这样占时再按来源地址分握手的名额

use std::{
    collections::HashMap,
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use iroh::{
    Endpoint, EndpointId, SecretKey,
    endpoint::{Incoming, presets},
};
use runode_paths::Dirs;
use runode_protocol::remote::{GATE_TIMEOUT, TUNNEL_ALPN};
use tokio::{
    io::AsyncWriteExt as _,
    net::TcpSocket,
    sync::{OwnedSemaphorePermit, Semaphore, oneshot},
    time::{timeout, timeout_at},
};

use crate::{
    files::{no_home, random, read_optional},
    limit::{RateLimiter, Source},
    listener::{MAX_CONNECTIONS, MAX_GATING_PER_GROUP, MAX_GATING_PER_SOURCE},
};

/// 监听方不再说话以后，最多等这么久让手机读完最后的字节。
const LINGER: Duration = Duration::from_secs(5);
/// 同时在握手的 iroh 连接最多这么多。握完手才知道对端的公钥，所以这份名额所有对端共用，握完就放。
const MAX_HANDSHAKES: usize = 32;
/// 没过过门禁的对端（公钥不在 `RateLimiter::trusted` 里）合起来最多开着这么多条转发，换着公钥连的占不满
/// `MAX_CONNECTIONS`；单个这样的对端还在转发的（`Count::forwarding`）另受 `MAX_GATING_PER_SOURCE` 管。
/// 过过门禁的不受这两个管，每台设备的上限由监听方管。
const MAX_UNKNOWN_FORWARDS: usize = MAX_GATING_PER_GROUP;

/// 监听方接到的连接对上一条还没确认（`Claim::connected`）的登记时，最多等这么久等它确认或撤掉。
const CLAIM_WAIT: Duration = Duration::from_secs(1);

/// 转发到监听方的 TCP 连接各是哪个 iroh 对端开的，按这边在回环地址上的源端口记。转发方绑好源端口后、连监听方
/// 之前登记（`claim`），连上了确认（`Claim::connected`），连不上就撤掉；监听方接到连接时取走确认过的登记
/// （`take`），门禁才能按对端公钥分开限速和算名额。
///
/// 本机别的进程能抢在转发方之前从同一个源端口连上监听方：macOS 上设了 `SO_REUSEADDR` 的 socket 能绑
/// `0.0.0.0` 的同一个端口，连 127.0.0.1 时源地址也是 127.0.0.1。所以只认确认过的登记：抢先连上的占了同一个
/// 四元组，转发方就连不上，登记随之撤掉。
#[derive(Default)]
pub(crate) struct TunnelPorts {
    /// 源端口对应的登记。
    claims: Mutex<HashMap<u16, Registered>>,
    /// 登记确认了或撤掉了。
    settled: Condvar,
    next: AtomicU64,
}

/// 一个源端口上的登记。
#[derive(Clone, Copy)]
struct Registered {
    serial: u64,
    peer: EndpointId,
    /// 转发方已经连上了监听方。
    connected: bool,
}

impl TunnelPorts {
    /// 记下源端口 `port` 上的连接是 `peer` 开的，返回的 `Claim` 丢掉时撤销这条登记。
    fn claim(self: &Arc<Self>, port: u16, peer: EndpointId) -> Claim {
        let serial = self.next.fetch_add(1, Ordering::Relaxed);
        self.claims().insert(port, Registered { serial, peer, connected: false });
        Claim { ports: self.clone(), port, serial }
    }

    /// 从 `peer` 连到监听方的这条连接是哪个 iroh 对端转来的；不是转来的返回 `None`。只认来自 127.0.0.1 的：
    /// 监听所有网卡时，局域网里的对端能自己挑源端口，撞上登记着的端口也不能冒充隧道的对端。登记还没确认时
    /// 最多等 `CLAIM_WAIT`，等不到确认也返回 `None`。
    pub(crate) fn take(&self, peer: SocketAddr) -> Option<EndpointId> {
        if peer.ip().to_canonical() != Ipv4Addr::LOCALHOST {
            return None;
        }
        let port = peer.port();
        let (mut claims, _) = self
            .settled
            .wait_timeout_while(self.claims(), CLAIM_WAIT, |claims| {
                claims.get(&port).is_some_and(|registered| !registered.connected)
            })
            .unwrap_or_else(PoisonError::into_inner);
        if !claims.get(&port)?.connected {
            return None;
        }
        claims.remove(&port).map(|registered| registered.peer)
    }

    fn claims(&self) -> MutexGuard<'_, HashMap<u16, Registered>> {
        self.claims.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 一条登记，丢掉时删掉它；这个端口已经被后来的连接重新登记过时不动那条。
struct Claim {
    ports: Arc<TunnelPorts>,
    port: u16,
    serial: u64,
}

impl Claim {
    /// 转发方连上了监听方：监听方取得走这条登记了。
    fn connected(&self) {
        if let Some(registered) = self.ports.claims().get_mut(&self.port)
            && registered.serial == self.serial
        {
            registered.connected = true;
        }
        self.ports.settled.notify_all();
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut claims = self.ports.claims();
        if claims.get(&self.port).is_some_and(|registered| registered.serial == self.serial) {
            claims.remove(&self.port);
        }
        drop(claims);
        self.ports.settled.notify_all();
    }
}

/// 接连接的任务和各条转发共用的。
struct Forwards {
    /// 本机监听的端口。
    port: u16,
    ports: Arc<TunnelPorts>,
    /// 监听方门禁的限速，放行转发时看对端是不是被挡住了、过没过过门禁。
    limiter: Arc<Mutex<RateLimiter>>,
    /// 开着的转发各是哪个对端的、各几条。
    live: Mutex<HashMap<EndpointId, Count>>,
}

/// 一个对端开着的转发。
#[derive(Clone, Copy, Default, Debug)]
struct Count {
    /// 还连着监听方的。
    forwarding: usize,
    /// 监听方已经说完、在等手机读完最后的字节（`LINGER`）的。不算进单个对端的名额：被门禁拒了马上重连的
    /// 手机，不该被自己上一条还没关完的连接挡住。
    lingering: usize,
}

impl Count {
    fn total(self) -> usize {
        self.forwarding + self.lingering
    }
}

impl Forwards {
    /// 给 `peer` 再开一条转发，返回的 `Slot` 丢掉时放回名额。`peer` 被限速挡住、或者没名额时返回 `None`。
    fn admit(self: &Arc<Self>, peer: EndpointId) -> Option<Slot> {
        let source = Source::Tunnel(peer);
        let mut limiter = self.limiter.lock().unwrap_or_else(PoisonError::into_inner);
        if limiter.limited(source, Instant::now()) {
            tracing::debug!("dropping a tunnelled remote connection from rate limited {source}");
            return None;
        }
        let mut live = self.live();
        if !room_for(&live, peer, |id| limiter.trusted(Source::Tunnel(id))) {
            tracing::debug!("too many tunnelled remote connections, refusing one from {source}");
            return None;
        }
        live.entry(peer).or_default().forwarding += 1;
        Some(Slot { forwards: self.clone(), peer, lingering: false })
    }

    fn live(&self) -> MutexGuard<'_, HashMap<EndpointId, Count>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 一条转发占着的名额，丢掉时放回。
struct Slot {
    forwards: Arc<Forwards>,
    peer: EndpointId,
    lingering: bool,
}

impl Slot {
    /// 监听方说完了，这条转发从 `Count::forwarding` 挪到 `Count::lingering`。
    fn linger(&mut self) {
        if self.lingering {
            return;
        }
        self.lingering = true;
        if let Some(count) = self.forwards.live().get_mut(&self.peer) {
            count.forwarding -= 1;
            count.lingering += 1;
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut live = self.forwards.live();
        if let Some(count) = live.get_mut(&self.peer) {
            if self.lingering {
                count.lingering -= 1;
            } else {
                count.forwarding -= 1;
            }
            if count.total() == 0 {
                live.remove(&self.peer);
            }
        }
    }
}

/// 已经开着 `live` 时，还能不能给 `peer` 再开一条转发。`known` 答一个对端现在过没过过门禁，每次现场问：
/// 没过过的对端过了门禁，它开着的转发随即不再占 `MAX_UNKNOWN_FORWARDS`。
fn room_for(live: &HashMap<EndpointId, Count>, peer: EndpointId, known: impl Fn(EndpointId) -> bool) -> bool {
    if live.values().map(|count| count.total()).sum::<usize>() >= MAX_CONNECTIONS {
        return false;
    }
    if known(peer) {
        return true;
    }
    let unknown: usize = live.iter().filter(|&(&id, _)| !known(id)).map(|(_, count)| count.total()).sum();
    unknown < MAX_UNKNOWN_FORWARDS && live.get(&peer).map_or(0, |count| count.forwarding) < MAX_GATING_PER_SOURCE
}

/// 开着的 iroh 端点。
pub(crate) struct Tunnel {
    node: String,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Tunnel {
    /// 开端点，连上来的转给本机 `port` 上的监听，各条转发的对端登记在 `ports`；`limiter` 是那个监听的门禁
    /// 限速。私钥第一次用时生成，存在 `remote_access_tunnel_key_file`；调用方要拿着 `remote_access_lock_file`
    /// 的锁。
    pub(crate) fn start(
        dirs: &Dirs,
        port: u16,
        ports: Arc<TunnelPorts>,
        limiter: Arc<Mutex<RateLimiter>>,
    ) -> io::Result<Self> {
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
                    () = accept(&endpoint, port, ports, limiter) => {}
                    _ = stopped => {}
                }
                endpoint.close().await;
            });
        })?;
        let node = started.recv().map_err(|_| io::Error::other("the tunnel thread ended early"))??;
        tracing::info!("remote access is reachable across networks as {node}");
        Ok(Self { node, stop: Some(stop), thread: Some(thread) })
    }

    /// 手机拨号用的地址。
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

async fn accept(endpoint: &Endpoint, port: u16, ports: Arc<TunnelPorts>, limiter: Arc<Mutex<RateLimiter>>) {
    let handshakes = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    let forwards = Arc::new(Forwards { port, ports, limiter, live: Mutex::default() });
    while let Some(incoming) = endpoint.accept().await {
        let Ok(handshake) = handshakes.clone().try_acquire_owned() else {
            tracing::debug!("too many tunnelled remote connections handshaking, refusing one");
            incoming.refuse();
            continue;
        };
        let forwards = forwards.clone();
        tokio::spawn(async move {
            if let Err(err) = forward(incoming, handshake, &forwards).await {
                tracing::debug!("a tunnelled remote connection ended: {err}");
            }
        });
    }
}

/// 一条 iroh 连接上的第一条双向流和到本机监听的 TCP 连接之间搬字节，直到手机和监听方都说完，
/// 或者监听方先说完。握手加开出第一条流限时 `GATE_TIMEOUT`。`handshake` 只拿到握完手；握完手按对端
/// 公钥占转发的名额（`Forwards::admit`），占不到时丢掉连接，占到的拿到关掉连接。
///
/// 到监听方的连接先绑好源端口、在 `ports` 登记了对端再连，连上了确认登记；连不上时返回、撤掉登记。
async fn forward(incoming: Incoming, handshake: OwnedSemaphorePermit, forwards: &Arc<Forwards>) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + GATE_TIMEOUT;
    let connection = timeout_at(deadline, incoming).await?.map_err(io::Error::other)?;
    drop(handshake);
    let peer = connection.remote_id();
    let Some(mut slot) = forwards.admit(peer) else {
        return Ok(());
    };
    let (mut send, mut recv) = timeout_at(deadline, connection.accept_bi()).await?.map_err(io::Error::other)?;
    let socket = TcpSocket::new_v4()?;
    #[cfg(windows)]
    exclusive(&socket)?;
    socket.bind((Ipv4Addr::LOCALHOST, 0).into())?;
    let claim = forwards.ports.claim(socket.local_addr()?.port(), peer);
    let mut tcp = socket.connect((Ipv4Addr::LOCALHOST, forwards.port).into()).await?;
    claim.connected();
    tcp.set_nodelay(true)?;
    let copied = {
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
        tokio::pin!(up, down);
        tokio::select! {
            up = &mut up => up.and(down.await),
            // 监听方说完就是关了连接（门禁拒绝、超时、撤销），手机再发的没人收，不等它说完。
            down = &mut down => down,
        }
    };
    drop(claim);
    drop(tcp);
    slot.linger();
    // 等手机读完最后的字节再关连接，不然它收不到 close_notify 和拒绝的回话；它不读也不久等。
    let _ = timeout(LINGER, send.stopped()).await;
    drop(connection);
    drop(slot);
    copied.map(drop)
}

/// 独占 `socket` 要绑的地址和端口：Windows 上别的 socket 设了 `SO_REUSEADDR` 就能绑到同一个地址和端口，
/// 抢走登记着的源端口。
#[cfg(windows)]
fn exclusive(socket: &TcpSocket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket as _;
    use windows_sys::Win32::Networking::WinSock::{SO_EXCLUSIVEADDRUSE, SOL_SOCKET, WSAGetLastError, setsockopt};
    let on: i32 = 1;
    // SAFETY: socket 来自 `socket`；值指向本地变量，长度是它的大小。
    let result = unsafe {
        setsockopt(socket.as_raw_socket() as usize, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (&raw const on).cast(), 4)
    };
    // SAFETY: 只读本线程上一次 socket 调用的错误码。
    if result == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() })) }
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
            runode_paths::replace_private(&path, &bytes)?;
            bytes
        }
    };
    Ok(SecretKey::from_bytes(&bytes))
}

/// 测试里只走本机的隧道。
#[cfg(test)]
pub(crate) mod testing {
    use iroh::EndpointAddr;

    use super::*;

    /// 只走本机：不连 n0 的中继和 DNS，测试不碰外网。
    pub(crate) async fn local_endpoint(alpns: Vec<Vec<u8>>) -> Endpoint {
        Endpoint::builder(presets::Minimal).alpns(alpns).bind().await.unwrap()
    }

    /// 配一份空的限速的 `tunnel_with`：转发的名额上谁都没过过门禁。
    pub(crate) async fn tunnel_to(port: u16, ports: Arc<TunnelPorts>) -> (EndpointAddr, tokio::task::JoinHandle<()>) {
        tunnel_with(port, ports, Arc::default()).await
    }

    /// 起一个转发到本机 `port` 的端点，对端登记在 `ports`，转发的名额按限速 `limiter` 算；返回它的地址和接
    /// 连接的任务（在当前运行时上跑）。
    pub(crate) async fn tunnel_with(
        port: u16,
        ports: Arc<TunnelPorts>,
        limiter: Arc<Mutex<RateLimiter>>,
    ) -> (EndpointAddr, tokio::task::JoinHandle<()>) {
        let server = local_endpoint(vec![TUNNEL_ALPN.to_vec()]).await;
        let addr = server
            .bound_sockets()
            .into_iter()
            .filter(SocketAddr::is_ipv4)
            .fold(EndpointAddr::new(server.id()), |addr, socket| {
                addr.with_ip_addr((Ipv4Addr::LOCALHOST, socket.port()).into())
            });
        (addr, tokio::spawn(async move { accept(&server, port, ports, limiter).await }))
    }
}

#[cfg(test)]
mod tests {
    use runode_protocol::remote::{Bytes, DeviceId, DeviceKind};
    use tokio::{io::AsyncReadExt as _, net::TcpListener};

    use super::{
        testing::{local_endpoint, tunnel_to, tunnel_with},
        *,
    };
    use crate::{devices, limit::MAX_FAILURES};

    fn peer(n: u8) -> EndpointId {
        SecretKey::from_bytes(&[n; 32]).public()
    }

    fn forwarding(n: usize) -> Count {
        Count { forwarding: n, lingering: 0 }
    }

    /// `count` 个各开着一条转发的对端。
    fn one_each(count: usize) -> HashMap<EndpointId, Count> {
        (0..count).map(|n| (peer(u8::try_from(n).unwrap()), forwarding(1))).collect()
    }

    #[test]
    fn unknown_peers_cannot_shut_out_a_known_one() {
        let live = one_each(MAX_UNKNOWN_FORWARDS);
        let known = peer(200);
        let is_known = |id| id == known;
        assert!(!room_for(&live, peer(100), is_known));
        assert!(room_for(&live, known, is_known));
    }

    #[test]
    fn one_unknown_peer_cannot_take_the_others_places() {
        let live = HashMap::from([(peer(1), forwarding(MAX_GATING_PER_SOURCE))]);
        assert!(!room_for(&live, peer(1), |_| false));
        assert!(room_for(&live, peer(2), |_| false));
        // 过过门禁的对端在这里没有单个的上限，只受总数管。
        let busy = HashMap::from([(peer(1), forwarding(MAX_CONNECTIONS - 1))]);
        assert!(room_for(&busy, peer(1), |_| true));
        let full = HashMap::from([(peer(1), forwarding(MAX_CONNECTIONS))]);
        assert!(!room_for(&full, peer(1), |_| true));
        assert!(!room_for(&full, peer(2), |_| false));
    }

    /// 等手机读完的连接不算进单个对端的名额，但照样占没过过门禁的那份和总数。
    #[test]
    fn lingering_forwards_count_only_toward_the_pools() {
        let lingering = |n| Count { forwarding: 0, lingering: n };
        let live = HashMap::from([(peer(1), lingering(MAX_GATING_PER_SOURCE))]);
        assert!(room_for(&live, peer(1), |_| false));
        let live = HashMap::from([(peer(1), lingering(MAX_UNKNOWN_FORWARDS))]);
        assert!(!room_for(&live, peer(2), |_| false));
        assert!(room_for(&live, peer(2), |id| id == peer(2)));
        let live = HashMap::from([(peer(1), lingering(MAX_CONNECTIONS))]);
        assert!(!room_for(&live, peer(2), |_| true));
    }

    #[test]
    fn peers_that_get_in_leave_the_unknown_pool() {
        let live = one_each(MAX_UNKNOWN_FORWARDS);
        let newcomer = peer(200);
        assert!(!room_for(&live, newcomer, |_| false));
        assert!(room_for(&live, newcomer, |id| id != newcomer));
    }

    /// 锁定名额丢掉时放回、各对端都放完时表是空的，`Slot::linger` 只挪一次，被限速挡住的对端占不到名额。
    #[test]
    fn slots_go_back_when_dropped() {
        let limiter = Arc::new(Mutex::new(RateLimiter::default()));
        let forwards =
            Arc::new(Forwards { port: 0, ports: Arc::default(), limiter: limiter.clone(), live: Mutex::default() });
        let mut slots: Vec<Slot> = (0..MAX_GATING_PER_SOURCE).map(|_| forwards.admit(peer(1)).unwrap()).collect();
        assert!(forwards.admit(peer(1)).is_none());
        slots.pop();
        assert!(forwards.admit(peer(1)).is_some());
        slots[0].linger();
        slots[0].linger();
        let count = forwards.live()[&peer(1)];
        assert_eq!((count.forwarding, count.lingering), (MAX_GATING_PER_SOURCE - 2, 1));
        assert!(forwards.admit(peer(1)).is_some());
        drop(slots);
        assert!(forwards.live().is_empty());

        let now = Instant::now();
        for _ in 0..MAX_FAILURES {
            limiter.lock().unwrap().failed(Source::Tunnel(peer(2)), now);
        }
        assert!(forwards.admit(peer(2)).is_none());
        assert!(forwards.admit(peer(3)).is_some());
    }

    /// 锁定一批没过过门禁的公钥占满 `MAX_UNKNOWN_FORWARDS` 时，再来的没过过的公钥握完手就被关掉（不等到
    /// `GATE_TIMEOUT`），过过门禁的公钥照样转得进监听方的门禁。
    #[tokio::test]
    async fn unknown_peers_cannot_shut_out_a_known_one_at_the_gate() {
        let root = std::env::temp_dir().join(format!("rra-tunnel-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        // 过过门禁的设备要在设备表里，不然监听方核对撤销时把它的公钥从 `RateLimiter::trusted` 里删掉。
        let device = DeviceId([7; 16]);
        let paired = devices::Device {
            device_id: device,
            name: "phone".into(),
            kind: DeviceKind::Mobile,
            public_key: Bytes(vec![4; 65]),
            paired_at: 0,
            last_seen: 0,
        };
        devices::add(&dirs, paired).unwrap();
        let none: crate::Connect = Arc::new(|| Err(io::Error::other("no host")));
        let listener = crate::Listener::start(crate::Options {
            dirs: dirs.clone(),
            port: 0,
            bind: crate::Bind::Loopback,
            host_name: Some("test".into()),
            advertise: false,
            tunnel: false,
            connect: none.clone(),
            connect_screen: none.clone(),
            connect_speech: none.clone(),
            connect_media: none,
            connect_project: None,
        })
        .unwrap();
        let (addr, accepting) = tunnel_with(listener.port(), listener.tunnel_ports(), listener.rate_limiter()).await;
        let known = local_endpoint(Vec::new()).await;
        listener.rate_limiter().lock().unwrap().succeeded(Source::Tunnel(known.id()), device, Instant::now());
        let in_gate = async |count: usize, source: Option<Source>| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let sources = listener.gating_sources();
                if sources.len() >= count && source.is_none_or(|source| sources.contains(&source)) {
                    return;
                }
                assert!(Instant::now() < deadline, "the listener sees {sources:?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };

        // 各开一条流，只发 TLS 记录的第一个字节，监听方一直等着握手。
        let mut unknown = Vec::new();
        for _ in 0..MAX_UNKNOWN_FORWARDS {
            let phone = local_endpoint(Vec::new()).await;
            let connection = phone.connect(addr.clone(), TUNNEL_ALPN).await.unwrap();
            let (mut send, recv) = connection.open_bi().await.unwrap();
            send.write_all(&[0x16]).await.unwrap();
            unknown.push((phone, connection, send, recv));
        }
        in_gate(MAX_UNKNOWN_FORWARDS, None).await;

        let stranger = local_endpoint(Vec::new()).await;
        let refused = stranger.connect(addr.clone(), TUNNEL_ALPN).await.unwrap();
        timeout(GATE_TIMEOUT / 2, refused.closed()).await.unwrap();

        let connection = known.connect(addr, TUNNEL_ALPN).await.unwrap();
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        send.write_all(&[0x16]).await.unwrap();
        in_gate(MAX_UNKNOWN_FORWARDS + 1, Some(Source::Tunnel(known.id()))).await;

        accepting.abort();
        drop(unknown);
        drop(listener);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 锁定转发两个方向原样搬字节：手机说完以后监听那边读到结尾，监听回的话手机读得完；监听方按源地址
    /// 查得到是哪个对端转来的，只认 127.0.0.1 来的。
    #[tokio::test]
    async fn forwards_both_ways_until_both_sides_are_done() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ports = Arc::new(TunnelPorts::default());
        let (addr, accepting) = tunnel_to(port, ports.clone()).await;
        let phone = local_endpoint(Vec::new()).await;
        let echo = {
            let ports = ports.clone();
            tokio::spawn(async move {
                let (mut tcp, from) = listener.accept().await.unwrap();
                let elsewhere = SocketAddr::new([192, 0, 2, 1].into(), from.port());
                assert_eq!(ports.take(elsewhere), None);
                // `take` 可能要等转发确认登记，转发跑在同一个线程上，不能在这里等。
                let from = tokio::task::spawn_blocking(move || ports.take(from)).await.unwrap();
                let mut got = Vec::new();
                tcp.read_to_end(&mut got).await.unwrap();
                tcp.write_all(&[got.as_slice(), b" back"].concat()).await.unwrap();
                from
            })
        };

        let connection = phone.connect(addr, TUNNEL_ALPN).await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"hello").await.unwrap();
        send.finish().unwrap();
        let reply = recv.read_to_end(1024).await.unwrap();
        assert_eq!(reply, b"hello back");
        assert_eq!(echo.await.unwrap(), Some(phone.id()));
        assert!(ports.claims().is_empty());
        accepting.abort();
    }

    /// 锁定监听方先关了连接、手机一直不关发送的一边时，转发照样结束、关掉 iroh 连接，名额才放得出来；
    /// 监听方没取走的登记也跟着删掉。
    #[tokio::test]
    async fn ends_when_the_listener_closes_first() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ports = Arc::new(TunnelPorts::default());
        let (addr, accepting) = tunnel_to(port, ports.clone()).await;
        let refuse = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            // 先读掉手机发来的：关连接时还有没读的，Windows 发 RST，对面还没读走的「no」跟着丢掉。
            tcp.read_exact(&mut [0; 5]).await.unwrap();
            tcp.write_all(b"no").await.unwrap();
        });

        let phone = local_endpoint(Vec::new()).await;
        let connection = phone.connect(addr, TUNNEL_ALPN).await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(b"hello").await.unwrap();
        assert_eq!(recv.read_to_end(1024).await.unwrap(), b"no");
        timeout(GATE_TIMEOUT, connection.closed()).await.unwrap();
        refuse.await.unwrap();
        assert!(ports.claims().is_empty());
        accepting.abort();
    }

    /// 锁定一条登记丢掉时只删自己：端口被后来的连接重新登记过时，后来的那条还在。
    #[test]
    fn a_stale_claim_leaves_a_newer_one_on_the_same_port() {
        let ports = Arc::new(TunnelPorts::default());
        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, 4000));
        drop(ports.claim(local.port(), peer(1)));
        assert!(ports.claims().is_empty());

        let stale = ports.claim(local.port(), peer(1));
        let fresh = ports.claim(local.port(), peer(2));
        stale.connected();
        drop(stale);
        fresh.connected();
        assert_eq!(ports.take(local), Some(peer(2)));
        assert_eq!(ports.take(local), None);
        drop(fresh);
    }

    /// 锁定转发方还没确认的登记监听方不认：等到确认了才取走，等到撤掉或者等过 `CLAIM_WAIT` 都不认。
    #[test]
    fn a_claim_counts_only_once_connected() {
        let ports = Arc::new(TunnelPorts::default());
        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, 4001));
        let waiting = |ports: &Arc<TunnelPorts>| {
            let ports = ports.clone();
            thread::spawn(move || ports.take(local))
        };

        let claim = ports.claim(local.port(), peer(1));
        let started = Instant::now();
        assert_eq!(ports.take(local), None);
        assert!(started.elapsed() >= CLAIM_WAIT);
        assert!(ports.claims().contains_key(&local.port()));

        let taking = waiting(&ports);
        thread::sleep(Duration::from_millis(50));
        claim.connected();
        assert_eq!(taking.join().unwrap(), Some(peer(1)));

        let claim = ports.claim(local.port(), peer(2));
        let started = Instant::now();
        let taking = waiting(&ports);
        thread::sleep(Duration::from_millis(50));
        drop(claim);
        assert_eq!(taking.join().unwrap(), None);
        assert!(started.elapsed() < CLAIM_WAIT);
    }

    /// 锁定转发方连不上监听方时撤掉登记：本机别的进程抢先从同一个源端口连上监听方，它占了同一个四元组，
    /// 转发方连不上，监听方取不到对端。只在别的 socket 绑得上 `0.0.0.0` 的同一个端口的系统上（macOS）复现得了。
    #[tokio::test]
    async fn a_local_process_on_the_claimed_port_is_not_the_peer() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let target: SocketAddr = listener.local_addr().unwrap();
        let ports = Arc::new(TunnelPorts::default());
        let socket = TcpSocket::new_v4().unwrap();
        #[cfg(windows)]
        exclusive(&socket).unwrap();
        socket.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let port = socket.local_addr().unwrap().port();
        let claim = ports.claim(port, peer(1));

        let intruder = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        intruder.set_reuse_address(true).unwrap();
        let bound = intruder.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into());
        if cfg!(target_os = "macos") {
            bound.unwrap();
        } else if bound.is_err() {
            return;
        }
        intruder.connect(&target.into()).unwrap();
        let taking = {
            let ports = ports.clone();
            thread::spawn(move || {
                let (_tcp, from) = listener.accept().unwrap();
                assert_eq!(from.port(), port);
                ports.take(from)
            })
        };

        assert!(socket.connect(target).await.is_err());
        drop(claim);
        assert_eq!(taking.join().unwrap(), None);
        drop(intruder);
    }
}
