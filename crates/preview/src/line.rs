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

/// `line` 照 `display_line` 画出来占几格：制表符展开成空格（一格一个）、最多留 `max_columns` 列，别的字按
/// `cell_width` 算。不拼出文字，逐行挑最宽的一行时不分配。
pub fn display_width(line: &str, max_columns: usize, cell_width: impl Fn(char) -> usize) -> usize {
    let (mut columns, mut width) = (0, 0);
    for ch in line.chars() {
        if columns >= max_columns {
            break;
        }
        if ch == '\t' {
            let tab = TAB_WIDTH - columns % TAB_WIDTH;
            columns += tab;
            width += tab;
        } else {
            columns += 1;
            width += cell_width(ch);
        }
    }
    width
}
