//! 把一行文字换成画出来的样子：制表符展开到制表位、多字节字符的高亮范围、过长截断。

use runode_preview::{Color, Span, Style, display_line, display_width};

fn span(range: std::ops::Range<usize>) -> Span {
    Span { range, style: Style { color: Color::Ansi(2), bold: false, italic: false } }
}

#[test]
fn expands_tabs_to_stops() {
    let line = display_line("\tab\tc", &[span(1..3), span(4..5)], 100);
    assert_eq!(line.text, "    ab  c");
    assert_eq!(line.spans, vec![span(4..6), span(8..9)]);
    assert!(!line.cut);
}

#[test]
fn keeps_multibyte_ranges() {
    let line = display_line("中\t文", &[span(0..3), span(4..7)], 100);
    assert_eq!(line.text, "中   文");
    assert_eq!(line.spans, vec![span(0..3), span(6..9)]);
}

#[test]
fn cuts_long_lines() {
    let line = display_line("abcdef", &[span(1..5), span(5..6)], 3);
    assert_eq!(line.text, "abc");
    assert_eq!(line.spans, vec![span(1..3)]);
    assert!(line.cut);
}

/// 挑最宽的行用的宽度和画出来的文字一致：制表符、截断和双宽字都照 `display_line` 算。
#[test]
fn display_width_matches_the_displayed_text() {
    let wide = |ch: char| if ch.is_ascii() { 1 } else { 2 };
    for (line, max) in [("\tab\tc", 100), ("中\t文", 100), ("abcdef", 3), ("", 10), ("\t\t中", 5)] {
        let shown = display_line(line, &[], max).text.chars().map(wide).sum::<usize>();
        assert_eq!(display_width(line, max, wide), shown, "{line:?}");
    }
}
