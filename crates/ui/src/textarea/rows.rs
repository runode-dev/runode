//! 多行输入框的视觉行：一段硬换行按宽度折成几行，以及按视觉行换算光标位置、横坐标和选区的纯函数。

use std::ops::Range;

use gpui::{Pixels, Point, WrappedLine, px};

use super::NEWLINE_WIDTH;

/// 一个视觉行：一段硬换行按宽度折出来的一行。
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    /// 在全文里的字节范围，不含换行符。
    pub(super) range: Range<usize>,
    /// 行尾是软换行，`range.end` 同时是下一行的开头。
    pub(super) soft: bool,
    /// 行里能停光标的位置和它相对行首的横坐标，按偏移升序；第一个是行首，最后一个是行尾，空行只有一个。
    pub(super) stops: Vec<(usize, Pixels)>,
}

/// 把一段硬换行（在全文里从 `line_start` 起、长 `len` 字节）在折行处 `breaks`（行内偏移）
/// 切成视觉行。`glyphs` 是各字形的行内偏移和不折行时的横坐标，按偏移升序；`width` 是整段
/// 不折行时的宽度。
pub(super) fn split_line(
    line_start: usize,
    len: usize,
    glyphs: &[(usize, Pixels)],
    breaks: &[usize],
    width: Pixels,
) -> Vec<Row> {
    // 和 `LineLayout::x_for_index` 一样：第一个不早于 `index` 的字形的位置，都没有时是行宽。
    let x_at = |index: usize| {
        let ix = glyphs.partition_point(|(i, _)| *i < index);
        glyphs.get(ix).map_or(width, |(_, x)| *x)
    };
    let mut rows = Vec::with_capacity(breaks.len() + 1);
    let mut start = 0;
    for (n, end) in breaks.iter().copied().chain([len]).enumerate() {
        let origin = x_at(start);
        let mut stops = vec![(line_start + start, px(0.))];
        let first = glyphs.partition_point(|(i, _)| *i <= start);
        for &(i, x) in glyphs[first..].iter().take_while(|(i, _)| *i < end) {
            // 几个字形算在同一个字符上时只留第一个。
            if stops.last().is_some_and(|(last, _)| *last < line_start + i) {
                stops.push((line_start + i, x - origin));
            }
        }
        // 空行的行首就是行尾，只留一个。
        if end > start {
            stops.push((line_start + end, x_at(end) - origin));
        }
        rows.push(Row { range: line_start + start..line_start + end, soft: n < breaks.len(), stops });
        start = end;
    }
    rows
}

/// `shape_text` 排出来的各段硬换行切成视觉行。
pub(super) fn rows_from_lines(lines: &[WrappedLine]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in lines {
        let layout = &line.unwrapped_layout;
        let glyphs: Vec<(usize, Pixels)> =
            layout.runs.iter().flat_map(|run| run.glyphs.iter().map(|glyph| (glyph.index, glyph.position.x))).collect();
        let breaks: Vec<usize> = line
            .wrap_boundaries()
            .iter()
            .map(|boundary| layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index)
            .collect();
        rows.extend(split_line(line_start, line.len(), &glyphs, &breaks, layout.width));
        line_start += line.len() + 1;
    }
    rows
}

/// 还没排过版时的退路，也给测试用：每段硬换行一行，每个字符宽 `char_width`。
pub(super) fn plain_rows(text: &str, char_width: Pixels) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let glyphs: Vec<(usize, Pixels)> =
            line.char_indices().enumerate().map(|(n, (i, _))| (i, char_width * n as f32)).collect();
        let width = char_width * line.chars().count() as f32;
        rows.extend(split_line(line_start, line.len(), &glyphs, &[], width));
        line_start += line.len() + 1;
    }
    rows
}

/// `offset` 所在的视觉行。软换行处的偏移既是上一行的行尾也是下一行的行首，`upstream` 时算
/// 上一行。
pub(super) fn row_at(rows: &[Row], offset: usize, upstream: bool) -> usize {
    let ix = rows.partition_point(|row| row.range.start <= offset).saturating_sub(1);
    if upstream && ix > 0 && rows[ix].range.start == offset && rows[ix - 1].soft { ix - 1 } else { ix }
}

/// 行里 `offset` 处光标的横坐标，相对行首。
pub(super) fn x_in_row(row: &Row, offset: usize) -> Pixels {
    row.stops.iter().find(|(i, _)| *i >= offset).or(row.stops.last()).map_or(px(0.), |(_, x)| *x)
}

pub(super) fn caret_x(rows: &[Row], offset: usize, upstream: bool) -> Pixels {
    rows.get(row_at(rows, offset, upstream)).map_or(px(0.), |row| x_in_row(row, offset))
}

/// 行里离横坐标 `x` 最近的光标位置；落在软换行的行尾时一并返回 `upstream`。
pub(super) fn index_in_row(row: &Row, x: Pixels) -> (usize, bool) {
    let mut best = row.stops[0];
    for &stop in &row.stops[1..] {
        if (stop.1 - x).abs() < (best.1 - x).abs() {
            best = stop;
        }
    }
    (best.0, row.soft && best.0 == row.range.end)
}

/// 相对内容左上角（已算上滚动）的一点对应的光标位置：在第一行上面算全文开头，在最后一行
/// 下面算全文末尾。
pub(super) fn index_at_point(rows: &[Row], at: Point<Pixels>, line_height: Pixels) -> (usize, bool) {
    if at.y < px(0.) {
        return (0, false);
    }
    match rows.get((at.y / line_height) as usize) {
        Some(row) => index_in_row(row, at.x),
        None => (rows.last().map_or(0, |row| row.range.end), false),
    }
}

/// 从 `offset` 往上或往下挪一个视觉行，停在横坐标最接近 `goal_x` 的位置；第一行再往上到全文
/// 开头，最后一行再往下到全文末尾。
pub(super) fn vertical(rows: &[Row], offset: usize, upstream: bool, goal_x: Pixels, up: bool) -> (usize, bool) {
    let row = row_at(rows, offset, upstream);
    let target = if up { row.checked_sub(1) } else { Some(row + 1).filter(|&next| next < rows.len()) };
    match target {
        Some(target) => index_in_row(&rows[target], goal_x),
        None if up => (0, false),
        None => (rows.last().map_or(0, |row| row.range.end), false),
    }
}

/// 光标所在视觉行的行首。
pub(super) fn row_start(rows: &[Row], offset: usize, upstream: bool) -> usize {
    rows.get(row_at(rows, offset, upstream)).map_or(0, |row| row.range.start)
}

/// 光标所在视觉行的行尾；行尾是软换行时光标画在这一行末尾。
pub(super) fn row_end(rows: &[Row], offset: usize, upstream: bool) -> (usize, bool) {
    rows.get(row_at(rows, offset, upstream)).map_or((offset, false), |row| (row.range.end, row.soft))
}

/// 选区在各视觉行上要涂的横向范围：（行号，起点，终点），相对行首。选区跨过换行符的行在行尾
/// 多涂 `NEWLINE_WIDTH`，整行都选中的空行也看得出来。
pub(super) fn selection_spans(rows: &[Row], selected: &Range<usize>) -> Vec<(usize, Pixels, Pixels)> {
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| {
            let start = selected.start.max(row.range.start);
            let end = selected.end.min(row.range.end);
            let newline = !row.soft && selected.start <= row.range.end && selected.end > row.range.end;
            if start > end || (start == end && !newline) {
                return None;
            }
            let right = x_in_row(row, end) + if newline { NEWLINE_WIDTH } else { px(0.) };
            Some((ix, x_in_row(row, start), right))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use gpui::point;

    use super::*;

    const W: Pixels = px(10.);

    /// 每个字符宽 10，每段硬换行按 `columns` 个字符硬折：测试用的等宽排版。
    fn wrapped(text: &str, columns: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in text.split('\n') {
            let indices: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
            let glyphs: Vec<(usize, Pixels)> = indices.iter().enumerate().map(|(n, &i)| (i, W * n as f32)).collect();
            let breaks: Vec<usize> = indices.iter().copied().skip(columns).step_by(columns).collect();
            rows.extend(split_line(line_start, line.len(), &glyphs, &breaks, W * indices.len() as f32));
            line_start += line.len() + 1;
        }
        rows
    }

    #[test]
    fn split_line_makes_rows_with_stops_relative_to_row_start() {
        let rows = wrapped("abcdefg\nhi", 3);
        let ranges: Vec<_> = rows.iter().map(|row| (row.range.clone(), row.soft)).collect();
        assert_eq!(ranges, [(0..3, true), (3..6, true), (6..7, false), (8..10, false)]);
        assert_eq!(rows[1].stops, [(3, px(0.)), (4, W), (5, W * 2.), (6, W * 3.)]);
        assert_eq!(rows[2].stops, [(6, px(0.)), (7, W)]);
    }

    #[test]
    fn empty_text_and_trailing_newline_still_have_rows() {
        assert_eq!(plain_rows("", W), [Row { range: 0..0, soft: false, stops: vec![(0, px(0.))] }]);
        let rows = plain_rows("ab\n", W);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].range, 3..3);
    }

    #[test]
    fn soft_wrap_offset_belongs_to_next_row_unless_upstream() {
        let rows = wrapped("abcdef", 3);
        assert_eq!(row_at(&rows, 3, false), 1);
        assert_eq!(row_at(&rows, 3, true), 0);
        assert_eq!(caret_x(&rows, 3, false), px(0.));
        assert_eq!(caret_x(&rows, 3, true), W * 3.);
        // 硬换行处没有歧义：换行符前面是上一行的行尾，后面是下一行的行首。
        let rows = wrapped("abc\ndef", 3);
        assert_eq!(row_at(&rows, 3, false), 0);
        assert_eq!(row_at(&rows, 4, true), 1);
    }

    #[test]
    fn vertical_moves_keep_goal_x_and_land_on_row_ends() {
        // 视觉行：abcd | efgh | ij（硬换行）| klmnop 折成 klmn | op
        let text = "abcdefghij\nklmnop";
        let rows = wrapped(text, 4);
        assert_eq!(rows.len(), 5);
        // 从 c（x = 20）往下到 g，再往下到第三行只有 ij，停在行尾。
        assert_eq!(vertical(&rows, 2, false, W * 2., false), (6, false));
        assert_eq!(vertical(&rows, 6, false, W * 3., false), (10, false));
        // 保持原来的横坐标 30：从 ij 行尾继续往下回到 n。
        assert_eq!(vertical(&rows, 10, false, W * 3., false), (14, false));
        // 横坐标超过软换行的行尾：停在行尾，光标画在这一行。
        assert_eq!(vertical(&rows, 9, false, W * 9., true), (8, true));
        // 第一行再往上到全文开头，最后一行再往下到全文末尾。
        assert_eq!(vertical(&rows, 2, false, W * 2., true), (0, false));
        assert_eq!(vertical(&rows, 15, false, W, false), (text.len(), false));
        // 光标画在上一行末尾时，往下是从上一行出发。
        assert_eq!(vertical(&rows, 4, true, W * 4., false), (8, true));
        assert_eq!(vertical(&rows, 4, false, W * 0., false), (8, false));
    }

    #[test]
    fn row_start_and_end_follow_visual_rows() {
        let rows = wrapped("abcdefg", 3);
        assert_eq!(row_start(&rows, 4, false), 3);
        assert_eq!(row_end(&rows, 4, false), (6, true));
        assert_eq!(row_start(&rows, 6, true), 3);
        assert_eq!(row_end(&rows, 6, false), (7, false));
    }

    #[test]
    fn point_maps_to_row_and_clamps_outside_content() {
        let rows = wrapped("abcdef\nxy", 3);
        let lh = px(20.);
        assert_eq!(index_at_point(&rows, point(W * 1.4, px(25.)), lh), (4, false));
        assert_eq!(index_at_point(&rows, point(W * 9., px(5.)), lh), (3, true));
        assert_eq!(index_at_point(&rows, point(W * 9., px(25.)), lh), (6, false));
        assert_eq!(index_at_point(&rows, point(px(0.), px(-1.)), lh), (0, false));
        assert_eq!(index_at_point(&rows, point(px(0.), px(500.)), lh), (9, false));
    }

    #[test]
    fn selection_spans_cover_rows_and_selected_newlines() {
        let rows = wrapped("abcdef\n\nxy", 3);
        // 从 b 选到 x 后面：第一行 b..行尾，第二行整行（软换行，不多涂），第三行行尾换行，空行，x。
        let spans = selection_spans(&rows, &(1..9));
        assert_eq!(
            spans,
            [(0, W, W * 3.), (1, px(0.), W * 3. + NEWLINE_WIDTH), (2, px(0.), NEWLINE_WIDTH), (3, px(0.), W),]
        );
        assert!(selection_spans(&rows, &(4..4)).is_empty());
        // 从软换行处开始的选区不在上一行画。
        assert_eq!(selection_spans(&rows, &(3..4)), [(1, px(0.), W)]);
    }
}
