//! 选区：全选含回滚历史、Shift 加方向键扩展、拖动和双击选中、打字清掉选区，以及读整屏文字。

mod common;

use common::{REPEAT, at, idle_session};
use runode_shared_types::{input::SelectionAdjust, theme};

#[test]
fn select_all_covers_the_scrollback() {
    let mut session = idle_session();
    session.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    session.select_all();
    assert_eq!(session.selection_text().as_deref(), Some("one\ntwo\nthree\nfour\nfive\nsix"));
}

#[test]
fn shift_arrows_extend_an_existing_selection_only() {
    let mut session = idle_session();
    session.feed(b"helloworld");
    assert!(!session.adjust_selection(SelectionAdjust::Right));
    session.select_press(at(0.2, 0.5), REPEAT);
    session.select_drag(at(4.8, 0.5), false);
    session.select_release(at(4.8, 0.5));
    assert_eq!(session.selection_text().as_deref(), Some("hello"));
    assert!(session.adjust_selection(SelectionAdjust::Right));
    assert_eq!(session.selection_text().as_deref(), Some("hellow"));
}

#[test]
fn screen_text_includes_the_scrollback() {
    let mut session = idle_session();
    session.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    assert_eq!(session.screen_text().as_deref(), Some("one\ntwo\nthree\nfour\nfive"));
}

#[test]
fn dragging_selects_text_and_highlights_it() {
    let mut session = idle_session();
    session.feed(b"hello world");
    session.select_press(at(0.2, 0.5), REPEAT);
    session.select_drag(at(4.8, 0.5), false);
    session.select_release(at(4.8, 0.5));
    assert_eq!(session.selection_text().as_deref(), Some("hello"));
    // 没配选区颜色时用统一的蓝色底，文字保持原来的颜色。
    let frame = session.frame();
    assert_eq!(frame.row(0)[0].bg, Some(theme::SELECTION_ON_DARK));
    assert_eq!(frame.row(0)[0].fg, theme::FOREGROUND);
    assert!(frame.row(0)[0].selected && !frame.row(0)[6].selected);
    assert_eq!(frame.row(0)[6].bg, None);
    drop(frame);

    // 单击清掉选区。
    session.select_press(at(2.5, 1.5), REPEAT);
    session.select_release(at(2.5, 1.5));
    assert_eq!(session.selection_text(), None);
    assert_eq!(session.frame().row(0)[0].bg, None);
}

#[test]
fn double_click_selects_a_word_and_typing_clears_it() {
    let mut session = idle_session();
    session.feed(b"hello world");
    for _ in 0..2 {
        session.select_press(at(7.5, 0.5), REPEAT);
        session.select_release(at(7.5, 0.5));
    }
    assert_eq!(session.selection_text().as_deref(), Some("world"));
    session.commit_text("x");
    assert_eq!(session.selection_text(), None);
}
