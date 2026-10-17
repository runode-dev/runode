use runode_protocol::{BuildId, Caps};
use runode_types::shell::IntegrationMode;

use super::*;

const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
const WAIT: Duration = Duration::from_secs(10);

fn send(stream: &mut UnixStream, message: &ClientMsg) {
    let frame = Frame::control(message).unwrap();
    write_frame(stream, frame.kind, 0, &frame.payload).unwrap();
}

/// 后台线程把读到的帧交过来，测试按超时等。
fn frames(stream: &UnixStream) -> mpsc::Receiver<Frame> {
    let mut reader = stream.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while let Ok(Some(frame)) = read_frame(&mut reader) {
            if tx.send(frame).is_err() {
                break;
            }
        }
    });
    rx
}

fn message(frames: &mpsc::Receiver<Frame>) -> HostMsg {
    loop {
        let frame = frames.recv_timeout(WAIT).expect("timed out");
        if frame.kind == FrameKind::Control {
            return frame.message().unwrap();
        }
    }
}

fn options(shell: &str) -> SpawnOptions {
    SpawnOptions {
        size: SIZE,
        cwd: None,
        integration: IntegrationMode::Off,
        start: true,
        shell: Some(shell.into()),
        settings: None,
    }
}

/// 测试这头的连接，丢掉时断开：读帧的线程拿着同一个 socket 的另一份描述符，只关自己这份的话宿主看不到
/// 断开，那条连接和宿主就一直留着。
struct Client(UnixStream);

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

impl std::ops::Deref for Client {
    type Target = UnixStream;

    fn deref(&self) -> &UnixStream {
        &self.0
    }
}

impl std::ops::DerefMut for Client {
    fn deref_mut(&mut self) -> &mut UnixStream {
        &mut self.0
    }
}

/// 经 `Host::connect_pair` 连上、按 `client` 握手，返回连接和读到的帧。
fn greet(host: &Host, client: ClientKind) -> (Client, mpsc::Receiver<Frame>) {
    let mut stream = Client(host.connect_pair().unwrap());
    let frames = frames(&stream);
    send(
        &mut stream,
        &ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId("test".into()),
            client,
            caps: Caps::default(),
            session: None,
            device: None,
        },
    );
    assert!(matches!(message(&frames), HostMsg::Welcome { .. }));
    (stream, frames)
}

/// 开一个线程卡在不返回的 `Subscribe::start` 里、答不了话的会话；丢掉返回的发送端时放开。
fn stuck_session(host: &Host) -> (SessionId, mpsc::Sender<()>) {
    let stuck = host.shared.spawn(options("/bin/cat")).unwrap();
    let (release, released) = mpsc::channel::<()>();
    let (entered, stuck_now) = mpsc::channel();
    let start = Box::new(move |_: Screen| {
        let _ = entered.send(());
        // 卡住会话线程，直到测试放开（丢掉发送的一端）。
        let _ = released.recv();
        None
    });
    let subscribe =
        Subscribe { connection: 0, size: None, mode: AttachMode::MetaOnly, start, desktop: false, device: None };
    assert!(host.shared.deliver(stuck, Inbox::Subscribe(subscribe)));
    stuck_now.recv_timeout(WAIT).unwrap();
    (stuck, release)
}

/// 列会话、读屏幕要等会话线程回话，等的时候同一条连接上的输入照常转发：这里有个会话的线程
/// 卡在一个不返回的 `Subscribe::start` 里，答不了话。
#[test]
fn waiting_for_answers_does_not_hold_up_input() {
    let host = Host::new(BuildId("test".into()));
    let (stuck, release) = stuck_session(&host);
    let (mut stream, frames) = greet(&host, ClientKind::Cli);
    let other = host.shared.spawn(options("/bin/cat")).unwrap();
    send(&mut stream, &ClientMsg::Attach { id: other, size: None, mode: AttachMode::VtReplay });
    let channel = loop {
        if let HostMsg::Attached { channel, .. } = message(&frames) {
            break channel;
        }
    };
    send(&mut stream, &ClientMsg::ListSessions);
    send(&mut stream, &ClientMsg::ReadScreen { id: stuck, lines: None, command: None });
    write_frame(&mut *stream, FrameKind::Input, channel, b"through\r").unwrap();
    let mut output = Vec::new();
    while !output.windows(7).any(|w| w == b"through") {
        let frame = frames.recv_timeout(WAIT).expect("timed out");
        match frame.kind {
            FrameKind::Output if frame.channel == channel => output.extend_from_slice(&frame.payload),
            FrameKind::Control => assert!(
                !matches!(frame.message().unwrap(), HostMsg::SessionList { .. } | HostMsg::ScreenText { .. }),
                "answered while a session was stuck"
            ),
            _ => {}
        }
    }
    drop(release);
    let mut answers = Vec::new();
    while answers.len() < 2 {
        match message(&frames) {
            HostMsg::SessionList { sessions } => {
                assert_eq!(sessions.len(), 2);
                answers.push("list");
            }
            HostMsg::ScreenText { id, .. } => {
                assert_eq!(id, stuck);
                answers.push("read");
            }
            _ => {}
        }
    }
    host.shared.kill_all();
}

/// 一条连接上在等回话的请求有上限：超出的当场回 `Error`，回完话的名额放开，之后的请求照常。
#[test]
fn too_many_waiting_requests_are_refused() {
    let host = Host::new(BuildId("test".into()));
    let (stuck, release) = stuck_session(&host);
    let (mut stream, frames) = greet(&host, ClientKind::Cli);
    for _ in 0..=MAX_WAITING {
        send(&mut stream, &ClientMsg::ReadScreen { id: stuck, lines: None, command: None });
    }
    match message(&frames) {
        HostMsg::Error { id: Some(id), message, .. } => {
            assert_eq!(id, stuck);
            assert!(message.contains("too many"), "{message}");
        }
        other => panic!("expected the extra request to be refused: {other:?}"),
    }
    drop(release);
    let mut answered = 0;
    while answered < MAX_WAITING {
        match message(&frames) {
            HostMsg::ScreenText { id, .. } if id == stuck => answered += 1,
            other => panic!("unexpected {other:?}"),
        }
    }
    send(&mut stream, &ClientMsg::ListSessions);
    assert!(matches!(message(&frames), HostMsg::SessionList { .. }));
    host.shared.kill_all();
}

/// git 请求的名额和列会话、读屏幕的分开算：排满了卡住的 git 请求，同一条连接上照样能读屏幕；
/// git 请求自己超出上限的当场回 `Error`。
#[test]
fn stuck_git_requests_do_not_take_the_other_slots() {
    let host = Host::new(BuildId("test".into()));
    let (stuck, release) = stuck_session(&host);
    let other = host.shared.spawn(options("/bin/cat")).unwrap();
    let (mut stream, frames) = greet(&host, ClientKind::Cli);
    // 会话答不了它的目录，git 工作线程卡在第一件上，后面的排着，名额都占着。
    for req in 0..=MAX_WAITING as u32 {
        send(&mut stream, &ClientMsg::Git { req, id: stuck, request: GitRequest::Status });
    }
    match message(&frames) {
        HostMsg::Error { req: Some(req), message, .. } => {
            assert_eq!(req, MAX_WAITING as u32);
            assert!(message.contains("too many"), "{message}");
        }
        other => panic!("expected the extra git request to be refused: {other:?}"),
    }
    send(&mut stream, &ClientMsg::ReadScreen { id: other, lines: None, command: None });
    assert!(matches!(message(&frames), HostMsg::ScreenText { id, .. } if id == other));
    drop(release);
    host.shared.kill_all();
}

/// 前端不读、积压过了上限时，连上会话、读屏幕、列会话和 git 请求直接回 `Error`，不再往积压上
/// 加快照和回话。
#[test]
fn a_backed_up_connection_gets_no_more_snapshots_or_replies() {
    let host = Host::new(BuildId("test".into()));
    let id = host.shared.spawn(options("/bin/cat")).unwrap();
    let (mut stream, frames) = greet(&host, ClientKind::Cli);
    // 假装积压过了上限：写的线程写出去时只减它真写的那些，假的这份一直在。
    let out = host.shared.peers().connections.values().find_map(|peer| peer.out.clone()).unwrap();
    out.queued.streaming.fetch_add(OUTBOX_LIMIT + 1, Ordering::Relaxed);
    send(&mut stream, &ClientMsg::Attach { id, size: None, mode: AttachMode::VtReplay });
    send(&mut stream, &ClientMsg::ReadScreen { id, lines: None, command: None });
    for _ in 0..2 {
        match message(&frames) {
            HostMsg::Error { id: Some(errored), message, .. } => {
                assert_eq!(errored, id);
                assert!(message.starts_with(ERROR_BACKED_UP), "{message}");
            }
            other => panic!("expected the request to be refused: {other:?}"),
        }
    }
    send(&mut stream, &ClientMsg::ListSessions);
    send(&mut stream, &ClientMsg::Git { req: 7, id, request: GitRequest::Status });
    for expected in [None, Some(7)] {
        match message(&frames) {
            HostMsg::Error { req, message, .. } => {
                assert_eq!(req, expected);
                assert!(message.starts_with(ERROR_BACKED_UP), "{message}");
            }
            other => panic!("expected the request to be refused: {other:?}"),
        }
    }
    out.queued.streaming.fetch_sub(OUTBOX_LIMIT + 1, Ordering::Relaxed);
    send(&mut stream, &ClientMsg::ReadScreen { id, lines: None, command: None });
    assert!(matches!(message(&frames), HostMsg::ScreenText { .. }));
    send(&mut stream, &ClientMsg::ListSessions);
    assert!(matches!(message(&frames), HostMsg::SessionList { .. }));
    host.shared.kill_all();
}

/// 手机连接积压的上限比别的连接小：同样的积压，命令行照常回话，手机已经算积压了。
#[test]
fn mobile_connections_back_up_sooner() {
    for (client, backed_up) in [(ClientKind::Cli, false), (ClientKind::Mobile, true)] {
        let host = Host::new(BuildId("test".into()));
        let id = host.shared.spawn(options("/bin/cat")).unwrap();
        let (mut stream, frames) = greet(&host, client);
        let out = host.shared.peers().connections.values().find_map(|peer| peer.out.clone()).unwrap();
        out.queued.streaming.fetch_add(MOBILE_OUTBOX_LIMIT + 1, Ordering::Relaxed);
        send(&mut stream, &ClientMsg::ReadScreen { id, lines: None, command: None });
        match message(&frames) {
            HostMsg::Error { message, .. } if backed_up => assert!(message.starts_with(ERROR_BACKED_UP), "{message}"),
            HostMsg::ScreenText { .. } if !backed_up => {}
            other => panic!("{client:?}: unexpected reply {other:?}"),
        }
        host.shared.kill_all();
    }
}

/// 只发不读、积压远超上限的连接，下一个请求到时直接断开，不再往积压上加回话；同样多的积压
/// 要是都是还没写出去的快照（桌面一下子连上很多会话），连接留着，请求照旧只回 `Error`。
#[test]
fn a_connection_that_stopped_reading_is_closed() {
    for (client, snapshots, closed) in
        [(ClientKind::Cli, false, true), (ClientKind::Mobile, false, true), (ClientKind::Desktop, true, false)]
    {
        let host = Host::new(BuildId("test".into()));
        let (mut stream, frames) = greet(&host, client);
        let out = host.shared.peers().connections.values().find_map(|peer| peer.out.clone()).unwrap();
        // 假装积压过了硬上限，同 `a_backed_up_connection_gets_no_more_snapshots_or_replies`。
        let kind = if snapshots { FrameKind::Snapshot } else { FrameKind::Control };
        out.queued.add(kind, OUTBOX_HARD_LIMIT + 1);
        // 输入不往积压上加回话，不断开。
        write_frame(&mut *stream, FrameKind::Input, 1, b"x").unwrap();
        assert!(
            matches!(frames.recv_timeout(Duration::from_millis(200)), Err(mpsc::RecvTimeoutError::Timeout)),
            "{client:?}"
        );
        send(&mut stream, &ClientMsg::ListSessions);
        if closed {
            assert!(matches!(frames.recv_timeout(WAIT), Err(mpsc::RecvTimeoutError::Disconnected)), "{client:?}");
        } else {
            match message(&frames) {
                HostMsg::Error { message, .. } => assert!(message.starts_with(ERROR_BACKED_UP), "{message}"),
                other => panic!("expected the request to be refused: {other:?}"),
            }
            out.queued.sub(kind, OUTBOX_HARD_LIMIT + 1);
            send(&mut stream, &ClientMsg::ListSessions);
            assert!(matches!(message(&frames), HostMsg::SessionList { .. }));
        }
    }
}

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
            match (Frame { kind: FrameKind::Control, channel, payload: payload.to_vec() }).message::<HostMsg>().unwrap()
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

/// 发请求的连接断开后，它转给界面、界面还没回话的请求跟着撤掉；界面之后再回话也只是丢掉。
#[test]
fn requests_from_a_closed_connection_are_forgotten() {
    let host = Host::new(BuildId("test".into()));
    let (mut desktop, desktop_frames) = greet(&host, ClientKind::Desktop);
    let (mut cli, _cli_frames) = greet(&host, ClientKind::Cli);
    send(&mut cli, &ClientMsg::Reveal { req: 1, id: SessionId(9) });
    let HostMsg::UiRequest { ui, .. } = message(&desktop_frames) else { panic!("expected a ui request") };
    assert_eq!(host.shared.peers().pending.len(), 1);
    cli.shutdown(Shutdown::Both).unwrap();
    let deadline = Instant::now() + WAIT;
    while !host.shared.peers().pending.is_empty() {
        assert!(Instant::now() < deadline, "the request outlived its connection");
        thread::sleep(Duration::from_millis(5));
    }
    send(&mut desktop, &ClientMsg::UiReply { ui, reply: Box::new(HostMsg::Done { req: 1 }) });
    send(&mut desktop, &ClientMsg::ListSessions);
    assert!(matches!(message(&desktop_frames), HostMsg::SessionList { .. }));
}

/// 界面说布局变了：只告诉问过 `Layout` 的连接；不是界面的连接不能这么说。
#[test]
fn layout_changes_reach_connections_that_asked_for_the_layout() {
    let host = Host::new(BuildId("test".into()));
    let (mut desktop, desktop_frames) = greet(&host, ClientKind::Desktop);
    let (mut mobile, mobile_frames) = greet(&host, ClientKind::Mobile);
    let (mut cli, cli_frames) = greet(&host, ClientKind::Cli);
    send(&mut mobile, &ClientMsg::Layout { req: 0 });
    let HostMsg::UiRequest { ui, .. } = message(&desktop_frames) else { panic!("expected a ui request") };
    send(&mut desktop, &ClientMsg::UiReply { ui, reply: Box::new(HostMsg::Layout { req: 0, windows: vec![] }) });
    assert!(matches!(message(&mobile_frames), HostMsg::Layout { .. }));

    send(&mut desktop, &ClientMsg::LayoutChanged { windows: None });
    assert_eq!(message(&mobile_frames), HostMsg::LayoutChanged);
    // 没问过布局的连接收不到：它下一条收到的是自己请求的回话。
    send(&mut cli, &ClientMsg::ListSessions);
    assert!(matches!(message(&cli_frames), HostMsg::SessionList { .. }));

    send(&mut mobile, &ClientMsg::LayoutChanged { windows: None });
    assert!(matches!(message(&mobile_frames), HostMsg::Error { .. }));
    send(&mut desktop, &ClientMsg::ListSessions);
    assert!(matches!(message(&desktop_frames), HostMsg::SessionList { .. }));
}

/// 处理消息时 panic 的会话（这里是连着的前端的 `EventSink` 一收到输出就 panic）：前端先收到
/// 带着会话标识的 `Error`，再收到 `Exited`，会话线程随后结束。
#[test]
fn a_panicking_session_tells_its_front_end() {
    let host = Host::new(BuildId("test".into()));
    let id = host.shared.spawn(options("/bin/cat")).unwrap();
    let (tx, rx) = mpsc::channel();
    let start = Box::new(move |_: Screen| {
        let sink: EventSink = Box::new(move |event| match event {
            Event::Output(_) => panic!("the sink cannot take output"),
            Event::Msg(message) => tx.send(*message).is_ok(),
        });
        Some(sink)
    });
    let subscribe =
        Subscribe { connection: 0, size: None, mode: AttachMode::VtReplay, start, desktop: false, device: None };
    assert!(host.shared.deliver(id, Inbox::Subscribe(subscribe)));
    assert!(host.shared.deliver(id, Inbox::Input { connection: 0, data: b"boom\r".to_vec() }));
    let next = || loop {
        match rx.recv_timeout(WAIT).expect("timed out") {
            HostMsg::Meta { .. } | HostMsg::Resized { .. } | HostMsg::SizeOwner { .. } => {}
            message => return message,
        }
    };
    match next() {
        HostMsg::Error { req: None, id: Some(errored), message } => {
            assert_eq!(errored, id);
            assert!(message.contains("crashed"), "{message}");
        }
        other => panic!("expected an error first: {other:?}"),
    }
    assert!(matches!(next(), HostMsg::Exited { id: exited, .. } if exited == id));
    let deadline = Instant::now() + WAIT;
    while host.shared.deliver(id, Inbox::ClearScreen) {
        assert!(Instant::now() < deadline, "the session thread kept running");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!host.shared.registry().sessions.contains_key(&id), "the crashed session is still registered");
    host.shared.kill_all();
}

/// 单独跑的宿主收到 `Shutdown` 以后不再开会话：同一次读进缓冲里、排在它后面的 `Spawn` 不办，要退出了的
/// 宿主（没有界面时另起的线程这时才开好会话）也不登记新会话。
#[test]
fn a_stopping_host_opens_no_more_sessions() {
    let host = Host::new(BuildId("test".into()));
    host.mark_standalone();
    let (mut stream, frames) = greet(&host, ClientKind::Cli);
    let mut both = Vec::new();
    for message in [
        ClientMsg::Shutdown { kill_sessions: true },
        ClientMsg::Spawn {
            req: 1,
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some("/bin/cat".into()),
            settings: None,
        },
    ] {
        let frame = Frame::control(&message).unwrap();
        write_frame(&mut both, frame.kind, 0, &frame.payload).unwrap();
    }
    stream.write_all(&both).unwrap();
    loop {
        match frames.recv_timeout(WAIT) {
            Ok(frame) if frame.kind == FrameKind::Control => {
                let message: HostMsg = frame.message().unwrap();
                assert!(!matches!(message, HostMsg::Spawned { .. }), "spawned after the shutdown");
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("the connection stayed open after the shutdown"),
        }
    }
    assert!(host.shared.registry().sessions.is_empty());

    // 判定空闲退出以后开好的会话也不登记。
    let host = Host::new(BuildId("test".into()));
    host.shared.peers().stop = Some(Stopped::Idle);
    assert!(host.shared.spawn(options("/bin/cat")).is_err());
    assert!(host.shared.registry().sessions.is_empty());
}

/// 要退出时正有会话在开（比如没有界面时另起的线程开终端）：`run_until_idle` 等它开好、被拒绝、杀掉 shell
/// 才返回，不先走，留下进程退出后才起来的 shell。
#[test]
fn stopping_waits_for_a_session_being_opened() {
    stops_after_a_session_being_opened(Stopped::Shutdown, 43);
}

/// 交接给新宿主时也一样：交接开始前就在开的会话不在交出去的里面，开好后被拒绝，shell 还得这边杀掉。
#[test]
fn a_handoff_waits_for_a_session_being_opened() {
    stops_after_a_session_being_opened(Stopped::Handoff, 44);
}

/// 见 `stopping_waits_for_a_session_being_opened`：宿主因为 `reason` 退出。`marker` 让各个测试的 shell 认得出。
fn stops_after_a_session_being_opened(reason: Stopped, marker: u32) {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = std::env::temp_dir().join(format!("rnh-{}-opening-{marker}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let marker = format!("{}{marker}", std::process::id());
    let shell = dir.join("nohup.sh");
    std::fs::write(&shell, format!("#!/bin/sh\ntrap '' HUP\nexec /bin/sleep {marker}\n")).unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shell = shell.to_string_lossy().into_owned();
    let running = || {
        std::process::Command::new("pgrep")
            .args(["-f", &format!("sleep {marker}")])
            .output()
            .is_ok_and(|out| out.status.success())
    };

    let host = Host::new(BuildId("test".into()));
    // `Shared::spawn_with` 记上会话线程以后、开伪终端以前要拿 `env` 的锁（`Shared::setup`）：先占着它，让开会话
    // 的线程停在这中间。
    let env = host.shared.env.lock().unwrap();
    let opening = {
        let host = host.clone();
        thread::spawn(move || host.shared.spawn(options(&shell)).is_ok())
    };
    let deadline = Instant::now() + WAIT;
    while host.shared.session_threads.live() == 0 {
        assert!(Instant::now() < deadline, "the session never started opening");
        thread::sleep(Duration::from_millis(1));
    }
    host.shared.peers().stop = Some(reason);
    let stopped = {
        let host = host.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(host.run_until_idle(Duration::from_secs(60))));
        rx
    };
    assert_eq!(
        stopped.recv_timeout(Duration::from_millis(300)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the host stopped while a session was being opened"
    );
    drop(env);
    assert!(!opening.join().unwrap(), "a session was opened after the host stopped");
    assert_eq!(stopped.recv_timeout(WAIT), Ok(reason));
    assert!(!running(), "the shell was still running when the host stopped");
    assert_eq!(host.shared.session_threads.live(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
