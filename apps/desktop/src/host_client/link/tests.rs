use std::{
    path::Path,
    time::{Duration, Instant},
};

use runode_host::Host;
use runode_terminal::session::Session;

use super::{
    reader::{dispatch, output, snapshot},
    *,
};

const WAIT: Duration = Duration::from_secs(10);
const SIZE: GridSize = GridSize { cols: 40, rows: 6, cell_width_px: 8, cell_height_px: 16 };
const BUILD: &str = "link-test";

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rnl-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 写一个当 shell 用的脚本；登录 shell 多带的参数它不看。
fn script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

fn connected(host: &Host) -> Link {
    let link = Link::new(BuildId(BUILD.into()));
    link.connect(host.connect_pair().unwrap()).unwrap();
    link
}

fn spawn(link: &Link, shell: String) -> SessionId {
    link.spawn(SpawnOptions {
        size: SIZE,
        cwd: None,
        integration: IntegrationMode::Off,
        start: true,
        shell: Some(shell),
        settings: None,
    })
    .unwrap()
}

fn next(rx: &mut UnboundedReceiver<LinkEvent>) -> LinkEvent {
    let deadline = Instant::now() + WAIT;
    loop {
        match rx.try_recv() {
            Ok(event) => return event,
            Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("the link dropped the session"),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            Err(_) => panic!("timed out waiting for an event"),
        }
    }
}

/// 等到输出里出现 `needle`，跳过别的事件。
fn wait_for_output(rx: &mut UnboundedReceiver<LinkEvent>, needle: &[u8]) {
    let mut seen = Vec::new();
    loop {
        if let LinkEvent::Output(data) = next(rx) {
            seen.extend_from_slice(&data);
            if seen.windows(needle.len()).any(|w| w == needle) {
                return;
            }
        }
    }
}

#[test]
fn attaching_assembles_a_snapshot_the_view_can_decode() {
    let dir = temp_dir("snapshot");
    let host = Host::new(BuildId(BUILD.into()));
    let link = connected(&host);
    let id = spawn(&link, script(&dir, "hello.sh", "printf 'ready\\n'\nexec /bin/cat"));
    // 先等输出出来，快照里才有它。
    let mut probe = link.attach(id, None, AttachMode::MetaOnly);
    assert!(matches!(next(&mut probe), LinkEvent::Screen(Screen { data, .. }) if data.is_empty()));
    let deadline = Instant::now() + WAIT;
    let (screen, _rx) = loop {
        let (screen, rx) = link.attach_now(id, Some(SIZE), AttachMode::Snapshot, WAIT).unwrap();
        assert_eq!(screen.attached.mode, AttachMode::Snapshot);
        assert_eq!(screen.attached.id, id);
        let session = Session::from_snapshot(&screen.data, Box::new(|_| {})).unwrap();
        if session.screen_text().unwrap_or_default().contains("ready") || Instant::now() > deadline {
            break (screen, rx);
        }
        thread::sleep(Duration::from_millis(20));
    };
    let session = Session::from_snapshot(&screen.data, Box::new(|_| {})).unwrap();
    assert!(session.screen_text().unwrap_or_default().contains("ready"), "{:?}", session.screen_text());
    link.kill(id);
}

#[test]
fn input_and_output_go_through_the_session_channel() {
    let dir = temp_dir("echo");
    let host = Host::new(BuildId(BUILD.into()));
    let link = connected(&host);
    let id = spawn(&link, script(&dir, "cat.sh", "exec /bin/cat"));
    let mut rx = link.attach(id, None, AttachMode::Snapshot);
    // 还没等到 `Attached` 就打字：攒着，知道通道后写出去。
    link.input(id, b"early\r");
    let LinkEvent::Screen(screen) = next(&mut rx) else { panic!("the first event is the screen") };
    assert!(screen.attached.channel > 0);
    wait_for_output(&mut rx, b"early");
    link.input(id, b"later\r");
    wait_for_output(&mut rx, b"later");
    link.kill(id);
}

#[test]
fn two_sessions_get_their_own_output() {
    let dir = temp_dir("two");
    let host = Host::new(BuildId(BUILD.into()));
    let link = connected(&host);
    let a = spawn(&link, script(&dir, "a.sh", "exec /bin/cat"));
    let b = spawn(&link, script(&dir, "b.sh", "exec /bin/cat"));
    let (_, mut rx_a) = link.attach_now(a, None, AttachMode::Snapshot, WAIT).unwrap();
    let (_, mut rx_b) = link.attach_now(b, None, AttachMode::Snapshot, WAIT).unwrap();
    link.input(a, b"apple\r");
    link.input(b, b"banana\r");
    wait_for_output(&mut rx_a, b"apple");
    wait_for_output(&mut rx_b, b"banana");
    // 各自只收到自己的。
    link.input(a, b"zzz\r");
    wait_for_output(&mut rx_a, b"zzz");
    while let Ok(event) = rx_b.try_recv() {
        if let LinkEvent::Output(data) = event {
            assert!(!data.windows(3).any(|w| w == b"zzz"));
        }
    }
    link.kill(a);
    link.kill(b);
}

#[test]
fn attaching_a_missing_session_fails() {
    let host = Host::new(BuildId(BUILD.into()));
    let link = connected(&host);
    let id = SessionId(42);
    assert!(link.attach_now(id, None, AttachMode::Snapshot, WAIT).is_err());
    let mut rx = link.attach(id, None, AttachMode::Snapshot);
    assert!(matches!(next(&mut rx), LinkEvent::Msg(HostMsg::Error { id: Some(got), .. }) if got == id));
}

#[test]
fn ui_requests_go_to_the_desktop_and_replies_come_back() {
    let host = Host::new(BuildId(BUILD.into()));
    let link = connected(&host);
    let mut requests = link.ui_requests().unwrap();
    assert!(link.ui_requests().is_none());
    // 命令行那一方经另一对 socket 连上来，请界面切到某个会话。
    let mut cli = host.connect_pair().unwrap();
    let send = |stream: &mut UnixStream, message: &ClientMsg| {
        let frame = Frame::control(message).unwrap();
        write_frame(stream, frame.kind, 0, &frame.payload).unwrap();
    };
    let receive = |stream: &mut UnixStream| loop {
        let frame = read_frame(stream).unwrap().unwrap();
        if frame.kind == FrameKind::Control {
            return frame.message::<HostMsg>().unwrap();
        }
    };
    let hello = ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: BuildId(BUILD.into()),
        client: ClientKind::Cli,
        caps: Caps::default(),
        session: None,
        device: None,
    };
    send(&mut cli, &hello);
    assert!(matches!(receive(&mut cli), HostMsg::Welcome { .. }));
    let id = SessionId(7);
    send(&mut cli, &ClientMsg::Reveal { req: 3, id });
    let deadline = Instant::now() + WAIT;
    let (ticket, request) = loop {
        match requests.try_recv() {
            Ok(request) => break request,
            _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => panic!("no ui request"),
        }
    };
    assert_eq!(request, ClientMsg::Reveal { req: 3, id });
    link.ui_reply(ticket, HostMsg::Done { req: 3 });
    assert_eq!(receive(&mut cli), HostMsg::Done { req: 3 });
}

/// 收到请求以后连接换过了（宿主换了一个）：回话不发给新的宿主，编号对它没有意义。
#[test]
fn answers_to_requests_from_an_earlier_connection_are_dropped() {
    let (send_request, request_sent) = mpsc::channel::<()>();
    let link = fake_host(move |mut stream| {
        let request = HostMsg::UiRequest { ui: 1, request: Box::new(ClientMsg::Layout { req: 2 }) };
        let frame = Frame::control(&request).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        let _ = send_request.send(());
        // 留着连接，直到对面换了一条。
        let _ = read_frame(&mut stream);
    });
    request_sent.recv_timeout(WAIT).unwrap();
    let mut requests = link.ui_requests().unwrap();
    let deadline = Instant::now() + WAIT;
    let (ticket, _) = loop {
        match requests.try_recv() {
            Ok(request) => break request,
            _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => panic!("no ui request"),
        }
    };
    let (seen, seen_rx) = mpsc::channel();
    let (ours, theirs) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut stream = theirs;
        let _hello = read_frame(&mut stream).unwrap().unwrap();
        let welcome = HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            host_pid: 2,
            snapshot_format: 1,
            standalone: true,
            handoff: 0,
        };
        let frame = Frame::control(&welcome).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        while let Ok(Some(frame)) = read_frame(&mut stream) {
            if seen.send(frame.message::<ClientMsg>().unwrap()).is_err() {
                break;
            }
        }
    });
    link.close();
    link.connect(ours).unwrap();
    link.ui_reply(ticket, HostMsg::Layout { req: 2, windows: Vec::new() });
    link.send(ClientMsg::ListSessions);
    assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), ClientMsg::ListSessions);
}

#[test]
fn frames_between_attach_and_attached_are_dropped() {
    let link = Link::new(BuildId(BUILD.into()));
    let id = SessionId(1);
    let (events, mut rx) = unbounded();
    {
        let mut state = link.inner.state();
        state.connected = true;
        let mut route = Route::new(events);
        route.channel = Some(1);
        state.sessions.insert(id, route);
        state.channels.insert(1, id);
    }
    output(&link.inner, 0, 1, b"live".to_vec());
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Output(data)) if data == b"live"));
    // 重新连上：这之后、新的 `Attached` 之前，旧订阅的输出和标记都不要。
    assert!(link.reattach(id, None, AttachMode::Snapshot));
    output(&link.inner, 0, 1, b"stale".to_vec());
    dispatch(&link.inner, 0, HostMsg::Resized { id, size: SIZE });
    dispatch(&link.inner, 0, HostMsg::Meta { id, meta: SessionMeta::default() });
    assert!(rx.try_recv().is_err(), "nothing reaches the view while attaching");
    dispatch(
        &link.inner,
        0,
        HostMsg::Attached {
            id,
            channel: 2,
            size: SIZE,
            mode: AttachMode::Snapshot,
            meta: SessionMeta::default(),
            settings: None,
        },
    );
    snapshot(&link.inner, 0, 2, b"snap");
    snapshot(&link.inner, 0, 2, b"shot");
    output(&link.inner, 0, 1, b"old channel".to_vec());
    dispatch(&link.inner, 0, HostMsg::SnapshotEnd { id });
    let LinkEvent::Screen(screen) = rx.try_recv().unwrap() else { panic!("expected the screen") };
    assert_eq!(screen.data, b"snapshot");
    assert_eq!(screen.attached.channel, 2);
    output(&link.inner, 0, 2, b"new".to_vec());
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Output(data)) if data == b"new"));
    dispatch(&link.inner, 0, HostMsg::Bell { id });
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Bell { .. }))));
    dispatch(&link.inner, 0, HostMsg::SizeOwner { id, mine: false, owner: Some("studio".into()) });
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::SizeOwner { mine: false, .. }))));
}

#[test]
fn two_attaches_in_a_row_wait_for_the_second_attached() {
    let link = Link::new(BuildId(BUILD.into()));
    link.inner.state().connected = true;
    let id = SessionId(1);
    let mut rx = link.attach(id, None, AttachMode::MetaOnly);
    assert!(link.reattach(id, None, AttachMode::MetaOnly));
    let attached = |channel| HostMsg::Attached {
        id,
        channel,
        size: SIZE,
        mode: AttachMode::MetaOnly,
        meta: SessionMeta::default(),
        settings: None,
    };
    dispatch(&link.inner, 0, attached(1));
    dispatch(&link.inner, 0, HostMsg::Meta { id, meta: SessionMeta::default() });
    assert!(rx.try_recv().is_err());
    dispatch(&link.inner, 0, attached(2));
    let LinkEvent::Screen(screen) = rx.try_recv().unwrap() else { panic!("expected the screen") };
    assert_eq!(screen.attached.channel, 2);
}

/// 连着、已经连上会话 `id`（通道 1）的 `Link`，返回收这个会话事件的一端。
fn attached_link(id: SessionId) -> (Link, UnboundedReceiver<LinkEvent>) {
    let link = Link::new(BuildId(BUILD.into()));
    let (events, rx) = unbounded();
    let mut state = link.inner.state();
    state.connected = true;
    let mut route = Route::new(events);
    route.channel = Some(1);
    state.sessions.insert(id, route);
    state.channels.insert(1, id);
    drop(state);
    (link, rx)
}

/// 连接换过以后，旧连接的读线程已经读进来的帧一律丢掉，不按新连接的通道和会话处理；宿主转来的
/// 请求的回执记的是收到它的那条连接。
#[test]
fn frames_from_an_earlier_connection_are_dropped() {
    let id = SessionId(1);
    let (link, mut rx) = attached_link(id);
    let mut requests = link.ui_requests().unwrap();
    link.inner.state().generation = 1;
    output(&link.inner, 0, 1, b"stale".to_vec());
    dispatch(&link.inner, 0, HostMsg::Bell { id });
    dispatch(&link.inner, 0, HostMsg::UiRequest { ui: 4, request: Box::new(ClientMsg::Layout { req: 1 }) });
    assert!(rx.try_recv().is_err());
    assert!(requests.try_recv().is_err());
    output(&link.inner, 1, 1, b"fresh".to_vec());
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Output(data)) if data == b"fresh"));
    dispatch(&link.inner, 1, HostMsg::UiRequest { ui: 5, request: Box::new(ClientMsg::Layout { req: 2 }) });
    let (ticket, _) = requests.try_recv().unwrap();
    assert_eq!(ticket, UiTicket::new(5, 1));
}

/// 重新连上时撞上会话被结束（比如 `runode kill`）：旧订阅的 `Exited` 先到、新的 `Attach` 因为
/// 会话没了回 `Error`。视图先收到 `Error`，再收到留下的 `Exited`，能关掉。
#[test]
fn an_exit_while_reattaching_reaches_the_view() {
    let id = SessionId(1);
    let (link, mut rx) = attached_link(id);
    assert!(link.reattach(id, None, AttachMode::Snapshot));
    dispatch(&link.inner, 0, HostMsg::Meta { id, meta: SessionMeta::default() });
    dispatch(&link.inner, 0, HostMsg::Exited { id, status: None });
    assert!(rx.try_recv().is_err(), "nothing reaches the view while attaching");
    dispatch(&link.inner, 0, HostMsg::Error { req: None, id: Some(id), message: format!("no session {id}") });
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Error { id: Some(got), .. })) if got == id));
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { id: got, .. })) if got == id));
    assert!(rx.try_recv().is_err());
}

/// 正连着时留下的 `Exited` 跟在新的屏幕后面交出去，快照和只看状态都一样；连接断了时先交它再
/// 交 `Lost`。
#[test]
fn an_exit_while_attaching_follows_the_new_screen() {
    let id = SessionId(1);
    let (link, mut rx) = attached_link(id);
    assert!(link.reattach(id, None, AttachMode::Snapshot));
    dispatch(&link.inner, 0, HostMsg::Exited { id, status: None });
    let attached = |channel, mode| HostMsg::Attached {
        id,
        channel,
        size: SIZE,
        mode,
        meta: SessionMeta::default(),
        settings: None,
    };
    dispatch(&link.inner, 0, attached(2, AttachMode::Snapshot));
    snapshot(&link.inner, 0, 2, b"snap");
    assert!(rx.try_recv().is_err(), "the exit waits for the screen");
    dispatch(&link.inner, 0, HostMsg::SnapshotEnd { id });
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Screen(screen)) if screen.data == b"snap"));
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { .. }))));

    assert!(link.reattach(id, None, AttachMode::MetaOnly));
    dispatch(&link.inner, 0, HostMsg::Exited { id, status: None });
    dispatch(&link.inner, 0, attached(3, AttachMode::MetaOnly));
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Screen(_))));
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { .. }))));

    assert!(link.reattach(id, None, AttachMode::MetaOnly));
    dispatch(&link.inner, 0, HostMsg::Exited { id, status: None });
    link.close();
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { .. }))));
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Lost)));
}

/// 设备名只要主机名第一个点之前的部分，不管加的是哪个域名。
#[test]
fn the_device_name_is_the_first_label_of_the_host_name() {
    assert_eq!(short_host_name("Mac.lan").as_deref(), Some("Mac"));
    assert_eq!(short_host_name("Ethans-MacBook-Pro.local").as_deref(), Some("Ethans-MacBook-Pro"));
    assert_eq!(short_host_name("studio.home.example.com").as_deref(), Some("studio"));
    assert_eq!(short_host_name("  build-box \n").as_deref(), Some("build-box"));
    assert_eq!(short_host_name("plain").as_deref(), Some("plain"));
    assert_eq!(short_host_name(""), None);
    assert_eq!(short_host_name(".lan"), None);
    // 这台机器上取得到的也是不带点的一段。
    if let Some(name) = device_name() {
        assert!(!name.is_empty() && !name.contains('.'), "{name}");
    }
}

#[test]
fn snapshots_need_the_same_format() {
    assert!(snapshots_usable(1, Some(1)));
    assert!(!snapshots_usable(2, Some(1)));
    assert!(!snapshots_usable(0, None));
    assert_eq!(attach_mode(AttachMode::Snapshot, true), AttachMode::Snapshot);
    assert_eq!(attach_mode(AttachMode::Snapshot, false), AttachMode::VtReplay);
    assert_eq!(attach_mode(AttachMode::VtReplay, true), AttachMode::VtReplay);
    assert_eq!(attach_mode(AttachMode::MetaOnly, false), AttachMode::MetaOnly);
}

/// 宿主编的快照格式和这边的对不上：要快照时改要 VT 重放，宿主给的也是重放，不至于拿到解不了的
/// 快照。
#[test]
fn a_host_with_another_snapshot_format_gets_asked_for_replays() {
    let (asked, asked_rx) = mpsc::channel();
    let (ours, theirs) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut stream = theirs;
        let _hello = read_frame(&mut stream).unwrap().unwrap();
        let welcome = HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            host_pid: 1,
            snapshot_format: local_snapshot_format().unwrap() + 1,
            standalone: true,
            handoff: 0,
        };
        let frame = Frame::control(&welcome).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        loop {
            let frame = read_frame(&mut stream).unwrap().unwrap();
            if let Ok(ClientMsg::Attach { mode, .. }) = frame.message::<ClientMsg>() {
                asked.send(mode).unwrap();
                return;
            }
        }
    });
    let link = Link::new(BuildId(BUILD.into()));
    link.connect(ours).unwrap();
    let _rx = link.attach(SessionId(1), Some(SIZE), AttachMode::Snapshot);
    assert_eq!(asked_rx.recv_timeout(WAIT).unwrap(), AttachMode::VtReplay);
}

/// 一个按脚本说话的宿主：握手后由 `then` 接着处理这条连接。
fn fake_host(then: impl FnOnce(UnixStream) + Send + 'static) -> Link {
    let (ours, theirs) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut stream = theirs;
        let hello = read_frame(&mut stream).unwrap().unwrap();
        assert!(matches!(hello.message::<ClientMsg>().unwrap(), ClientMsg::Hello { client: ClientKind::Desktop, .. }));
        let welcome = HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            host_pid: 1,
            snapshot_format: 1,
            standalone: true,
            handoff: 0,
        };
        let frame = Frame::control(&welcome).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        then(stream);
    });
    let link = Link::new(BuildId(BUILD.into()));
    link.connect(ours).unwrap();
    link
}

/// 读到 `Attach` 为止。
fn wait_for_attach(stream: &mut UnixStream) {
    loop {
        let frame = read_frame(stream).unwrap().unwrap();
        if matches!(frame.message::<ClientMsg>(), Ok(ClientMsg::Attach { .. })) {
            return;
        }
    }
}

#[test]
fn every_session_hears_when_the_host_goes_away() {
    let (attached, attached_rx) = mpsc::channel();
    let link = fake_host(move |mut stream| {
        wait_for_attach(&mut stream);
        wait_for_attach(&mut stream);
        attached.send(()).unwrap();
        // 断开。
    });
    let mut a = link.attach(SessionId(1), None, AttachMode::Snapshot);
    let mut b = link.attach(SessionId(2), None, AttachMode::Snapshot);
    attached_rx.recv_timeout(WAIT).unwrap();
    assert!(matches!(next(&mut a), LinkEvent::Lost));
    assert!(matches!(next(&mut b), LinkEvent::Lost));
    let deadline = Instant::now() + WAIT;
    while link.connected() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!link.connected());
    assert!(
        link.spawn(SpawnOptions {
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: None,
            settings: None,
        })
        .is_err()
    );
    assert!(matches!(next(&mut link.attach(SessionId(3), None, AttachMode::Snapshot)), LinkEvent::Lost));
}

/// 宿主卡住了、socket 却没断：一个请求等不到回话，这条连接就按断开处理，会话收到 `Lost`，之后的
/// 请求立刻失败，不再每次等满超时。
#[test]
fn a_request_that_times_out_drops_the_connection() {
    let (stuck, stuck_rx) = mpsc::channel::<()>();
    let link = fake_host(move |mut stream| {
        wait_for_attach(&mut stream);
        // 读着不回，直到测试结束。
        let _ = stuck_rx.recv();
        drop(stream);
    });
    let mut rx = link.attach(SessionId(1), None, AttachMode::Snapshot);
    let wait = Duration::from_millis(100);
    assert!(link.list_sessions(wait).is_err());
    assert!(!link.connected());
    assert!(matches!(next(&mut rx), LinkEvent::Lost));
    let started = Instant::now();
    assert!(link.list_sessions(WAIT).is_err());
    assert!(link.attach_now(SessionId(1), None, AttachMode::Snapshot, WAIT).is_err());
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    drop(stuck);
}

/// 开会话超时了连接照旧：宿主之后才回的 `Spawned` 没人等，读线程结束那个会话，不让它留在宿主里。
#[test]
fn a_terminal_spawned_after_the_caller_gave_up_is_ended() {
    let (gave_up, gave_up_rx) = mpsc::channel::<()>();
    let (killed, killed_rx) = mpsc::channel();
    let link = fake_host(move |mut stream| {
        let req = loop {
            let frame = read_frame(&mut stream).unwrap().unwrap();
            if let Ok(ClientMsg::Spawn { req, .. }) = frame.message::<ClientMsg>() {
                break req;
            }
        };
        gave_up_rx.recv().unwrap();
        let frame = Frame::control(&HostMsg::Spawned { req, id: SessionId(7) }).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        while let Ok(Some(frame)) = read_frame(&mut stream) {
            if let Ok(ClientMsg::Kill { id }) = frame.message::<ClientMsg>() {
                let _ = killed.send(id);
                return;
            }
        }
    });
    let options = SpawnOptions {
        size: SIZE,
        cwd: None,
        integration: IntegrationMode::Off,
        start: true,
        shell: None,
        settings: None,
    };
    assert!(link.spawn_within(options, Duration::from_millis(100)).is_err());
    assert!(link.connected());
    gave_up.send(()).unwrap();
    assert_eq!(killed_rx.recv_timeout(WAIT).unwrap(), SessionId(7));
}

#[test]
fn a_shutdown_goodbye_ends_every_session() {
    let link = fake_host(|mut stream| {
        wait_for_attach(&mut stream);
        let goodbye = HostMsg::Goodbye { reason: runode_protocol::GoodbyeReason::Shutdown };
        let frame = Frame::control(&goodbye).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
    });
    let id = SessionId(1);
    let mut rx = link.attach(id, None, AttachMode::Snapshot);
    assert!(matches!(next(&mut rx), LinkEvent::Msg(HostMsg::Exited { id: got, .. }) if got == id));
}

#[test]
fn theme_and_options_are_sent_again_after_reconnecting() {
    let host = Host::new(BuildId(BUILD.into()));
    let link = connected(&host);
    let settings = TermSettings { scrollback_limit: 3 << 20, ..TermSettings::default() };
    link.send(ClientMsg::SetTheme { settings: settings.clone() });
    let clipboard = ClipboardAccess {
        write: runode_shared_types::clipboard::ClipboardWrite::Deny,
        read: runode_shared_types::clipboard::ClipboardRead::Allow,
    };
    link.send(ClientMsg::SetOptions { record_history: false, clipboard });
    let (seen, seen_rx) = mpsc::channel();
    let (ours, theirs) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut stream = theirs;
        let _hello = read_frame(&mut stream).unwrap().unwrap();
        let welcome = HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            host_pid: 1,
            snapshot_format: 1,
            standalone: true,
            handoff: 0,
        };
        let frame = Frame::control(&welcome).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        for _ in 0..2 {
            let frame = read_frame(&mut stream).unwrap().unwrap();
            seen.send(frame.message::<ClientMsg>().unwrap()).unwrap();
        }
    });
    link.connect(ours).unwrap();
    assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), ClientMsg::SetTheme { settings });
    assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), ClientMsg::SetOptions { record_history: false, clipboard });
}

/// 基准：经 `Link` 的按键到回显延迟和 `cat` 大文件的吞吐，宿主跑在 app 里（一对 socket）和单独
/// 一个进程（`runode --host`，要先构建出 runode 本身）各一份。手动跑：
///
/// ```sh
/// cargo build --release -p runode
/// cargo test --release -p runode bench_ -- --ignored --nocapture --test-threads=1
/// ```
///
/// `RUNODE_LATENCY_N` 改回显的次数（默认 1 万），`RUNODE_THROUGHPUT_MB` 改文件大小（默认 100）。
mod bench {
    use std::process::{Child, Command};

    use futures::StreamExt as _;

    use super::*;

    const LATENCY_SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
    const THROUGHPUT_SIZE: GridSize = GridSize { cols: 200, rows: 50, cell_width_px: 8, cell_height_px: 16 };
    const DONE: &[u8] = b"RUNODE-DONE";

    #[allow(clippy::print_stderr)]
    fn report(line: &str) {
        eprintln!("{line}");
    }

    /// 宿主在哪里跑；单独一个进程的那个在丢掉时结束。跑在 app 里的拿着它，基准跑完前不丢掉。
    enum Where {
        InApp(#[allow(dead_code)] Host),
        Process(Child, PathBuf),
    }

    impl Drop for Where {
        fn drop(&mut self) {
            if let Self::Process(child, dir) = self {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn in_app() -> (Where, Link) {
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        (Where::InApp(host), link)
    }

    /// 拉起 `runode --host`，家目录换成临时目录、socket 放在它的 `.runode` 里，经 socket 连上。
    fn process(name: &str) -> (Where, Link) {
        let exe = std::env::current_exe().unwrap();
        let runode = exe.parent().unwrap().parent().unwrap().join("runode");
        assert!(runode.exists(), "build runode first: {}", runode.display());
        let config = PathBuf::from(format!("/tmp/rnb-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        std::fs::create_dir_all(&config).unwrap();
        let child = Command::new(&runode).arg("--host").env("HOME", &config).spawn().unwrap();
        let dirs = runode_paths::Dirs { data: Some(config.join(".runode")), ..Default::default() };
        let socket = dirs.host_socket_file().unwrap();
        let deadline = Instant::now() + WAIT;
        // 单独一个进程的宿主和 app 的构建一样，不然给不了快照。
        let link = Link::new(BuildId(env!("RUNODE_BUILD").into()));
        loop {
            if let Ok(stream) = UnixStream::connect(&socket)
                && link.connect(stream).is_ok()
            {
                break;
            }
            assert!(Instant::now() < deadline, "the host process did not come up");
            thread::sleep(Duration::from_millis(5));
        }
        (Where::Process(child, config), link)
    }

    fn iterations() -> usize {
        std::env::var("RUNODE_LATENCY_N").ok().and_then(|n| n.parse().ok()).unwrap_or(10_000)
    }

    fn spawn_at(link: &Link, shell: String, size: GridSize) -> SessionId {
        link.spawn(SpawnOptions {
            size,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some(shell),
            settings: None,
        })
        .unwrap()
    }

    fn output(rx: &mut UnboundedReceiver<LinkEvent>) -> Vec<u8> {
        loop {
            match futures::executor::block_on(rx.next()).expect("the link dropped the session") {
                LinkEvent::Output(data) => return data,
                LinkEvent::Lost => panic!("lost the host"),
                _ => {}
            }
        }
    }

    fn latency(name: &str, (_host, link): (Where, Link)) {
        let dir = temp_dir(&format!("bench-echo-{name}"));
        let id = spawn_at(&link, script(&dir, "echo.sh", "stty raw -echo\nexec /bin/cat -u"), LATENCY_SIZE);
        let (_, mut rx) = link.attach_now(id, None, AttachMode::Snapshot, WAIT).unwrap();
        thread::sleep(Duration::from_millis(500));
        link.input(id, b"!");
        while !output(&mut rx).contains(&b'!') {}
        let mut samples = Vec::with_capacity(iterations());
        for i in 0..iterations() {
            let byte = b'a' + (i % 26) as u8;
            let start = Instant::now();
            link.input(id, &[byte]);
            while !output(&mut rx).contains(&byte) {}
            samples.push(start.elapsed());
        }
        link.kill(id);
        samples.sort();
        let pct = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
        report(&format!(
            "link {name} echo n={} p50={:?} p90={:?} p99={:?} max={:?}",
            samples.len(),
            pct(0.5),
            pct(0.9),
            pct(0.99),
            samples[samples.len() - 1]
        ));
    }

    fn throughput(name: &str, (_host, link): (Where, Link)) {
        let dir = temp_dir(&format!("bench-cat-{name}"));
        let mb: usize = std::env::var("RUNODE_THROUGHPUT_MB").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
        let data = dir.join("data.txt");
        let line: String = (0..99).map(|i| char::from(b'!' + (i % 90) as u8)).collect::<String>() + "\n";
        std::fs::write(&data, line.repeat(mb * 1024 * 1024 / line.len())).unwrap();
        let body = format!("stty -echo\nread go\ncat {}\nprintf 'RUNODE-%s\\n' DONE\nexec /bin/cat", data.display());
        let id = spawn_at(&link, script(&dir, "cat.sh", &body), THROUGHPUT_SIZE);
        let (screen, mut rx) = link.attach_now(id, None, AttachMode::Snapshot, WAIT).unwrap();
        let mut session = Session::from_snapshot(&screen.data, Box::new(|_| {})).unwrap();
        thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        link.input(id, b"go\r");
        let (mut tail, mut total) = (Vec::new(), 0);
        loop {
            let chunk = output(&mut rx);
            session.feed(&chunk);
            total += chunk.len();
            tail.extend_from_slice(&chunk);
            if tail.windows(DONE.len()).any(|w| w == DONE) {
                break;
            }
            let keep = tail.len().saturating_sub(DONE.len());
            tail.drain(..keep);
        }
        let elapsed = start.elapsed();
        link.kill(id);
        let _ = std::fs::remove_dir_all(dir);
        report(&format!(
            "link {name} cat {mb} MiB: {total} bytes in {elapsed:?} = {:.1} MiB/s",
            total as f64 / 1048576.0 / elapsed.as_secs_f64()
        ));
    }

    #[test]
    #[ignore = "测量延迟，手动跑"]
    fn bench_echo_latency_in_app() {
        latency("in-app", in_app());
    }

    #[test]
    #[ignore = "测量延迟，手动跑；要先构建 runode"]
    fn bench_echo_latency_host_process() {
        latency("host-process", process("echo"));
    }

    #[test]
    #[ignore = "测量吞吐，手动跑"]
    fn bench_cat_throughput_in_app() {
        throughput("in-app", in_app());
    }

    #[test]
    #[ignore = "测量吞吐，手动跑；要先构建 runode"]
    fn bench_cat_throughput_host_process() {
        throughput("host-process", process("cat"));
    }
}
