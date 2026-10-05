//! 视口滚动：按页和到两头、平滑滚动错开的小数行、在标出的提示符之间跳。

mod common;

use common::{PROMPT, idle_session, row_text};
use runode_shared_types::grid::ViewportScroll;

#[test]
fn scrolling_the_viewport_by_page_and_to_the_ends() {
    let mut session = idle_session();
    for n in 0..20 {
        session.feed(format!("{n}\r\n").as_bytes());
    }
    session.scroll_viewport(ViewportScroll::Top);
    assert_eq!(row_text(&session.frame(), 0), "0");
    session.scroll_viewport(ViewportScroll::Page(1));
    assert_eq!(row_text(&session.frame(), 0), "4");
    session.scroll_viewport(ViewportScroll::Bottom);
    assert_eq!(row_text(&session.frame(), 0), "17");
}

#[test]
fn smooth_scrolling_shifts_by_fractions_and_stops_at_the_ends() {
    let mut session = idle_session();
    for n in 0..20 {
        session.feed(format!("{n}\r\n").as_bytes());
    }
    // 在底部往回滚半行：视口不动，整屏错开半行，露出上面那一行。
    assert!(session.scroll_smoothly(0.5));
    let frame = session.frame();
    assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("17", 0.5));
    assert_eq!(frame.above.iter().map(|c| c.text.as_str()).collect::<String>().trim_end(), "16");
    drop(frame);
    // 再滚 0.75 行：凑够一行挪视口，剩下 0.25 行。
    session.scroll_smoothly(0.75);
    let frame = session.frame();
    assert_eq!(row_text(&frame, 0), "16");
    assert!((frame.scroll_offset - 0.25).abs() < 1e-6);
    drop(frame);
    // 往底部滚过头：回到底部，不留错开。
    session.scroll_smoothly(-5.);
    let frame = session.frame();
    assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("17", 0.));
    drop(frame);
    // 滚到历史顶上以后不能再错开。
    session.scroll_smoothly(100.5);
    let frame = session.frame();
    assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("0", 0.));
    assert!(frame.above.is_empty());
}

#[test]
fn jumping_between_marked_prompts() {
    let mut session = idle_session();
    for n in 0..3 {
        session.feed(PROMPT);
        session.feed(format!("cmd{n}\r\n\x1b]133;C\x07out\r\nout\r\n\x1b]133;D;0\x07").as_bytes());
    }
    session.feed(PROMPT);
    // 4 行高的视口最上面一行是 "$ cmd2"，往上跳到它之前的那个提示符。
    assert_eq!(row_text(&session.frame(), 0), "$ cmd2");
    session.jump_to_prompt(true);
    assert_eq!(row_text(&session.frame(), 0), "$ cmd1");
    session.jump_to_prompt(true);
    assert_eq!(row_text(&session.frame(), 0), "$ cmd0");
    session.jump_to_prompt(false);
    assert_eq!(row_text(&session.frame(), 0), "$ cmd1");
}
