//! 排版视图里一段能选中的文字（`MdText`）：包着 `StyledText`，画字之前先在底下画行内代码和按键的
//! 圆角底色、选中部分的高亮；排好版以后把自己的文本布局登记到视图的表里，鼠标按下、拖动时据此
//! 换算出点在哪段文字的第几个字节（`hit`）。居中排的文字每个折行往右挪的那段，画底色和命中测试
//! 都自己补上：GPUI 画的时候按对齐挪，量位置的接口却不管对齐。

use std::{cell::RefCell, ops::Range, rc::Rc};

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels,
    StyledText, TextAlign, TextLayout, Window, WrappedLineLayout, fill, point, px, quad, size,
};

use super::select::{MdPos, TextKey};

/// 这一帧画出来的各段文字和它们的布局，鼠标事件据此找点在哪段文字上。每帧画之前清空。
pub(in crate::window::preview) type Texts = Rc<RefCell<Vec<Placed>>>;

/// 画出来的一段文字：是哪段、排好的布局、是不是居中排的。
pub(in crate::window::preview) struct Placed {
    key: TextKey,
    layout: TextLayout,
    centered: bool,
}

/// 一段文字底下要垫的圆角框：行内代码只有底色，按键另带边框。
pub(super) struct TextBox {
    pub range: Range<usize>,
    pub background: Hsla,
    pub border: Option<Hsla>,
}

pub(super) struct MdText {
    text: StyledText,
    key: TextKey,
    boxes: Vec<TextBox>,
    selected: Option<(Range<usize>, Hsla)>,
    texts: Texts,
}

impl MdText {
    pub fn new(text: StyledText, key: TextKey, texts: Texts) -> Self {
        Self { text, key, boxes: Vec::new(), selected: None, texts }
    }

    pub fn boxes(mut self, boxes: Vec<TextBox>) -> Self {
        self.boxes = boxes;
        self
    }

    /// 选中的那段和高亮的颜色。
    pub fn selected(mut self, selected: Option<(Range<usize>, Hsla)>) -> Self {
        self.selected = selected;
        self
    }
}

impl IntoElement for MdText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MdText {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.prepaint(id, inspector_id, bounds, state, window, cx);
        // 到这里布局才有位置，登记之后命中测试才能用。
        let centered = matches!(window.text_style().text_align, TextAlign::Center);
        self.texts.borrow_mut().push(Placed { key: self.key, layout: self.text.layout().clone(), centered });
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.text.layout().clone();
        let centered = matches!(window.text_style().text_align, TextAlign::Center);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = layout.line_height();
        // 框比字高一点，在行里上下居中；左右各多出 0.25 个字宽，不挤开旁边的字。
        let box_height = (font_size * 1.3).min(line_height);
        let inset_y = (line_height - box_height) / 2.;
        let pad_x = font_size * 0.25;
        for text_box in &self.boxes {
            for rect in range_rects(&layout, centered, text_box.range.clone()) {
                let rect = Bounds::new(
                    point(rect.left() - pad_x, rect.top() + inset_y),
                    size(rect.size.width + pad_x * 2., box_height),
                );
                let border = text_box.border.unwrap_or(text_box.background);
                let width = if text_box.border.is_some() { px(1.) } else { px(0.) };
                window.paint_quad(quad(rect, px(6.), text_box.background, width, border, Default::default()));
            }
        }
        if let Some((range, color)) = &self.selected {
            for rect in range_rects(&layout, centered, range.clone()) {
                window.paint_quad(fill(rect, *color));
            }
        }
        self.text.paint(id, inspector_id, bounds, state, prepaint, window, cx);
    }
}

/// `layout` 里 `range` 那段字占的各个矩形，自动换行折成几行时每行一个。
fn range_rects(layout: &TextLayout, centered: bool, range: Range<usize>) -> Vec<Bounds<Pixels>> {
    let mut rects = Vec::new();
    if range.is_empty() {
        return rects;
    }
    let bounds = layout.bounds();
    let line_height = layout.line_height();
    let mut top = bounds.top();
    let mut line_start = 0;
    for line in layout.line_layouts() {
        let line_end = line_start + line.len();
        if line_start >= range.end {
            break;
        }
        let unwrapped = &line.unwrapped_layout;
        // 折出来的各行在这一行里的起止字节。
        let breaks: Vec<usize> = line
            .wrap_boundaries
            .iter()
            .map(|boundary| unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index)
            .collect();
        let shifts = row_shifts(&line, bounds.size.width, centered);
        let mut visual_start = 0;
        for (row, visual_end) in breaks.iter().copied().chain([line.len()]).enumerate() {
            let start = range.start.saturating_sub(line_start).max(visual_start);
            let end = (range.end - line_start).min(visual_end);
            // 选区跨过这一行的换行符时，行尾多画一点，看得出换行也选上了。
            let past_end = visual_end == line.len() && range.end > line_end && range.start <= line_end;
            if start < end || (past_end && start <= end) {
                let origin_x = unwrapped.x_for_index(visual_start);
                let x0 = unwrapped.x_for_index(start) - origin_x;
                let mut x1 = unwrapped.x_for_index(end) - origin_x;
                if past_end {
                    x1 += line_height / 4.;
                }
                let y = top + line_height * row as f32;
                rects.push(Bounds::new(point(bounds.left() + shifts[row] + x0, y), size(x1 - x0, line_height)));
            }
            visual_start = visual_end;
        }
        top += line_height * (breaks.len() + 1) as f32;
        line_start = line_end + 1;
    }
    rects
}

/// 逻辑行 `line` 自动换行折成的各行往右挪了多少：居中排时和 GPUI 画的时候一样，是栏宽 `width` 减去
/// 这一折行的宽再除以二；靠左排的都是 0。
fn row_shifts(line: &WrappedLineLayout, width: Pixels, centered: bool) -> Vec<Pixels> {
    let unwrapped = &line.unwrapped_layout;
    let mut start = px(0.);
    line.wrap_boundaries
        .iter()
        .map(|boundary| unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix].position.x)
        .chain([unwrapped.width])
        .map(|end| {
            let shift = if centered { (width - (end - start)) / 2. } else { px(0.) };
            start = end;
            shift
        })
        .collect()
}

/// 把 `position` 换回不挪的排版里的位置：减去它那一折行往右挪的量。
fn unshift(placed: &Placed, position: gpui::Point<Pixels>) -> gpui::Point<Pixels> {
    if !placed.centered {
        return position;
    }
    let layout = &placed.layout;
    let bounds = layout.bounds();
    let line_height = layout.line_height();
    let mut bottom = bounds.top();
    let mut last = px(0.);
    for line in layout.line_layouts() {
        for shift in row_shifts(&line, bounds.size.width, true) {
            bottom += line_height;
            last = shift;
            if position.y < bottom {
                return point(position.x - shift, position.y);
            }
        }
    }
    point(position.x - last, position.y)
}

/// 鼠标在 `position` 时落在哪段文字的哪个位置：正好在某段文字上就是那里；和几段文字同一高度时取
/// 横向最近的那段；在文字之间的空白里取下面那段的开头，再往下没有文字了取最后一段的末尾。
pub(super) fn hit(texts: &[Placed], position: gpui::Point<Pixels>) -> Option<MdPos> {
    let at = |placed: &Placed, position| {
        let layout = &placed.layout;
        let offset = layout.index_for_position(unshift(placed, position)).unwrap_or_else(|offset| offset);
        MdPos { row: placed.key.0, text: placed.key.1, offset: offset.min(layout.len()) }
    };
    if let Some(placed) = texts.iter().find(|placed| placed.layout.bounds().contains(&position)) {
        return Some(at(placed, position));
    }
    let level = texts
        .iter()
        .filter(|placed| (placed.layout.bounds().top()..placed.layout.bounds().bottom()).contains(&position.y))
        .min_by(|a, b| distance_x(&a.layout, position.x).total_cmp(&distance_x(&b.layout, position.x)));
    if let Some(placed) = level {
        let bounds = placed.layout.bounds();
        let clamped = point(position.x.clamp(bounds.left(), bounds.right() - px(1.)), position.y);
        return Some(at(placed, clamped));
    }
    if let Some(placed) =
        texts.iter().filter(|placed| placed.layout.bounds().top() > position.y).min_by_key(|placed| placed.key)
    {
        return Some(MdPos { row: placed.key.0, text: placed.key.1, offset: 0 });
    }
    texts.iter().max_by_key(|placed| placed.key).map(|placed| MdPos {
        row: placed.key.0,
        text: placed.key.1,
        offset: placed.layout.len(),
    })
}

/// 这一帧画着的文字里，正好在 `position` 底下的那个字的位置；不在任何字上时为空。点链接、悬停用。
pub(super) fn exact_hit(texts: &[Placed], position: gpui::Point<Pixels>) -> Option<(TextKey, usize)> {
    texts.iter().find(|placed| placed.layout.bounds().contains(&position)).and_then(|placed| {
        placed.layout.index_for_position(unshift(placed, position)).ok().map(|offset| (placed.key, offset))
    })
}

fn distance_x(layout: &TextLayout, x: Pixels) -> f32 {
    let bounds = layout.bounds();
    f32::from((bounds.left() - x).max(x - bounds.right()).max(px(0.)))
}
