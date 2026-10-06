//! 会话里的程序用 OSC 52 读写剪贴板：宿主把请求转给桌面（`HostMsg::UiRequest`），按配置
//! （`SetOptions::clipboard`）丢掉、回空或者等桌面的回话，再把读到的文字写回程序。程序是 shell
//! 脚本，读请求的回话由 `cat -v` 原样显示在输出里（ESC 显示成 `^[`，BEL 显示成 `^G`）。

mod common;

use common::{Peer, contains, host, script, temp_dir};
use runode_host::{ClientMsg, HostMsg, SessionId};
use runode_protocol::{AttachMode, ClientKind};
use runode_shared_types::clipboard::{ClipboardAccess, ClipboardRead, ClipboardWrite};

/// 每读到一行，把它当 base64 写进剪贴板。
const WRITER: &str = r"while read line; do printf '\033]52;c;%s\007' $line; done";

fn set_access(desktop: &mut Peer, write: ClipboardWrite, read: ClipboardRead) {
    desktop.send(&ClientMsg::SetOptions { record_history: false, clipboard: ClipboardAccess { write, read } });
}

/// 下一条转给界面的请求，跳过中间别的消息和输出。
fn ui_request(peer: &Peer) -> (u64, ClientMsg) {
    loop {
        if let HostMsg::UiRequest { ui, request } = peer.message() {
            return (ui, *request);
        }
    }
}

/// 一直读到输出里出现 `needle`，返回这期间转给界面的请求。
fn requests_until(peer: &Peer, channel: u32, needle: &[u8]) -> Vec<ClientMsg> {
    let mut requests = Vec::new();
    peer.wait(channel, |message, output| {
        if let Some(HostMsg::UiRequest { request, .. }) = message {
            requests.push((**request).clone());
        }
        contains(output, needle)
    });
    requests
}

/// 跑 `body` 的会话（先等一行输入再往下走），桌面带着屏幕连上它，返回会话和通道。
fn reader_session(desktop: &mut Peer, name: &str, body: &str) -> (SessionId, u32) {
    let dir = temp_dir(name);
    let id = desktop.spawn(&script(&dir, name, &format!("read go; stty raw -echo; {body}; exec cat -v")));
    let (channel, _) = desktop.attach(id, AttachMode::VtReplay);
    desktop.input(channel, b"go\r");
    (id, channel)
}

#[test]
fn writes_go_to_the_desktop() {
    let host = host();
    let mut desktop = Peer::pair(&host);
    let id = desktop.spawn(&script(&temp_dir("clip-write"), "writer", WRITER));
    let (channel, _) = desktop.attach(id, AttachMode::VtReplay);
    desktop.input(channel, b"aGVsbG8=\r");
    let (_, request) = ui_request(&desktop);
    assert_eq!(request, ClientMsg::WriteClipboard { id, text: "hello".into() });
    desktop.send(&ClientMsg::Kill { id });
}

/// `clipboard-write = deny` 时写请求不出宿主；规矩改在会话开出来之后也照样生效。
#[test]
fn denied_writes_are_dropped() {
    let host = host();
    let mut desktop = Peer::pair(&host);
    let marked = r"while read line; do printf '\033]52;c;%s\007' $line; echo wrote-$line; done";
    let id = desktop.spawn(&script(&temp_dir("clip-deny"), "writer", marked));
    let (channel, _) = desktop.attach(id, AttachMode::VtReplay);
    set_access(&mut desktop, ClipboardWrite::Deny, ClipboardRead::Ask);
    desktop.input(channel, b"aGk=\r");
    let mut requests = requests_until(&desktop, channel, b"wrote-aGk=");
    // 写请求要转的话紧跟在它那块输出后面；下一行的输出在之后的块里，到它为止都没有就是丢掉了。
    desktop.input(channel, b"Ynll\r");
    requests.extend(requests_until(&desktop, channel, b"wrote-Ynll"));
    assert!(requests.is_empty(), "{requests:?}");
    desktop.send(&ClientMsg::Kill { id });
}

/// 几个桌面连着时交给最近和这个会话交互过的那个，不是最近连上的那个；命令行发的粘贴不算交互。
#[test]
fn writes_go_to_the_desktop_that_interacted_last() {
    let host = host();
    let mut first = Peer::pair(&host);
    let mut second = Peer::pair(&host);
    let id = first.spawn(&script(&temp_dir("clip-route"), "writer", WRITER));
    first.attach(id, AttachMode::VtReplay);
    second.attach(id, AttachMode::VtReplay);
    let mut cli = Peer::over(host.connect_pair().unwrap()).greet(ClientKind::Cli, false);

    first.send(&ClientMsg::Focus { id, focused: true });
    cli.send(&ClientMsg::Paste { req: 1, id, text: "aGk=\r".into() });
    assert_eq!(ui_request(&first).1, ClientMsg::WriteClipboard { id, text: "hi".into() });

    second.send(&ClientMsg::Focus { id, focused: true });
    cli.send(&ClientMsg::Paste { req: 2, id, text: "Ynll\r".into() });
    assert_eq!(ui_request(&second).1, ClientMsg::WriteClipboard { id, text: "bye".into() });
    first.send(&ClientMsg::Kill { id });
}

/// 读请求在 allow 时不问用户，桌面回的文字按请求的终止符写回程序。
#[test]
fn allowed_reads_answer_with_the_desktop_text() {
    let host = host();
    let mut desktop = Peer::pair(&host);
    set_access(&mut desktop, ClipboardWrite::Allow, ClipboardRead::Allow);
    let (id, channel) = reader_session(&mut desktop, "clip-read", r"printf '\033]52;c;?\007'");
    let (ui, request) = ui_request(&desktop);
    assert!(matches!(request, ClientMsg::ReadClipboard { id: asked, ask: false, .. } if asked == id), "{request:?}");
    let reply = HostMsg::ClipboardText { id, text: Some("hello".into()) };
    desktop.send(&ClientMsg::UiReply { ui, reply: Box::new(reply) });
    desktop.wait_for_output(channel, b"^[]52;c;aGVsbG8=^G");

    let (id, channel) = reader_session(&mut desktop, "clip-read-st", r"printf '\033]52;s;?\033\\'");
    let (ui, _) = ui_request(&desktop);
    let reply = HostMsg::ClipboardText { id, text: Some("中文".into()) };
    desktop.send(&ClientMsg::UiReply { ui, reply: Box::new(reply) });
    desktop.wait_for_output(channel, br"^[]52;s;5Lit5paH^[\");
    desktop.send(&ClientMsg::Kill { id });
}

/// 问用户的期间再来的读请求当场回空的，不再转给桌面；问的那个回话后照常回它。
#[test]
fn reads_while_asking_are_answered_with_nothing() {
    let host = host();
    let mut desktop = Peer::pair(&host);
    let (id, channel) = reader_session(&mut desktop, "clip-ask", r"printf '\033]52;c;?\007\033]52;c;?\033\\'");
    let (ui, request) = ui_request(&desktop);
    assert!(matches!(request, ClientMsg::ReadClipboard { ask: true, .. }), "{request:?}");
    assert!(requests_until(&desktop, channel, br"^[]52;c;^[\").is_empty());
    desktop.send(&ClientMsg::UiReply { ui, reply: Box::new(HostMsg::ClipboardText { id, text: Some("hi".into()) }) });
    desktop.wait_for_output(channel, b"^[]52;c;aGk=^G");
    desktop.send(&ClientMsg::Kill { id });
}

/// `clipboard-read = deny` 时不转给桌面，当场回一个空的剪贴板。
#[test]
fn denied_reads_are_answered_with_nothing() {
    let host = host();
    let mut desktop = Peer::pair(&host);
    set_access(&mut desktop, ClipboardWrite::Allow, ClipboardRead::Deny);
    let (id, channel) = reader_session(&mut desktop, "clip-read-deny", r"printf '\033]52;c;?\007'");
    assert!(requests_until(&desktop, channel, b"^[]52;c;^G").is_empty());
    desktop.send(&ClientMsg::Kill { id });
}

/// 没有桌面连着时读请求回空的；桌面接了读请求、没回话就断开时也回空的。
#[test]
fn reads_without_a_desktop_are_answered_with_nothing() {
    let host = host();
    let mut cli = Peer::over(host.connect_pair().unwrap()).greet(ClientKind::Cli, false);
    let (id, channel) = reader_session(&mut cli, "clip-no-ui", r"printf '\033]52;c;?\007'");
    assert!(requests_until(&cli, channel, b"^[]52;c;^G").is_empty());
    cli.send(&ClientMsg::Kill { id });
    cli.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { .. })));

    let desktop = Peer::pair(&host);
    let (_, channel) = reader_session(&mut cli, "clip-gone", r"printf '\033]52;c;?\007'");
    let (_, request) = ui_request(&desktop);
    assert!(matches!(request, ClientMsg::ReadClipboard { .. }));
    drop(desktop);
    cli.wait_for_output(channel, b"^[]52;c;^G");
}

/// 前端不能自己发读写剪贴板的请求：那是宿主替会话里的程序请界面办的。
#[test]
fn front_ends_cannot_ask_for_the_clipboard() {
    let host = host();
    let mut cli = Peer::over(host.connect_pair().unwrap()).greet(ClientKind::Cli, false);
    let _desktop = Peer::pair(&host);
    cli.send(&ClientMsg::ReadClipboard { id: SessionId(1), ask: false, program: None });
    assert!(matches!(cli.reply(), HostMsg::Error { .. }));
}
