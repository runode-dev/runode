//! 看一台 SSH 主机的远程桌面：经主连接起那边的 `runode --ssh-screen`，ssh 的 stdin、stdout 上说
//! `runode_protocol::screen`，和手机经远程访问说的一样。主机连上以后才起：远端已经装好这个构建；没有
//! 主连接（Windows）时这条 ssh 自己认证，要问用户时经 askpass 问。
//!
//! 连上就提出画面改走 UDP（`Datagram`）：远端答应以后开 `runode_remote::datagram::Receiver` 收，UDP 上来了画面
//! 以后 ssh 上还在路上的旧画面都丢掉，直到远端说 UDP 断了（`DatagramLost`）。两边交来的都排进一个线程按到的
//! 先后交出去，解码器只在那个线程上用。

use std::{
    io::{BufReader, BufWriter, Write as _},
    net::SocketAddr,
    process::{Child, Stdio},
    sync::{Arc, Condvar, Mutex, PoisonError, atomic::Ordering, mpsc},
    thread,
};

use runode_protocol::{
    Frame, FrameKind, read_frame,
    screen::{SCREEN_VERSION, ScreenClientMsg, ScreenHostMsg, decode_video},
    write_frame,
};
use runode_remote::datagram;
use runode_types::screen::VideoFrame;

use super::{RemoteHost, Status, deploy, ssh};

/// 远端交来的东西，在读线程上交出。
pub enum ScreenEvent {
    Message(ScreenHostMsg),
    Video(VideoFrame),
    /// 连接断了（ssh 退出、远端断开或者发来读不懂的帧），之后不会再有事件。
    Closed,
}

/// 往远端发消息的一头，可以复制给别的线程用；连接断了以后发的消息丢掉。
#[derive(Clone)]
pub struct ScreenSender(mpsc::Sender<ScreenClientMsg>);

impl ScreenSender {
    pub fn send(&self, message: ScreenClientMsg) {
        let _ = self.0.send(message);
    }
}

/// 一条远程桌面的连接。丢掉时结束远端的桥接和 ssh。
pub struct ScreenLink {
    sender: ScreenSender,
    child: Arc<Mutex<Option<Child>>>,
    /// 画面走 UDP 以后它的统计。
    datagram: Arc<Mutex<Option<Arc<datagram::Stats>>>>,
}

impl ScreenLink {
    pub fn sender(&self) -> ScreenSender {
        self.sender.clone()
    }

    /// 画面走 UDP 时到现在一共收到、丢了多少片；没走 UDP 时为空。
    pub fn datagram_counts(&self) -> Option<(u64, u64)> {
        let stats = self.datagram.lock().unwrap_or_else(PoisonError::into_inner);
        stats.as_ref().map(|stats| (stats.received.load(Ordering::Relaxed), stats.lost.load(Ordering::Relaxed)))
    }
}

/// ssh 上读进来、交出线程还没取走的帧最多这么多：再多读线程就停下不读，画面堆在 ssh 和远端的缓冲里，远端据此
/// 丢帧，不在这里越排越久。
const MAX_BACKLOG: usize = 4;

/// ssh 上读进来、交出线程还没取走的帧数，和交出线程是不是已经结束了。
#[derive(Default)]
struct Backlog {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Backlog {
    /// 等到排得下（`MAX_BACKLOG`）再记上一帧；交出线程已经结束时返回假。
    fn admit(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut state = self
            .changed
            .wait_while(state, |(queued, closed)| *queued >= MAX_BACKLOG && !*closed)
            .unwrap_or_else(PoisonError::into_inner);
        if state.1 {
            return false;
        }
        state.0 += 1;
        true
    }

    /// 交出线程取走了一帧。
    fn taken(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = state.0.saturating_sub(1);
        self.changed.notify_all();
    }

    /// 交出线程结束了，读线程不再等。
    fn close(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.changed.notify_all();
    }
}

/// 排进交出线程的：ssh 上来的帧和 UDP 上来的。
enum Arrival {
    Stream(Frame),
    Datagram(datagram::Event),
    Closed,
}

impl Drop for ScreenLink {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// 连上 `host` 的远程桌面，先发 `ScreenHello`；远端交来的消息和画面经 `on_event` 在同一个后台线程上按到的先后
/// 交出，最后一个是 `ScreenEvent::Closed`，同时交给它往远端发消息的一头（解不了的画面当场要关键帧）。会阻塞着起
/// ssh，在后台线程上调。
pub fn open(
    host: &RemoteHost,
    on_event: impl FnMut(ScreenEvent, &ScreenSender) + Send + 'static,
) -> Result<ScreenLink, String> {
    if !matches!(host.status(), Status::Connected(_)) {
        return Err(format!("not connected to {}", host.name));
    }
    let script = host.family().run_command(&deploy::install_dir(), runode_ssh::bootstrap::Entry::Screen);
    let mut command = host.exec(&ssh(), &script, true, false);
    command.stderr(Stdio::null());
    let pipes = runode_ssh::process::Pipes { stdin: true, stdout: true, stderr: false };
    let piped = runode_ssh::process::spawn(&mut command, pipes).map_err(|err| format!("cannot run ssh: {err}"))?;
    let (Some(input), Some(output)) = (piped.stdin, piped.stdout) else {
        return Err("ssh gave no pipes".into());
    };
    let child = Arc::new(Mutex::new(Some(piped.child)));

    let (tx, rx) = mpsc::channel::<ScreenClientMsg>();
    let _ = tx.send(ScreenClientMsg::ScreenHello { version: SCREEN_VERSION });
    // 收 UDP 的套接字先开好，远端在 `Ready` 里说认得时把端口和密钥交给它；开不了就只走 ssh。
    let receiver = match datagram::Receiver::bind() {
        Ok(receiver) => Some(receiver),
        Err(err) => {
            tracing::info!("the remote desktop of {} stays on ssh: {err}", host.name);
            None
        }
    };
    thread::Builder::new()
        .name("screen-writer".into())
        .spawn(move || {
            let mut input = BufWriter::new(input);
            for message in rx {
                let Ok(frame) = Frame::control(&message) else { continue };
                if write_frame(&mut input, FrameKind::Control, 0, &frame.payload).is_err() || input.flush().is_err() {
                    return;
                }
            }
        })
        .map_err(|err| format!("cannot start a thread: {err}"))?;

    let sender = ScreenSender(tx);
    let (arrivals, arrived) = mpsc::channel::<Arrival>();
    let stats: Arc<Mutex<Option<Arc<datagram::Stats>>>> = Arc::default();
    let name = host.name.clone();
    let dispatch_sender = sender.clone();
    let dispatch_stats = stats.clone();
    let udp_arrivals = arrivals.clone();
    let backlog = Arc::new(Backlog::default());
    let dispatch_backlog = backlog.clone();
    thread::Builder::new()
        .name("screen-events".into())
        .spawn(move || {
            let link = Dispatch {
                name,
                sender: dispatch_sender,
                arrivals: udp_arrivals,
                stats: dispatch_stats,
                backlog: dispatch_backlog,
            };
            link.run(&arrived, receiver, on_event);
        })
        .map_err(|err| format!("cannot start a thread: {err}"))?;
    let name = host.name.clone();
    thread::Builder::new()
        .name("screen-reader".into())
        .spawn(move || {
            let mut output = BufReader::with_capacity(256 * 1024, output);
            loop {
                match read_frame(&mut output) {
                    Ok(Some(frame)) => {
                        if !backlog.admit() || arrivals.send(Arrival::Stream(frame)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        tracing::info!("the remote desktop of {name} closed: {err}");
                        break;
                    }
                }
            }
            let _ = arrivals.send(Arrival::Closed);
        })
        .map_err(|err| format!("cannot start a thread: {err}"))?;

    Ok(ScreenLink { sender, child, datagram: stats })
}

/// 交出线程手里的东西。
struct Dispatch {
    name: String,
    sender: ScreenSender,
    /// UDP 上来的也排进同一个队列。
    arrivals: mpsc::Sender<Arrival>,
    stats: Arc<Mutex<Option<Arc<datagram::Stats>>>>,
    backlog: Arc<Backlog>,
}

/// 交出线程 panic 退栈时也放开等着的读线程。
impl Drop for Dispatch {
    fn drop(&mut self) {
        self.backlog.close();
    }
}

/// 画面的 UDP 走到哪一步了。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Udp {
    /// 还没从 UDP 上收到画面，画面走 ssh。
    Waiting,
    /// 画面走 UDP，ssh 上来的画面是换路之前的。
    Active,
    /// 远端说 UDP 断了，画面回到 ssh，UDP 上排着没交的不要了。
    Closed,
}

impl Dispatch {
    /// 按到的先后交出 ssh 和 UDP 上来的东西，到连接断了为止，最后交出 `ScreenEvent::Closed`。
    fn run(
        &self,
        arrived: &mpsc::Receiver<Arrival>,
        mut receiver: Option<datagram::Receiver>,
        mut on_event: impl FnMut(ScreenEvent, &ScreenSender),
    ) {
        let mut receiving: Option<datagram::Receiving> = None;
        let mut udp = Udp::Waiting;
        for arrival in arrived {
            if matches!(arrival, Arrival::Stream(_)) {
                self.backlog.taken();
            }
            let event = match arrival {
                Arrival::Closed => break,
                Arrival::Stream(frame) => match frame.kind {
                    FrameKind::Control => match frame.message::<ScreenHostMsg>() {
                        Ok(ScreenHostMsg::Datagram { port, addrs }) => {
                            if let Some(receiver) = receiver.take() {
                                receiving = self.receive(receiver, port, &addrs);
                            }
                            continue;
                        }
                        Ok(ready @ ScreenHostMsg::Ready { datagram: true, .. }) => {
                            if let Some(receiver) = &receiver {
                                self.sender.send(ScreenClientMsg::Datagram {
                                    key: receiver.key().to_hex(),
                                    port: receiver.port(),
                                    addrs: receiver.candidates(),
                                });
                            }
                            ScreenEvent::Message(ready)
                        }
                        Ok(ScreenHostMsg::DatagramLost) => {
                            tracing::info!("the remote desktop of {} is back on ssh", self.name);
                            udp = Udp::Closed;
                            receiving = None;
                            *self.stats.lock().unwrap_or_else(PoisonError::into_inner) = None;
                            continue;
                        }
                        Ok(message) => ScreenEvent::Message(message),
                        // 更新的远端多出来的消息不认得，跳过。
                        Err(err) => {
                            tracing::debug!("unknown remote desktop message from {}: {err}", self.name);
                            continue;
                        }
                    },
                    FrameKind::Video if udp == Udp::Active => continue,
                    FrameKind::Video => match decode_video(&frame.payload) {
                        Some(video) => ScreenEvent::Video(video),
                        None => {
                            tracing::warn!("bad video frame from {}", self.name);
                            break;
                        }
                    },
                    _ => continue,
                },
                Arrival::Datagram(_) if udp == Udp::Closed => continue,
                Arrival::Datagram(datagram::Event::Frame(payload)) => {
                    if udp == Udp::Waiting {
                        tracing::info!("the remote desktop of {} comes over datagrams", self.name);
                        udp = Udp::Active;
                    }
                    match decode_video(&payload) {
                        Some(video) => ScreenEvent::Video(video),
                        None => {
                            tracing::warn!("bad video datagram from {}", self.name);
                            continue;
                        }
                    }
                }
                Arrival::Datagram(datagram::Event::Gap) => {
                    self.sender.send(ScreenClientMsg::Keyframe);
                    continue;
                }
            };
            on_event(event, &self.sender);
        }
        self.backlog.close();
        drop(receiving);
        on_event(ScreenEvent::Closed, &self.sender);
    }

    /// 远端答应了走 UDP：往它给的地址打洞、收画面。
    fn receive(
        &self,
        receiver: datagram::Receiver,
        port: u16,
        addrs: &[std::net::IpAddr],
    ) -> Option<datagram::Receiving> {
        let candidates = addrs.iter().map(|&addr| SocketAddr::new(addr, port)).collect();
        let arrivals = self.arrivals.clone();
        match receiver.start(candidates, move |event| {
            let _ = arrivals.send(Arrival::Datagram(event));
        }) {
            Ok(receiving) => {
                *self.stats.lock().unwrap_or_else(PoisonError::into_inner) = Some(receiving.stats());
                Some(receiving)
            }
            Err(err) => {
                tracing::info!("the remote desktop of {} stays on ssh: {err}", self.name);
                None
            }
        }
    }
}
