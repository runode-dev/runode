//! 别的进程发来的控制键和粘贴：按宿主那份 VT 当前的模式编出的字节。

mod common;

use common::idle_host;
use runode_shared_types::input::{KeyChord, parse_keys};

fn keys(spec: &[&str]) -> Vec<KeyChord> {
    spec.iter().flat_map(|s| parse_keys(s).unwrap()).collect()
}

#[test]
fn arrows_follow_the_cursor_key_mode() {
    let mut session = idle_host();
    assert_eq!(session.encode_keys(&keys(&["up"])).unwrap(), b"\x1b[A");
    // DECCKM 开着时是应用光标键。
    session.feed(b"\x1b[?1h");
    assert_eq!(session.encode_keys(&keys(&["up", "down*2"])).unwrap(), b"\x1bOA\x1bOB\x1bOB");
    session.feed(b"\x1b[?1l");
    assert_eq!(session.encode_keys(&keys(&["left"])).unwrap(), b"\x1b[D");
}

#[test]
fn control_keys_and_text_keys() {
    let session = idle_host();
    let encode = |spec: &str| session.encode_keys(&keys(&[spec])).unwrap();
    assert_eq!(encode("ctrl-c"), [0x03]);
    assert_eq!(encode("ctrl-d"), [0x04]);
    assert_eq!(encode("esc"), b"\x1b");
    assert_eq!(encode("enter"), b"\r");
    assert_eq!(encode("tab"), b"\t");
    assert_eq!(encode("backspace"), [0x7f]);
    assert_eq!(encode("a"), b"a");
    assert_eq!(encode("shift-a"), b"A");
    assert_eq!(encode("shift-/"), b"?");
    assert_eq!(encode("alt-b"), b"\x1bb");
    assert_eq!(encode("space"), b" ");
    assert_eq!(encode("shift-tab"), b"\x1b[Z");
    assert_eq!(encode("f5"), b"\x1b[15~");
}

#[test]
fn the_kitty_keyboard_protocol_is_honoured() {
    let mut session = idle_host();
    // 程序推入 Kitty 键盘协议的「消除歧义」：Esc 和 Ctrl 组合键改用 CSI u。
    session.feed(b"\x1b[>1u");
    assert_eq!(session.encode_keys(&keys(&["esc"])).unwrap(), b"\x1b[27u");
    assert_eq!(session.encode_keys(&keys(&["ctrl-c"])).unwrap(), b"\x1b[99;5u");
    session.feed(b"\x1b[<u");
    assert_eq!(session.encode_keys(&keys(&["esc"])).unwrap(), b"\x1b");
}

#[test]
fn paste_is_bracketed_when_the_program_asks() {
    let mut session = idle_host();
    assert_eq!(session.encode_paste("a\nb").unwrap(), b"a\rb");
    session.feed(b"\x1b[?2004h");
    assert_eq!(session.encode_paste("a\nb").unwrap(), b"\x1b[200~a\nb\x1b[201~");
    // 粘贴的内容里藏着结束序列也逃不出括号。
    let escaped = session.encode_paste("x\x1b[201~rm").unwrap();
    assert!(escaped.starts_with(b"\x1b[200~") && escaped.ends_with(b"\x1b[201~"));
    assert_eq!(escaped.windows(6).filter(|w| w == b"\x1b[201~").count(), 1, "{escaped:?}");
    assert_eq!(session.encode_paste("").unwrap(), b"");
}
