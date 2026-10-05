//! 界面那份会话的输入：多行粘贴要确认（bracketed paste 时不用），清屏经宿主做。

mod common;

use common::capturing_session;
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
