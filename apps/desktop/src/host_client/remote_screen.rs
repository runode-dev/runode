//! 看一台远端主机的远程桌面，说 `runode_protocol::screen`，和手机经远程访问说的一样。主机连上以后才起。SSH
//! 主机经主连接起那边的 `runode --ssh-screen`，在 ssh 的 stdin、stdout 上说：远端已经装好这个构建；没有主连接
//! （Windows）时这条 ssh 自己认证，要问用户时经 askpass 问。配对的电脑另拨一条过了门禁的连接，在上面说。
//!
//! 连上就提出画面改走 UDP（`Datagram`）：远端答应以后开 `runode_remote::datagram::Receiver` 收，UDP 上来了画面
//! 以后连接上还在路上的旧画面都丢掉，直到远端说 UDP 断了（`DatagramLost`）。两边交来的都排进一个线程按到的
//! 先后交出去，解码器只在那个线程上用。

use std::{
    io::{BufReader, BufWriter, Read, Write},
    net::{Shutdown, SocketAddr},
    process::Child,
    sync::{Arc, Condvar, Mutex, PoisonError, atomic::Ordering, mpsc},
    thread,
    time::Duration,
};

use runode_protocol::{
    Frame, FrameKind, read_frame,
    screen::{SCREEN_VERSION, ScreenClientMsg, ScreenHostMsg, decode_video},
    write_frame,
};
use runode_remote::datagram;
use runode_types::screen::VideoFrame;

use super::{
    UnixStream,
    hosts::{HostId, Status},
    paired,
    ssh::{self, deploy},
};

/// 远端交来的东西，在读线程上交出。
pub enum ScreenEvent {
    Message(ScreenHostMsg),
    Video(VideoFrame),
    /// 连接断了（ssh 退出、远端断开或者发来读不懂的帧），之后不会再有事件。还没交出过别的事件就断了时，
    /// `detail` 是 ssh 在 stderr 上最后说的几行（`Tail::text`），远端的桥接没起来时只有它说得出为什么；交出过
    /// 事件以后断的、配对的电脑和 ssh 什么也没说的为空。
    Closed {
        detail: Option<String>,
    },
}

/// 往远端发消息的一头，可以复制给别的线程用；连接断了以后发的消息丢掉。
#[derive(Clone)]
pub struct ScreenSender(mpsc::Sender<ScreenClientMsg>);

impl ScreenSender {
    pub fn send(&self, message: ScreenClientMsg) {
        let _ = self.0.send(message);
    }
}

/// 一条远程桌面的连接。丢掉时断开：结束远端的桥接和 ssh，或者关掉到配对的电脑的那条连接。
pub struct ScreenLink {
    sender: ScreenSender,
    transport: Option<Transport>,
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

/// ssh 的 stderr 留下最后这么多字节。
const STDERR_KEEP: usize = 2048;
/// 断开时给人看的 stderr 最多这么多行。
const STDERR_LINES: usize = 3;
/// 断开时等 ssh 关掉 stderr 最多这么久：stdout 先到头时，stderr 上的最后几句可能还没读到。
const STDERR_WAIT: Duration = Duration::from_secs(1);

/// 一条流最后的 `STDERR_KEEP` 字节。
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    /// 前面丢掉过，`bytes` 的第一行不完整。
    cut: bool,
}

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > STDERR_KEEP {
            self.bytes.drain(..self.bytes.len() - STDERR_KEEP);
            self.cut = true;
        }
    }

    /// 最后 `STDERR_LINES` 个非空行，去掉首尾空白和控制字符，用空格连成一行；什么也没有时为空。
    fn text(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.bytes);
        let text = match text.split_once('\n') {
            Some((_, rest)) if self.cut => rest,
            _ => &text,
        };
        let lines: Vec<String> = text
            .lines()
            .map(|line| line.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        let start = lines.len().saturating_sub(STDERR_LINES);
        let text = lines[start..].join(" ");
        (!text.is_empty()).then_some(text)
    }
}

/// 读线程收着的 ssh 的 stderr，和读完了没有。
#[derive(Default)]
struct Stderr {
    state: Mutex<(Tail, bool)>,
    ended: Condvar,
}

impl Stderr {
    /// 在后台一直读到 ssh 关掉 stderr，只留最后一段：不读的话 ssh 写满管道就卡住。
    fn collect(reader: impl Read + Send + 'static) -> std::io::Result<Arc<Self>> {
        let stderr = Arc::new(Self::default());
        let collecting = stderr.clone();
        thread::Builder::new().name("screen-stderr".into()).spawn(move || {
            let mut reader = reader;
            let mut chunk = [0u8; 1024];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => collecting.state.lock().unwrap_or_else(PoisonError::into_inner).0.push(&chunk[..read]),
                }
            }
            collecting.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
            collecting.ended.notify_all();
        })?;
        Ok(stderr)
    }

    /// 等 ssh 关掉 stderr（最多 `STDERR_WAIT`），交出它最后说的（`Tail::text`）。
    fn last_words(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, _) = self
            .ended
            .wait_timeout_while(state, STDERR_WAIT, |(_, ended)| !*ended)
            .unwrap_or_else(PoisonError::into_inner);
        state.0.text()
    }
}

/// 排进交出线程的：ssh 上来的帧和 UDP 上来的。
enum Arrival {
    Stream(Frame),
    Datagram(datagram::Event),
    Closed,
}

/// 远程桌面的消息走在什么上面，断开时怎么关。
enum Transport {
    /// SSH 主机：起着 `runode --ssh-screen` 的 ssh。
    Ssh(Child),
    /// 配对的电脑：拨过去的那条连接和它在 `PairedHost` 里的登记。
    Socket(UnixStream, paired::Held),
}

impl Drop for ScreenLink {
    fn drop(&mut self) {
        match self.transport.take() {
            Some(Transport::Ssh(mut child)) => {
                let _ = child.kill();
                thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Some(Transport::Socket(stream, _held)) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            None => {}
        }
    }
}

/// 连上 `host` 的远程桌面，先发 `ScreenHello`；远端交来的消息和画面经 `on_event` 在同一个后台线程上按到的先后
/// 交出，最后一个是 `ScreenEvent::Closed`，同时交给它往远端发消息的一头（解不了的画面当场要关键帧）。会阻塞着起
/// ssh 或拨号，在后台线程上调。
pub fn open(
    host: &HostId,
    on_event: impl FnMut(ScreenEvent, &ScreenSender) + Send + 'static,
) -> Result<ScreenLink, String> {
    let name = host.key().unwrap_or_default();
    let mut stderr = None;
    let (input, output, transport): (Box<dyn Write + Send>, Box<dyn Read + Send>, Transport) = match host {
        HostId::Ssh(name) => {
            let host = ssh::find(name).ok_or_else(|| format!("no such host: {name}"))?;
            if !matches!(host.status(), Status::Connected(_)) {
                return Err(format!("not connected to {}", host.name));
            }
            let script = host.family().run_command(&deploy::install_dir(), runode_ssh::bootstrap::Entry::Screen);
            // 要问用户的经 askpass 问，提示和回答都不经 stderr。
            let mut command = host.exec(&ssh::ssh(), &script, true, false);
            let pipes = runode_ssh::process::Pipes { stdin: true, stdout: true, stderr: true };
            let piped =
                runode_ssh::process::spawn(&mut command, pipes).map_err(|err| format!("cannot run ssh: {err}"))?;
            let (Some(input), Some(output), Some(errors)) = (piped.stdin, piped.stdout, piped.stderr) else {
                return Err("ssh gave no pipes".into());
            };
            let mut child = piped.child;
            match Stderr::collect(errors) {
                Ok(errors) => stderr = Some(errors),
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("cannot start a thread: {err}"));
                }
            }
            (Box::new(input), Box::new(output), Transport::Ssh(child))
        }
        HostId::Paired(id) => {
            let host = paired::find(id).ok_or_else(|| format!("no such host: {name}"))?;
            let stream = host.dial().map_err(|err| err.to_string())?;
            let held = host.hold(&stream);
            let clone = || stream.try_clone().map_err(|err| format!("cannot use the connection to {name}: {err}"));
            (Box::new(clone()?), Box::new(clone()?), Transport::Socket(stream, held))
        }
        HostId::Local => return Err("the remote desktop is for other computers".into()),
    };

    let (tx, rx) = mpsc::channel::<ScreenClientMsg>();
    let _ = tx.send(ScreenClientMsg::ScreenHello { version: SCREEN_VERSION });
    // 收 UDP 的套接字先开好，远端在 `Ready` 里说认得时把端口和密钥交给它；开不了就只走 ssh。
    let receiver = match datagram::Receiver::bind() {
        Ok(receiver) => Some(receiver),
        Err(err) => {
            tracing::info!("the remote desktop of {name} stays on the stream: {err}");
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
    let dispatch_name = name.clone();
    let dispatch_sender = sender.clone();
    let dispatch_stats = stats.clone();
    let udp_arrivals = arrivals.clone();
    let backlog = Arc::new(Backlog::default());
    let dispatch_backlog = backlog.clone();
    thread::Builder::new()
        .name("screen-events".into())
        .spawn(move || {
            let link = Dispatch {
                name: dispatch_name,
                sender: dispatch_sender,
                arrivals: udp_arrivals,
                stats: dispatch_stats,
                backlog: dispatch_backlog,
                stderr,
            };
            link.run(&arrived, receiver, on_event);
        })
        .map_err(|err| format!("cannot start a thread: {err}"))?;
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

    Ok(ScreenLink { sender, transport: Some(transport), datagram: stats })
}

/// 交出线程手里的东西。
struct Dispatch {
    name: String,
    sender: ScreenSender,
    /// UDP 上来的也排进同一个队列。
    arrivals: mpsc::Sender<Arrival>,
    stats: Arc<Mutex<Option<Arc<datagram::Stats>>>>,
    backlog: Arc<Backlog>,
    /// 走 ssh 时它的 stderr。
    stderr: Option<Arc<Stderr>>,
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
        let mut heard = false;
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
            heard = true;
            on_event(event, &self.sender);
        }
        self.backlog.close();
        drop(receiving);
        let detail = if heard { None } else { self.stderr.as_ref().and_then(|stderr| stderr.last_words()) };
        if let Some(detail) = &detail {
            tracing::info!("ssh to the remote desktop of {} said: {detail}", self.name);
        }
        on_event(ScreenEvent::Closed { detail }, &self.sender);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(chunks: &[&[u8]]) -> Option<String> {
        let mut tail = Tail::default();
        for chunk in chunks {
            tail.push(chunk);
        }
        tail.text()
    }

    #[test]
    fn nothing_said_is_no_detail() {
        assert_eq!(tail(&[]), None);
        assert_eq!(tail(&[b"\n  \r\n\t\n"]), None);
    }

    #[test]
    fn keeps_the_last_lines_on_one_line() {
        let said = tail(&[b"one\ntwo\n", b"\nthree\r\nfour: not ", b"found\n"]);
        assert_eq!(said.as_deref(), Some("two three four: not found"));
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(tail(&[b"\x1b[31mboom\x07\n"]).as_deref(), Some("[31mboom"));
    }

    /// 超过 `STDERR_KEEP` 时只留最后一段，被截断的那一行不要。
    #[test]
    fn long_output_keeps_only_the_end() {
        let mut tail = Tail::default();
        tail.push(&vec![b'x'; STDERR_KEEP]);
        tail.push(b"\nlast words\n");
        assert!(tail.bytes.len() <= STDERR_KEEP);
        assert_eq!(tail.text().as_deref(), Some("last words"));
    }

    /// 截在一个字的中间时，第一行是半个字，整行丢掉，不留乱码。
    #[test]
    fn a_cut_character_does_not_show() {
        let mut tail = Tail::default();
        tail.push("找".repeat(STDERR_KEEP).as_bytes());
        tail.push("\n找不到 runode\n".as_bytes());
        assert_eq!(tail.text().as_deref(), Some("找不到 runode"));
    }

    #[test]
    fn waits_for_stderr_to_end() {
        let stderr = Stderr::collect(&b"bash: runode: command not found\n"[..]).expect("thread");
        assert_eq!(stderr.last_words().as_deref(), Some("bash: runode: command not found"));
    }
}
