//! 宿主的黑盒测试：经 `Host::connect_pair` 按协议说话（桌面走的路），开会话、连上、发输入和控制
//! 消息，看输出流和事件。

mod common;

use std::time::{Duration, Instant};

use common::{Peer, SIZE, WAIT, contains, host, script, temp_dir};
use runode_host::{ClientMsg, HostMsg, SessionId};
use runode_protocol::AttachMode;
use runode_shared_types::{settings::TermSettings, shell::IntegrationMode};

/// `cat` 当 shell：输入经宿主写给它，行规程的回显经宿主转回来；连上时给的是快照，带着宿主的主题。
#[test]
fn input_reaches_the_program_and_output_comes_back() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("/bin/cat");
    peer.send(&ClientMsg::Attach { id, size: Some(SIZE), mode: AttachMode::Snapshot });
    let HostMsg::Attached { channel, size, mode, settings, .. } = peer.reply() else { panic!("expected attached") };
    assert_eq!((size, mode, settings), (SIZE, AttachMode::Snapshot, Some(TermSettings::default())));
    assert!(peer.screen(id, channel, mode).starts_with(b"GHOSTSNP"));
    peer.input(channel, b"hello\r");
    peer.wait_for_output(channel, b"hello");
    peer.send(&ClientMsg::Kill { id });
}

/// 改尺寸和换主题都在输出流里标出位置；没变的不标。
#[test]
fn resize_and_theme_changes_are_marked_in_the_stream() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("/bin/cat");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    let size = runode_shared_types::grid::GridSize { cols: 30, ..SIZE };
    peer.send(&ClientMsg::Resize { id, size: SIZE });
    peer.send(&ClientMsg::Resize { id, size });
    peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Resized { size: s, .. }) if *s == size));
    peer.send(&ClientMsg::SetTheme { settings: TermSettings::default() });
    let dark = TermSettings { cursor_blink: Some(false), ..TermSettings::default() };
    peer.send(&ClientMsg::SetTheme { settings: dark.clone() });
    let mut themes = Vec::new();
    peer.input(channel, b"x\r");
    peer.wait(channel, |message, output| {
        if let Some(HostMsg::ThemeApplied { settings, .. }) = message {
            themes.push(settings.clone());
        }
        contains(output, b"x")
    });
    // 默认主题和会话开出来时的一样，只有第二次算换了；标记里带着宿主这时套的那份设置。
    assert_eq!(themes, [dark]);
    peer.send(&ClientMsg::Kill { id });
}

/// 开会话时带的主题只在宿主还没收到过 `SetTheme` 时生效，之后一律用宿主当前的主题；连上时
/// `Attached` 带着会话那份 VT 现在套着的主题。
#[test]
fn spawn_settings_apply_only_before_the_first_theme() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let early = TermSettings { scrollback_limit: 3 << 20, ..TermSettings::default() };
    let spawn = |peer: &mut Peer, req: u32| {
        peer.send(&ClientMsg::Spawn {
            req,
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some("/bin/cat".into()),
            settings: Some(early.clone()),
        });
        match peer.reply() {
            HostMsg::Spawned { req: answered, id } if answered == req => id,
            other => panic!("expected spawned, got {other:?}"),
        }
    };
    let attached_settings = |peer: &mut Peer, id: SessionId| {
        peer.send(&ClientMsg::Attach { id, size: None, mode: AttachMode::MetaOnly });
        match peer.reply() {
            HostMsg::Attached { id: attached, settings, .. } if attached == id => settings,
            other => panic!("expected attached, got {other:?}"),
        }
    };
    let first = spawn(&mut peer, 1);
    assert_eq!(attached_settings(&mut peer, first), Some(early.clone()));

    let theme = TermSettings { cursor_blink: Some(false), scrollback_limit: 1 << 20, ..TermSettings::default() };
    // 只看状态的前端收不到 `ThemeApplied`；宿主按先后处理同一条连接上的消息，之后的 `Attach`
    // 一定在换主题之后。
    peer.send(&ClientMsg::SetTheme { settings: theme.clone() });
    assert_eq!(attached_settings(&mut peer, first), Some(theme.clone()));
    let second = spawn(&mut peer, 2);
    assert_eq!(attached_settings(&mut peer, second), Some(theme));
    peer.send(&ClientMsg::Kill { id: first });
    peer.send(&ClientMsg::Kill { id: second });
}

/// 清屏的字节当成一段输出发回来；前台是 shell 时还给它发一个 FF，`cat` 把它回显成 `^L`。
#[test]
fn clear_screen_comes_back_as_output() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("/bin/cat");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.send(&ClientMsg::ClearScreen { id });
    let output = peer.wait_for_output(channel, b"^L");
    assert!(output.starts_with(b"\x1b[H\x1b[2J\x1b[3J"), "{output:?}");
    peer.send(&ClientMsg::Kill { id });
}

/// 结束会话时最后发一条 `Exited`，之后发给它的输入都丢掉，列会话里也没有了。
#[test]
fn killed_sessions_go_quiet() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("/bin/cat");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.send(&ClientMsg::Kill { id });
    peer.input(channel, b"after\r");
    let output = peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { .. })));
    assert!(!contains(&output, b"after"));
    assert!(peer.sessions().is_empty());
    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: None });
    assert!(matches!(peer.reply(), HostMsg::Error { id: Some(errored), .. } if errored == id));
}

/// 还没启动的会话：标题和目录是起始目录的；`Start` 后 shell 才跑起来。
#[test]
fn unstarted_sessions_start_on_request() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let dir = std::env::temp_dir();
    let id = peer.spawn_with("/bin/cat", false, Some(dir.clone()));
    peer.send(&ClientMsg::Attach { id, size: Some(SIZE), mode: AttachMode::Snapshot });
    let HostMsg::Attached { channel, mode, meta, .. } = peer.reply() else { panic!("expected attached") };
    peer.screen(id, channel, mode);
    assert_eq!(meta.cwd, Some(dir));
    peer.send(&ClientMsg::Start { id, integration: IntegrationMode::Off });
    peer.input(channel, b"go\r");
    peer.wait_for_output(channel, b"go");
    peer.send(&ClientMsg::Kill { id });
}

/// shell 的环境里有自己的会话标识和宿主设的变量。
#[test]
fn shells_know_their_session() {
    let host = host();
    host.set_env("RUNODE_TEST", "first");
    host.set_env("RUNODE_TEST", "second");
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("/bin/sh");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.input(channel, b"env; exit\r");
    let output = peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { .. })));
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains(&format!("RUNODE_SESSION={id}")), "{output}");
    assert!(output.contains("RUNODE_TEST=second") && !output.contains("RUNODE_TEST=first"), "{output}");
    peer.send(&ClientMsg::Kill { id });
}

/// 一个会话，在里面不读启动配置的交互式 zsh 里跑 `yes <marker>` 不停输出：开着作业控制，`yes`
/// 有自己的进程组，标签名能认出它。返回会话、通道和 `marker`。
fn flooding(peer: &mut Peer, name: &str) -> (SessionId, u32, String) {
    let dir = temp_dir(name);
    let id = peer.spawn(&script(&dir, "zsh.sh", "exec /bin/zsh -f -i"));
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    // 等 zsh 出提示符。
    std::thread::sleep(Duration::from_millis(500));
    while peer.frames.try_recv().is_ok() {}
    let marker = format!("runode-pair-{name}-{}", std::process::id());
    peer.input(channel, format!("yes {marker}\r").as_bytes());
    (id, channel, marker)
}

/// 有没有命令行里带着 `marker` 的进程在跑。
fn running(marker: &str) -> bool {
    std::process::Command::new("pgrep").args(["-f", marker]).output().is_ok_and(|out| out.status.success())
}

/// 输出一直不断时前台进程照样按时重读：`yes` 刷屏期间，标签名一秒左右就变成 `yes`。
#[test]
fn the_foreground_is_read_while_output_floods() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let (id, channel, _) = flooding(&mut peer, "pflood");
    let started = Instant::now();
    peer.wait(channel, |message, _| {
        matches!(message, Some(HostMsg::Meta { meta, .. }) if meta.fallback_title.as_deref() == Some("yes"))
    });
    let elapsed = started.elapsed();
    // 这时它还在刷屏。
    peer.wait(channel, |_, output| output.len() > 16 * 1024);
    peer.send(&ClientMsg::Kill { id });
    assert!(elapsed < Duration::from_millis(1500), "the foreground was read after {elapsed:?}");
}

/// 刷屏到一半结束会话：前端收到一次 `Exited`，`yes` 也被结束。
#[test]
fn killing_a_flooding_session_cleans_up() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let (id, channel, marker) = flooding(&mut peer, "pkill");
    peer.wait(channel, |_, output| output.len() > 32 * 1024);
    assert!(running(&marker));
    peer.send(&ClientMsg::Kill { id });
    peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { .. })));
    // 之后不再有第二次。
    peer.send(&ClientMsg::ListSessions);
    loop {
        match peer.message() {
            HostMsg::SessionList { sessions } => break assert!(sessions.is_empty()),
            HostMsg::Exited { .. } => panic!("exit reported twice"),
            _ => {}
        }
    }
    let deadline = Instant::now() + WAIT;
    while running(&marker) {
        assert!(Instant::now() < deadline, "the flooding program outlived its session");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// shell 退出以后才连上：屏幕里有退出前的输出，`Attached` 之后补上 `Exited`；会话留到前端结束它。
#[test]
fn attaching_after_the_shell_exited() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("/bin/echo");
    std::thread::sleep(Duration::from_millis(500));
    let (_, screen) = peer.attach(id, AttachMode::Snapshot);
    assert!(screen.starts_with(b"GHOSTSNP"));
    assert!(matches!(peer.reply(), HostMsg::Exited { id: exited, .. } if exited == id));
    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: None });
    let HostMsg::ScreenText { text, .. } = peer.reply() else { panic!("expected screen text") };
    assert!(text.contains("-l"), "{text:?}");
    assert_eq!(peer.sessions().iter().map(|info| (info.id, info.exited)).collect::<Vec<_>>(), [(id, true)]);
    peer.send(&ClientMsg::Kill { id });
}
