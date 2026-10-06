//! 别的终端里的程序经 socket 操作会话：发控制键、粘贴、读某条命令的输出，以及宿主记下谁在
//! 操作（`SessionMeta::driver`）、用户在界面里打字后清掉。

mod common;

use std::{thread, time::Duration, time::Instant};

use common::{Peer, WAIT, listen, script, temp_dir};
use runode_host::{ClientMsg, HostMsg, SessionId};
use runode_protocol::{AttachMode, ClientKind, FrameKind};
use runode_shared_types::session::{DriveAction, SessionMeta};

/// 关掉回显、不按行等、不认 Ctrl-C 的 `cat -v`：程序收到的每个字节原样显示出来（ESC 是 `^[`），
/// 回车时一起输出。`setup` 是之前给终端的序列（比如打开应用光标键）；设好终端后打印 `ready`，
/// 测试等到它再发键，免得键在 `stty` 之前到达、按原来的设置处理（比如 Ctrl-C 结束了程序）。
fn visible_cat(dir: &std::path::Path, setup: &str) -> String {
    script(dir, "cat.sh", &format!("printf '{setup}'\nstty -echo -icanon -isig\nprintf 'ready\\n'\nexec /bin/cat -v"))
}

/// 控制键按宿主 VT 当前的模式编好写给程序：应用光标键开着时方向键是 `ESC O A`。
#[test]
fn keys_reach_the_program_encoded_for_its_modes() {
    let dir = temp_dir("keys");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn(&visible_cat(&dir, ""));
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    peer.wait_for_output(channel, b"ready");
    let keys = vec!["up".into(), "ctrl-c".into(), "x*2".into(), "enter".into()];
    peer.send(&ClientMsg::SendKeys { req: 1, id, keys });
    assert!(matches!(peer.reply(), HostMsg::Done { req: 1 }));
    peer.wait_for_output(channel, b"^[[A^Cxx");
    // 写错的键：回 `Error`，什么都不写，连接照旧。
    peer.send(&ClientMsg::SendKeys { req: 2, id, keys: vec!["enter".into(), "hyper-x".into()] });
    assert!(matches!(peer.reply(), HostMsg::Error { req: Some(2), id: Some(errored), .. } if errored == id));
    peer.send(&ClientMsg::SendKeys { req: 3, id: SessionId(9), keys: vec!["up".into()] });
    assert!(matches!(peer.reply(), HostMsg::Error { req: Some(3), .. }));
    peer.send(&ClientMsg::Kill { id });

    let dir = temp_dir("keysapp");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn(&visible_cat(&dir, "\\033[?1h"));
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    // 程序打开应用光标键的序列在 `ready` 之前，宿主的 VT 已经处理过它。
    peer.wait_for_output(channel, b"ready");
    peer.send(&ClientMsg::SendKeys { req: 1, id, keys: vec!["up".into(), "enter".into()] });
    assert!(matches!(peer.reply(), HostMsg::Done { req: 1 }));
    peer.wait_for_output(channel, b"^[OA");
    peer.send(&ClientMsg::Kill { id });
}

/// 粘贴按程序有没有开括号粘贴决定套不套括号。
#[test]
fn pastes_are_bracketed_when_the_program_asks() {
    let dir = temp_dir("paste");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn(&visible_cat(&dir, "\\033[?2004h"));
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    peer.wait_for_output(channel, b"ready");
    peer.send(&ClientMsg::Paste { req: 4, id, text: "pasted".into() });
    assert!(matches!(peer.reply(), HostMsg::Done { req: 4 }));
    peer.send(&ClientMsg::SendKeys { req: 5, id, keys: vec!["enter".into()] });
    assert!(matches!(peer.reply(), HostMsg::Done { req: 5 }));
    peer.wait_for_output(channel, b"^[[200~pasted^[[201~");
    peer.send(&ClientMsg::Kill { id });
}

/// 读倒数第几条命令的输出要 shell 集成标出的提示符，没有时回给读的一方看的说明。
#[test]
fn command_output_needs_prompt_marks() {
    let dir = temp_dir("cmdout");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let marked = script(
        &dir,
        "marked.sh",
        "printf '\\033]133;A\\007$ \\033]133;B\\007ls\\r\\n\\033]133;C\\007one\\r\\ntwo\\r\\n'\n\
         printf '\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'\nexec /bin/cat",
    );
    let id = peer.spawn(&marked);
    let (channel, _) = peer.attach(id, AttachMode::VtReplay);
    peer.wait_for_output(channel, b"two");
    // 输出到了宿主那份 VT 才读得到。
    let deadline = Instant::now() + WAIT;
    loop {
        peer.send(&ClientMsg::ReadScreen { id, lines: None, command: Some(1) });
        match peer.reply() {
            HostMsg::ScreenText { text, truncated, .. } if text == "one\ntwo\n" => {
                assert!(!truncated);
                break;
            }
            other => assert!(Instant::now() < deadline, "{other:?}"),
        }
        thread::sleep(Duration::from_millis(20));
    }
    peer.send(&ClientMsg::Kill { id });

    // 没有标记的会话；换一条连接问，免得混进刚才那个会话结束的消息。
    let mut peer = Peer::hello(&socket, false);
    let id = peer.spawn("/bin/cat");
    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: Some(1) });
    let reply = peer.reply();
    let HostMsg::Error { id: Some(errored), message, .. } = reply else { panic!("expected an error: {reply:?}") };
    assert_eq!(errored, id);
    assert!(message.contains("needs shell integration"), "{message}");
    peer.send(&ClientMsg::Kill { id });
}

/// 等到会话的状态满足 `wanted`，跳过别的消息和输出。
fn wait_for_meta(peer: &Peer, id: SessionId, wanted: impl Fn(&SessionMeta) -> bool) -> SessionMeta {
    let deadline = Instant::now() + WAIT;
    loop {
        let frame = peer.frames.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("timed out");
        if frame.kind == FrameKind::Control
            && let HostMsg::Meta { id: changed, meta } = frame.message().unwrap()
            && changed == id
            && wanted(&meta)
        {
            return meta;
        }
    }
}

/// 命令行（不是桌面的连接）发来的输入、按键记成谁在操作；桌面的连接（socket 上的，和进程内
/// 一对 socket 的）发来输入时清掉。
#[test]
fn drivers_are_recorded_and_cleared_by_the_desktop() {
    let dir = temp_dir("driven");
    let (host, socket) = listen(&dir);
    let me = SessionId(0x2a);
    let mut agent = Peer::connect(&socket).greet_from(ClientKind::Cli, false, Some(me));
    let id = agent.spawn("/bin/cat");
    let (channel, _) = agent.attach(id, AttachMode::MetaOnly);
    agent.input(channel, b"hi");
    let meta = wait_for_meta(&agent, id, |meta| meta.driver.is_some());
    let driver = meta.driver.unwrap();
    assert_eq!((driver.by, driver.action), (Some(me.to_string()), DriveAction::Input));
    assert!(driver.at_ms > 0);

    let mut desktop = Peer::desktop(&socket);
    let (desk, _) = desktop.attach(id, AttachMode::VtReplay);
    desktop.input(desk, b"x");
    wait_for_meta(&agent, id, |meta| meta.driver.is_none());

    // 再标一次，换进程内那一对 socket 上的桌面来清。
    agent.send(&ClientMsg::SendKeys { req: 1, id, keys: vec!["left".into()] });
    wait_for_meta(&agent, id, |meta| meta.driver.as_ref().is_some_and(|d| d.action == DriveAction::Keys));
    let mut paired = Peer::pair(&host);
    let (pair_channel, _) = paired.attach(id, AttachMode::VtReplay);
    paired.input(pair_channel, b"y");
    wait_for_meta(&agent, id, |meta| meta.driver.is_none());
    agent.send(&ClientMsg::Kill { id });
}
