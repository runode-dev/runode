//! 远程访问的客户端：这台电脑配对、连上别的电脑上开着远程访问的 Runode，把它当远程主机用。线上格式见
//! `runode_protocol::remote`，配对时报 `DeviceKind::Desktop`。
//!
//! 配对过的电脑记在 `Dirs::paired_machines_file`（`PairedMachine`），每台一把自己的设备私钥。`dial` 过了
//! 门禁后交回一条本机的 socket：TLS 由这里的线程在另一头搬，调用方在 socket 上发第一帧（终端的 `Hello`、
//! `ScreenHello` 或 `ChannelHello`），之后照普通连接说话。
//!
//! 连配对过的电脑时先单独试一个首选的地址（上次连上的，或调用方给的），它在 `PREFERRED_HEAD_START` 里过了
//! 门禁就用它，Bonjour、iroh 都不开。它失败了或者没来得及，几条路同时走（`race`）：直连的地址错开一点先后
//! 一个个开（前一个失败了马上开下一个），死地址不拖着后面的；Bonjour 按指纹找到的地址找到了再加进来；有跨
//! 网络的地址时稍等一下再开 iroh，局域网通的时候用不上它。先过门禁的那条胜出（还没完的首选地址也算一条），
//! 之后才开、才到的关掉。

mod handshake;
mod pump;
mod store;
mod tunnel;

use std::{
    collections::HashSet,
    io,
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

#[cfg(windows)]
use uds_windows::UnixStream;

use ring::signature::EcdsaKeyPair;
use runode_paths::Dirs;
use runode_protocol::remote::{DeviceId, FINGERPRINT_LEN, PairingUri, RejectReason, SECRET_LEN};

use self::{handshake::Gated, store::Entry};
use crate::{files::random, now_unix};

/// 配对过的一台电脑。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedMachine {
    /// 这边给它起的标识（32 个小写十六进制数字），配对时定下，桌面的存档和主机列表按它认。
    pub id: String,
    /// 那台电脑报的主机名（`RemoteChallenge::host_name`），每次连上时更新。
    pub host_name: String,
    pub fingerprint: [u8; FINGERPRINT_LEN],
    pub port: u16,
    /// 配对链接里的地址。
    pub addrs: Vec<IpAddr>,
    /// 上次连上的地址，下次先试。
    pub last_addr: Option<SocketAddr>,
    /// 跨网络连接的地址，连上时按 `RemoteChallenge::node` 更新。
    pub node: Option<String>,
    /// 那台电脑分给这边的设备标识。
    pub device_id: DeviceId,
    /// 配对的时刻，Unix 秒。
    pub paired_at: u64,
}

/// 配对过的电脑，按配对的先后。读不了文件时为空。
pub fn list_machines(dirs: &Dirs) -> Vec<PairedMachine> {
    match store::list(dirs) {
        Ok(entries) => entries.into_iter().map(|entry| entry.machine).collect(),
        Err(err) => {
            tracing::warn!("cannot read the paired computers: {err}");
            Vec::new()
        }
    }
}

/// 标识是 `id` 的那台。
pub fn find_machine(dirs: &Dirs, id: &str) -> Option<PairedMachine> {
    store::find(dirs, id).ok().flatten().map(|entry| entry.machine)
}

/// 忘掉 `id` 那台：删掉记录和设备私钥。不通知那台电脑，要它也忘掉这边，先连上发 `ClientMsg::Unpair`。
pub fn forget_machine(dirs: &Dirs, id: &str) -> io::Result<()> {
    store::forget(dirs, id).map(drop)
}

/// 连不上、或者门禁没过。
#[derive(Debug)]
pub enum DialError {
    /// 那台电脑拒绝了：`UnknownDevice` 说明那边已经撤销了这台设备，该提示重新配对。
    Rejected(RejectReason),
    /// 哪个地址都连不上（不在线、不在同一网络又没开跨网络连接）。`last` 是最后一个尝试的错误。
    Unreachable {
        last: String,
    },
    /// 证书指纹对不上：不是配对时那台，或者那边重新生成了证书。
    FingerprintMismatch,
    /// 配对链接过期了。
    Expired,
    /// 对方说的不是这套协议，或者门禁版本不认识。
    Protocol(String),
    Io(io::Error),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(RejectReason::UnknownDevice) => write!(f, "the computer no longer knows this device"),
            Self::Rejected(RejectReason::BadSignature) => write!(f, "the computer did not accept this device's key"),
            Self::Rejected(RejectReason::PairingInvalid) => write!(f, "the pairing code is wrong or already used"),
            Self::Rejected(RejectReason::RateLimited) => write!(f, "too many failed attempts, try again later"),
            Self::Rejected(RejectReason::Disabled) => write!(f, "remote access on the computer is unavailable"),
            Self::Rejected(RejectReason::Unknown) => write!(f, "the computer refused the connection"),
            Self::Unreachable { last } => write!(f, "cannot reach the computer: {last}"),
            Self::FingerprintMismatch => write!(f, "the computer's certificate is not the paired one"),
            Self::Expired => write!(f, "the pairing link has expired"),
            Self::Protocol(message) => write!(f, "the computer does not speak runode remote access: {message}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DialError {}

impl From<io::Error> for DialError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// 一条过了门禁的连接。
pub struct Dialed {
    /// 调用方这一头；另一头由这里的线程接到 TLS 上，哪边关了另一边跟着关。
    pub stream: UnixStream,
    /// 这次连的是谁（存下的记录，已经按这次的 `RemoteChallenge` 和地址更新过）。
    pub machine: PairedMachine,
    /// 这次走的是哪个地址；经跨网络连接时为空。
    pub addr: Option<SocketAddr>,
}

/// 用配对链接 `uri` 配对，这边叫 `device_name`。链接里的地址、局域网里 Bonjour 找到的同一指纹的那台、
/// （链接带 `node` 时）跨网络连接一起连，配对口令只能用一次，所以只拿最先握完手、收到 `RemoteChallenge`
/// 的那条去配对，别的关掉。成功后记下这台电脑，返回过了门禁的连接（同 `dial`）。
pub fn pair(dirs: &Dirs, uri: &PairingUri, device_name: &str) -> Result<Dialed, DialError> {
    if now_unix() >= uri.expires_at {
        return Err(DialError::Expired);
    }
    let pkcs8 = handshake::generate_key()?;
    let race = Arc::new(Race::new(
        uri.fingerprint,
        handshake::load_key(&pkcs8)?,
        Gate::Pair { secret: uri.secret, name: device_name.into(), claimed: AtomicBool::new(false) },
    ));
    let direct = uri.addrs.iter().map(|ip| SocketAddr::new(*ip, uri.port)).collect();
    let won = race.run(None, direct, uri.node.clone()).map_err(|err| match err {
        DialError::Rejected(RejectReason::PairingInvalid) if now_unix() >= uri.expires_at => DialError::Expired,
        err => err,
    })?;
    let id: String = random::<16>()?.iter().map(|b| format!("{b:02x}")).collect();
    let machine = PairedMachine {
        id,
        host_name: won.host_name.clone(),
        fingerprint: uri.fingerprint,
        port: uri.port,
        addrs: uri.addrs.clone(),
        last_addr: won.addr,
        node: won.node.clone().or_else(|| uri.node.clone()),
        device_id: won.gated.device_id,
        paired_at: now_unix(),
    };
    store::add(dirs, Entry { machine: machine.clone(), key: pkcs8 })?;
    tracing::info!("paired with {} ({})", machine.host_name, machine.id);
    won.hand_over(machine)
}

/// 连配对过的 `id` 那台，先单独试上次连上的地址（`PairedMachine::last_addr`）。
pub fn dial(dirs: &Dirs, id: &str) -> Result<Dialed, DialError> {
    dial_via(dirs, id, None)
}

/// 连配对过的 `id` 那台：先单独试 `prefer`（为空时是上次连上的地址），通常一次 TCP、TLS 和门禁的来回就连上；
/// 它在 `PREFERRED_HEAD_START` 里没过门禁时，上次的地址和配对链接里的地址、Bonjour 找到的地址、跨网络连接同时
/// 试，先过门禁的那条胜出，别的关掉，首选的地址不通最多多花 `PREFERRED_HEAD_START`。整个过程最长约
/// `DIAL_TIMEOUT`。已经有一条连接连着那台电脑时把它连上的地址（`Dialed::addr`）给 `prefer`，另开的连接走同一处。
pub fn dial_via(dirs: &Dirs, id: &str, prefer: Option<SocketAddr>) -> Result<Dialed, DialError> {
    let Entry { machine, key } = store::find(dirs, id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no paired computer {id}")))?;
    let race = Arc::new(Race::new(machine.fingerprint, handshake::load_key(&key)?, Gate::Auth(machine.device_id)));
    let mut direct: Vec<SocketAddr> = machine.last_addr.into_iter().collect();
    for addr in machine.addrs.iter().map(|ip| SocketAddr::new(*ip, machine.port)) {
        if !direct.contains(&addr) {
            direct.push(addr);
        }
    }
    let won = race.run(prefer.or(machine.last_addr), direct, machine.node.clone())?;
    let recorded = store::connected(dirs, id, &won.host_name, won.node.as_deref(), won.addr).unwrap_or_else(|err| {
        tracing::warn!("cannot record how {id} was reached: {err}");
        None
    });
    let machine = recorded.unwrap_or_else(|| PairedMachine {
        host_name: won.host_name.clone(),
        node: won.node.clone(),
        last_addr: won.addr.or(machine.last_addr),
        ..machine
    });
    won.hand_over(machine)
}

/// `dial` 最长等多久。
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// 一个直连的地址从开始连到过门禁最多等多久。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
/// 直连的地址前一个开了这么久还没结果就开下一个。
const STAGGER: Duration = Duration::from_millis(250);
/// 首选的地址单独连这么久还没过门禁，就把别的路也开起来。局域网里一次 TCP、TLS 和门禁的来回远短于它。
const PREFERRED_HEAD_START: Duration = Duration::from_millis(250);
/// Bonjour 最多找这么久。
const BROWSE_WAIT: Duration = Duration::from_secs(2);
/// 跨网络连接晚这么久再开：局域网通的时候先过门禁，用不上它。
const TUNNEL_DELAY: Duration = Duration::from_millis(500);

/// 局域网里 Bonjour 公布的一台开着远程访问的电脑。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// 公布的实例名（那台电脑的主机名）。
    pub name: String,
    pub fingerprint: [u8; FINGERPRINT_LEN],
    pub addrs: Vec<IpAddr>,
    pub port: u16,
}

/// 在局域网里找开着远程访问的电脑，`changed` 在找到的列表变了时收到当前完整的列表（在后台线程里调）。
/// 丢掉返回值就停。
pub fn browse(changed: impl Fn(Vec<Found>) + Send + 'static) -> io::Result<Browser> {
    crate::bonjour::browse(changed).map(|browsing| Browser { _browsing: browsing })
}

/// `browse` 开着的查找，丢掉就停。
pub struct Browser {
    _browsing: crate::bonjour::Browsing,
}

/// 门禁时发什么。
enum Gate {
    Auth(DeviceId),
    /// `claimed` 在第一条连接拿口令去配对时置上，之后到的不再用它。
    Pair {
        secret: [u8; SECRET_LEN],
        name: String,
        claimed: AtomicBool,
    },
}

/// 一次 `dial` 或 `pair` 里几条路共用的。
struct Race {
    fingerprint: [u8; FINGERPRINT_LEN],
    key: EcdsaKeyPair,
    gate: Gate,
    deadline: Instant,
    /// 有一条胜出了，或者整个放弃了：还没开的不再开，还没到门禁的到了也不再往下走。
    over: AtomicBool,
    /// 开过的直连地址，同一个地址不连两次。
    tried: Mutex<HashSet<SocketAddr>>,
}

/// 过了门禁的那条。
struct Won {
    gated: Gated,
    host_name: String,
    node: Option<String>,
    /// 直连的地址；经跨网络连接时为空。
    addr: Option<SocketAddr>,
}

impl Won {
    fn hand_over(self, machine: PairedMachine) -> Result<Dialed, DialError> {
        let stream = pump::spawn(self.gated.conn, self.gated.tcp)?;
        Ok(Dialed { stream, machine, addr: self.addr })
    }
}

/// 几条路报给 `Race::run` 的。每个来源开头算一个、结束时报 `SourceDone`；每开一次尝试先报 `Started`，
/// 尝试结束报 `Finished`（输掉了、没往下走的是 `None`）。都结束了 `run` 就不再等。
enum Event {
    Started,
    Finished(Option<Result<Box<Won>, DialError>>),
    SourceDone,
}

/// `Race::run` 的一个来源，在自己的线程里跑。
type Source = Box<dyn FnOnce(&Arc<Race>, &mpsc::Sender<Event>) + Send>;

/// 怎么连到那台电脑。
#[derive(Clone)]
enum Route {
    Direct(SocketAddr),
    Tunnel(String),
}

impl Race {
    fn new(fingerprint: [u8; FINGERPRINT_LEN], key: EcdsaKeyPair, gate: Gate) -> Self {
        Self {
            fingerprint,
            key,
            gate,
            deadline: Instant::now() + DIAL_TIMEOUT,
            over: AtomicBool::new(false),
            tried: Mutex::new(HashSet::new()),
        }
    }

    /// 有 `prefer` 时先单独连它，`PREFERRED_HEAD_START` 里过了门禁就用它；它失败了或没来得及，直连 `direct`、
    /// Bonjour 和（有 `node` 时）跨网络连接一起走，返回先过门禁的那条。被拒（限速除外：按来源算，别的路未必被限）
    /// 马上结束；都失败时按限速、指纹不对、协议不对、连不上的先后报一个。
    fn run(
        self: &Arc<Self>,
        prefer: Option<SocketAddr>,
        direct: Vec<SocketAddr>,
        node: Option<String>,
    ) -> Result<Won, DialError> {
        let result = self.run_inner(prefer, direct, node);
        self.over.store(true, Ordering::Relaxed);
        result
    }

    fn run_inner(
        self: &Arc<Self>,
        prefer: Option<SocketAddr>,
        direct: Vec<SocketAddr>,
        node: Option<String>,
    ) -> Result<Won, DialError> {
        let (tx, rx) = mpsc::channel();
        let mut pending = 0;
        let mut failures = Failures::default();
        if let Some(addr) = prefer {
            self.tried.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(addr);
            let (race, events) = (self.clone(), tx.clone());
            let spawned = thread::Builder::new().name("remote-dial-preferred".into()).spawn(move || {
                let _ = events.send(Event::Finished(race.attempt(&Route::Direct(addr))));
            });
            match spawned {
                Ok(_) => pending += 1,
                Err(err) => tracing::warn!("cannot start dialing {addr}: {err}"),
            }
            let head_start = (Instant::now() + PREFERRED_HEAD_START).min(self.deadline);
            if let Some(result) = self.collect(&rx, &mut pending, &mut failures, head_start) {
                return result;
            }
        }
        let mut source = |name: &str, body: Source| {
            let (race, tx) = (self.clone(), tx.clone());
            let spawned = thread::Builder::new().name(format!("remote-dial-{name}")).spawn(move || {
                body(&race, &tx);
                let _ = tx.send(Event::SourceDone);
            });
            match spawned {
                Ok(_) => pending += 1,
                Err(err) => tracing::warn!("cannot start dialing {name}: {err}"),
            }
        };
        let direct = direct.into_iter().map(Route::Direct).collect();
        source("direct", Box::new(move |race, tx| race.stagger(tx, direct)));
        source("bonjour", Box::new(|race, tx| race.bonjour(tx)));
        if let Some(node) = node {
            source(
                "tunnel",
                Box::new(move |race, tx| {
                    thread::sleep(TUNNEL_DELAY);
                    if !race.over.load(Ordering::Relaxed) {
                        let _ = tx.send(Event::Started);
                        let _ = tx.send(Event::Finished(race.attempt(&Route::Tunnel(node))));
                    }
                }),
            );
        }
        drop(tx);
        let deadline = self.deadline;
        self.collect(&rx, &mut pending, &mut failures, deadline).unwrap_or_else(|| Err(failures.worst()))
    }

    /// 收各条路的结果，直到有了定论（胜出的、被拒的）、`pending` 条都结束了或到了 `until`；后两种为 `None`，没
    /// 胜出的失败记在 `failures` 里。
    fn collect(
        &self,
        rx: &mpsc::Receiver<Event>,
        pending: &mut usize,
        failures: &mut Failures,
        until: Instant,
    ) -> Option<Result<Won, DialError>> {
        while *pending > 0 {
            let left = until.saturating_duration_since(Instant::now());
            let Ok(event) = rx.recv_timeout(left) else { break };
            match event {
                Event::Started => *pending += 1,
                Event::SourceDone | Event::Finished(None) => *pending -= 1,
                Event::Finished(Some(Ok(won))) => return Some(Ok(*won)),
                Event::Finished(Some(Err(err))) => {
                    *pending -= 1;
                    if let DialError::Rejected(reason) = err
                        && reason != RejectReason::RateLimited
                    {
                        return Some(Err(err));
                    }
                    failures.note(err);
                }
            }
        }
        None
    }

    /// 按先后一个个开 `routes`：前一个失败了，或者开了 `STAGGER` 还没结果，就开下一个。
    fn stagger(self: &Arc<Self>, tx: &mpsc::Sender<Event>, routes: Vec<Route>) {
        let (failed, failures) = mpsc::channel();
        for route in routes {
            if self.over.load(Ordering::Relaxed) {
                return;
            }
            if let Route::Direct(addr) = route
                && !self.tried.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(addr)
            {
                continue;
            }
            let _ = tx.send(Event::Started);
            let (race, events, failed) = (self.clone(), tx.clone(), failed.clone());
            let spawned = thread::Builder::new().name("remote-dial".into()).spawn(move || {
                let outcome = race.attempt(&route);
                if !matches!(outcome, Some(Ok(_))) {
                    let _ = failed.send(());
                }
                let _ = events.send(Event::Finished(outcome));
            });
            if let Err(err) = spawned {
                let _ = tx.send(Event::Finished(Some(Err(err.into()))));
                continue;
            }
            let _ = failures.recv_timeout(STAGGER);
        }
    }

    /// 用 Bonjour 找指纹相同的那台，最多找 `BROWSE_WAIT`，找到了按它公布的地址连。
    fn bonjour(self: &Arc<Self>, tx: &mpsc::Sender<Event>) {
        let (found_tx, found) = mpsc::channel();
        let fingerprint = self.fingerprint;
        let browsing = crate::bonjour::browse(move |list| {
            if let Some(machine) = list.into_iter().find(|machine| machine.fingerprint == fingerprint) {
                let _ = found_tx.send(machine);
            }
        });
        let browsing = match browsing {
            Ok(browsing) => browsing,
            Err(err) => return tracing::debug!("cannot browse for remote access: {err}"),
        };
        let wait = BROWSE_WAIT.min(self.deadline.saturating_duration_since(Instant::now()));
        let machine = found.recv_timeout(wait);
        drop(browsing);
        if let Ok(machine) = machine {
            let routes = machine.addrs.iter().map(|ip| Route::Direct(SocketAddr::new(*ip, machine.port))).collect();
            self.stagger(tx, routes);
        }
    }

    /// 沿 `route` 连、握手、过门禁。已经有别的胜出了、或者配对的口令已经交给了别的连接时为 `None`。
    fn attempt(&self, route: &Route) -> Option<Result<Box<Won>, DialError>> {
        let tcp = match route {
            Route::Direct(addr) => {
                let until = (Instant::now() + CONNECT_TIMEOUT).min(self.deadline);
                connect(*addr, until).map(|tcp| (tcp, until))
            }
            Route::Tunnel(node) => tunnel::open(node, self.deadline).map(|tcp| (tcp, self.deadline)),
        };
        let challenged = match tcp.and_then(|(tcp, until)| handshake::open(tcp, &self.fingerprint, until)) {
            Ok(challenged) => challenged,
            Err(err) => return Some(Err(at(route, err))),
        };
        if self.over.load(Ordering::Relaxed) {
            return None;
        }
        let (host_name, node) = (challenged.host_name.clone(), challenged.node.clone());
        let gated = match &self.gate {
            Gate::Auth(device_id) => challenged.auth(*device_id, &self.key),
            Gate::Pair { claimed, .. } if claimed.swap(true, Ordering::Relaxed) => return None,
            Gate::Pair { secret, name, .. } => challenged.pair(secret, name, &self.key),
        };
        let addr = match route {
            Route::Direct(addr) => Some(*addr),
            Route::Tunnel(_) => None,
        };
        Some(gated.map(|gated| Box::new(Won { gated, host_name, node, addr })).map_err(|err| at(route, err)))
    }
}

/// 直连 `addr`，最晚到 `until`。
fn connect(addr: SocketAddr, until: Instant) -> Result<TcpStream, DialError> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(DialError::Unreachable { last: "timed out".into() });
    }
    let tcp =
        TcpStream::connect_timeout(&addr, left).map_err(|err| DialError::Unreachable { last: err.to_string() })?;
    let _ = tcp.set_nodelay(true);
    if let Err(err) = crate::gate::keep_alive(&tcp) {
        tracing::warn!("cannot turn on keepalive for a remote connection: {err}");
    }
    Ok(tcp)
}

/// 连不上的错误前面标上是哪条路。
fn at(route: &Route, err: DialError) -> DialError {
    match err {
        DialError::Unreachable { last } => DialError::Unreachable {
            last: match route {
                Route::Direct(addr) => format!("{addr}: {last}"),
                Route::Tunnel(_) => last,
            },
        },
        DialError::Io(err) => DialError::Unreachable { last: err.to_string() },
        err => err,
    }
}

/// 没胜出的几条各自的失败，最后挑一个报。
#[derive(Default)]
struct Failures {
    rate_limited: bool,
    mismatch: bool,
    protocol: Option<String>,
    last: Option<String>,
}

impl Failures {
    fn note(&mut self, err: DialError) {
        match err {
            DialError::Rejected(_) => self.rate_limited = true,
            DialError::FingerprintMismatch => self.mismatch = true,
            DialError::Protocol(message) => self.protocol = Some(message),
            DialError::Unreachable { last } => self.last = Some(last),
            DialError::Expired => self.last = Some("expired".into()),
            DialError::Io(err) => self.last = Some(err.to_string()),
        }
    }

    fn worst(self) -> DialError {
        if self.rate_limited {
            DialError::Rejected(RejectReason::RateLimited)
        } else if self.mismatch {
            DialError::FingerprintMismatch
        } else if let Some(message) = self.protocol {
            DialError::Protocol(message)
        } else {
            DialError::Unreachable { last: self.last.unwrap_or_else(|| "timed out".into()) }
        }
    }
}
