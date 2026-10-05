//! 经 Unix socket 连上宿主的黑盒测试：握手、开会话、连上、发输入、读屏幕、列会话，以及同一时间
//! 只有一个宿主能监听。

use std::{
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, Host, HostMsg, SessionId, SpawnOptions};
use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, Frame, FrameKind, PROTOCOL_VERSION, read_frame, write_frame,
};
use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
const WAIT: Duration = Duration::from_secs(10);
const BUILD: &str = "test-build";

/// 一个空的临时目录，socket 和锁放在里面。路径要短，放得进 `sun_path`。
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rnh-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 开一个宿主监听 `dir` 里的 socket。
fn listen(dir: &std::path::Path) -> (Host, PathBuf) {
    let host = Host::new();
    let socket = dir.join("host.sock");
    host.listen(&socket, &dir.join("host.lock"), BuildId(BUILD.into())).unwrap();
    (host, socket)
}

/// 连上 socket 的一个前端：读到的帧由后台线程交过来，测试里按超时等。
struct Peer {
    stream: UnixStream,
    frames: mpsc::Receiver<Frame>,
}

impl Peer {
    fn connect(socket: &std::path::Path) -> Self {
        let stream = UnixStream::connect(socket).unwrap();
        let mut reader = stream.try_clone().unwrap();
        let (tx, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        Self { stream, frames }
    }

    /// 连上并握手，`snapshot` 时说自己能解同一个构建的快照。
    fn hello(socket: &std::path::Path, snapshot: bool) -> Self {
        let mut peer = Self::connect(socket);
        peer.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            client: ClientKind::Cli,
            caps: Caps { snapshot, vt_replay: true },
        });
        assert!(matches!(peer.message(), HostMsg::Welcome { protocol: PROTOCOL_VERSION, .. }));
        peer
    }

    fn send(&mut self, message: &ClientMsg) {
        let frame = Frame::control(message).unwrap();
        write_frame(&mut self.stream, frame.kind, 0, &frame.payload).unwrap();
    }

    fn input(&mut self, channel: u32, data: &[u8]) {
        write_frame(&mut self.stream, FrameKind::Input, channel, data).unwrap();
    }

    fn frame(&self) -> Frame {
        self.frames.recv_timeout(WAIT).expect("timed out waiting for a frame")
    }

    /// 下一条控制消息，跳过中间的输出帧。
    fn message(&self) -> HostMsg {
        loop {
            let frame = self.frame();
            if frame.kind == FrameKind::Control {
                return frame.message().unwrap();
            }
        }
    }

    /// 回话：下一条不是 `Meta`、`CommandFinished` 这类随时会插进来的状态的控制消息。
    fn reply(&self) -> HostMsg {
        loop {
            match self.message() {
                HostMsg::Meta { .. } | HostMsg::CommandFinished { .. } => {}
                message => return message,
            }
        }
    }

    /// 等到输出里出现 `needle`。
    fn wait_for_output(&self, channel: u32, needle: &[u8]) {
        let deadline = Instant::now() + WAIT;
        let mut output = Vec::new();
        while !output.windows(needle.len()).any(|w| w == needle) {
            let frame =
                self.frames.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("timed out");
            if frame.kind == FrameKind::Output && frame.channel == channel {
                output.extend_from_slice(&frame.payload);
            }
        }
    }

    /// 连上会话，返回它的通道和快照（或 VT 重放）的字节。
    fn attach(&mut self, id: SessionId, mode: AttachMode) -> (u32, Vec<u8>) {
        self.send(&ClientMsg::Attach { id, size: Some(SIZE), mode });
        let HostMsg::Attached { id: attached, channel, size, mode: given, .. } = self.reply() else {
            panic!("expected attached");
        };
        assert_eq!((attached, size), (id, SIZE));
        let mut screen = Vec::new();
        if given != AttachMode::MetaOnly {
            loop {
                let frame = self.frame();
                match frame.kind {
                    FrameKind::Snapshot => screen.extend_from_slice(&frame.payload),
                    FrameKind::Control => {
                        assert!(matches!(frame.message().unwrap(), HostMsg::SnapshotEnd { id: end } if end == id));
                        break;
                    }
                    kind => panic!("unexpected {kind:?} before the snapshot ended"),
                }
            }
        }
        (channel, screen)
    }
}

/// 开一个 `cat` 当 shell 的会话。经 socket 开的会话用的是用户的 shell，所以从进程内开。
fn cat(host: &Host) -> SessionId {
    host.connect_in_process()
        .spawn(SpawnOptions {
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some("/bin/cat".into()),
            settings: None,
        })
        .unwrap()
}

/// 协议版本对不上时回 `Incompatible` 后断开。
#[test]
fn a_different_protocol_is_refused() {
    let dir = temp_dir("proto");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::connect(&socket);
    peer.send(&ClientMsg::Hello {
        protocol: PROTOCOL_VERSION + 1,
        build: BuildId(BUILD.into()),
        client: ClientKind::Cli,
        caps: Caps::default(),
    });
    assert!(matches!(peer.message(), HostMsg::Incompatible { protocol: PROTOCOL_VERSION, .. }));
    assert!(peer.frames.recv_timeout(WAIT).is_err(), "the host should close the connection");
}

/// 输入帧写给会话里的程序，输出帧原样回来；读屏幕和列会话看得到同一个会话。
#[test]
fn input_and_output_flow_over_the_socket() {
    let dir = temp_dir("io");
    let (host, socket) = listen(&dir);
    let id = cat(&host);
    let mut peer = Peer::hello(&socket, false);
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    peer.input(channel, b"hello\r");
    peer.wait_for_output(channel, b"hello");

    peer.send(&ClientMsg::ReadScreen { id, lines: None });
    let HostMsg::ScreenText { text, .. } = peer.reply() else { panic!("expected screen text") };
    assert!(text.contains("hello"), "{text:?}");

    peer.send(&ClientMsg::ListSessions);
    let HostMsg::SessionList { sessions } = peer.reply() else { panic!("expected a session list") };
    let info = sessions.iter().find(|info| info.id == id).expect("the session is listed");
    assert_eq!((info.size, info.clients, info.exited), (SIZE, 1, false));

    peer.send(&ClientMsg::Kill { id });
}

/// 后连上来的前端先拿到现在的屏幕：构建一样时是快照，否则是 VT 重放。
#[test]
fn a_late_client_gets_the_current_screen() {
    let dir = temp_dir("late");
    let (host, socket) = listen(&dir);
    let id = cat(&host);
    let mut first = Peer::hello(&socket, false);
    let (channel, _) = first.attach(id, AttachMode::VtReplay);
    first.input(channel, b"earlier\r");
    first.wait_for_output(channel, b"earlier");

    let mut replay = Peer::hello(&socket, false);
    let (_, screen) = replay.attach(id, AttachMode::Snapshot);
    assert!(screen.windows(7).any(|w| w == b"earlier"), "the replay redraws the screen");

    let mut snapshot = Peer::hello(&socket, true);
    let (_, screen) = snapshot.attach(id, AttachMode::Snapshot);
    assert!(screen.starts_with(b"GHOSTSNP"), "same build gets a snapshot");

    first.send(&ClientMsg::Kill { id });
}

/// 只看状态的前端收不到输出。
#[test]
fn meta_only_clients_get_no_output() {
    let dir = temp_dir("meta");
    let (host, socket) = listen(&dir);
    let id = cat(&host);
    let mut watcher = Peer::hello(&socket, false);
    watcher.attach(id, AttachMode::MetaOnly);
    let mut typist = Peer::hello(&socket, false);
    let (channel, _) = typist.attach(id, AttachMode::VtReplay);
    typist.input(channel, b"quiet\r");
    typist.wait_for_output(channel, b"quiet");
    while let Ok(frame) = watcher.frames.recv_timeout(Duration::from_millis(200)) {
        assert_ne!(frame.kind, FrameKind::Output);
    }
    typist.send(&ClientMsg::Kill { id });
}

/// 经 socket 开会话，连上的是用户的 shell。
#[test]
fn sessions_can_be_spawned_over_the_socket() {
    let dir = temp_dir("spawn");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    peer.send(&ClientMsg::Spawn { req: 7, size: SIZE, cwd: None, integration: IntegrationMode::Off });
    let HostMsg::Spawned { req: 7, id } = peer.reply() else { panic!("expected spawned") };
    peer.attach(id, AttachMode::VtReplay);
    peer.send(&ClientMsg::Kill { id });
}

/// 一个宿主在监听时，另一个拿不到锁；上次留下的 socket 文件不挡路。
#[test]
fn only_one_host_listens() {
    let dir = temp_dir("lock");
    std::fs::write(dir.join("host.sock"), b"stale").unwrap();
    let (_host, socket) = listen(&dir);
    Peer::hello(&socket, false);
    let err = Host::new().listen(&socket, &dir.join("host.lock"), BuildId(BUILD.into())).unwrap_err();
    assert!(err.to_string().contains("another runode"), "{err:#}");
}

/// 同一条连接再连一次同一个会话：换一个通道，旧通道作废，输入走新通道。
#[test]
fn attaching_again_replaces_the_channel() {
    let dir = temp_dir("again");
    let (host, socket) = listen(&dir);
    let id = cat(&host);
    let mut peer = Peer::hello(&socket, false);
    let (old, _) = peer.attach(id, AttachMode::VtReplay);
    let (new, _) = peer.attach(id, AttachMode::VtReplay);
    assert_ne!(old, new);
    peer.input(old, b"ignored\r");
    peer.input(new, b"again\r");
    peer.wait_for_output(new, b"again");
    peer.send(&ClientMsg::ListSessions);
    let HostMsg::SessionList { sessions } = peer.reply() else { panic!("expected a session list") };
    assert_eq!(sessions.iter().find(|info| info.id == id).map(|info| info.clients), Some(1));
    peer.send(&ClientMsg::ReadScreen { id, lines: None });
    let HostMsg::ScreenText { text, .. } = peer.reply() else { panic!("expected screen text") };
    assert!(!text.contains("ignored"), "{text:?}");
    peer.send(&ClientMsg::Kill { id });
}

/// 宿主跑在 app 里，别的进程让它退出、交接，或者改主题和选项，都回 `Error`，连接照旧。
#[test]
fn app_owned_requests_are_refused() {
    let dir = temp_dir("refuse");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    for message in [
        ClientMsg::Shutdown { kill_sessions: true },
        ClientMsg::Handoff,
        ClientMsg::SetTheme { settings: Default::default() },
        ClientMsg::SetOptions { record_history: false },
    ] {
        peer.send(&message);
        assert!(matches!(peer.reply(), HostMsg::Error { .. }), "{message:?}");
    }
    peer.send(&ClientMsg::ListSessions);
    assert!(matches!(peer.reply(), HostMsg::SessionList { .. }));
}

/// 会话被结束时，连着的前端收到 `Exited`。
#[test]
fn killing_a_session_tells_its_clients() {
    let dir = temp_dir("kill");
    let (host, socket) = listen(&dir);
    let id = cat(&host);
    let mut watcher = Peer::hello(&socket, false);
    watcher.attach(id, AttachMode::MetaOnly);
    host.connect_in_process().send(ClientMsg::Kill { id });
    assert!(matches!(watcher.reply(), HostMsg::Exited { id: exited, .. } if exited == id));
}

/// shell 退出以后才连上来的前端，在 `Attached` 之后马上收到 `Exited`。
#[test]
fn a_late_client_learns_the_shell_has_exited() {
    let dir = temp_dir("dead");
    let (host, socket) = listen(&dir);
    let client = host.connect_in_process();
    let id = cat(&host);
    let mut first = Peer::hello(&socket, false);
    let (channel, _) = first.attach(id, AttachMode::MetaOnly);
    first.input(channel, b"\x04");
    assert!(matches!(first.reply(), HostMsg::Exited { .. }));
    let mut late = Peer::hello(&socket, false);
    late.attach(id, AttachMode::MetaOnly);
    assert!(matches!(late.reply(), HostMsg::Exited { id: exited, .. } if exited == id));
    client.send(ClientMsg::Kill { id });
}

/// `Open`、`Reveal` 交给登记的界面去办，界面的回话原样转回来；没有界面、或者界面没回话就丢掉
/// 请求时回 `Error`。
#[test]
fn window_requests_go_to_the_app() {
    use runode_protocol::Placement;

    let dir = temp_dir("ui");
    let (host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let open = |req| ClientMsg::Open { req, placement: Placement::Tab, near: None, cwd: None, focus: false };
    peer.send(&open(1));
    assert!(matches!(peer.reply(), HostMsg::Error { req: Some(1), .. }));

    host.set_ui(Box::new(|request| match request.message {
        ClientMsg::Open { req, .. } => request.reply(HostMsg::Opened { req, id: SessionId(9) }),
        _ => drop(request),
    }));
    peer.send(&open(2));
    assert_eq!(peer.reply(), HostMsg::Opened { req: 2, id: SessionId(9) });
    peer.send(&ClientMsg::Reveal { req: 3, id: SessionId(9) });
    assert!(matches!(peer.reply(), HostMsg::Error { req: Some(3), .. }));
}
