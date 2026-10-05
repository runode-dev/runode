//! 搜索：高亮匹配、在匹配之间走、新输出里的匹配和行尾的宽字符。

mod common;

use common::idle_session;
use runode_shared_types::theme;

#[test]
fn search_highlights_matches_and_steps_through_them() {
    let mut session = idle_session();
    session.feed(b"error one\r\nok\r\nerror two");
    session.search("error");
    // 先选中最新的那个。
    assert_eq!(session.search_status(), Some((Some(0), 2)));
    let frame = session.frame();
    let selected = theme::SEARCH_SELECTED_BACKGROUND;
    let other = theme::SEARCH_BACKGROUND;
    assert_eq!(frame.row(2)[0].bg, Some(selected));
    assert_eq!(frame.row(2)[4].bg, Some(selected));
    assert_eq!(frame.row(2)[5].bg, None);
    assert_eq!(frame.row(0)[0].bg, Some(other));
    assert_eq!(frame.row(1)[0].bg, None);
    drop(frame);

    session.search_step(false);
    assert_eq!(session.search_status(), Some((Some(1), 2)));
    assert_eq!(session.frame().row(0)[0].bg, Some(selected));

    // 新输出里的匹配也会算进来。
    session.feed(b"\r\nerror three");
    session.frame();
    assert_eq!(session.search_status().map(|(_, total)| total), Some(3));

    session.end_search();
    assert_eq!(session.search_status(), None);
    assert_eq!(session.frame().row(0)[0].bg, None);
}

#[test]
fn search_highlight_covers_both_halves_of_a_trailing_wide_char() {
    let mut session = idle_session();
    session.feed("a你好b".as_bytes());
    session.search("你好");
    let frame = session.frame();
    let selected = Some(theme::SEARCH_SELECTED_BACKGROUND);
    let colored: Vec<bool> = (0..6).map(|x| frame.row(0)[x].bg == selected).collect();
    assert_eq!(colored, [false, true, true, true, true, false]);
}
