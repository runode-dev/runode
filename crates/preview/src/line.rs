//! 把一行文字换成画出来的样子：制表符展开成空格，太长的截掉，高亮的范围跟着换算。

use crate::highlight::Span;

/// 制表符对齐到这么多列的倍数。
pub const TAB_WIDTH: usize = 4;

/// 画出来的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayLine {
    pub text: String,
    /// 范围是 `text` 里的字节位置。
    pub spans: Vec<Span>,
    /// 超过列数上限，后面没画。
    pub cut: bool,
}

/// 把 `line` 里的制表符展开成空格，最多留 `max_columns` 列（一个字符算一列），`spans` 的
/// 字节范围换算到展开后的文字上。
pub fn display_line(line: &str, spans: &[Span], max_columns: usize) -> DisplayLine {
    let mut text = String::with_capacity(line.len());
    // 原文每个字节位置对应到展开后的位置，末尾也有一项。
    let mut map = Vec::with_capacity(line.len() + 1);
    let mut columns = 0;
    let mut end = line.len();
    for (ix, ch) in line.char_indices() {
        if columns >= max_columns {
            end = ix;
            break;
        }
        let at = text.len();
        map.extend(std::iter::repeat_n(at, ch.len_utf8()));
        if ch == '\t' {
            let width = TAB_WIDTH - columns % TAB_WIDTH;
            text.extend(std::iter::repeat_n(' ', width));
            columns += width;
        } else {
            text.push(ch);
            columns += 1;
        }
    }
    map.push(text.len());
    let spans = spans
        .iter()
        .filter(|span| span.range.start < end)
        .map(|span| {
            let start = map[span.range.start.min(end)];
            let stop = map[span.range.end.min(end)];
            Span { range: start..stop, style: span.style }
        })
        .filter(|span| !span.range.is_empty())
        .collect();
    DisplayLine { text, spans, cut: end < line.len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::{Color, Style};

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
}
