//! Windows 上的会话：shell 跑在 ConPTY 里，输入、输出、改尺寸和退出都经宿主走通。用系统自带的
//! cmd 当 shell，起得快，输出也好认。

#![cfg(windows)]

mod common;

use common::{Peer, SIZE, host};
use runode_host::{ClientMsg, HostMsg};
use runode_protocol::AttachMode;
use runode_types::grid::GridSize;

/// 屏幕上有一行正好是 `line`（命令自己回显的那行带着提示符，不算）。
fn shows_line(text: &str, line: &str) -> bool {
    text.lines().any(|shown| shown.trim() == line)
}

#[test]
fn a_command_typed_into_cmd_prints_its_output() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("cmd.exe");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.input(channel, b"echo runode-%PROCESSOR_ARCHITECTURE%\r");
    peer.wait_screen(id, Some(50), |text| shows_line(text, "runode-AMD64") || shows_line(text, "runode-ARM64"));
    peer.send(&ClientMsg::Kill { id });
}

/// 改尺寸经 ConPTY 交到程序那里：cmd 的 `mode con` 报出新的列数。
#[test]
fn resizing_reaches_the_program() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn_sized("cmd.exe", GridSize { cols: 60, rows: 20, ..SIZE }, true);
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.send(&ClientMsg::Resize { id, size: GridSize { cols: 70, rows: 20, ..SIZE } });
    peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Resized { .. })));
    peer.input(channel, b"mode con\r");
    peer.wait_screen(id, Some(50), |text| text.lines().any(|line| line.contains(':') && line.trim_end().ends_with(" 70")));
    peer.send(&ClientMsg::Kill { id });
}

/// shell 自己退出后伪控制台关掉，输出读到头，前端收到 `Exited`。
#[test]
fn the_shell_exiting_ends_the_session() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("cmd.exe");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.input(channel, b"exit\r");
    peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { id: exited, .. }) if *exited == id));
}

/// 结束会话时连同 shell 一起结束，列会话里没有了。
#[test]
fn killing_a_session_ends_it() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("cmd.exe");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.send(&ClientMsg::Kill { id });
    peer.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { .. })));
    assert!(peer.sessions().iter().all(|session| session.id != id));
}

/// 反复列会话，直到 `id` 的状态让 `done` 认可。
fn wait_meta(peer: &mut Peer, id: runode_host::SessionId, done: impl Fn(&runode_types::session::SessionMeta) -> bool) {
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let meta = peer.sessions().into_iter().find(|session| session.id == id).map(|session| session.meta);
        if meta.as_ref().is_some_and(&done) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "timed out waiting for the state of {id}: {meta:?}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// shell 起的程序算前台，名字取自它的可执行文件；它退出后前台又是 shell 自己。
#[test]
fn a_running_command_is_the_foreground() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("cmd.exe");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    peer.input(channel, b"ping -n 30 127.0.0.1\r");
    wait_meta(&mut peer, id, |meta| {
        !meta.foreground_is_shell && meta.foreground.as_deref().is_some_and(|name| name.eq_ignore_ascii_case("PING.EXE"))
    });
    peer.input(channel, b"\x03");
    wait_meta(&mut peer, id, |meta| meta.foreground_is_shell);
    peer.send(&ClientMsg::Kill { id });
}

/// 没有 shell 集成时，shell 所在的目录从它的进程里读出来。
#[test]
fn the_shell_directory_is_read_from_the_process() {
    let host = host();
    let mut peer = Peer::pair(&host);
    let id = peer.spawn("cmd.exe");
    let (channel, _) = peer.attach(id, AttachMode::Snapshot);
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let dir = dir.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
    peer.input(channel, format!("cd /d \"{dir}\"\r").as_bytes());
    wait_meta(&mut peer, id, |meta| {
        meta.cwd.as_deref().is_some_and(|cwd| cwd.to_string_lossy().eq_ignore_ascii_case(&dir))
    });
    peer.send(&ClientMsg::Kill { id });
}
