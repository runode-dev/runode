//! 界面这份会话画出的帧：同步输出时冻结、主题的默认颜色、跟随单元格的选区和光标颜色、光标闪烁。

mod common;

use common::{REPEAT, at, idle_session, row_text};
use runode_shared_types::{
    color::{Rgb, TerminalColor},
    settings::TermSettings,
    theme,
};
use runode_terminal::session::Session;

#[test]
fn synchronized_output_freezes_the_frame_until_released() {
    let mut session = idle_session();
    session.feed(b"before");
    assert_eq!(row_text(&session.frame(), 0), "before");

    session.feed(b"\x1b[?2026h\r\x1b[2Kafter");
    assert!(session.render_held());
    assert_eq!(row_text(&session.frame(), 0), "before", "held frame must not change");

    session.feed(b"\x1b[?2026l");
    assert!(!session.render_held());
    assert_eq!(row_text(&session.frame(), 0), "after");
}

#[test]
fn default_colors_come_from_the_theme() {
    let mut session = idle_session();
    session.feed(b"\x1b[32mok\x1b[0m");
    let frame = session.frame();
    assert_eq!(frame.background, theme::BACKGROUND);
    assert_eq!(frame.foreground, theme::FOREGROUND);
    assert_eq!(frame.row(0)[0].fg, theme::ANSI[2]);
    assert_eq!(frame.cursor.map(|c| c.color), Some(theme::FOREGROUND));
}

#[test]
fn selection_colors_can_follow_the_cell() {
    let mut session = idle_session();
    session.apply_config(&TermSettings {
        selection_background: Some(TerminalColor::CellForeground),
        selection_foreground: Some(TerminalColor::Rgb(Rgb(1, 2, 3))),
        ..TermSettings::default()
    });
    session.feed(b"\x1b[31mred\x1b[0m");
    session.select_press(at(0.2, 0.5), REPEAT);
    session.select_drag(at(2.8, 0.5), false);
    let cell = session.frame().row(0)[0].clone();
    assert_eq!(cell.bg, Some(theme::ANSI[1]));
    assert_eq!(cell.fg, Rgb(1, 2, 3));
}

#[test]
fn cursor_colors_can_follow_the_cell() {
    let mut session = idle_session();
    session.apply_config(&TermSettings {
        cursor_color: Some(TerminalColor::CellForeground),
        cursor_text: Some(TerminalColor::CellBackground),
        ..TermSettings::default()
    });
    // 光标退回到红色的 X 上。
    session.feed(b"\x1b[31mX\x1b[0m\x1b[D");
    let cursor = session.frame().cursor.unwrap();
    assert_eq!(cursor.color, theme::ANSI[1]);
    assert_eq!(cursor.text, theme::BACKGROUND);

    // 程序用 OSC 12 设的光标色优先于跟随单元格。
    session.feed(b"\x1b]12;#010203\x07");
    let color = session.frame().cursor.unwrap().color;
    assert_eq!(color, Rgb(1, 2, 3));
}

#[test]
fn cursor_blinks_unless_configured_or_steadied() {
    let blinking = |session: &mut Session| session.frame().cursor.map(|c| c.blinking);
    let mut session = idle_session();
    session.feed(b"ok");
    assert_eq!(blinking(&mut session), Some(true));
    // DECSCUSR 2：稳定的块状光标。
    session.feed(b"\x1b[2 q");
    assert_eq!(blinking(&mut session), Some(false));

    // 光标闪烁是主题的一部分，改的是 VT 的默认值。
    session.apply_theme(&TermSettings {
        cursor_blink: Some(false),
        ..TermSettings::default()
    });
    session.feed(b"\x1b[0 q");
    assert_eq!(blinking(&mut session), Some(false));
}
