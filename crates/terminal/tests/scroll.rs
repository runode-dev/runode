//! 视口滚动：按页和到两头、平滑滚动错开的小数行、在标出的提示符之间跳。

mod common;

use common::{PROMPT, capturing_session_sized, idle_session, row_text};
use runode_shared_types::grid::{GridSize, ViewportScroll};

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

/// 录下来的真实会话：隔离了家目录的 zsh 加载 runode 的集成，左提示符两行（`%1~` 换行 `%%`）、
/// 右侧提示符 `<r>`，依次跑 `echo one` 到 `echo four`，40 列 8 行。
const RECORDED_ZSH_TWO_LINE_RPROMPT: &[u8] = include_bytes!("../testdata/zsh-two-line-rprompt.typescript");

/// 两行的左提示符配上右侧提示符：右侧提示符画在第二行，每个提示符仍只停一次，停在第一行。
#[test]
fn jumping_stops_once_per_two_line_prompt_with_a_right_prompt() {
    let (mut session, _) =
        capturing_session_sized(GridSize { cols: 40, rows: 8, cell_width_px: 8, cell_height_px: 16 });
    session.feed(RECORDED_ZSH_TWO_LINE_RPROMPT);
    let top = |session: &mut runode_terminal::session::Session| {
        let frame = session.frame();
        // 右侧提示符前面空出来的那段不比较。
        let words = |y| row_text(&frame, y).split_whitespace().collect::<Vec<_>>().join(" ");
        (words(0), words(1))
    };
    // 视口最上面是 `echo three` 的提示符。
    assert_eq!(top(&mut session), ("proj".into(), "% echo three <r>".into()));
    session.jump_to_prompt(true);
    assert_eq!(top(&mut session), ("proj".into(), "% echo two <r>".into()));
    session.jump_to_prompt(true);
    assert_eq!(top(&mut session), ("proj".into(), "% echo one <r>".into()));
    session.jump_to_prompt(false);
    assert_eq!(top(&mut session), ("proj".into(), "% echo two <r>".into()));
}
