//! 远程桌面的画面改走的 UDP 通道。画面走 TCP（经 ssh）时丢一个包后面的数据全得等重传，Wi-Fi 和公网上一卡就是
//! 几百毫秒；这里一帧切成小片各自发，缺了的片单独补（观看端发 `Nack`），补不上就丢到下一个关键帧，不让一个包卡住
//! 整条流。控制消息和输入仍走原来的连接，密钥和两边的地址也经它交换（`runode_protocol::screen` 的 `Datagram`），
//! 所以这里不另做认证：只有拿着密钥的一方发得出解得开的包。
//!
//! 电脑一头是 `Sender`：往观看端给的各个地址打洞（`Probe`），收到观看端的 `Ping` 就记下它的地址、回 `Pong`；
//! `Ping` 说收到过电脑的包（两个方向都通了）以后 `path` 变成 `Path::Ready`，调用方才改发画面。之后太久收不到
//! `Ping` 就是 `Path::Failed`，调用方把画面改回原来的连接。观看端一头是 `Receiver`：往电脑的各个地址发 `Ping`
//! 直到收到电脑的包，之后定时 `Ping` 保活、量往返时间，拼好的帧按顺序交出去（`assembler`）。

mod assembler;
mod packet;

use std::{
    collections::VecDeque,
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver as Inbox, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ring::rand::{SecureRandom as _, SystemRandom};
use socket2::{Domain, Protocol, Socket, Type};

use assembler::Assembler;
use packet::{CHUNK, Direction, MAX_PACKET, Opener, Packet, Sealer};

/// 电脑打洞、观看端没连上时发 `Ping` 隔这么久一次。
const PROBE_INTERVAL: Duration = Duration::from_millis(50);
/// 连上以后观看端隔这么久 `Ping` 一次。
const PING_INTERVAL: Duration = Duration::from_millis(200);
/// 这么久还没打通就放弃，画面留在原来的连接上。
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(5);
/// 打通以后电脑这么久收不到观看端的包就算断了。
const PATH_TIMEOUT: Duration = Duration::from_secs(2);
/// 电脑记着最近这么多帧，等观看端要重发。
const HISTORY_FRAMES: usize = 120;
/// 记着的帧最多占这么多字节。
const HISTORY_BYTES: usize = 16 << 20;
/// 发画面的速度上限是码率的这么多倍，不低于 `PACE_FLOOR`：一帧的片不一下子全扔给网卡，路上的缓冲才不溢出、
/// 不丢包；倍数留得够，平常的帧不会因此晚到。
const PACE_FACTOR: f64 = 4.;
const PACE_FLOOR: f64 = 40_000_000.;
/// 不限速连发这么多字节。
const PACE_BURST: f64 = 64. * 1024.;
/// 还没发出去的画面最多这么多帧，调用方据此丢帧。
const MAX_QUEUED: usize = 3;
/// 对面给的地址最多用这么多个：打洞时每个地址都要发包，不能让对面拿一长串地址叫这边往别处乱发。
const MAX_CANDIDATES: usize = 16;
/// 收包的缓冲：关键帧的片一下子到，系统默认的缓冲装不下。
const SOCKET_BUFFER: usize = 4 << 20;
/// 观看端收包时最多等这么久就回来看要不要重发、`Ping`。
const VIEWER_TICK: Duration = Duration::from_millis(5);
/// 观看端连着这么多次放弃、中间一帧都没交出，就当 UDP 上过不去画面（小包过得去、大包丢光这类），停下收包；
/// 电脑收不到 `Ping` 就判定断了，画面回到原来的连接。
const MAX_GAPS: u32 = 5;
/// 电脑收包时最多等这么久就回来看要不要打洞、断没断。
const HOST_TICK: Duration = Duration::from_millis(50);

/// 两边共用的密钥，32 字节。
#[derive(Clone, PartialEq, Eq)]
pub struct Key([u8; 32]);

impl Key {
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        SystemRandom::new().fill(&mut bytes).map_err(|_| io::Error::other("the system random source failed"))?;
        Ok(Self(bytes))
    }

    /// 64 位小写十六进制。
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (at, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

/// 本机给对面打洞用的地址：开着的网卡上的地址，套接字收发得了的那几种。
fn local_candidates(socket_port: u16, dual_stack: bool) -> Vec<SocketAddr> {
    crate::local_addresses()
        .into_iter()
        .filter(|addr| dual_stack || addr.is_ipv4())
        .map(|addr| SocketAddr::new(addr, socket_port))
        .collect()
}

/// 开一个收发 UDP 的套接字：能开 IPv6 时开双栈的，IPv4 和 IPv6 都收发；开不了时只开 IPv4。返回它和是不是双栈。
fn bind() -> io::Result<(UdpSocket, bool)> {
    let open = |domain: Domain| -> io::Result<Socket> {
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        if domain == Domain::IPV6 {
            socket.set_only_v6(false)?;
        }
        // 系统给不了这么大时照它能给的。
        let _ = socket.set_recv_buffer_size(SOCKET_BUFFER);
        let _ = socket.set_send_buffer_size(SOCKET_BUFFER);
        let any: SocketAddr = match domain {
            Domain::IPV6 => (Ipv6Addr::UNSPECIFIED, 0).into(),
            _ => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
        };
        socket.bind(&any.into())?;
        Ok(socket)
    };
    match open(Domain::IPV6) {
        Ok(socket) => Ok((socket.into(), true)),
        Err(err) => {
            tracing::debug!("no dual-stack UDP socket ({err}), using IPv4 only");
            Ok((open(Domain::IPV4)?.into(), false))
        }
    }
}

/// 对面给的地址里能打洞的：去掉未指定、组播和广播的，最多 `MAX_CANDIDATES` 个。
fn usable(mut candidates: Vec<SocketAddr>) -> Vec<SocketAddr> {
    candidates.retain(|addr| {
        let ip = addr.ip().to_canonical();
        let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
        addr.port() != 0 && !ip.is_unspecified() && !ip.is_multicast() && !broadcast
    });
    candidates.dedup();
    candidates.truncate(MAX_CANDIDATES);
    candidates
}

/// 收包时这个错只关乎一个包，接着收：超时；Windows 上发往不通的地址以后报的连接重置和网络重置
/// （WSAENETRESET），收到比缓冲区大的包时报的 WSAEMSGSIZE（谁都能往端口上发，不能让它断掉通道）。
fn passing(err: &io::Error) -> bool {
    const WSAEMSGSIZE: i32 = 10040;
    const WSAENETRESET: i32 = 10052;
    matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::ConnectionReset)
        || cfg!(windows) && matches!(err.raw_os_error(), Some(WSAEMSGSIZE | WSAENETRESET))
}

/// 双栈的套接字收到的 IPv4 地址写成 IPv4 映射的 IPv6 地址，换回 IPv4 好和对面报的地址比。
fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// 发往 `addr`：双栈的套接字发 IPv4 时要写成映射的 IPv6 地址。
fn target(addr: SocketAddr, dual_stack: bool) -> SocketAddr {
    match (addr.ip(), dual_stack) {
        (IpAddr::V4(v4), true) => SocketAddr::new(IpAddr::V6(v4.to_ipv6_mapped()), addr.port()),
        _ => addr,
    }
}

/// 电脑一头的 UDP 通道走到哪一步了。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// 在打洞，还不能发画面。
    Connecting,
    /// 两个方向都通了，可以发画面。
    Ready,
    /// 没打通或者断了，不会再好；画面要回到原来的连接上。
    Failed,
}

/// 电脑一头：把画面发给观看端。丢掉时停下它的线程。
pub struct Sender {
    shared: Arc<HostShared>,
    frames: Option<SyncSender<Vec<u8>>>,
    threads: Vec<JoinHandle<()>>,
    port: u16,
    dual_stack: bool,
}

struct HostShared {
    socket: UdpSocket,
    dual_stack: bool,
    sealer: Sealer,
    state: Mutex<HostState>,
    history: Mutex<History>,
    queued: AtomicUsize,
    bitrate: AtomicU32,
    /// 最近发出的帧号，`u32::MAX` 是还没发过。
    last_frame: AtomicU32,
    stop: AtomicBool,
    /// 发新帧和重发共用的限速：重发多的时候正是路上堵的时候，两边加起来也不能超过上限。
    pacer: Mutex<Pacer>,
}

struct HostState {
    path: Path,
    /// 观看端给的地址，打洞用。
    candidates: Vec<SocketAddr>,
    /// 观看端的地址，以最近收到的包为准（换了网络也跟得上）。
    peer: Option<SocketAddr>,
    started: Instant,
    last_heard: Option<Instant>,
}

/// 最近发出的帧，等观看端要重发。
#[derive(Default)]
struct History {
    frames: VecDeque<(u32, Vec<u8>)>,
    bytes: usize,
}

impl History {
    fn push(&mut self, frame: u32, payload: Vec<u8>) {
        self.bytes += payload.len();
        self.frames.push_back((frame, payload));
        while self.frames.len() > HISTORY_FRAMES || self.bytes > HISTORY_BYTES {
            let Some((_, old)) = self.frames.pop_front() else { break };
            self.bytes -= old.len();
        }
    }

    fn get(&self, frame: u32) -> Option<&[u8]> {
        self.frames.iter().find(|(id, _)| *id == frame).map(|(_, payload)| payload.as_slice())
    }
}

impl Sender {
    /// 开套接字，开始往观看端的 `candidates` 打洞（只用 `usable` 留下的）。`bitrate` 是画面的平均码率，按它限
    /// 发送的速度。
    pub fn start(key: &Key, candidates: Vec<SocketAddr>, bitrate: u32) -> io::Result<Self> {
        let candidates = usable(candidates);
        let (socket, dual_stack) = bind()?;
        socket.set_read_timeout(Some(HOST_TICK))?;
        let port = socket.local_addr()?.port();
        let shared = Arc::new(HostShared {
            socket,
            dual_stack,
            sealer: Sealer::new(&key.0, Direction::FromHost),
            state: Mutex::new(HostState {
                path: Path::Connecting,
                candidates,
                peer: None,
                started: Instant::now(),
                last_heard: None,
            }),
            history: Mutex::new(History::default()),
            queued: AtomicUsize::new(0),
            bitrate: AtomicU32::new(bitrate),
            last_frame: AtomicU32::new(u32::MAX),
            stop: AtomicBool::new(false),
            pacer: Mutex::new(Pacer::new()),
        });
        let (tx, rx) = mpsc::sync_channel(MAX_QUEUED);
        let opener = Opener::new(&key.0, Direction::FromViewer);
        let listening = shared.clone();
        let listen = thread::Builder::new().name("screen-udp-in".into()).spawn(move || listening.listen(opener))?;
        let sending = shared.clone();
        let send = thread::Builder::new().name("screen-udp-out".into()).spawn(move || sending.send_frames(&rx))?;
        Ok(Self { shared, frames: Some(tx), threads: vec![listen, send], port, dual_stack })
    }

    /// 收包的端口，告诉观看端。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 本机的地址，告诉观看端。
    pub fn candidates(&self) -> Vec<IpAddr> {
        local_candidates(self.port, self.dual_stack).into_iter().map(|addr| addr.ip()).collect()
    }

    pub fn path(&self) -> Path {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner).path
    }

    /// 画面的平均码率变了，发送的速度上限跟着变。
    pub fn set_bitrate(&self, bitrate: u32) {
        self.shared.bitrate.store(bitrate, Ordering::Relaxed);
    }

    /// 还没发出去的帧数，到了 `max_queued` 时调用方该丢帧。
    pub fn queued(&self) -> usize {
        self.shared.queued.load(Ordering::Relaxed)
    }

    /// `queued` 到了这么多帧时调用方该丢帧。
    pub fn max_queued(&self) -> usize {
        MAX_QUEUED
    }

    /// 发一帧（`encode_video` 的载荷）。还没打通、排满了时丢掉，返回 false。
    pub fn send(&self, payload: Vec<u8>) -> bool {
        if self.path() != Path::Ready {
            return false;
        }
        let Some(frames) = &self.frames else { return false };
        self.shared.queued.fetch_add(1, Ordering::Relaxed);
        if frames.try_send(payload).is_err() {
            self.shared.queued.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.frames = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl HostShared {
    fn send_to(&self, packet: &Packet, addr: SocketAddr) {
        let sealed = self.sealer.seal(packet);
        if let Err(err) = self.socket.send_to(&sealed, target(addr, self.dual_stack)) {
            tracing::trace!("cannot send a datagram to {addr}: {err}");
        }
    }

    fn peer(&self) -> Option<SocketAddr> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).peer
    }

    /// 收观看端的包、打洞、看断没断，到丢掉 `Sender` 为止。
    fn listen(&self, mut opener: Opener) {
        let mut buffer = vec![0u8; MAX_PACKET * 2];
        let mut last_probe: Option<Instant> = None;
        while !self.stop.load(Ordering::Relaxed) {
            match self.socket.recv_from(&mut buffer) {
                Ok((len, from)) => {
                    let from = canonical(from);
                    if let Some(packet) = opener.open(&mut buffer[..len]) {
                        self.handle(&packet, from);
                    }
                }
                Err(err) if passing(&err) => {}
                Err(err) => {
                    tracing::warn!("the remote desktop datagrams stopped: {err}");
                    self.state.lock().unwrap_or_else(PoisonError::into_inner).path = Path::Failed;
                    return;
                }
            }
            let now = Instant::now();
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match state.path {
                Path::Connecting if now.duration_since(state.started) >= ESTABLISH_TIMEOUT => {
                    tracing::info!("no datagram path to the viewer, the video stays on the connection");
                    state.path = Path::Failed;
                }
                Path::Connecting if state.peer.is_none() && last_probe.is_none_or(|at| now - at >= PROBE_INTERVAL) => {
                    let candidates = state.candidates.clone();
                    drop(state);
                    for addr in candidates {
                        self.send_to(&Packet::Probe, addr);
                    }
                    last_probe = Some(now);
                }
                Path::Ready if state.last_heard.is_none_or(|at| now.duration_since(at) >= PATH_TIMEOUT) => {
                    tracing::info!("the datagram path to the viewer went quiet");
                    state.path = Path::Failed;
                }
                _ => {}
            }
        }
    }

    fn handle(&self, packet: &Packet, from: SocketAddr) {
        match packet {
            Packet::Ping { sent_us, heard } => {
                {
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.path == Path::Failed {
                        return;
                    }
                    if state.peer != Some(from) {
                        tracing::info!("remote desktop datagrams go to {from}");
                    }
                    state.peer = Some(from);
                    state.last_heard = Some(Instant::now());
                    if *heard && state.path == Path::Connecting {
                        state.path = Path::Ready;
                    }
                }
                let last = self.last_frame.load(Ordering::Relaxed);
                let last_frame = (last != u32::MAX).then_some(last);
                self.send_to(&Packet::Pong { sent_us: *sent_us, last_frame }, from);
            }
            Packet::Nack { frame, missing } => {
                if self.peer() != Some(from) {
                    return;
                }
                // 复制出来再发，限速等着的时候不占着记录，发画面的线程照样能记新帧。
                let Some(payload) =
                    self.history.lock().unwrap_or_else(PoisonError::into_inner).get(*frame).map(<[u8]>::to_vec)
                else {
                    return;
                };
                let chunks: Vec<&[u8]> = payload.chunks(CHUNK).collect();
                let count = chunks.len() as u16;
                let resend: Vec<u16> = if missing.is_empty() { (0..count).collect() } else { missing.clone() };
                for index in resend {
                    if self.stop.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some(data) = chunks.get(usize::from(index)) {
                        self.pace();
                        self.send_to(&Packet::Video { frame: *frame, index, count, data }, from);
                    }
                }
            }
            Packet::Probe | Packet::Pong { .. } | Packet::Video { .. } => {}
        }
    }

    /// 等到能再发一个包：速度上限按码率算（`PACE_FACTOR`、`PACE_FLOOR`）。只在算的时候拿锁，等的时候不拿，
    /// 另一个线程照样能排。
    fn pace(&self) {
        let rate = (f64::from(self.bitrate.load(Ordering::Relaxed)) * PACE_FACTOR).max(PACE_FLOOR) / 8.;
        let wait = self.pacer.lock().unwrap_or_else(PoisonError::into_inner).reserve(MAX_PACKET as f64, rate);
        if !wait.is_zero() {
            thread::sleep(wait);
        }
    }

    /// 把排着的帧切片、限着速度发给观看端，记进 `history` 等重发。
    fn send_frames(&self, frames: &Inbox<Vec<u8>>) {
        let mut next: u32 = 0;
        for payload in frames {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            let Some(peer) = self.peer() else { continue };
            let frame = next;
            next = next.wrapping_add(1);
            let chunks: Vec<&[u8]> = payload.chunks(CHUNK).collect();
            let count = chunks.len() as u16;
            for (index, data) in chunks.into_iter().enumerate() {
                self.pace();
                self.send_to(&Packet::Video { frame, index: index as u16, count, data }, peer);
            }
            self.last_frame.store(frame, Ordering::Relaxed);
            self.history.lock().unwrap_or_else(PoisonError::into_inner).push(frame, payload);
        }
    }
}

/// 令牌桶：攒满 `PACE_BURST` 字节以后按速度往下发。
struct Pacer {
    tokens: f64,
    last: Instant,
}

impl Pacer {
    fn new() -> Self {
        Self { tokens: PACE_BURST, last: Instant::now() }
    }

    /// 记下要发 `bytes` 字节，返回发之前要等多久，速度按每秒 `rate` 字节。令牌可以欠着，几个线程排着时一个接一个
    /// 往后等。
    fn reserve(&mut self, bytes: f64, rate: f64) -> Duration {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * rate).min(PACE_BURST);
        self.last = now;
        self.tokens -= bytes;
        if self.tokens >= 0. { Duration::ZERO } else { Duration::from_secs_f64(-self.tokens / rate) }
    }
}

/// 观看端那头交出来的。
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// 拼好的一帧（`encode_video` 的载荷），按发出的先后。
    Frame(Vec<u8>),
    /// 有帧补不齐、放弃了，后面的帧丢到下一个关键帧，要向电脑要一个。
    Gap,
}

/// 观看端收包的统计，给码率调整估丢包。
#[derive(Debug, Default)]
pub struct Stats {
    /// 收到的片数。
    pub received: AtomicU64,
    /// 发现缺了的片数（补回来的也算）。
    pub lost: AtomicU64,
    /// 估计的往返时间，微秒。
    pub rtt_us: AtomicU64,
}

/// 观看端一头：开好的套接字，`start` 以后收画面。
pub struct Receiver {
    socket: UdpSocket,
    dual_stack: bool,
    key: Key,
    port: u16,
}

impl Receiver {
    pub fn bind() -> io::Result<Self> {
        let (socket, dual_stack) = bind()?;
        let port = socket.local_addr()?.port();
        Ok(Self { socket, dual_stack, key: Key::generate()?, port })
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 本机的地址，告诉电脑。
    pub fn candidates(&self) -> Vec<IpAddr> {
        local_candidates(self.port, self.dual_stack).into_iter().map(|addr| addr.ip()).collect()
    }

    /// 往电脑的 `candidates`（只用 `usable` 留下的）发 `Ping`，打通以后收画面，经 `on_event` 在收包的线程上交出。
    /// 丢掉返回的 `Receiving` 时停下；一直打不通时自己停下。
    pub fn start(
        self,
        candidates: Vec<SocketAddr>,
        on_event: impl FnMut(Event) + Send + 'static,
    ) -> io::Result<Receiving> {
        self.socket.set_read_timeout(Some(VIEWER_TICK))?;
        let candidates = usable(candidates);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let (stopping, counting) = (stop.clone(), stats.clone());
        let thread = thread::Builder::new()
            .name("screen-udp".into())
            .spawn(move || self.receive(&candidates, &stopping, &counting, on_event))?;
        Ok(Receiving { stop, stats, thread: Some(thread) })
    }

    fn send_to(&self, sealer: &Sealer, packet: &Packet, addr: SocketAddr) {
        if let Err(err) = self.socket.send_to(&sealer.seal(packet), target(addr, self.dual_stack)) {
            tracing::trace!("cannot send a datagram to {addr}: {err}");
        }
    }

    fn receive(&self, candidates: &[SocketAddr], stop: &AtomicBool, stats: &Stats, mut on_event: impl FnMut(Event)) {
        let sealer = Sealer::new(&self.key.0, Direction::FromViewer);
        let mut opener = Opener::new(&self.key.0, Direction::FromHost);
        let mut assembler = Assembler::new();
        let mut buffer = vec![0u8; MAX_PACKET * 2];
        let started = Instant::now();
        let mut peer: Option<SocketAddr> = None;
        let mut last_ping: Option<Instant> = None;
        let mut rtt = Duration::from_millis(20);
        let mut gaps = 0;
        while !stop.load(Ordering::Relaxed) {
            match self.socket.recv_from(&mut buffer) {
                Ok((len, from)) => {
                    let from = canonical(from);
                    let Some(packet) = opener.open(&mut buffer[..len]) else { continue };
                    let now = Instant::now();
                    if peer != Some(from) {
                        // 刚打通：马上告诉电脑收到了，它才开始发画面。
                        peer = Some(from);
                        self.send_to(&sealer, &Packet::Ping { sent_us: micros(started, now), heard: true }, from);
                        last_ping = Some(now);
                    }
                    match packet {
                        Packet::Pong { sent_us, last_frame } => {
                            let sample = Duration::from_micros(micros(started, now).saturating_sub(sent_us));
                            rtt = (rtt * 7 + sample) / 8;
                            stats.rtt_us.store(rtt.as_micros() as u64, Ordering::Relaxed);
                            if let Some(frame) = last_frame {
                                assembler.note_top(frame, now);
                            }
                        }
                        Packet::Video { frame, index, count, data } => {
                            for payload in assembler.chunk(frame, index, count, data, now) {
                                gaps = 0;
                                on_event(Event::Frame(payload));
                            }
                        }
                        Packet::Probe | Packet::Ping { .. } | Packet::Nack { .. } => {}
                    }
                }
                Err(err) if passing(&err) => {}
                Err(err) => {
                    tracing::warn!("the remote desktop datagrams stopped: {err}");
                    return;
                }
            }
            let now = Instant::now();
            match peer {
                // 电脑那边早就放弃了，不再占着端口空转。
                None if now.duration_since(started) >= ESTABLISH_TIMEOUT * 2 => {
                    tracing::info!("no datagram path to the remote desktop");
                    return;
                }
                None if last_ping.is_none_or(|at| now - at >= PROBE_INTERVAL) => {
                    for &addr in candidates {
                        self.send_to(&sealer, &Packet::Ping { sent_us: micros(started, now), heard: false }, addr);
                    }
                    last_ping = Some(now);
                }
                Some(peer) => {
                    if last_ping.is_none_or(|at| now - at >= PING_INTERVAL) {
                        self.send_to(&sealer, &Packet::Ping { sent_us: micros(started, now), heard: true }, peer);
                        last_ping = Some(now);
                    }
                    let poll = assembler.poll(now, rtt);
                    for payload in poll.frames {
                        gaps = 0;
                        on_event(Event::Frame(payload));
                    }
                    for (frame, missing) in poll.nacks {
                        self.send_to(&sealer, &Packet::Nack { frame, missing }, peer);
                    }
                    if poll.gave_up {
                        gaps += 1;
                        if gaps >= MAX_GAPS {
                            tracing::info!("no video gets through the datagram path, leaving it");
                            return;
                        }
                        on_event(Event::Gap);
                    }
                }
                None => {}
            }
            stats.received.store(assembler.received, Ordering::Relaxed);
            stats.lost.store(assembler.lost, Ordering::Relaxed);
        }
    }
}

fn micros(since: Instant, now: Instant) -> u64 {
    now.duration_since(since).as_micros() as u64
}

/// 在收画面的观看端，丢掉时停下。
pub struct Receiving {
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl Receiving {
    pub fn stats(&self) -> Arc<Stats> {
        self.stats.clone()
    }
}

impl Drop for Receiving {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // 收包的线程最多 `VIEWER_TICK` 就回来看一眼；在它自己的回调里丢掉时不等自己。
            if thread.thread().id() != thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 攒着的令牌够时不用等；欠了以后按速度往后等，两个线程排着时一个接一个。
    #[test]
    fn pacer_spreads_packets_beyond_the_burst() {
        let mut pacer = Pacer::new();
        let rate = 1_000_000.;
        assert_eq!(pacer.reserve(PACE_BURST, rate), Duration::ZERO);
        let first = pacer.reserve(1000., rate);
        let second = pacer.reserve(1000., rate);
        assert!(first > Duration::ZERO && second > first, "{first:?} {second:?}");
    }

    #[test]
    fn unusable_candidates_are_dropped() {
        let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
        let candidates = vec![
            addr("10.0.0.5:5000"),
            addr("0.0.0.0:5000"),
            addr("255.255.255.255:5000"),
            addr("224.0.0.1:5000"),
            addr("10.0.0.6:0"),
            addr("[fd00::5]:5000"),
        ];
        assert_eq!(usable(candidates), vec![addr("10.0.0.5:5000"), addr("[fd00::5]:5000")]);
        assert_eq!(usable(vec![addr("10.0.0.5:5000"); 40]).len(), 1);
        let many: Vec<SocketAddr> = (1..=40).map(|n| addr(&format!("10.0.1.{n}:5000"))).collect();
        assert_eq!(usable(many).len(), MAX_CANDIDATES);
    }
}
