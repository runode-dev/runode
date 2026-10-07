//! 界面那份会话的输入：多行粘贴要确认（bracketed paste 时不用），清屏经宿主做，重新载入 shell
//! 先清掉正在编辑的输入。

mod common;

use std::sync::Arc;

use common::{PROMPT, capturing_session};
use runode_shared_types::{session::SessionMeta, shell::ShellNames};
use runode_terminal::session::{Paste, Request};

#[test]
fn multiline_paste_needs_confirmation_unless_bracketed() {
    let (mut session, requests) = capturing_session();
    let input = |requests: &std::cell::RefCell<Vec<Request>>| match requests.borrow_mut().pop() {
        Some(Request::Input(data)) => Some(data),
        _ => None,
    };
    assert_eq!(session.paste("ls", false), Paste::Done);
    assert_eq!(input(&requests).as_deref(), Some(&b"ls"[..]));
    assert_eq!(session.paste("rm -rf x\nls", false), Paste::NeedsConfirmation);
    assert_eq!(input(&requests), None);
    // 确认后照写，换行换成回车，控制字节换成空格。
    assert_eq!(session.paste("rm -rf x\nls\x03", true), Paste::Done);
    assert_eq!(input(&requests).as_deref(), Some(&b"rm -rf x\rls "[..]));

    // 程序开启 bracketed paste 后，换行不会被直接执行，无需确认；结束序列逃得出括号，要确认。
    session.feed(b"\x1b[?2004h");
    assert_eq!(session.paste("a\nb", false), Paste::Done);
    assert_eq!(input(&requests).as_deref(), Some(&b"\x1b[200~a\nb\x1b[201~"[..]));
    assert_eq!(session.paste("a\x1b[201~b", false), Paste::NeedsConfirmation);
    assert_eq!(session.paste("", false), Paste::Done);
    assert_eq!(input(&requests), None);
}

#[test]
fn clear_screen_goes_through_the_host() {
    let (mut session, requests) = capturing_session();
    session.clear_screen();
    assert_eq!(*requests.borrow(), [Request::ClearScreen]);
    // 备用屏幕上不清。
    session.feed(b"\x1b[?1049hvim");
    session.clear_screen();
    assert_eq!(requests.borrow().len(), 1);
}

#[test]
fn reload_shell_clears_the_input_first() {
    let (mut session, requests) = capturing_session();
    session.feed(PROMPT);
    session.feed(b"git st\x1b[2D");
    // 集成还没报告过 runode-reload（比如更早的版本启动的 shell）时不发。
    assert!(!session.reload_shell());
    let names = ShellNames { functions: vec!["runode-reload".into()], ..ShellNames::default() };
    session.apply_meta(SessionMeta { shell_names: Arc::new(names), ..SessionMeta::default() });
    assert!(session.reload_shell());
    // 光标后还有两个字：右移两下到末尾，退格六下，再输入命令回车。
    let expected = [&b"\x1b[C\x1b[C"[..], &b"\x7f".repeat(6), b" runode-reload\r"].concat();
    assert_eq!(*requests.borrow(), [Request::Input(expected)]);

    // 命令在跑（输出不是提示符）时不发。
    requests.borrow_mut().clear();
    session.feed(b"\r\n\x1b]133;C\x07running");
    assert!(!session.reload_shell());
    assert!(requests.borrow().is_empty());
}
