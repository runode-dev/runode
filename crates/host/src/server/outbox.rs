//! 往连接上发消息：`Outbox` 把帧排给这条连接的写线程、记着还没写出去的积压，`session_sink` 是会话往这条
//! 连接发事件的一头，积压过了上限时叫前端重新连上。

use std::{
    io::{BufWriter, Write as _},
    net::Shutdown,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use runode_protocol::{Frame, FrameError, FrameKind, HostMsg, MAX_PAYLOAD, SessionId, ShellReport, write_frame};
use runode_terminal::pty;

use super::UnixStream;
use crate::session::{Event, EventSink};

/// 一条连接最多积压这么多字节还没写出去，再多就让输出最多的会话改发 `Resync`。
pub(super) const OUTBOX_LIMIT: usize = 32 << 20;
/// 手机连接（`ClientKind::Mobile`）的 `OUTBOX_LIMIT`：手机隔着 Wi-Fi 和 TLS 读得慢，积压的每个字节都是
/// 回显前要等的，宁可早点 `Resync` 跳过一段输出，也不让画面落后几秒。
pub(super) const MOBILE_OUTBOX_LIMIT: usize = 1 << 20;
/// 不算快照的积压过了它，读的线程不再办这条连接的请求，直接断开，见 `Outbox::overflowing`。过了
/// `OUTBOX_LIMIT` 的会话输出改发 `Resync`、多数请求回 `Error`，但列目录、看板、`Done` 这些回话不看积压，
/// 只发不读的前端能让它们一直涨；正常读着的前端积压到这里之前早就 `Resync` 了，留了好几倍的余量。
pub(super) const OUTBOX_HARD_LIMIT: usize = 8 * OUTBOX_LIMIT;
/// 写的线程一次最多攒这么多字节再写出去。
const WRITE_BUFFER: usize = 256 << 10;

/// 起这条连接的写线程，返回往它送帧的 `Outbox`。
pub(super) fn start_writer(stream: &UnixStream) -> Option<Outbox> {
    let write_half = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("failed to set up a connection: {err}");
            return None;
        }
    };
    let (tx, rx) = mpsc::channel();
    let out = Outbox { tx, queued: Arc::default(), limit: OUTBOX_LIMIT };
    let queued = out.queued.clone();
    let spawned = thread::Builder::new().name("host-connection-writer".into()).spawn(move || {
        pty::set_current_thread_interactive();
        write_out(write_half, &rx, &queued);
    });
    match spawned {
        Ok(_) => Some(out),
        Err(err) => {
            tracing::warn!("failed to start a connection writer: {err}");
            None
        }
    }
}

/// 一条连接上等着写出去的帧。可以随意克隆，各份往同一个写的线程送。
#[derive(Clone)]
pub(crate) struct Outbox {
    tx: mpsc::Sender<Out>,
    pub(super) queued: Arc<Queued>,
    /// `queued` 过了它就算积压，见 `backed_up`；收到 `Hello` 后按前端种类定，之后才有会话的 sink 克隆它。
    pub(super) limit: usize,
}

/// 已经交给写的线程、还没写出去的载荷字节数，快照和别的分开记：`session_sink` 判断积压时只看
/// `streaming`（见 `streaming_backed_up`），各记各的，读它不会撞上另一个数正改到一半。
#[derive(Default)]
pub(super) struct Queued {
    pub(super) streaming: AtomicUsize,
    /// `FrameKind::Snapshot` 的。
    snapshots: AtomicUsize,
}

impl Queued {
    pub(super) fn add(&self, kind: FrameKind, len: usize) {
        self.of(kind).fetch_add(len, Ordering::Relaxed);
    }

    pub(super) fn sub(&self, kind: FrameKind, len: usize) {
        self.of(kind).fetch_sub(len, Ordering::Relaxed);
    }

    fn of(&self, kind: FrameKind) -> &AtomicUsize {
        if kind == FrameKind::Snapshot { &self.snapshots } else { &self.streaming }
    }
}

enum Out {
    Frame {
        kind: FrameKind,
        channel: u32,
        payload: Arc<[u8]>,
    },
    /// 之前的帧写完后断开连接。
    Close,
    /// 之前的帧写完后写的线程结束，但不断开连接，写完时往里发一声：交接时之后由交出会话的线程
    /// 直接在 socket 上发描述符消息，见 `handoff::give`。
    Detach(mpsc::Sender<()>),
}

impl Outbox {
    /// 交一帧给写的线程；连接已经断了时返回 false。
    pub(super) fn push(&self, kind: FrameKind, channel: u32, payload: Arc<[u8]>) -> bool {
        self.queued.add(kind, payload.len());
        self.tx.send(Out::Frame { kind, channel, payload }).is_ok()
    }

    pub(crate) fn control(&self, message: &HostMsg) -> bool {
        match Frame::control(message) {
            // 写线程写不出超过上限的帧，只能断开整条连接，重连后再要还是断：这条不发，回话改回 `Error`，请求方
            // 不必等到超时。读屏幕的回话（`ScreenText`）不带 `req`，前端按会话认。
            Ok(frame) if frame.payload.len() > MAX_PAYLOAD as usize => {
                tracing::warn!("dropping a control message of {} bytes, over the frame limit", frame.payload.len());
                let req = HostMsg::req_in(&frame.payload);
                let id = match message {
                    HostMsg::ScreenText { id, .. } => Some(*id),
                    _ => None,
                };
                if req.is_none() && id.is_none() {
                    return true;
                }
                self.control(&HostMsg::Error {
                    req,
                    id,
                    message: "the answer is too large to send".into(),
                    reason: None,
                })
            }
            Ok(frame) => self.push(FrameKind::Control, 0, frame.payload.into()),
            Err(err) => {
                tracing::warn!("failed to encode {message:?}: {err}");
                true
            }
        }
    }

    /// 已经交给写线程的帧写完后断开连接，之后交的不再写。
    pub(crate) fn close(&self) {
        let _ = self.tx.send(Out::Close);
    }

    /// 已经交给写线程的帧写完后写线程结束，连接留着，之后交的不再写；返回的一端在写完时收到
    /// 一声，写不出去（对面断了）时断开。
    pub(crate) fn detach(&self) -> mpsc::Receiver<()> {
        let (done, finished) = mpsc::channel();
        let _ = self.tx.send(Out::Detach(done));
        finished
    }

    pub(super) fn backed_up(&self) -> bool {
        self.streaming() + self.queued.snapshots.load(Ordering::Relaxed) > self.limit
    }

    /// 不算还没写出去的快照，只看排在后面的输出和状态有没有过上限。连上会话时排进去的一整份
    /// 屏幕（手机的 `VtReplay` 带着回滚，常常超过 `MOBILE_OUTBOX_LIMIT`）要是也算，紧接着的
    /// `SizeOwner` 就让这条连接上所有会话的 sink 回 `Resync`，前端重连又排一份，永远连不上。
    fn streaming_backed_up(&self) -> bool {
        self.streaming() > self.limit
    }

    /// 不算快照的积压过了 `OUTBOX_HARD_LIMIT`，这条连接该断了。快照不算：连上会话本来就看积压，
    /// 桌面一下子恢复几十个分屏时排进去的屏幕加起来可以很大，但写得出去，不是对面不读。
    pub(super) fn overflowing(&self) -> bool {
        self.streaming() > OUTBOX_HARD_LIMIT
    }

    /// 已经交给写线程、还没写出去的字节里不是快照的那些。
    fn streaming(&self) -> usize {
        self.queued.streaming.load(Ordering::Relaxed)
    }
}

/// 写的线程：有帧就写，一批写完再刷出去。写不出去（对面断了）或者要断开时关掉连接，读的那边
/// 也随之结束；`Out::Detach` 时写完就结束，不断开。
fn write_out(stream: UnixStream, frames: &mpsc::Receiver<Out>, queued: &Queued) {
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER, &stream);
    let mut write_all = || -> Result<Option<mpsc::Sender<()>>, FrameError> {
        while let Ok(first) = frames.recv() {
            let mut next = Some(first);
            while let Some(out) = next {
                match out {
                    Out::Frame { kind, channel, payload } => {
                        write_frame(&mut writer, kind, channel, &payload)?;
                        queued.sub(kind, payload.len());
                    }
                    Out::Close => {
                        writer.flush()?;
                        return Ok(None);
                    }
                    Out::Detach(done) => {
                        writer.flush()?;
                        return Ok(Some(done));
                    }
                }
                next = frames.try_recv().ok();
            }
            writer.flush()?;
        }
        Ok(None)
    };
    match write_all() {
        Ok(Some(done)) => {
            let _ = done.send(());
            return;
        }
        Ok(None) => {}
        Err(err) => tracing::debug!("connection closed while writing: {err}"),
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// 一个会话往这条连接发事件的 `EventSink`。`MetaOnly` 的前端没有 VT，只要状态（含 `Bell`），
/// 不要输出和改 VT 的标记。`shell` 是前端说了 `Caps::shell_report` 时最近一次发给它的 shell 集成
/// 报告（`Attached` 里的那份起头）：`Meta` 里的这部分拆出来，和它一样就不带。
pub(super) fn session_sink(
    id: SessionId,
    channel: u32,
    meta_only: bool,
    mut shell: Option<ShellReport>,
    out: Outbox,
) -> EventSink {
    // 积压过了上限时不再往里排，叫前端重新 Attach；返回 false 让会话摘掉这个 sink。状态消息也一样管：
    // 只看状态的前端（手机挂起了、TCP 还没断）读不动时，频繁改标题的程序每次都发一整份 `SessionMeta`，
    // 不管的话队列无上限地涨。重新 Attach 时拿到最新状态。
    let resync = move |out: &Outbox| {
        out.control(&HostMsg::Resync { id, reason: "the client reads too slowly".into() });
        false
    };
    Box::new(move |event| match event {
        Event::Output(_) if meta_only => true,
        Event::Output(data) => {
            if out.streaming_backed_up() {
                return resync(&out);
            }
            out.push(FrameKind::Output, channel, data)
        }
        Event::Msg(mut message) => {
            if meta_only && matches!(*message, HostMsg::Resized { .. } | HostMsg::ThemeApplied { .. }) {
                return true;
            }
            if out.streaming_backed_up() {
                return resync(&out);
            }
            if let (Some(sent), HostMsg::Meta { meta, shell: carried, .. }) = (&mut shell, &mut *message) {
                let report = ShellReport::take(meta);
                if report != *sent {
                    *sent = report.clone();
                    *carried = Some(report);
                }
            }
            out.control(&message)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连上会话时排进去的大屏幕还没写出去，不算会话输出的积压：紧接着的状态照常发，不回 `Resync`；
    /// 但新的连上、读屏幕照旧被拒。快照之后真积压了，照旧回 `Resync`。
    #[test]
    fn a_pending_snapshot_does_not_resync_the_session() {
        let (tx, rx) = mpsc::channel();
        let out = Outbox { tx, queued: Arc::default(), limit: MOBILE_OUTBOX_LIMIT };
        let id = SessionId(1);
        let control = |out: Out| match out {
            Out::Frame { kind: FrameKind::Control, channel, payload } => {
                Frame { kind: FrameKind::Control, channel, payload: payload.to_vec() }.message::<HostMsg>().unwrap()
            }
            _ => panic!("expected a control frame"),
        };
        out.push(FrameKind::Snapshot, 1, vec![0; MOBILE_OUTBOX_LIMIT + 1].into());
        assert!(matches!(rx.recv().unwrap(), Out::Frame { kind: FrameKind::Snapshot, .. }));
        let mut sink = session_sink(id, 1, false, None, out.clone());
        assert!(sink(Event::msg(HostMsg::SizeOwner { id, mine: true, owner: None })));
        assert!(matches!(control(rx.recv().unwrap()), HostMsg::SizeOwner { .. }));
        assert!(out.backed_up());
        out.push(FrameKind::Output, 1, vec![0; MOBILE_OUTBOX_LIMIT + 1].into());
        rx.recv().unwrap();
        assert!(!sink(Event::msg(HostMsg::Bell { id })));
        assert!(matches!(control(rx.recv().unwrap()), HostMsg::Resync { .. }));
    }

    /// 装不进一帧的回话不交给写线程（交了它写不出、只能断开连接）：带 `req` 的改回一条带着它的 `Error`，
    /// 不带的丢掉。
    #[test]
    fn an_answer_over_the_frame_limit_is_not_queued() {
        let (tx, rx) = mpsc::channel();
        let out = Outbox { tx, queued: Arc::default(), limit: OUTBOX_LIMIT };
        let huge = "x".repeat(MAX_PAYLOAD as usize);
        assert!(out.control(&HostMsg::Error { req: Some(4), id: None, message: huge.clone(), reason: None }));
        let Ok(Out::Frame { kind: FrameKind::Control, channel, payload }) = rx.try_recv() else {
            panic!("expected a control frame")
        };
        let frame = Frame { kind: FrameKind::Control, channel, payload: payload.to_vec() };
        match frame.message::<HostMsg>().unwrap() {
            HostMsg::Error { req: Some(4), message, .. } => assert!(message.contains("too large"), "{message}"),
            other => panic!("expected an error for the request: {other:?}"),
        }
        let id = SessionId(1);
        assert!(out.control(&HostMsg::ScreenText { id, text: huge.clone(), truncated: false }));
        let Ok(Out::Frame { payload: screen, .. }) = rx.try_recv() else { panic!("expected a control frame") };
        let frame = Frame { kind: FrameKind::Control, channel, payload: screen.to_vec() };
        assert!(
            matches!(frame.message::<HostMsg>().unwrap(), HostMsg::Error { req: None, id: Some(got), .. } if got == id),
            "the screen text turns into an error for its session"
        );
        assert!(out.control(&HostMsg::Error { req: None, id: None, message: huge, reason: None }));
        assert!(rx.try_recv().is_err());
        assert_eq!(out.streaming(), payload.len() + screen.len());
    }

    /// 说了 `Caps::shell_report` 的连接：`Meta` 里的 shell 集成报告拆出来，和上一次发的一样就不带；
    /// 没说的照旧整份放在 `meta` 里。
    #[test]
    fn meta_carries_the_shell_report_only_when_it_changes() {
        use runode_types::{session::SessionMeta, shell::ShellNames};

        let (tx, rx) = mpsc::channel();
        let out = Outbox { tx, queued: Arc::default(), limit: MOBILE_OUTBOX_LIMIT };
        let id = SessionId(1);
        let next = || match rx.recv().unwrap() {
            Out::Frame { kind: FrameKind::Control, channel, payload } => {
                match (Frame { kind: FrameKind::Control, channel, payload: payload.to_vec() })
                    .message::<HostMsg>()
                    .unwrap()
                {
                    HostMsg::Meta { meta, shell, .. } => (meta, shell),
                    other => panic!("expected meta, got {other:?}"),
                }
            }
            _ => panic!("expected a control frame"),
        };
        let names = Arc::new(ShellNames { functions: vec!["runode-reload".into()] });
        let meta = |path: &str, title: &str| SessionMeta {
            title: Some(title.into()),
            shell_path: Some(path.into()),
            shell_names: names.clone(),
            ..SessionMeta::default()
        };
        let stripped = |title: &str| SessionMeta { title: Some(title.into()), ..SessionMeta::default() };
        let report = |path: &str| ShellReport { path: Some(path.into()), names: names.clone() };

        let mut sink = session_sink(id, 1, true, Some(report("/bin")), out.clone());
        assert!(sink(Event::msg(HostMsg::Meta { id, meta: meta("/bin", "a"), shell: None })));
        assert_eq!(next(), (stripped("a"), None));
        assert!(sink(Event::msg(HostMsg::Meta { id, meta: meta("/opt/bin", "a"), shell: None })));
        assert_eq!(next(), (stripped("a"), Some(report("/opt/bin"))));
        assert!(sink(Event::msg(HostMsg::Meta { id, meta: meta("/opt/bin", "b"), shell: None })));
        assert_eq!(next(), (stripped("b"), None));

        let mut legacy = session_sink(id, 2, true, None, out);
        assert!(legacy(Event::msg(HostMsg::Meta { id, meta: meta("/bin", "a"), shell: None })));
        assert_eq!(next(), (meta("/bin", "a"), None));
    }
}
