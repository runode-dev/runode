//! 预览的自动换行：文本的行和 diff 的行按正文的宽度折成几段，一段占屏幕上的一行。列表仍是等高的
//! 行，只是一项从「文件的一行」换成「折出来的一段」。折的位置由 GPUI 的 `LineWrapper` 按字体实际的
//! 字宽算，中文这类宽字符也准。

use std::{cell::RefCell, ops::Range, sync::Arc};

use gpui::{App, HighlightStyle, IndentAdjustment, LineFragment, SharedString, font, px};

/// 折好的各段。条目是文本的一行或 diff 的一行，按显示的文字（制表符已展开）折。
pub(super) struct Wrapped {
    key: (f32, f32, SharedString),
    /// 每一段：第几个条目、在它显示的文字里的字节范围。
    pub rows: Vec<(usize, Range<usize>)>,
    /// 每个条目的第一段在 `rows` 里的位置。
    first: Vec<usize>,
}

impl Wrapped {
    /// 第 `item` 个条目折出来的各段在 `rows` 里的范围；条目不存在时为空。
    pub fn span(&self, item: usize) -> Range<usize> {
        let Some(&start) = self.first.get(item) else {
            return self.rows.len()..self.rows.len();
        };
        start..self.first.get(item + 1).copied().unwrap_or(self.rows.len())
    }

    /// 第 `row` 段是不是它那个条目的第一段。
    pub fn starts_item(&self, row: usize) -> bool {
        self.rows.get(row).is_some_and(|(item, _)| self.first.get(*item) == Some(&row))
    }
}

/// 上一次折行的结果，宽度、字号、字体都没变时直接用；跟着内容放，换了内容就跟着换掉。
#[derive(Default)]
pub(super) struct WrapCache(RefCell<Option<Arc<Wrapped>>>);

impl WrapCache {
    /// `count` 个条目在宽 `width` 里折行；`text` 给出第几个条目显示的文字，不折的条目（diff 的块头、
    /// 截断说明）给 `None`，占一段。
    pub fn get(
        &self,
        width: f32,
        font_size: f32,
        family: &SharedString,
        count: usize,
        text: impl Fn(usize) -> Option<String>,
        cx: &App,
    ) -> Arc<Wrapped> {
        // 栏再窄也留几个字宽，不至于一个字折一行。
        let width = width.max(font_size * 4.);
        let key = (width, font_size, family.clone());
        if let Some(wrapped) = self.0.borrow().as_ref().filter(|wrapped| wrapped.key == key) {
            return wrapped.clone();
        }
        let mut wrapper = cx.text_system().line_wrapper(font(family.clone()), px(font_size));
        let (mut rows, mut first) = (Vec::with_capacity(count), Vec::with_capacity(count));
        for item in 0..count {
            first.push(rows.len());
            let Some(text) = text(item) else {
                rows.push((item, 0..0));
                continue;
            };
            let mut start = 0;
            let fragments = [LineFragment::text(&text)];
            for boundary in wrapper.wrap_line(&fragments, px(width), IndentAdjustment::NoIndent) {
                rows.push((item, start..boundary.ix));
                start = boundary.ix;
            }
            rows.push((item, start..text.len()));
        }
        let wrapped = Arc::new(Wrapped { key, rows, first });
        *self.0.borrow_mut() = Some(wrapped.clone());
        wrapped
    }

    /// 这一帧折好的结果；没折过时为空。
    pub fn current(&self) -> Option<Arc<Wrapped>> {
        self.0.borrow().clone()
    }
}

/// 显示的一行 `text` 里 `range` 那一段的文字和高亮，高亮的范围换算到这一段上。
pub(super) fn segment(
    text: &str,
    runs: &[(Range<usize>, HighlightStyle)],
    range: &Range<usize>,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let piece = text.get(range.clone()).unwrap_or_default().to_owned();
    let runs = runs
        .iter()
        .filter_map(|(run, style)| {
            let (start, end) = (run.start.max(range.start), run.end.min(range.end));
            (start < end).then(|| (start - range.start..end - range.start, *style))
        })
        .collect();
    (piece, runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_shifts_highlights_into_the_piece() {
        let bold = HighlightStyle::default();
        let runs = [(0..3, bold), (4..9, bold), (10..12, bold)];
        let (piece, runs) = segment("let x = foo();", &runs, &(4..10));
        assert_eq!(piece, "x = fo");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, 0..5);
    }

    #[test]
    fn span_covers_every_piece_of_an_item() {
        let wrapped = Wrapped {
            key: (0., 0., SharedString::default()),
            rows: vec![(0, 0..4), (0, 4..6), (1, 0..2)],
            first: vec![0, 2],
        };
        assert_eq!(wrapped.span(0), 0..2);
        assert_eq!(wrapped.span(1), 2..3);
        assert_eq!(wrapped.span(5), 3..3);
        assert!(wrapped.starts_item(2) && !wrapped.starts_item(1));
    }
}
