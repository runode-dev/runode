//! 经 Unix socket 连上宿主的黑盒测试：握手、开会话、连上、发输入、读屏幕、列会话，同一时间
//! 只有一个宿主能监听；转给界面的请求、响铃、`claimed`，单独一个进程跑时的退出，以及认对端是不是
//! 同一个用户。

mod common;

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use common::{BUILD, Peer, SIZE, WAIT, contains, host, listen, script, temp_dir};
use runode_host::{ClientMsg, Host, HostMsg, Placement, SessionId, Stopped};
use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, FrameKind, GoodbyeReason, PROTOCOL_VERSION, PaneLayout, PaneRect, TabLayout,
    WindowLayout, WorkspaceLayout,
};
use runode_types::{clipboard::ClipboardAccess, settings::TermSettings, shell::IntegrationMode};

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
        session: None,
        device: None,
    });
    assert!(matches!(peer.message(), HostMsg::Incompatible { protocol: PROTOCOL_VERSION, .. }));
    // 断开（读的线程读到头）才算，等满 `WAIT` 也没动静的不算。
    assert!(
        matches!(peer.next_frame(WAIT), Err(mpsc::RecvTimeoutError::Disconnected)),
        "the host should close the connection"
    );
}

/// 输入帧写给会话里的程序，输出帧原样回来；读屏幕和列会话看得到同一个会话。
#[test]
fn input_and_output_flow_over_the_socket() {
    let dir = temp_dir("io");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn("/bin/cat");
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    peer.input(channel, b"hello\r");
    peer.wait_for_output(channel, b"hello");

    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: None });
    let HostMsg::ScreenText { text, truncated, .. } = peer.reply() else { panic!("expected screen text") };
    assert!(text.contains("hello") && !truncated, "{text:?}");

    let sessions = peer.sessions();
    let info = sessions.iter().find(|info| info.id == id).expect("the session is listed");
    assert_eq!((info.size, info.clients, info.claimed, info.exited), (SIZE, 1, false, false));

    peer.send(&ClientMsg::Kill { id });
}

/// 后连上来的前端先拿到现在的屏幕：构建一样时是快照，否则是 VT 重放。`Attached` 带着宿主那份
/// VT 套着的主题。
#[test]
fn a_late_client_gets_the_current_screen() {
    let dir = temp_dir("late");
    let (_host, socket) = listen(&dir);
    let mut first = Peer::hello(&socket, false);
    let id = first.spawn("/bin/cat");
    let (channel, _) = first.attach(id, AttachMode::VtReplay);
    first.input(channel, b"earlier\r");
    first.wait_for_output(channel, b"earlier");

    let mut replay = Peer::hello(&socket, false);
    replay.send(&ClientMsg::Attach { id, size: Some(SIZE), mode: AttachMode::Snapshot });
    let HostMsg::Attached { channel, mode, settings, .. } = replay.reply() else { panic!("expected attached") };
    assert_eq!((mode, settings), (AttachMode::VtReplay, Some(TermSettings::default())));
    let screen = replay.screen(id, channel, mode);
    assert!(contains(&screen, b"earlier"), "the replay redraws the screen");

    let mut snapshot = Peer::hello(&socket, true);
    let (_, screen) = snapshot.attach(id, AttachMode::Snapshot);
    assert!(screen.starts_with(b"GHOSTSNP"), "same build gets a snapshot");

    first.send(&ClientMsg::Kill { id });
}

/// 只看状态的前端收不到输出。
#[test]
fn meta_only_clients_get_no_output() {
    let dir = temp_dir("meta");
    let (_host, socket) = listen(&dir);
    let mut typist = Peer::hello(&socket, false);
    let id = typist.spawn("/bin/cat");
    let mut watcher = Peer::hello(&socket, false);
    watcher.attach(id, AttachMode::MetaOnly);
    let (channel, _) = typist.attach(id, AttachMode::VtReplay);
    typist.input(channel, b"quiet\r");
    typist.wait_for_output(channel, b"quiet");
    while let Ok(frame) = watcher.next_frame(Duration::from_millis(200)) {
        assert_ne!(frame.kind, FrameKind::Output);
    }
    typist.send(&ClientMsg::Kill { id });
}

/// 经 socket 开会话，不指定程序时连上的是用户的 shell。
#[test]
fn sessions_can_be_spawned_over_the_socket() {
    let dir = temp_dir("spawn");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    peer.send(&ClientMsg::Spawn {
        req: 7,
        size: SIZE,
        cwd: None,
        integration: IntegrationMode::Off,
        start: true,
        shell: None,
        settings: None,
    });
    let HostMsg::Spawned { req: 7, id } = peer.reply() else { panic!("expected spawned") };
    peer.attach(id, AttachMode::VtReplay);
    peer.send(&ClientMsg::Kill { id });
}

/// `start` 为假时只开好伪终端：标题和目录是起始目录的，`Start` 后程序才跑起来，带着宿主设的环境
/// 变量和会话自己的标识。
#[test]
fn sessions_spawned_unstarted_start_on_request() {
    let dir = temp_dir("start");
    let (host, socket) = listen(&dir);
    host.set_env("RUNODE_TEST", "host");
    let mut peer = Peer::hello(&socket, false);
    let shell = script(&dir, "env.sh", "env\nexec /bin/cat");
    let id = peer.spawn_with(&shell, false, Some(dir.clone()));
    peer.send(&ClientMsg::Attach { id, size: Some(SIZE), mode: AttachMode::VtReplay });
    let HostMsg::Attached { channel, mode, meta, .. } = peer.reply() else { panic!("expected attached") };
    peer.screen(id, channel, mode);
    assert_eq!(meta.cwd, Some(dir));
    peer.send(&ClientMsg::Start { id, integration: IntegrationMode::Off });
    let output = peer.wait_for_output(channel, b"RUNODE_TEST=");
    peer.input(channel, b"go\r");
    let output = [output, peer.wait_for_output(channel, b"go")].concat();
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("RUNODE_TEST=host"), "{output}");
    assert!(output.contains(&format!("RUNODE_SESSION={id}")), "{output}");
    // 没有这个会话时回 `Error`。
    peer.send(&ClientMsg::Start { id: SessionId(1), integration: IntegrationMode::Off });
    assert!(matches!(peer.reply(), HostMsg::Error { id: Some(SessionId(1)), .. }));
    peer.send(&ClientMsg::Kill { id });
}

/// 一个宿主在监听时，另一个拿不到锁；上次留下的 socket 文件不挡路。
#[test]
fn only_one_host_listens() {
    let dir = temp_dir("lock");
    std::fs::write(dir.join("host.sock"), b"stale").unwrap();
    let (_host, socket) = listen(&dir);
    Peer::hello(&socket, false);
    let err = host().listen(&socket, &dir.join("host.lock")).unwrap_err();
    assert!(err.to_string().contains("another runode"), "{err:#}");
}

/// 同一条连接再连一次同一个会话：换一个通道，旧通道作废，输入走新通道。
#[test]
fn attaching_again_replaces_the_channel() {
    let dir = temp_dir("again");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn("/bin/cat");
    let (old, _) = peer.attach(id, AttachMode::VtReplay);
    let (new, _) = peer.attach(id, AttachMode::VtReplay);
    assert_ne!(old, new);
    peer.input(old, b"ignored\r");
    peer.input(new, b"again\r");
    peer.wait_for_output(new, b"again");
    let sessions = peer.sessions();
    assert_eq!(sessions.iter().find(|info| info.id == id).map(|info| info.clients), Some(1));
    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: None });
    let HostMsg::ScreenText { text, .. } = peer.reply() else { panic!("expected screen text") };
    assert!(!text.contains("ignored"), "{text:?}");
    peer.send(&ClientMsg::Kill { id });
}

/// 从看屏幕切到只看状态再切回来：旧通道的帧都在下一个 `Attached` 之前，新通道的帧（快照和输出）
/// 都在它自己的 `Attached` 之后，前端按 `Attached` 切换不会混进别的通道的东西。
#[test]
fn switching_attach_modes_keeps_channels_apart() {
    let dir = temp_dir("switch");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::desktop(&socket);
    // 不停地输出，但不刷屏：刷屏时调试构建里会话线程积压的输出要处理好一阵，`Attach` 排在后面。
    let id = peer.spawn(&script(&dir, "ticker.sh", "while :; do echo runode-switch; sleep 0.002; done"));
    let (first, _) = peer.attach(id, AttachMode::Snapshot);
    peer.wait(first, |_, output| output.len() > 512);
    peer.send(&ClientMsg::Attach { id, size: None, mode: AttachMode::MetaOnly });
    peer.send(&ClientMsg::Attach { id, size: Some(SIZE), mode: AttachMode::Snapshot });
    // 通道按 `Attached` 认：之前的帧可能还是旧通道的。
    let mut attached: Vec<u32> = vec![first];
    let mut last_output = 0;
    let deadline = Instant::now() + WAIT;
    while attached.len() < 3 || last_output < 2048 {
        assert!(Instant::now() < deadline, "timed out with {attached:?}");
        let frame = peer.frame();
        let current = *attached.last().unwrap();
        match frame.kind {
            FrameKind::Control => {
                if let HostMsg::Attached { channel, .. } = frame.message().unwrap() {
                    attached.push(channel);
                }
            }
            FrameKind::Output | FrameKind::Snapshot => {
                assert_eq!(frame.channel, current, "a {:?} frame on another channel than {attached:?}", frame.kind);
                assert_ne!(attached.len(), 2, "a {:?} frame while only watching the meta", frame.kind);
                if attached.len() == 3 {
                    last_output += frame.payload.len();
                }
            }
            FrameKind::Input => unreachable!(),
        }
    }
    assert_eq!(attached.iter().collect::<std::collections::HashSet<_>>().len(), 3);
    peer.send(&ClientMsg::Kill { id });
}

/// 跑在 app 里（没调 `Host::run_until_idle`）时 `Shutdown` 只结束所有会话，连接照旧；要留着
/// 会话退出、交接都还做不了，回 `Error`。
#[test]
fn shutting_down_inside_the_app_only_ends_sessions() {
    let dir = temp_dir("refuse");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn("/bin/cat");
    peer.attach(id, AttachMode::MetaOnly);
    for message in [ClientMsg::Shutdown { kill_sessions: false }, ClientMsg::Handoff { min_format: 1, max_format: 1 }] {
        peer.send(&message);
        assert!(matches!(peer.reply(), HostMsg::Error { .. }), "{message:?}");
    }
    peer.send(&ClientMsg::Shutdown { kill_sessions: true });
    assert!(matches!(peer.reply(), HostMsg::Exited { id: exited, .. } if exited == id));
    assert!(peer.sessions().is_empty());
}

/// 别的进程也能换主题：连着的前端，不管是不是发 `SetTheme` 的那个，都在输出流里收到带着宿主
/// 套用的那份设置的 `ThemeApplied`。改宿主的选项只认 app 的界面。
#[test]
fn socket_clients_can_change_the_theme() {
    let dir = temp_dir("theme");
    let (_host, socket) = listen(&dir);
    let mut desktop = Peer::desktop(&socket);
    let id = desktop.spawn("/bin/cat");
    desktop.attach(id, AttachMode::Snapshot);
    let mut peer = Peer::hello(&socket, false);
    peer.attach(id, AttachMode::VtReplay);
    peer.send(&ClientMsg::SetOptions { record_history: false, clipboard: ClipboardAccess::default() });
    assert!(matches!(peer.reply(), HostMsg::Error { .. }));
    desktop.send(&ClientMsg::SetOptions { record_history: false, clipboard: ClipboardAccess::default() });
    let settings = TermSettings { cursor_blink: Some(false), scrollback_limit: 1 << 20, ..TermSettings::default() };
    peer.send(&ClientMsg::SetTheme { settings: settings.clone() });
    assert_eq!(peer.reply(), HostMsg::ThemeApplied { id, settings: settings.clone() });
    assert_eq!(desktop.reply(), HostMsg::ThemeApplied { id, settings });
    desktop.send(&ClientMsg::Kill { id });
}

/// shell 集成的报告带着口令，转给连接上的前端（经 socket 的、经一对 socket 的桌面）时抹掉内容，
/// 报告被切在两块输出之间也一样。之后连上来的前端拿到的屏幕（VT 重放）里本来就没有这些报告。
#[test]
fn shell_reports_do_not_leave_the_host() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let dir = temp_dir("redact");
    let (host, socket) = listen(&dir);
    // 一条报告分两次写，中间停一下，宿主多半分两块读到；之后的输出照常。
    let shell = script(
        &dir,
        "shell.sh",
        &format!("printf '\\033]6973;{TOKEN};cwd=/tm'\nsleep 0.3\nprintf 'p\\007visible\\n'\nexec /bin/cat"),
    );
    let mut pair = Peer::pair(&host);
    let id = pair.spawn_with(&shell, false, None);
    let (pair_channel, _) = pair.attach(id, AttachMode::Snapshot);
    let mut peer = Peer::hello(&socket, false);
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    pair.send(&ClientMsg::Start { id, integration: IntegrationMode::Off });

    for (peer, channel) in [(&peer, channel), (&pair, pair_channel)] {
        let output = peer.wait_for_output(channel, b"visible");
        let text = String::from_utf8_lossy(&output);
        assert!(!text.contains(TOKEN) && !text.contains("cwd="), "{text:?}");
        assert!(text.contains("\x1b]6973;\x07visible"), "{text:?}");
    }

    let mut late = Peer::hello(&socket, false);
    let (_, screen) = late.attach(id, AttachMode::VtReplay);
    let screen = String::from_utf8_lossy(&screen);
    assert!(screen.contains("visible") && !screen.contains(TOKEN), "{screen:?}");
    pair.send(&ClientMsg::Kill { id });
}

/// 报告写到一半时连上来要快照：宿主那份 VT 正停在报告里，没写完的报告连着口令在它的续接里。
/// 前端收到的快照和之后的输出里都没有口令；解出快照接着喂输出，屏幕和宿主读到的一样。
#[test]
fn a_snapshot_taken_inside_a_shell_report_has_no_token() {
    use runode_terminal::session::Session;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    let dir = temp_dir("midreport");
    let (_host, socket) = listen(&dir);
    // 报告的前半截写出去后停下等一行输入，前端这时连上来；之后写完报告和别的输出。
    let names: String = (0..300).map(|i| format!("a{i}.ls-la.")).collect();
    let shell = script(
        &dir,
        "shell.sh",
        &format!(
            "printf '\\033]6973;{TOKEN};functions={names}'\nread line\nprintf 'tail\\007visible\\n'\nexec /bin/cat"
        ),
    );
    // 先连上一个前端看着输出：它收到抹过的报告开头时，宿主那份 VT 已经喂过报告的前半截（先转发
    // 再喂，都在会话线程里，之后的 `Attach` 排在后面）。
    let mut watcher = Peer::hello(&socket, false);
    let id = watcher.spawn_with(&shell, false, None);
    let (watched, _) = watcher.attach(id, AttachMode::VtReplay);
    watcher.send(&ClientMsg::Start { id, integration: IntegrationMode::Off });
    watcher.wait_for_output(watched, b"\x1b]6973;");

    let mut peer = Peer::hello(&socket, true);
    let (channel, snapshot) = peer.attach(id, AttachMode::Snapshot);
    assert!(snapshot.starts_with(b"GHOSTSNP"), "same build gets a snapshot");
    let mut view = Session::from_snapshot(&snapshot, Box::new(|_| {})).unwrap();
    peer.input(channel, b"go\r");
    let output = peer.wait_for_output(channel, b"visible");
    for (name, bytes) in [("snapshot", &snapshot), ("output", &output)] {
        assert!(!contains(bytes, TOKEN.as_bytes()), "the {name} has the token");
        assert!(!contains(bytes, b"functions"), "the {name} has the report");
    }
    view.feed(&output);
    assert!(!view.take_bell(), "the end of the report rang the bell");
    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: None });
    let HostMsg::ScreenText { text, .. } = peer.reply() else { panic!("expected screen text") };
    assert!(text.contains("visible"), "{text:?}");
    assert_eq!(view.screen_text().unwrap().trim_end(), text.trim_end());
    peer.send(&ClientMsg::Kill { id });
}

/// 会话被结束时，还排在积压的输出后面没连上的前端也有回话：`Attached`，接着 `Exited`，不会
/// 干等到超时。
#[test]
fn clients_still_attaching_when_a_session_is_killed_get_an_answer() {
    let dir = temp_dir("killq");
    let (_host, socket) = listen(&dir);
    let mut desktop = Peer::desktop(&socket);
    let id = desktop.spawn(&script(&dir, "flood.sh", "exec yes runode-kill-queued"));
    let (channel, _) = desktop.attach(id, AttachMode::VtReplay);
    // 等它刷起屏来，读线程那边积压一大堆输出（调试构建里宿主的 VT 处理得慢得多）。
    desktop.wait(channel, |_, output| output.len() >= 64 * 1024);
    let mut peer = Peer::hello(&socket, false);
    // 同一条连接上先连再结束：`Attach` 一定先送到会话线程，排在积压的输出后面。
    peer.send(&ClientMsg::Attach { id, size: Some(SIZE), mode: AttachMode::VtReplay });
    peer.send(&ClientMsg::Kill { id });
    assert!(matches!(peer.reply(), HostMsg::Attached { id: attached, .. } if attached == id));
    loop {
        match peer.reply() {
            HostMsg::Exited { id: exited, .. } => {
                assert_eq!(exited, id);
                break;
            }
            HostMsg::SnapshotEnd { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// 会话被结束时，连着的前端收到 `Exited`。
#[test]
fn killing_a_session_tells_its_clients() {
    let dir = temp_dir("kill");
    let (_host, socket) = listen(&dir);
    let mut killer = Peer::hello(&socket, false);
    let id = killer.spawn("/bin/cat");
    let mut watcher = Peer::hello(&socket, false);
    watcher.attach(id, AttachMode::MetaOnly);
    killer.send(&ClientMsg::Kill { id });
    assert!(matches!(watcher.reply(), HostMsg::Exited { id: exited, .. } if exited == id));
}

/// shell 退出以后才连上来的前端，在 `Attached` 之后马上收到 `Exited`。
#[test]
fn a_late_client_learns_the_shell_has_exited() {
    let dir = temp_dir("dead");
    let (_host, socket) = listen(&dir);
    let mut first = Peer::hello(&socket, false);
    let id = first.spawn("/bin/cat");
    let (channel, _) = first.attach(id, AttachMode::MetaOnly);
    first.input(channel, b"\x04");
    assert!(matches!(first.reply(), HostMsg::Exited { .. }));
    let mut late = Peer::hello(&socket, false);
    late.attach(id, AttachMode::MetaOnly);
    assert!(matches!(late.reply(), HostMsg::Exited { id: exited, .. } if exited == id));
    first.send(&ClientMsg::Kill { id });
}

/// 收到转给界面的请求，按 `answer` 回话，返回请求。
fn answer_ui(desktop: &mut Peer, answer: impl FnOnce(&ClientMsg) -> HostMsg) -> ClientMsg {
    let HostMsg::UiRequest { ui, request } = desktop.reply() else { panic!("expected a ui request") };
    let reply = answer(&request);
    desktop.send(&ClientMsg::UiReply { ui, reply: Box::new(reply) });
    *request
}

fn layout(id: SessionId) -> Vec<WindowLayout> {
    let rect = PaneRect { x: 0, y: 0, width: PaneRect::EXTENT, height: PaneRect::EXTENT };
    let pane = PaneLayout { index: 1, id, rect, focused: true };
    let tab = TabLayout { index: 1, active: true, panes: vec![pane] };
    let workspace = WorkspaceLayout { index: 1, name: None, dir: None, active: true, tabs: vec![tab] };
    vec![WindowLayout { index: 1, front: true, workspaces: vec![workspace] }]
}

/// `Open`、`OpenWorkspace`、`Reveal`、`Layout` 包成 `UiRequest` 转给界面的连接，原样带着请求方的 `req`；界面的
/// `UiReply` 原样转回请求方。别的连接冒充界面回话不算。
#[test]
fn window_requests_go_to_the_desktop_connection() {
    let dir = temp_dir("ui");
    let (_host, socket) = listen(&dir);
    let mut desktop = Peer::desktop(&socket);
    let mut cli = Peer::hello(&socket, false);
    let open = ClientMsg::Open {
        req: 4,
        placement: Placement::Right,
        near: None,
        cwd: None,
        focus: true,
        fold: None,
        from: None,
    };
    cli.send(&open);
    let forwarded = answer_ui(&mut desktop, |_| HostMsg::Opened { req: 4, id: SessionId(9) });
    assert_eq!(forwarded, open);
    assert_eq!(cli.reply(), HostMsg::Opened { req: 4, id: SessionId(9) });

    let open = ClientMsg::OpenWorkspace { req: 7, dir, focus: false, name: None };
    cli.send(&open);
    let forwarded = answer_ui(&mut desktop, |_| HostMsg::Opened { req: 7, id: SessionId(10) });
    assert_eq!(forwarded, open);
    assert_eq!(cli.reply(), HostMsg::Opened { req: 7, id: SessionId(10) });

    cli.send(&ClientMsg::Reveal { req: 5, id: SessionId(9) });
    let HostMsg::UiRequest { ui, request } = desktop.reply() else { panic!("expected a ui request") };
    assert_eq!(*request, ClientMsg::Reveal { req: 5, id: SessionId(9) });
    // 命令行冒充界面回话：丢掉。
    cli.send(&ClientMsg::UiReply { ui, reply: Box::new(HostMsg::Done { req: 99 }) });
    desktop.send(&ClientMsg::UiReply { ui, reply: Box::new(HostMsg::Done { req: 5 }) });
    assert_eq!(cli.reply(), HostMsg::Done { req: 5 });
    // 同一个请求回第二次：丢掉。
    desktop.send(&ClientMsg::UiReply { ui, reply: Box::new(HostMsg::Done { req: 5 }) });

    cli.send(&ClientMsg::Layout { req: 6 });
    let request = answer_ui(&mut desktop, |_| HostMsg::Layout { req: 6, windows: layout(SessionId(9)) });
    assert_eq!(request, ClientMsg::Layout { req: 6 });
    assert_eq!(cli.reply(), HostMsg::Layout { req: 6, windows: layout(SessionId(9)) });
    cli.send(&ClientMsg::ListSessions);
    assert!(matches!(cli.reply(), HostMsg::SessionList { .. }), "no stray replies");
}

/// 命令行不停发要转给界面的请求时，新连上的界面收到的第一条仍是 `Welcome`：登记和回
/// `Welcome` 之间没有空当让请求抢在前面。
#[test]
fn a_desktop_hears_welcome_before_any_request() {
    let dir = temp_dir("ui-order");
    let (_host, socket) = listen(&dir);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spam = {
        let stop = stop.clone();
        let mut cli = Peer::hello(&socket, false);
        thread::spawn(move || {
            let mut req = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                req += 1;
                cli.send(&ClientMsg::Layout { req });
            }
        })
    };
    // 每个界面都留着不回话，请求就一直转给最新连上的那个。`Peer::desktop` 断言第一条是
    // `Welcome`。
    let mut desktops = Vec::new();
    for _ in 0..500 {
        desktops.push(Peer::desktop(&socket));
        if desktops.len() > 20 {
            desktops.remove(0);
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    spam.join().unwrap();
}

/// 有几个界面连接时转给最近连上的那个；它断开时还没回话的请求回 `Error`，之后的请求转给剩下
/// 的；一个界面都没有时宿主自己回。
#[test]
fn window_requests_follow_the_latest_desktop() {
    let dir = temp_dir("uis");
    let (_host, socket) = listen(&dir);
    let mut cli = Peer::hello(&socket, false);
    cli.send(&ClientMsg::Layout { req: 1 });
    assert_eq!(cli.reply(), HostMsg::Layout { req: 1, windows: empty_window() });

    let mut older = Peer::desktop(&socket);
    let newer = Peer::desktop(&socket);
    cli.send(&ClientMsg::Layout { req: 2 });
    assert!(matches!(newer.reply(), HostMsg::UiRequest { .. }));
    drop(newer);
    assert!(matches!(cli.reply(), HostMsg::Error { req: Some(2), .. }));

    cli.send(&ClientMsg::Layout { req: 3 });
    answer_ui(&mut older, |_| HostMsg::Layout { req: 3, windows: Vec::new() });
    assert_eq!(cli.reply(), HostMsg::Layout { req: 3, windows: Vec::new() });
    drop(older);
    // 等宿主看到连接断开再问。
    let deadline = Instant::now() + WAIT;
    loop {
        cli.send(&ClientMsg::Layout { req: 4 });
        if matches!(cli.reply(), HostMsg::Layout { req: 4, .. }) {
            break;
        }
        assert!(Instant::now() < deadline, "the desktop is still registered");
        thread::sleep(Duration::from_millis(20));
    }
}

/// 宿主自己回的布局里一个工作区都没有时：一个空窗口。
fn empty_window() -> Vec<WindowLayout> {
    vec![WindowLayout { index: 1, front: true, workspaces: Vec::new() }]
}

/// 布局里各个工作区的目录、名字和会话。
fn workspaces(windows: &[WindowLayout]) -> Vec<(Option<PathBuf>, Option<String>, Vec<SessionId>)> {
    let sessions =
        |workspace: &WorkspaceLayout| workspace.tabs.iter().flat_map(|tab| &tab.panes).map(|pane| pane.id).collect();
    windows
        .iter()
        .flat_map(|window| &window.workspaces)
        .map(|workspace| (workspace.dir.clone(), workspace.name.clone(), sessions(workspace)))
        .collect()
}

/// 没有界面连着时宿主照自己手里的布局办：新建工作区、在工作区里开标签、改名都改这份布局，问过布局的
/// 连接先收到 `LayoutChanged`；同一个目录的工作区不另建；会话没了工作区还在；切到终端没有窗口可切，回
/// `Error`。
#[test]
fn without_a_desktop_the_host_keeps_the_workspaces() {
    let dir = temp_dir("headless");
    let canonical = std::fs::canonicalize(&dir).unwrap();
    let (_host, socket) = listen(&dir);
    let mut phone = Peer::connect(&socket).greet(ClientKind::Mobile, false);
    phone.send(&ClientMsg::Layout { req: 1 });
    assert_eq!(phone.reply(), HostMsg::Layout { req: 1, windows: empty_window() });
    let open = |req, near| ClientMsg::Open {
        req,
        placement: Placement::Right,
        near,
        cwd: None,
        focus: true,
        fold: None,
        from: None,
    };
    phone.send(&open(2, None));
    assert!(matches!(phone.reply(), HostMsg::Error { req: Some(2), .. }), "no workspace to open it in");

    phone.send(&ClientMsg::OpenWorkspace { req: 3, dir: dir.clone(), focus: false, name: Some(" app ".into()) });
    assert_eq!(phone.reply(), HostMsg::LayoutChanged);
    let HostMsg::Opened { req: 3, id: first } = phone.reply() else { panic!("expected opened") };
    phone.send(&ClientMsg::OpenWorkspace { req: 4, dir, focus: false, name: None });
    assert_eq!(phone.reply(), HostMsg::Opened { req: 4, id: first });
    phone.send(&open(5, Some(first)));
    assert_eq!(phone.reply(), HostMsg::LayoutChanged);
    let HostMsg::Opened { req: 5, id: second } = phone.reply() else { panic!("expected opened") };
    phone.send(&ClientMsg::RenameWorkspace { req: 6, window: 1, workspace: 1, name: "renamed".into() });
    assert_eq!(phone.reply(), HostMsg::LayoutChanged);
    assert_eq!(phone.reply(), HostMsg::Done { req: 6 });
    phone.send(&ClientMsg::RenameWorkspace { req: 7, window: 1, workspace: 2, name: "x".into() });
    assert!(matches!(phone.reply(), HostMsg::Error { req: Some(7), .. }));

    phone.send(&ClientMsg::Layout { req: 8 });
    let HostMsg::Layout { windows, .. } = phone.reply() else { panic!("expected the layout") };
    assert_eq!(workspaces(&windows), [(Some(canonical.clone()), Some("renamed".into()), vec![first, second])]);

    phone.send(&ClientMsg::Kill { id: first });
    phone.send(&ClientMsg::Kill { id: second });
    phone.send(&ClientMsg::Layout { req: 9 });
    let HostMsg::Layout { windows, .. } = phone.reply() else { panic!("expected the layout") };
    assert_eq!(workspaces(&windows), [(Some(canonical), Some("renamed".into()), vec![])]);
    phone.send(&ClientMsg::Reveal { req: 10, id: first });
    assert!(
        matches!(phone.reply(), HostMsg::Error { req: Some(10), message, .. } if message.contains("no runode window"))
    );
}

/// 一个界面报了布局 `windows` 后断开，等宿主看到它断开了（`cli` 问布局时不再转给界面），返回宿主回的布局。
fn report_and_leave(socket: &Path, cli: &mut Peer, windows: Vec<WindowLayout>) -> Vec<WindowLayout> {
    let mut desktop = Peer::desktop(socket);
    desktop.send(&ClientMsg::LayoutChanged { windows: Some(windows) });
    // 宿主读完了 `LayoutChanged` 再断开：命令行还没问过布局，不会收到它。
    desktop.sessions();
    drop(desktop);
    let deadline = Instant::now() + WAIT;
    loop {
        cli.send(&ClientMsg::Layout { req: 1 });
        if let HostMsg::Layout { windows, .. } = cli.reply() {
            return windows;
        }
        assert!(Instant::now() < deadline, "the desktop is still registered");
        thread::sleep(Duration::from_millis(20));
    }
}

/// 界面记的目录经过软链接、没规范化：手机对同一个目录发 `OpenWorkspace` 时，不管写成布局里的软链接
/// 还是真实路径都认得出这个工作区，不另建一个。
#[test]
fn a_workspace_is_found_by_the_dir_the_desktop_wrote() {
    let dir = temp_dir("written");
    let real = dir.join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let (_host, socket) = listen(&dir);
    let mut cli = Peer::hello(&socket, false);
    let workspace =
        WorkspaceLayout { index: 1, name: Some("w".into()), dir: Some(link.clone()), active: true, tabs: vec![] };
    report_and_leave(&socket, &mut cli, vec![WindowLayout { index: 1, front: true, workspaces: vec![workspace] }]);
    cli.send(&ClientMsg::OpenWorkspace { req: 2, dir: link.clone(), focus: false, name: None });
    assert_eq!(cli.reply(), HostMsg::LayoutChanged);
    let HostMsg::Opened { req: 2, id } = cli.reply() else { panic!("expected opened") };
    cli.send(&ClientMsg::Layout { req: 3 });
    let HostMsg::Layout { windows, .. } = cli.reply() else { panic!("expected the layout") };
    assert_eq!(workspaces(&windows), [(Some(link), Some("w".into()), vec![id])]);
    // 目录写成软链接指向的真实路径（手机浏览目录时拿到的就是它）也认得出。
    cli.send(&ClientMsg::OpenWorkspace { req: 4, dir: real, focus: false, name: None });
    assert_eq!(cli.reply(), HostMsg::Opened { req: 4, id });
    cli.send(&ClientMsg::Kill { id });
}

/// 界面报来的布局宿主记着：界面断开后照它回布局、在里面开终端；界面再连上时问 `Layout`，宿主回自己
/// 手里的，不转给界面。
#[test]
fn the_desktops_layout_outlives_the_desktop() {
    let (_host, socket) = listen(&temp_dir("held"));
    let mut cli = Peer::hello(&socket, false);
    let id = cli.spawn("/bin/cat");
    assert_eq!(report_and_leave(&socket, &mut cli, layout(id)), layout(id));
    cli.send(&ClientMsg::Open {
        req: 2,
        placement: Placement::Tab,
        near: Some(id),
        cwd: None,
        focus: false,
        fold: None,
        from: None,
    });
    assert_eq!(cli.reply(), HostMsg::LayoutChanged);
    let HostMsg::Opened { req: 2, id: opened } = cli.reply() else { panic!("expected opened") };

    let mut desktop = Peer::desktop(&socket);
    desktop.send(&ClientMsg::Layout { req: 3 });
    let HostMsg::Layout { req: 3, windows } = desktop.reply() else { panic!("expected the held layout") };
    assert_eq!(workspaces(&windows), [(None, None, vec![id, opened])]);
    cli.send(&ClientMsg::Layout { req: 4 });
    assert!(matches!(desktop.reply(), HostMsg::UiRequest { .. }), "with a desktop the layout is its own");
    cli.send(&ClientMsg::Kill { id: opened });
}

/// `claimed` 只算桌面的界面连着的（只看状态的也算），命令行连着的不算。
#[test]
fn only_desktops_claim_sessions() {
    let dir = temp_dir("claim");
    let (_host, socket) = listen(&dir);
    let mut cli = Peer::hello(&socket, false);
    let id = cli.spawn("/bin/cat");
    let claimed = |cli: &mut Peer| cli.sessions().iter().find(|info| info.id == id).map(|info| info.claimed);
    cli.attach(id, AttachMode::VtReplay);
    assert_eq!(claimed(&mut cli), Some(false));
    let mut desktop = Peer::desktop(&socket);
    desktop.attach(id, AttachMode::MetaOnly);
    assert_eq!(claimed(&mut cli), Some(true));
    desktop.send(&ClientMsg::Detach { id });
    // `Detach` 先送到会话线程，之后的列会话看得到。
    desktop.send(&ClientMsg::ListSessions);
    assert!(matches!(desktop.reply(), HostMsg::SessionList { .. }));
    assert_eq!(claimed(&mut cli), Some(false));
    cli.send(&ClientMsg::Kill { id });
}

/// 程序响铃时，含 BEL 的那块输出之后紧跟一条 `Bell`；只看状态的前端也收得到。OSC 结尾的 BEL
/// 不算响铃。
#[test]
fn bells_follow_the_output_that_rang() {
    let dir = temp_dir("bell");
    let (_host, socket) = listen(&dir);
    let mut viewer = Peer::desktop(&socket);
    let id =
        viewer.spawn_with(&script(&dir, "bell.sh", "printf '\\033]0;title\\007ding\\007'\nexec /bin/cat"), false, None);
    let (channel, _) = viewer.attach(id, AttachMode::Snapshot);
    let mut watcher = Peer::hello(&socket, false);
    watcher.attach(id, AttachMode::MetaOnly);
    viewer.send(&ClientMsg::Start { id, integration: IntegrationMode::Off });
    let output = viewer.wait(channel, |message, _| matches!(message, Some(HostMsg::Bell { .. })));
    assert!(contains(&output, b"ding\x07"), "the bell came before its output: {output:?}");
    let mut bells = 1;
    viewer.input(channel, b"end\r");
    viewer.wait(channel, |message, output| {
        bells += usize::from(matches!(message, Some(HostMsg::Bell { .. })));
        contains(output, b"end")
    });
    assert_eq!(bells, 1, "the title's BEL rang too");
    loop {
        match watcher.message() {
            HostMsg::Bell { id: rang } => break assert_eq!(rang, id),
            HostMsg::Meta { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    viewer.send(&ClientMsg::Kill { id });
}

/// 有没有带着 `marker` 的进程在跑。
fn running(marker: &str) -> bool {
    std::process::Command::new("pgrep").args(["-f", marker]).output().is_ok_and(|out| out.status.success())
}

/// 单独一个进程跑时收到 `Shutdown`：结束所有会话，每条连接收到 `Goodbye { Shutdown }` 后断开，
/// `run_until_idle` 返回，socket 删掉、锁放开，下一个宿主拿得到。
#[test]
fn shutdown_ends_a_standalone_host() {
    let dir = temp_dir("shutdown");
    let (host, socket) = listen(&dir);
    let stopped = {
        let host = host.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(host.run_until_idle(Duration::from_secs(60))));
        rx
    };
    let marker = format!("{}{}", std::process::id(), 7);
    let mut desktop = Peer::desktop(&socket);
    let id = desktop.spawn(&script(&dir, "sleep.sh", &format!("exec /bin/sleep {marker}")));
    desktop.attach(id, AttachMode::MetaOnly);
    let mut cli = Peer::hello(&socket, false);
    let deadline = Instant::now() + WAIT;
    while !running(&format!("sleep {marker}")) {
        assert!(Instant::now() < deadline, "the session did not start");
        thread::sleep(Duration::from_millis(20));
    }
    cli.send(&ClientMsg::Shutdown { kill_sessions: true });
    for peer in [&cli, &desktop] {
        let messages = peer.until_closed();
        assert!(messages.contains(&HostMsg::Goodbye { reason: GoodbyeReason::Shutdown }), "no goodbye in {messages:?}");
    }
    assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Shutdown));
    assert!(!socket.exists());
    assert!(host.connect_pair().is_err(), "a host that shut down takes no more connections");
    common::host().listen(&socket, &dir.join("host.lock")).unwrap();
    let deadline = Instant::now() + WAIT;
    while running(&format!("sleep {marker}")) {
        assert!(Instant::now() < deadline, "the session outlived the host");
        thread::sleep(Duration::from_millis(50));
    }
}

/// 发 `Shutdown` 的连接一时不读、写线程卡着时，`run_until_idle` 等它把 `Goodbye` 写出去才返回（最多等一会儿），
/// 不是一收到就撤掉这条连接的登记、当作没有连接了马上返回，单独跑的进程随之退出，`Goodbye` 没写出去。
#[test]
fn a_shutdown_waits_for_its_goodbye_to_be_written() {
    use runode_protocol::{Frame, read_frame, write_frame};

    let host = host();
    let stopped = {
        let host = host.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(host.run_until_idle(Duration::from_secs(60))));
        rx
    };
    let stream = host.connect_pair().unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut frames = Vec::new();
    let mut push = |message: &ClientMsg| {
        let frame = Frame::control(message).unwrap();
        write_frame(&mut frames, frame.kind, 0, &frame.payload).unwrap();
    };
    let hello = ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: BuildId(BUILD.into()),
        client: ClientKind::Cli,
        caps: Caps { snapshot: false, vt_replay: true },
        session: None,
        device: None,
    };
    // 第一声之后的每一声都回一条错，回话（远多于 socket 的收发缓冲）塞满 socket，这边不读，写线程卡住，
    // `Goodbye` 排在后面写不出去。
    for _ in 0..200_000 {
        push(&hello);
    }
    push(&ClientMsg::Shutdown { kill_sessions: true });
    let writer = {
        let mut stream = stream.try_clone().unwrap();
        thread::spawn(move || std::io::Write::write_all(&mut stream, &frames).unwrap())
    };
    writer.join().unwrap();
    assert_eq!(
        stopped.recv_timeout(Duration::from_secs(1)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the host stopped before its goodbye was written"
    );
    let mut goodbye = false;
    while let Some(frame) = read_frame(&mut reader).unwrap() {
        goodbye |= frame.kind == FrameKind::Control
            && frame.message::<HostMsg>().unwrap() == HostMsg::Goodbye { reason: GoodbyeReason::Shutdown };
    }
    assert!(goodbye, "no goodbye before the connection closed");
    assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Shutdown));
    drop(stream);
}

/// 没有会话也没有连接、持续给定的时长后 `run_until_idle` 返回：有连接、有会话时都不算空闲。
/// 返回时 socket 删掉、锁放开。
#[test]
fn an_idle_standalone_host_stops() {
    const IDLE: Duration = Duration::from_millis(300);
    let dir = temp_dir("idle");
    let (host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let stopped = {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(host.run_until_idle(IDLE)));
        rx
    };
    // 连着一个前端。
    assert!(stopped.recv_timeout(IDLE * 3).is_err());
    // 前端走了，留下一个会话。
    let id = peer.spawn("/bin/cat");
    drop(peer);
    assert!(stopped.recv_timeout(IDLE * 3).is_err());
    let mut peer = Peer::hello(&socket, false);
    peer.send(&ClientMsg::Kill { id });
    assert!(peer.sessions().is_empty());
    let left = Instant::now();
    drop(peer);
    assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Idle));
    assert!(left.elapsed() >= IDLE, "stopped {:?} after the last client left", left.elapsed());
    assert!(!socket.exists());
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_err());
    common::host().listen(&socket, &dir.join("host.lock")).unwrap();
}

/// `set_stay_up` 时没有会话也没有连接也不退出；改回去以后空闲从那一刻起算，到时照常退出。
#[test]
fn a_host_told_to_stay_up_does_not_stop_when_idle() {
    const IDLE: Duration = Duration::from_millis(200);
    let host: Host = host();
    host.set_stay_up(true);
    let stopped = {
        let (tx, rx) = mpsc::channel();
        let host = host.clone();
        thread::spawn(move || tx.send(host.run_until_idle(IDLE)));
        rx
    };
    assert!(stopped.recv_timeout(IDLE * 4).is_err());
    let released = Instant::now();
    host.set_stay_up(false);
    assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Idle));
    assert!(released.elapsed() >= IDLE, "stopped {:?} after staying up ended", released.elapsed());
}

/// 退出以后不再接新连接：`connect_pair` 返回错误。
#[test]
fn a_stopped_host_takes_no_connections() {
    let host: Host = host();
    assert_eq!(host.run_until_idle(Duration::from_millis(10)), Stopped::Idle);
    assert!(host.connect_pair().is_err());
}

/// `Welcome` 说宿主是不是单独一个进程在跑：只开着 socket（跑在某个 app 里）时为假，进了
/// `Host::run_until_idle` 后为真。
#[test]
fn welcome_tells_whether_the_host_runs_on_its_own() {
    let dir = temp_dir("standalone");
    let (host, socket) = listen(&dir);
    let welcome = || {
        let mut peer = Peer::connect(&socket);
        peer.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            client: ClientKind::Cli,
            caps: Caps::default(),
            session: None,
            device: None,
        });
        match peer.message() {
            HostMsg::Welcome { standalone, .. } => (peer, standalone),
            other => panic!("expected welcome, got {other:?}"),
        }
    };
    assert!(!welcome().1);
    let stopped = {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(host.run_until_idle(Duration::from_secs(60))));
        rx
    };
    let deadline = Instant::now() + WAIT;
    let mut peer = loop {
        match welcome() {
            (peer, true) => break peer,
            _ => assert!(Instant::now() < deadline, "the host never said it runs on its own"),
        }
        thread::sleep(Duration::from_millis(5));
    };
    peer.send(&ClientMsg::Shutdown { kill_sessions: true });
    assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Shutdown));
}

/// 界面读不懂转来的请求时回的 `Error` 不带 `req`，宿主转回去时补上原请求的。
#[test]
fn ui_errors_without_req_get_the_request_s() {
    let dir = temp_dir("uierr");
    let (_host, socket) = listen(&dir);
    let mut desktop = Peer::desktop(&socket);
    let mut cli = Peer::hello(&socket, false);
    cli.send(&ClientMsg::Reveal { req: 12, id: SessionId(9) });
    answer_ui(&mut desktop, |_| HostMsg::Error { req: None, id: None, message: "unknown request".into() });
    assert_eq!(cli.reply(), HostMsg::Error { req: Some(12), id: None, message: "unknown request".into() });
}

/// `same_user`：同一个进程两端的 socket 是同一个用户；拿不到对端凭据（描述符根本不是 socket）时
/// 当成别人，不放行。换个 uid 连上来要 root，测不了。
#[test]
fn same_user_needs_the_peer_credentials() {
    let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(runode_host::same_user(&ours));
    let (pipe, _writer) = std::io::pipe().unwrap();
    let not_a_socket = std::os::unix::net::UnixStream::from(std::os::fd::OwnedFd::from(pipe));
    assert!(!runode_host::same_user(&not_a_socket));
}
