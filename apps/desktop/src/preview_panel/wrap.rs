//! 预览的自动换行：文本的行和 diff 的行按正文的宽度折成几段，一段占屏幕上的一行。列表仍是等高的
//! 行，只是一项从「文件的一行」换成「折出来的一段」。折的位置由 GPUI 的 `LineWrapper` 按字体实际的
//! 字宽算，中文这类宽字符也准。

use std::{
    cell::RefCell,
    ops::Range,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{Context, HighlightStyle, IndentAdjustment, LineFragment, SharedString, Task, font, px};

/// 宽度、字号、字体。
type Key = (f32, f32, SharedString);

/// 整遍折一次超过这么久，宽度连着变（拖分隔条、改窗口大小）时就不每帧重折。
const REWRAP_BUDGET: Duration = Duration::from_millis(4);
/// 宽度停下这么久才按新宽度重折。
const REWRAP_SETTLE: Duration = Duration::from_millis(120);

/// 折好的各段。条目是文本的一行或 diff 的一行，按显示的文字（制表符已展开）折。
pub(super) struct Wrapped {
    key: Key,
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
pub(super) struct WrapCache(RefCell<CacheState>);

#[derive(Default)]
struct CacheState {
    wrapped: Option<Arc<Wrapped>>,
    /// 上一次整遍折行花了多久。
    cost: Duration,
    /// 沿用旧结果时最近要的宽度，和它第一次被要的时刻。
    pending: Option<(f32, Instant)>,
    /// 宽度停下后叫视图重画、按新宽度重折的计时；换一个就取消旧的。
    settle: Option<Task<()>>,
}

impl WrapCache {
    /// `count` 个条目在宽 `width` 里折行；`text` 给出第几个条目显示的文字，不折的条目（diff 的块头、
    /// 截断说明）给 `None`，占一段。
    ///
    /// 大文件整遍折一次要好几毫秒，宽度连着变时先沿用上一次的结果（见 `defer_rewrap`），宽度停下一会儿再
    /// 叫视图重画、重折：拖动时右边可能短暂被裁，换来不每帧都卡。
    pub fn get<V: 'static>(
        &self,
        width: f32,
        font_size: f32,
        family: &SharedString,
        count: usize,
        text: impl Fn(usize) -> Option<String>,
        cx: &mut Context<V>,
    ) -> Arc<Wrapped> {
        // 栏再窄也留几个字宽，不至于一个字折一行。
        let width = width.max(font_size * 4.);
        let key = (width, font_size, family.clone());
        let now = Instant::now();
        {
            let mut state = self.0.borrow_mut();
            let state = &mut *state;
            if let Some(wrapped) = &state.wrapped {
                if wrapped.key == key {
                    return wrapped.clone();
                }
                if defer_rewrap(&wrapped.key, &key, state.cost, state.pending, now) {
                    if state.pending.is_none_or(|(pending, _)| pending != width) {
                        state.pending = Some((width, now));
                    }
                    state.settle = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(REWRAP_SETTLE).await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    }));
                    return wrapped.clone();
                }
            }
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
        let mut state = self.0.borrow_mut();
        state.wrapped = Some(wrapped.clone());
        state.cost = now.elapsed();
        state.pending = None;
        state.settle = None;
        wrapped
    }

    /// 这一帧折好的结果；没折过时为空。
    pub fn current(&self) -> Option<Arc<Wrapped>> {
        self.0.borrow().wrapped.clone()
    }
}

/// 折好的是 `cached`，这次要 `key`：只有宽度变了、上一次整遍折行花了 `cost` 超出一帧的预算，而且宽度还在变
/// （`pending` 记着沿用旧结果时要的宽度和时刻，这次的宽度又不一样，或者那个宽度还没停够 `REWRAP_SETTLE`）
/// 时，先沿用旧结果。小文件、字号或字体变了都马上重折。
fn defer_rewrap(cached: &Key, key: &Key, cost: Duration, pending: Option<(f32, Instant)>, now: Instant) -> bool {
    cached.1 == key.1
        && cached.2 == key.2
        && cost > REWRAP_BUDGET
        && pending.is_none_or(|(width, at)| width != key.0 || now.duration_since(at) < REWRAP_SETTLE)
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
    fn defers_rewrapping_only_while_a_slow_wrap_is_being_resized() {
        let mono = SharedString::from("Mono");
        let cached: Key = (300., 12., mono.clone());
        let (t0, slow, fast) = (Instant::now(), Duration::from_millis(30), Duration::from_millis(1));
        let later = |ms| t0 + Duration::from_millis(ms);
        let cases = [
            // 字号或字体变了：马上重折。
            ((300., 13., mono.clone()), slow, None, t0, false),
            ((300., 12., "Other".into()), slow, None, t0, false),
            // 小文件：照旧每次都折。
            ((310., 12., mono.clone()), fast, None, t0, false),
            // 大文件、宽度刚变：沿用旧的。
            ((310., 12., mono.clone()), slow, None, t0, true),
            ((320., 12., mono.clone()), slow, Some((310., t0)), later(200), true),
            ((310., 12., mono.clone()), slow, Some((310., t0)), later(50), true),
            // 宽度停够了：重折。
            ((310., 12., mono), slow, Some((310., t0)), later(200), false),
        ];
        for (ix, (key, cost, pending, now, defer)) in cases.into_iter().enumerate() {
            assert_eq!(defer_rewrap(&cached, &key, cost, pending, now), defer, "case {ix}");
        }
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
