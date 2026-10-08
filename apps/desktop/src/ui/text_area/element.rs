//! 多行输入框里画文字、选区、光标和提示文字的元素：自己排版，量高度时按宽度折行，画完把排好的
//! 视觉行和位置交回给 `TextArea`，按键、鼠标和输入法换算位置都靠它们。

use gpui::{
    App, AvailableSpace, Bounds, ContentMask, DispatchPhase, Element, ElementId, Entity, GlobalElementId, IntoElement,
    LayoutId, MouseButton, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, SharedString, Style, TextAlign,
    TextRun, TextStyle, Window, WrappedLine, fill, point, px, relative, size,
};

use super::{
    CARET_WIDTH, Metrics, PLACEHOLDER_INSET, TextArea,
    rows::{Row, row_at, rows_from_lines, selection_spans, x_in_row},
};

/// 留给光标的宽度之外才是折行宽度，免得行尾的光标被裁掉。
fn wrap_width(width: Pixels) -> Pixels {
    (width - CARET_WIDTH).max(px(1.))
}

/// 提示文字往右让开了光标，折行宽度跟着减。
fn placeholder_wrap_width(width: Pixels) -> Pixels {
    (wrap_width(width) - CARET_WIDTH - PLACEHOLDER_INSET).max(px(1.))
}

/// 提示文字的样式段：颜色调淡。
fn placeholder_run(style: &TextStyle, len: usize) -> TextRun {
    TextRun {
        len,
        font: style.font(),
        color: style.color.opacity(0.4),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// 输入框里的文字、选区和光标，自己排版绘制以便按位置换算字符。
pub(super) struct TextAreaText {
    pub(super) area: Entity<TextArea>,
}

pub(super) struct TextAreaTextLayout {
    /// 每段硬换行排好的文字。
    lines: Vec<WrappedLine>,
    /// 提示文字；有内容时为 `None`。
    placeholder: Option<Vec<WrappedLine>>,
    rows: Vec<Row>,
    /// 排这一帧时文字的 `version`。
    version: u64,
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
    wrap_width: Pixels,
    scroll_y: Pixels,
    selections: Vec<PaintQuad>,
    /// 有选区时不画光标。
    caret: Option<PaintQuad>,
}

impl IntoElement for TextAreaText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// 一组排好的硬换行一共占几个视觉行。
fn row_count(lines: &[WrappedLine]) -> usize {
    lines.iter().map(|line| line.wrap_boundaries().len() + 1).sum()
}

/// 画一组排好的硬换行，左上角在 `origin`；整段落在 `clip` 上下之外的跳过。
fn paint_lines(
    lines: &[WrappedLine],
    origin: Point<Pixels>,
    line_height: Pixels,
    clip: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut top = origin.y;
    for line in lines {
        let bottom = top + line_height * (line.wrap_boundaries().len() + 1);
        if bottom > clip.top() && top < clip.bottom() {
            line.paint(point(origin.x, top), line_height, TextAlign::Left, None, window, cx).ok();
        }
        top = bottom;
    }
}

impl Element for TextAreaText {
    type RequestLayoutState = ();
    type PrepaintState = TextAreaTextLayout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        // 高度要等知道宽度、折好行才定，所以量尺寸放到排版时做；那时文本样式已经出栈，先取好。
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let area = self.area.clone();
        let mut layout = Style::default();
        layout.size.width = relative(1.).into();
        let id = window.request_measured_layout(layout, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let area = area.read(cx);
            // 和 `prepaint` 用一样的折行宽度和样式段，排好的结果在同一帧里能复用。
            let count = |text: SharedString, runs: &[TextRun], wrap: Option<Pixels>, window: &mut Window| match wrap {
                Some(wrap) => window
                    .text_system()
                    .shape_text(text.clone(), font_size, runs, Some(wrap), None)
                    .map_or_else(|_| text.split('\n').count(), |lines| row_count(&lines)),
                None => text.split('\n').count(),
            };
            let mut rows = count(area.content.clone().into(), &area.runs(&style), width.map(wrap_width), window);
            // 空着时提示文字折成几行就撑几行，免得被裁掉。
            if area.content.is_empty() && !area.placeholder.is_empty() {
                let run = placeholder_run(&style, area.placeholder.len());
                let wrap = width.map(placeholder_wrap_width);
                rows = rows.max(count(area.placeholder.clone(), &[run], wrap, window));
            }
            size(width.unwrap_or_default(), line_height * rows.clamp(area.min_lines, area.max_lines))
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> TextAreaTextLayout {
        let area = self.area.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let wrap_width = wrap_width(bounds.size.width);
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(area.content.clone().into(), font_size, &area.runs(&style), Some(wrap_width), None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();
        let rows = rows_from_lines(&lines);
        let placeholder = (area.content.is_empty() && !area.placeholder.is_empty()).then(|| {
            let run = placeholder_run(&style, area.placeholder.len());
            let wrap = placeholder_wrap_width(bounds.size.width);
            window
                .text_system()
                .shape_text(area.placeholder.clone(), font_size, &[run], Some(wrap), None)
                .map(|lines| lines.into_iter().collect())
                .unwrap_or_default()
        });

        // 竖向滚动：光标动过时滚到它露出来，内容变短时也别在下面留空。
        let cursor = area.cursor();
        let caret_row = row_at(&rows, cursor, area.upstream);
        let max_scroll = (line_height * rows.len() - bounds.size.height).max(px(0.));
        let mut scroll_y = area.scroll_y;
        if area.autoscroll {
            let top = line_height * caret_row;
            if top < scroll_y {
                scroll_y = top;
            }
            if top + line_height > scroll_y + bounds.size.height {
                scroll_y = top + line_height - bounds.size.height;
            }
        }
        let scroll_y = scroll_y.clamp(px(0.), max_scroll);

        let row_top = |row: usize| bounds.top() - scroll_y + line_height * row;
        let selections = selection_spans(&rows, &area.selected)
            .into_iter()
            .map(|(row, left, right)| {
                fill(
                    Bounds::from_corners(
                        point(bounds.left() + left, row_top(row)),
                        point(bounds.left() + right, row_top(row) + line_height),
                    ),
                    style.color.opacity(0.3),
                )
            })
            .collect();
        let caret = (area.selected.is_empty() && !rows.is_empty()).then(|| {
            let height = (font_size * 1.25).min(line_height);
            fill(
                Bounds::new(
                    point(
                        bounds.left() + x_in_row(&rows[caret_row], cursor),
                        row_top(caret_row) + (line_height - height) / 2.,
                    ),
                    size(CARET_WIDTH, height),
                ),
                style.color,
            )
        });
        TextAreaTextLayout {
            lines,
            placeholder,
            rows,
            version: area.version,
            style,
            font_size,
            line_height,
            wrap_width,
            scroll_y,
            selections,
            caret,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut TextAreaTextLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let area = self.area.read(cx);
        let focus_handle = area.focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        let dragging = area.drag.is_some();
        window.handle_input(&focus_handle, crate::ui::input_handler::ElementInput::new(bounds, self.area.clone()), cx);

        let line_height = layout.line_height;
        let origin = point(bounds.left(), bounds.top() - layout.scroll_y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in layout.selections.drain(..) {
                window.paint_quad(selection);
            }
            paint_lines(&layout.lines, origin, line_height, bounds, window, cx);
            // 没有内容时光标停在提示文字前面。
            if let Some(placeholder) = &layout.placeholder {
                let origin = point(origin.x + CARET_WIDTH + PLACEHOLDER_INSET, origin.y);
                paint_lines(placeholder, origin, line_height, bounds, window, cx);
            }
            if focused && let Some(caret) = layout.caret.take() {
                window.paint_quad(caret);
            }
        });

        // 拖选时鼠标出了输入框也要跟着选，所以监听整个窗口的移动和松开。
        if dragging {
            let area = self.area.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left) {
                    area.update(cx, |area, cx| area.drag_to(event.position, window, cx));
                }
            });
            let area = self.area.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    area.update(cx, |area, _| area.drag = None);
                }
            });
        }

        let metrics = Metrics {
            style: layout.style.clone(),
            font_size: layout.font_size,
            line_height,
            wrap_width: layout.wrap_width,
            bounds,
        };
        let rows = std::mem::take(&mut layout.rows);
        let (version, scroll_y) = (layout.version, layout.scroll_y);
        self.area.update(cx, |area, _| {
            area.rows = rows;
            area.rows_version = Some(version);
            area.metrics = Some(metrics);
            area.scroll_y = scroll_y;
            area.autoscroll = false;
        });
    }
}
