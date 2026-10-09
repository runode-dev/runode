use runode_protocol::{BuildId, Caps};
use runode_shared_types::shell::IntegrationMode;

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

/// 经 `Host::connect_pair` 连上、按 `client` 握手，返回连接和读到的帧。
fn greet(host: &Host, client: ClientKind) -> (UnixStream, mpsc::Receiver<Frame>) {
    let mut stream = host.connect_pair().unwrap();
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
    write_frame(&mut stream, FrameKind::Input, channel, b"through\r").unwrap();
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
    out.queued.fetch_add(OUTBOX_LIMIT + 1, Ordering::Relaxed);
    send(&mut stream, &ClientMsg::Attach { id, size: None, mode: AttachMode::VtReplay });
    send(&mut stream, &ClientMsg::ReadScreen { id, lines: None, command: None });
    for _ in 0..2 {
        match message(&frames) {
            HostMsg::Error { id: Some(errored), message, .. } => {
                assert_eq!(errored, id);
                assert!(message.contains("backed up"), "{message}");
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
                assert!(message.contains("backed up"), "{message}");
            }
            other => panic!("expected the request to be refused: {other:?}"),
        }
    }
    out.queued.fetch_sub(OUTBOX_LIMIT + 1, Ordering::Relaxed);
    send(&mut stream, &ClientMsg::ReadScreen { id, lines: None, command: None });
    assert!(matches!(message(&frames), HostMsg::ScreenText { .. }));
    send(&mut stream, &ClientMsg::ListSessions);
    assert!(matches!(message(&frames), HostMsg::SessionList { .. }));
    host.shared.kill_all();
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

    send(&mut desktop, &ClientMsg::LayoutChanged);
    assert_eq!(message(&mobile_frames), HostMsg::LayoutChanged);
    // 没问过布局的连接收不到：它下一条收到的是自己请求的回话。
    send(&mut cli, &ClientMsg::ListSessions);
    assert!(matches!(message(&cli_frames), HostMsg::SessionList { .. }));

    send(&mut mobile, &ClientMsg::LayoutChanged);
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
    host.shared.kill_all();
}
