//! 开着的远程访问监听：拿锁、备好证书、开 TCP 监听、写状态文件、Bonjour 公布，一个线程接连接并
//! 每秒看一眼设备表（撤销了的设备断开），每条连接一个线程（`gate::serve`）。丢掉 `Listener` 时
//! 全部停下，连着的连接也断开。

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, Read as _, Write as _},
    mem::size_of,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use runode_paths::Dirs;
use runode_protocol::remote::{Bytes, DeviceId, GATE_VERSION, encode_base64url};
use rustls::{ServerConfig, crypto::ring::default_provider, version::TLS13};

use crate::{
    Connect, addrs, bonjour, devices,
    files::{no_home, write_json},
    gate, identity,
    limit::RateLimiter,
    pairing::Desk,
    status::{ListenerStatus, lock_listener},
};

/// 还没过门禁的连接最多这么多，再来的直接关掉。
const MAX_GATING: usize = 32;
/// 连接（含已经接到宿主上的）最多这么多。
const MAX_CONNECTIONS: usize = 64;
/// 同一台设备最多同时连着这么多条，见 `Shared::attach_device`。
const MAX_PER_DEVICE: usize = 8;
/// 隔多久看一次设备表，撤销了的设备在这么久之内断开。
const REVOKE_CHECK: Duration = Duration::from_secs(1);
/// 接受连接出错（比如文件描述符用完了）后等一会儿再接，免得空转。
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// 在哪些地址上听。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bind {
    /// 所有网卡：IPv4 的 `0.0.0.0` 和 IPv6 的 `::`（只收 IPv6，IPv4 由前一个收）。
    All,
    /// 只听 `127.0.0.1`，测试用。
    Loopback,
}

/// 怎么开监听。
#[derive(Clone)]
pub struct Options {
    /// 证书、设备表这些文件放在哪，见 `runode_paths::Dirs::remote_access_dir`。
    pub dirs: Dirs,
    /// 端口；0 时由系统挑一个（只用于 `Bind::Loopback`），见 `Listener::local_addrs`。
    pub port: u16,
    pub bind: Bind,
    /// 给手机看的主机名；`None` 时用电脑名，见 `host_name`。
    pub host_name: Option<String>,
    /// 用 Bonjour 公布。
    pub advertise: bool,
    /// 门禁过了以后要一条到宿主的新连接。
    pub connect: Connect,
}

/// 开着的监听，丢掉时停下。
pub struct Listener {
    shared: Arc<Shared>,
    port: u16,
    addrs: Vec<SocketAddr>,
    wake: io::PipeWriter,
    thread: Option<JoinHandle<()>>,
    _bonjour: Option<bonjour::Registration>,
    /// 一直拿着，丢掉时放开，见 `status::lock_listener`。
    _lock: File,
}

/// 接连接的线程和各条连接的线程共用的。
pub(crate) struct Shared {
    pub(crate) dirs: Dirs,
    pub(crate) host_name: String,
    pub(crate) tls: Arc<ServerConfig>,
    pub(crate) connect: Connect,
    /// 要停了：门禁阶段的连接回 `RejectReason::Disabled`。
    pub(crate) stopping: AtomicBool,
    connections: Mutex<HashMap<u64, Live>>,
    next_id: AtomicU64,
    limiter: Mutex<RateLimiter>,
    desk: Mutex<Desk>,
}

/// 一条连着的连接。
struct Live {
    /// 手机的连接，断开它时 `shutdown`，那条连接的线程随之醒来结束。
    tcp: TcpStream,
    /// 过了门禁的设备；还在门禁阶段时为 `None`。
    device: Option<DeviceId>,
    cut: Arc<AtomicBool>,
}

impl Shared {
    pub(crate) fn limiter(&self) -> MutexGuard<'_, RateLimiter> {
        self.limiter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn desk(&self) -> MutexGuard<'_, Desk> {
        self.desk.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn connections(&self) -> MutexGuard<'_, HashMap<u64, Live>> {
        self.connections.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 连接 `id` 过了门禁，是设备 `device`：之后撤销这台设备时断开它。
    ///
    /// 同一台设备的连接超过 `MAX_PER_DEVICE` 条时断开最早的：手机反复重连、旧连接又还没被 keepalive
    /// 发现断了时（见 `gate::keep_alive`），不让它们攒到 `MAX_CONNECTIONS` 把别的连接挡在外面。
    pub(crate) fn attach_device(&self, id: u64, device: DeviceId) {
        let mut connections = self.connections();
        if let Some(live) = connections.get_mut(&id) {
            live.device = Some(device);
        }
        let mut same: Vec<u64> =
            connections.iter().filter(|(_, live)| live.device == Some(device)).map(|(&id, _)| id).collect();
        same.sort_unstable();
        let excess = same.len().saturating_sub(MAX_PER_DEVICE);
        for old in &same[..excess] {
            if let Some(live) = connections.get(old) {
                tracing::info!("remote device {device} has too many connections, closing an old one");
                cut(live);
            }
        }
    }

    /// 断开设备不在 `known` 里的连接（门禁阶段的不管）。
    fn cut_revoked(&self, known: &HashSet<DeviceId>) {
        for live in self.connections().values() {
            if let Some(device) = live.device
                && !known.contains(&device)
            {
                tracing::info!("remote device {device} was revoked, disconnecting it");
                cut(live);
            }
        }
    }
}

fn cut(live: &Live) {
    live.cut.store(true, Ordering::Relaxed);
    let _ = live.tcp.shutdown(Shutdown::Both);
}

impl Listener {
    /// 开监听。另一个进程已经开着（锁在它手里）、端口被占时返回错误，调用方隔一会儿再试。
    pub fn start(options: Options) -> io::Result<Self> {
        let Options { dirs, port, bind, host_name, advertise, connect } = options;
        let lock = lock_listener(&dirs)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::AddrInUse, "another runode process is serving remote access")
        })?;
        let identity = identity::load_or_create(&dirs)?;
        let fingerprint = identity.fingerprint;
        let tls = ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_protocol_versions(&[&TLS13])
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(vec![identity.cert], identity.key)
            .map_err(io::Error::other)?;
        let listeners = open(bind, port)?;
        let addrs: Vec<SocketAddr> = listeners.iter().map(TcpListener::local_addr).collect::<io::Result<_>>()?;
        let port = addrs.first().map_or(port, SocketAddr::port);
        let host_name = host_name.unwrap_or_else(addrs::host_name);
        let fp = encode_base64url(&fingerprint);
        let status = ListenerStatus { port, fingerprint: Bytes(fingerprint.to_vec()), host_name: host_name.clone() };
        write_json(&dirs.remote_access_status_file().ok_or_else(no_home)?, &status)?;
        let shared = Arc::new(Shared {
            dirs,
            host_name: host_name.clone(),
            tls: Arc::new(tls),
            connect,
            stopping: AtomicBool::new(false),
            connections: Mutex::default(),
            next_id: AtomicU64::new(1),
            limiter: Mutex::default(),
            desk: Mutex::default(),
        });
        let (wake_rx, wake) = io::pipe()?;
        let thread_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("remote-listener".into())
            .spawn(move || accept_loop(&thread_shared, &listeners, &wake_rx))?;
        let bonjour = if advertise {
            let version = GATE_VERSION.to_string();
            match bonjour::register(&host_name, port, &[("v", &version), ("fp", &fp)]) {
                Ok(registration) => Some(registration),
                Err(err) => {
                    tracing::warn!("cannot advertise remote access over Bonjour: {err}");
                    None
                }
            }
        } else {
            None
        };
        tracing::info!("remote access listening on {addrs:?} as {host_name:?}, certificate {fp}");
        Ok(Self { shared, port, addrs, wake, thread: Some(thread), _bonjour: bonjour, _lock: lock })
    }

    /// 开监听时要的端口（`Options::port`，可能是 0）。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 实际在听的地址。
    pub fn local_addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Relaxed);
        let _ = self.wake.write_all(&[1]);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the remote access listener thread panicked");
        }
        for live in self.shared.connections().values() {
            cut(live);
        }
        if let Some(path) = self.shared.dirs.remote_access_status_file() {
            let _ = std::fs::remove_file(path);
        }
        tracing::info!("remote access stopped");
    }
}

/// 按 `bind` 开 TCP 监听。`Bind::All` 时 IPv6 开不了（系统没开 IPv6）只记日志，端口被占照样报错。
fn open(bind: Bind, port: u16) -> io::Result<Vec<TcpListener>> {
    let listeners = match bind {
        Bind::Loopback => vec![TcpListener::bind((Ipv4Addr::LOCALHOST, port))?],
        Bind::All => {
            let mut listeners = vec![TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))?];
            match listen_v6_only(port) {
                Ok(listener) => listeners.push(listener),
                Err(err) if err.raw_os_error() == Some(libc::EAFNOSUPPORT) => {
                    tracing::info!("no IPv6, remote access listens on IPv4 only");
                }
                Err(err) => return Err(err),
            }
            listeners
        }
    };
    for listener in &listeners {
        listener.set_nonblocking(true)?;
    }
    Ok(listeners)
}

/// 在 `[::]:port` 上开只收 IPv6 的监听（`IPV6_V6ONLY`）：macOS 默认 IPv6 的监听也收 IPv4，会和
/// `0.0.0.0` 上的那个撞端口。标准库开监听时没法在绑定前设这个选项，所以自己建 socket。
fn listen_v6_only(port: u16) -> io::Result<TcpListener> {
    // SAFETY: 只建一个 socket，返回的描述符马上交给 `OwnedFd`。
    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` 是上面新建的、没人用的描述符。
    let socket = unsafe { OwnedFd::from_raw_fd(fd) };
    let set = |level, name| {
        let on: libc::c_int = 1;
        // SAFETY: 描述符开着；值指向本地变量，长度是它的大小。
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                level,
                name,
                (&raw const on).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if result == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
    };
    set(libc::IPPROTO_IPV6, libc::IPV6_V6ONLY)?;
    // 和标准库开 IPv4 监听时一样，旧连接还在 TIME_WAIT 时也能重新绑定。
    set(libc::SOL_SOCKET, libc::SO_REUSEADDR)?;
    // SAFETY: 全零的 sockaddr_in6 是合法的值（`::`）；下面填上族和端口。
    let mut addr: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
    addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
    addr.sin6_port = port.to_be();
    addr.sin6_addr.s6_addr = Ipv6Addr::UNSPECIFIED.octets();
    #[cfg(target_os = "macos")]
    {
        addr.sin6_len = size_of::<libc::sockaddr_in6>() as u8;
    }
    // SAFETY: `addr` 是填好的 sockaddr_in6，长度如实给出。
    let bound = unsafe {
        libc::bind(socket.as_raw_fd(), (&raw const addr).cast(), size_of::<libc::sockaddr_in6>() as libc::socklen_t)
    };
    if bound != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 描述符开着、已经绑好。
    if unsafe { libc::listen(socket.as_raw_fd(), 128) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(TcpListener::from(socket))
}

/// 接连接的线程：`poll` 唤醒管道和各个监听，有连接就接下来起线程服务；每隔 `REVOKE_CHECK` 看一眼
/// 设备表，断开撤销了的设备。唤醒管道有动静（`Listener` 丢掉了）时结束。
fn accept_loop(shared: &Arc<Shared>, listeners: &[TcpListener], wake: &io::PipeReader) {
    let mut seen_devices = None;
    let mut checked_at = Instant::now();
    loop {
        let mut fds: Vec<libc::pollfd> = std::iter::once(wake.as_raw_fd())
            .chain(listeners.iter().map(|listener| listener.as_raw_fd()))
            .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        let tick = REVOKE_CHECK.as_millis() as libc::c_int;
        // SAFETY: `fds` 是本地的数组，长度如实给出；各个描述符在这次调用期间都开着。
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, tick) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                tracing::warn!("failed to wait for remote connections: {err}");
                thread::sleep(ACCEPT_BACKOFF);
            }
            continue;
        }
        if fds[0].revents != 0 || shared.stopping.load(Ordering::Relaxed) {
            let mut drained = [0u8; 16];
            let _ = (&*wake).read(&mut drained);
            return;
        }
        for (listener, fd) in listeners.iter().zip(&fds[1..]) {
            if fd.revents != 0 {
                accept_ready(shared, listener);
            }
        }
        if checked_at.elapsed() >= REVOKE_CHECK {
            checked_at = Instant::now();
            check_revoked(shared, &mut seen_devices);
        }
    }
}

/// 设备表变了（或者第一次看）时读一遍，断开撤销了的设备连着的连接。读不了时不断开谁。
fn check_revoked(shared: &Shared, seen: &mut Option<Option<(SystemTime, u64)>>) {
    let stamp = devices::stamp(&shared.dirs);
    if seen.as_ref() == Some(&stamp) {
        return;
    }
    match devices::list_devices(&shared.dirs) {
        Ok(list) => {
            *seen = Some(stamp);
            let known: HashSet<DeviceId> = list.iter().map(|device| device.device_id).collect();
            shared.cut_revoked(&known);
        }
        Err(err) => tracing::warn!("cannot read the paired device table: {err}"),
    }
}

/// 把 `listener` 的 backlog 里排着的连接都接下来，各起一个线程服务。
fn accept_ready(shared: &Arc<Shared>, listener: &TcpListener) {
    loop {
        let (tcp, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::warn!("failed to accept a remote connection: {err}");
                thread::sleep(ACCEPT_BACKOFF);
                return;
            }
        };
        // IPv4 映射成的 IPv6 地址按 IPv4 算，限速才认得出是同一个地址。
        let ip = match peer.ip() {
            IpAddr::V6(v6) => v6.to_canonical(),
            ip => ip,
        };
        if let Err(err) = tcp.set_nonblocking(false) {
            tracing::warn!("failed to set up a remote connection: {err}");
            continue;
        }
        let Some((id, cut)) = register(shared, &tcp) else {
            tracing::warn!("too many remote connections, dropping one from {ip}");
            continue;
        };
        let thread_shared = shared.clone();
        let spawned = thread::Builder::new().name("remote-connection".into()).spawn(move || {
            gate::serve(&thread_shared, id, &tcp, ip, &cut);
            thread_shared.connections().remove(&id);
        });
        if let Err(err) = spawned {
            tracing::warn!("failed to start a remote connection thread: {err}");
            shared.connections().remove(&id);
        }
    }
}

/// 登记一条新连接，返回它的编号和断开它的开关；连接太多时返回 `None`。
fn register(shared: &Shared, tcp: &TcpStream) -> Option<(u64, Arc<AtomicBool>)> {
    let mut connections = shared.connections();
    let gating = connections.values().filter(|live| live.device.is_none()).count();
    if gating >= MAX_GATING || connections.len() >= MAX_CONNECTIONS {
        return None;
    }
    let tcp = tcp.try_clone().ok()?;
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let cut = Arc::new(AtomicBool::new(false));
    connections.insert(id, Live { tcp, device: None, cut: cut.clone() });
    Some((id, cut))
}
