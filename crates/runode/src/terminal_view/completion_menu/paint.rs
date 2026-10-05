//! 补全菜单画在终端网格上：盖在光标下面（下面放不下而上面更宽裕时画在当前词上面）的几行，
//! 占满终端宽度，用终端的字体、单元格和 ANSI 颜色画，不写进终端屏幕。样子是这样：
//!
//! ```text
//! 12/340 ──────────────────────────────────────────────
//! [子命令] [选项]
//! ▌checkout      -- Switch branches or restore working tree files
//!  cherry-pick   -- Apply the changes introduced by some existing commits
//! ```
//!
//! 第一行是匹配上的个数和总数，后面画一条横线到行尾；接着是结果里出现的分组，各用一种颜色，
//! 一行放不下就换行；然后每行一个候选，按分组着色，名字一列对齐，后面是说明，匹配上的字
//! 换一种颜色加粗；选中的那一行整行高亮，左边有一道分组颜色的色条。

use std::ops::Range;

use gpui::{Bounds, ContentMask, Pixels, Point, Window, fill, point, px, size};
use runode_completion::{self as completion, Kind};
use runode_model::{
    color::Rgb,
    frame::{Attrs, Frame},
};

use super::super::{Metrics, TerminalView, faint, hsla, paint_glyphs};

/// 名字一列最宽占终端宽度的这么多（百分比）。
const NAME_COLUMN_PERCENT: usize = 40;
/// 名字都不超过这么多格时，名字一列取最长的那个，见 `name_column`。
const SHORT_NAME: usize = 24;
/// 分组标签的先后。
const KINDS: [Kind; 11] = [
    Kind::Command,
    Kind::Alias,
    Kind::Function,
    Kind::Builtin,
    Kind::Keyword,
    Kind::Subcommand,
    Kind::Option,
    Kind::Value,
    Kind::Generated,
    Kind::Folder,
    Kind::File,
];

/// 菜单上次画在网格里的位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::terminal_view) struct Shown {
    /// 计数那一行。
    top_row: i32,
    /// 候选从哪一行开始、一共几行。
    list_row: i32,
    list_rows: usize,
    /// 候选的第一行显示的是第几项，以及一共几项。
    first_item: usize,
    items: usize,
}

impl Shown {
    /// 菜单占的行。
    pub(in crate::terminal_view) fn rows(&self) -> Range<i32> {
        self.top_row..self.list_row + self.list_rows as i32
    }

    /// 第 `row` 行画的是第几项；不是候选行时为 `None`。
    pub(in crate::terminal_view) fn item_at(&self, row: i32) -> Option<usize> {
        let offset = usize::try_from(row - self.list_row).ok().filter(|&offset| offset < self.list_rows)?;
        Some(self.first_item + offset).filter(|&item| item < self.items)
    }
}

/// 菜单各部分在网格里占哪几行。
#[derive(Debug, PartialEq, Eq)]
struct Layout {
    /// 计数那一行。
    top_row: usize,
    /// 每一行放哪几个分组标签（标签的下标）。
    label_lines: Vec<Vec<usize>>,
    /// 候选占几行，至少一行（没有候选时用来写「没有匹配项」）。
    list_rows: usize,
}

/// 终端 `cols` 列、`rows` 行，光标在第 `cursor_row` 行、当前词从第 `word_row` 行开始，要列
/// `items` 项、分组标签各占 `labels` 格时，菜单画在哪里。默认从光标下一行开始往下画；下面
/// 放不下全部、上面又更宽裕时画在当前词的上面。上下都不到两行时不画。
fn layout(cols: usize, rows: usize, cursor_row: usize, word_row: usize, items: usize, labels: &[usize]) -> Option<Layout> {
    let mut label_lines: Vec<Vec<usize>> = Vec::new();
    let mut used = 0;
    for (i, &width) in labels.iter().enumerate() {
        match label_lines.last_mut() {
            Some(line) if used + 1 + width <= cols => {
                line.push(i);
                used += 1 + width;
            }
            _ => {
                label_lines.push(vec![i]);
                used = width;
            }
        }
    }
    let wanted = 1 + label_lines.len() + items.max(1);
    let below = rows.saturating_sub(cursor_row + 1);
    let above = word_row.min(cursor_row);
    // 放得下全部就往下；放不下时哪边宽裕往哪边，一样宽时往下。
    let down = below >= wanted || below >= above;
    let space = if down { below } else { above };
    if space < 2 {
        return None;
    }
    // 地方不够时先少画几行分组，计数和至少一行候选要留着。
    label_lines.truncate(space - 2);
    let list_rows = items.max(1).min(space - 1 - label_lines.len());
    let total = 1 + label_lines.len() + list_rows;
    let top_row = if down { cursor_row + 1 } else { above - total };
    Some(Layout { top_row, label_lines, list_rows })
}

/// 一段要画的字。
struct Span {
    row: i32,
    col: usize,
    /// 画到这一列之前为止，放不下时最后一格画省略号。
    end: usize,
    text: String,
    color: Rgb,
    bold: bool,
    /// 匹配上的字（按字算的下标），用 `mark_color` 加粗画。
    marks: Vec<usize>,
    mark_color: Rgb,
}

/// 一个分组用终端色板里的哪个 ANSI 色；文件用前景色，为 `None`。同时出现在一份结果里的分组
/// 各不相同。
fn kind_color(kind: Kind) -> Option<usize> {
    match kind {
        Kind::Command | Kind::Subcommand => Some(2),
        Kind::Alias | Kind::Value => Some(5),
        Kind::Function | Kind::Option => Some(6),
        Kind::Builtin | Kind::Generated => Some(3),
        Kind::Keyword => Some(1),
        Kind::Folder => Some(4),
        Kind::File => None,
    }
}

/// 名字里匹配上的字用的 ANSI 色：亮黄；分组本身是黄色时换成亮青，免得看不出来。
fn mark_color(kind: Kind) -> usize {
    if kind_color(kind) == Some(3) { 14 } else { 11 }
}

/// 分组标签：`[组名]`，生成器的结果带上参数在规格里的名字，如 `[动态值: branch]`。
fn kind_label(kind: Kind, detail: Option<&str>) -> String {
    let key = match kind {
        Kind::Command => "completion.group.command",
        Kind::Alias => "completion.group.alias",
        Kind::Function => "completion.group.function",
        Kind::Builtin => "completion.group.builtin",
        Kind::Keyword => "completion.group.keyword",
        Kind::Subcommand => "completion.group.subcommand",
        Kind::Option => "completion.group.option",
        Kind::Value => "completion.group.value",
        Kind::Generated => "completion.group.generated",
        Kind::Folder => "completion.group.folder",
        Kind::File => "completion.group.file",
    };
    match detail {
        Some(detail) => format!("[{}]", rust_i18n::t!("completion.group.named", group = rust_i18n::t!(key), name = detail)),
        None => format!("[{}]", rust_i18n::t!(key)),
    }
}

/// 名字一列的宽度：看得到的名字都不超过 `SHORT_NAME` 格时取最长的；有更长的时取九成名字放得下
/// 的宽度（至少 `SHORT_NAME`），个别特别长的名字自己伸进说明那一列，不为它把整列撑宽。
/// 最多占终端宽度的 `NAME_COLUMN_PERCENT`。
fn name_column(widths: &[usize], cols: usize) -> usize {
    let mut sorted = widths.to_vec();
    sorted.sort_unstable();
    let longest = sorted.last().copied().unwrap_or(0);
    let width = if longest <= SHORT_NAME {
        longest
    } else {
        let p90 = sorted[(sorted.len() * 9).div_ceil(10).saturating_sub(1)];
        p90.max(SHORT_NAME).min(longest)
    };
    width.min(cols * NAME_COLUMN_PERCENT / 100).max(1)
}

impl TerminalView {
    /// 把开着的补全菜单画在网格上，记下画在了哪几行。在画完终端内容之后调用，盖在上面。
    pub(in crate::terminal_view) fn paint_completion(&mut self, frame: &Frame, metrics: Metrics, window: &mut Window) {
        let Some(cursor) = frame.cursor else {
            if let Some(menu) = &mut self.completion {
                menu.shown = None;
            }
            return;
        };
        let ansi = self.session.ansi_colors();
        let Some(menu) = &mut self.completion else {
            return;
        };
        let (cols, rows) = (usize::from(frame.cols), usize::from(frame.rows));
        let (fg, bg) = (frame.foreground, frame.background);
        let dim = faint(fg, bg);
        let color = |kind: Kind| kind_color(kind).map_or(fg, |i| ansi[i]);

        // 当前词的起点所在的行：从光标往回数，词折行时在上面几行。
        let back = menu.cells_before_cursor.saturating_sub(usize::from(cursor.x));
        let word_row = usize::from(cursor.y).saturating_sub(back.div_ceil(cols.max(1)));
        // 结果里出现的分组，按 `KINDS` 的先后，同一类里带不同参数名的分开列。
        let mut groups: Vec<(Kind, Option<&str>)> = Vec::new();
        for kind in KINDS {
            let mut details: Vec<Option<&str>> = menu
                .items
                .iter()
                .map(|&i| &menu.candidates[i])
                .filter(|c| c.kind == kind)
                .map(|c| c.detail.as_deref())
                .collect();
            details.sort_unstable();
            details.dedup();
            groups.extend(details.into_iter().map(|detail| (kind, detail)));
        }
        let labels: Vec<String> = groups.iter().map(|&(kind, detail)| kind_label(kind, detail)).collect();
        let widths: Vec<usize> = labels.iter().map(|label| completion::cells(label)).collect();
        let Some(layout) = layout(cols, rows, usize::from(cursor.y), word_row, menu.items.len(), &widths) else {
            menu.shown = None;
            return;
        };

        // 让选中的那一项在能看到的几行里。
        let list_rows = layout.list_rows;
        if menu.selected < menu.top {
            menu.top = menu.selected;
        } else if menu.selected >= menu.top + list_rows {
            menu.top = menu.selected + 1 - list_rows;
        }
        menu.top = menu.top.min(menu.items.len().saturating_sub(list_rows));
        let top_row = layout.top_row as i32;
        let list_row = top_row + 1 + layout.label_lines.len() as i32;
        menu.shown = Some(Shown { top_row, list_row, list_rows, first_item: menu.top, items: menu.items.len() });
        let scrollable = menu.items.len() > list_rows;
        // 有滚动条时最后一列留给它。
        let text_end = if scrollable { cols.saturating_sub(1) } else { cols };

        let mut spans = Vec::new();
        // 菜单底下的整行用终端背景色盖住；选中的那一行另外高亮。
        let mut quads: Vec<(Bounds<Pixels>, Rgb)> = Vec::new();
        let (cw, ch) = (metrics.cell.width, metrics.cell.height);
        let origin = self.grid_origin;
        let row_bounds = |row: i32| Bounds::new(origin + point(px(0.), ch * row as f32), size(cw * cols as f32, ch));
        for row in top_row..list_row + list_rows as i32 {
            quads.push((row_bounds(row), bg));
        }
        let plain = |row: i32, col: usize, end: usize, text: String, color: Rgb| Span {
            row,
            col,
            end,
            text,
            color,
            bold: false,
            marks: Vec::new(),
            mark_color: color,
        };

        // 第一行：选中的是第几项/匹配上几项，筛掉了一些时再用暗色写出一共几项；还在跑生成器时
        // 写「加载中」；上面或下面还有看不到的项时在行尾画箭头；中间一条横线。
        let total = completion::total(&menu.candidates);
        let selected = if menu.items.is_empty() { 0 } else { menu.selected + 1 };
        let count = format!("{selected}/{}", menu.items.len());
        let mut col = completion::cells(&count);
        spans.push(Span { bold: true, ..plain(top_row, 0, cols, count, fg) });
        let mut notes = Vec::new();
        if total != menu.items.len() {
            notes.push(format!("({total})"));
        }
        if menu.loading() {
            notes.push(rust_i18n::t!("completion.loading").into_owned());
        }
        for note in notes {
            let width = completion::cells(&note);
            spans.push(plain(top_row, col + 1, cols, note, dim));
            col += 1 + width;
        }
        let arrows = match (menu.top > 0, menu.top + list_rows < menu.items.len()) {
            (true, true) => "↑↓",
            (true, false) => "↑ ",
            (false, true) => " ↓",
            (false, false) => "",
        };
        let line_end = cols.saturating_sub(if arrows.is_empty() { 0 } else { 3 });
        if !arrows.is_empty() {
            spans.push(plain(top_row, cols - 2, cols, arrows.into(), fg));
        }
        if col + 1 < line_end {
            let line = Bounds::new(
                origin + point(cw * (col + 1) as f32, ch * top_row as f32 + ch / 2.),
                size(cw * (line_end - col - 1) as f32, px(1.)),
            );
            quads.push((line, dim));
        }

        // 分组标签。
        for (n, line) in layout.label_lines.iter().enumerate() {
            let row = top_row + 1 + n as i32;
            let mut col = 0;
            for &i in line {
                spans.push(plain(row, col, cols, labels[i].clone(), color(groups[i].0)));
                col += widths[i] + 1;
            }
        }

        // 候选：色条占开头一列，然后是名字一列和说明。
        let visible = &menu.items[menu.top..(menu.top + list_rows).min(menu.items.len())];
        let name_widths: Vec<usize> = visible.iter().map(|&i| completion::cells(&menu.candidates[i].label)).collect();
        let name_width = name_column(&name_widths, cols);
        for (n, &i) in visible.iter().enumerate() {
            let row = list_row + n as i32;
            let candidate = &menu.candidates[i];
            let group_color = color(candidate.kind);
            if menu.top + n == menu.selected {
                quads.push((row_bounds(row), bg.mix(fg, 0.15)));
                let bar = Bounds::new(origin + point(px(0.), ch * row as f32), size((cw * 0.25).max(px(2.)), ch));
                quads.push((bar, group_color));
            }
            let query: String = menu.typed.chars().skip(candidate.from).collect();
            spans.push(Span {
                marks: completion::highlight(&candidate.label, &candidate.value, &query),
                mark_color: ansi[mark_color(candidate.kind)],
                ..plain(row, 1, text_end, candidate.label.clone(), group_color)
            });
            if let Some(description) = &candidate.description {
                // 名字超出这一列时说明往后挪。
                let col = 1 + name_width.max(name_widths[n]) + 2;
                let text = format!("-- {}", description.replace(['\n', '\r', '\t'], " "));
                spans.push(plain(row, col, text_end, text, dim));
            }
        }
        if visible.is_empty() {
            let status = if menu.loading() { "completion.loading" } else { "completion.no_matches" };
            spans.push(plain(list_row, 1, cols, rust_i18n::t!(status).into_owned(), dim));
        }

        // 放不下全部时在最右边画一条细滚动条。
        if scrollable {
            let width = (cw * 0.25).max(px(2.));
            let x = origin.x + cw * cols as f32 - width;
            let track = ch * list_rows as f32;
            let thumb = (track * (list_rows as f32 / menu.items.len() as f32)).max(ch / 2.);
            let offset = (track - thumb) * (menu.top as f32 / (menu.items.len() - list_rows) as f32);
            let y = origin.y + ch * list_row as f32;
            quads.push((Bounds::new(point(x, y), size(width, track)), bg.mix(fg, 0.08)));
            quads.push((Bounds::new(point(x, y + offset), size(width, thumb)), dim));
        }

        let grid = Bounds::new(origin, size(cw * cols as f32, ch * rows as f32));
        window.with_content_mask(Some(ContentMask { bounds: grid }), |window| {
            window.paint_layer(grid, |window| {
                for (bounds, color) in &quads {
                    window.paint_quad(fill(*bounds, hsla(*color)));
                }
                for span in &spans {
                    self.paint_span(span, origin, metrics, window);
                }
            })
        });
    }

    /// 在网格上画一段字，每个字按自己的宽度占格；放不下时最后一格画省略号。
    fn paint_span(&mut self, span: &Span, origin: Point<Pixels>, metrics: Metrics, window: &mut Window) {
        let (cw, ch) = (metrics.cell.width, metrics.cell.height);
        let baseline = (ch - metrics.ascent - metrics.descent) / 2. + metrics.ascent;
        let available = span.end.saturating_sub(span.col);
        let truncated = completion::cells(&span.text) > available;
        let limit = span.col + if truncated { available.saturating_sub(1) } else { available };
        let mut col = span.col;
        // 宽字符和终端里一样，字形从它占的两格的左边画起。
        let draw = |view: &mut Self, c: char, col: usize, bold: bool, color: Rgb, window: &mut Window| {
            if c != ' ' {
                let position = origin + point(cw * col as f32, ch * span.row as f32);
                let mut buf = [0; 4];
                let attrs = Attrs { bold, ..Attrs::default() };
                let line = view.shape(c.encode_utf8(&mut buf), attrs, window);
                paint_glyphs(&line, position + point(px(0.), baseline), hsla(color), window);
            }
        };
        for (i, c) in span.text.chars().enumerate() {
            let width = usize::from(runode_term::cell_width(c));
            if width == 0 {
                continue;
            }
            if col + width > limit {
                break;
            }
            let marked = span.marks.contains(&i);
            let color = if marked { span.mark_color } else { span.color };
            draw(self, c, col, span.bold || marked, color, window);
            col += width;
        }
        if truncated && col < span.end {
            draw(self, '…', col, false, span.color, window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_below_the_cursor_when_there_is_room() {
        // 24 行，光标在第 2 行，两个分组标签放在一行里。
        let placed = layout(80, 24, 2, 2, 5, &[10, 8]).unwrap();
        assert_eq!(placed, Layout { top_row: 3, label_lines: vec![vec![0, 1]], list_rows: 5 });
        // 候选太多：列表占满下面剩下的行。
        let placed = layout_rows(80, 24, 2, 100);
        assert_eq!(placed.list_rows, 24 - 3 - 2);
    }

    fn layout_rows(cols: usize, rows: usize, cursor: usize, items: usize) -> Layout {
        layout(cols, rows, cursor, cursor, items, &[6]).unwrap()
    }

    #[test]
    fn flips_above_when_below_is_tight() {
        // 光标在最后一行：下面没地方，画在上面，紧挨着光标那一行。
        let placed = layout_rows(80, 24, 23, 50);
        assert_eq!(placed.top_row, 0);
        assert_eq!(1 + placed.label_lines.len() + placed.list_rows, 23);
        // 下面有 6 行、放不下全部，上面更宽裕：往上画。
        let placed = layout_rows(80, 24, 17, 50);
        assert!(placed.top_row < 17);
        // 下面放得下全部：即使上面更宽裕也往下画。
        let placed = layout_rows(80, 24, 17, 3);
        assert_eq!(placed.top_row, 18);
        // 下面只有 2 行、上面也只有 1 行：画在下面，只剩计数和一行候选。
        let placed = layout(80, 4, 1, 1, 9, &[6]).unwrap();
        assert_eq!(placed, Layout { top_row: 2, label_lines: vec![], list_rows: 1 });
        // 一行的终端放不下。
        assert!(layout_none(1));
    }

    fn layout_none(rows: usize) -> bool {
        layout(80, rows, 0, 0, 3, &[]).is_none()
    }

    #[test]
    fn wraps_group_labels_and_avoids_a_wrapped_word() {
        let placed = layout(20, 24, 2, 2, 1, &[10, 8, 12]).unwrap();
        assert_eq!(placed.label_lines, vec![vec![0, 1], vec![2]]);
        // 当前词从上一行开始：往上画时让开整个词。
        let placed = layout(80, 24, 23, 22, 50, &[]).unwrap();
        assert_eq!(1 + placed.list_rows, 22);
        assert_eq!(placed.top_row, 0);
    }

    #[test]
    fn a_few_long_names_do_not_widen_the_name_column() {
        assert_eq!(name_column(&[5, 8, 12], 200), 12);
        // 只有一个很长的：按 24 格。
        let mut widths = vec![8; 19];
        widths.push(27);
        assert_eq!(name_column(&widths, 200), 24);
        // 长名字占了一成以上：九成能放下的宽度。
        let mut widths = vec![8; 8];
        widths.extend([30, 32]);
        assert_eq!(name_column(&widths, 200), 30);
        // 不超过终端宽度的四成。
        assert_eq!(name_column(&[30], 50), 20);
    }

    #[test]
    fn maps_rows_to_items() {
        let shown = Shown { top_row: 3, list_row: 5, list_rows: 4, first_item: 10, items: 12 };
        assert_eq!(shown.rows(), 3..9);
        assert_eq!(shown.item_at(4), None);
        assert_eq!(shown.item_at(5), Some(10));
        assert_eq!(shown.item_at(6), Some(11));
        // 列表行比剩下的项多。
        assert_eq!(shown.item_at(7), None);
        assert_eq!(shown.item_at(9), None);
    }
}
