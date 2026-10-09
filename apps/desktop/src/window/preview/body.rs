//! 预览栏的正文：说明文字、图片和 SVG（底下垫棋盘格）、文本的行（行号、改动标记、语法高亮、
//! 按行选中），以及语法高亮的颜色怎么按终端主题换算。diff 标签的正文在 `diff`。

use std::ops::Range;

use gpui::{
    AnyElement, App, Axis, Bounds, Context, FontStyle, FontWeight, HighlightStyle, ImageSource,
    ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, Role, SMOOTH_SVG_SCALE_FACTOR,
    SharedString, StyledText, Window, canvas, div, fill, img, point, prelude::*, px, size, uniform_list,
};
use runode_shared_types::{color::Rgb, theme};

use super::{
    BODY_PADDING, Loaded, MAX_COLUMNS, Note, Preview, ROW_EXTRA_HEIGHT, marks::Mark, right_fade, wrap::segment,
};
use crate::{
    config::AppConfig,
    ui::{
        hsla,
        scrollbar::{row_markers, scrollbar},
    },
    window::{
        PANE_HEADER_HEIGHT, TITLEBAR_HEIGHT, WindowView,
        project::{ADDED, MODIFIED, REMOVED, RENAMED, panel_message},
        status_bar,
    },
};

/// 改动标记的宽度；只删了行的地方在下一行顶上画一小段，这么高。
const MARK_WIDTH: f32 = 3.;
const REMOVED_MARK_HEIGHT: f32 = 5.;
/// 文字右边留的空，竖的滚动条盖在这里。
pub(super) const TEXT_RIGHT_PADDING: f32 = 12.;
/// 换行时算出来的宽度再让一点，免得估的边框、内边距差了一两个像素时最后一个字被截掉。
pub(super) const WRAP_SLACK: f32 = 4.;
/// 图片底下棋盘格的两种颜色和格子边长。
const CHECKER_LIGHT: Rgb = Rgb(0xFF, 0xFF, 0xFF);
const CHECKER_DARK: Rgb = Rgb(0xE4, 0xE4, 0xE4);
const CHECKER_CELL: f32 = 8.;

/// 当前终端主题的 ANSI 16 色：默认配色上盖上配置里改过的那几项。
pub(super) fn ansi_palette(cx: &App) -> [Rgb; 16] {
    let mut colors = theme::ANSI;
    for &(ix, rgb) in &cx.global::<AppConfig>().0.palette {
        if let Some(slot) = colors.get_mut(usize::from(ix)) {
            *slot = rgb;
        }
    }
    colors
}

/// 一行显示出来的文字：制表符展开，太长的截断后带省略号。换行按它折。
pub(super) fn shown_text(line: &str) -> String {
    let shown = runode_preview::display_line(line, &[], MAX_COLUMNS);
    let mut text = shown.text;
    if shown.cut {
        text.push('…');
    }
    text
}

pub(super) fn highlight_style(style: runode_preview::Style, fg: Rgb, palette: &[Rgb; 16]) -> HighlightStyle {
    let color = match style.color {
        runode_preview::Color::Foreground => fg,
        runode_preview::Color::Ansi(ix) => palette[usize::from(ix.min(15))],
    };
    HighlightStyle {
        color: Some(hsla(color)),
        font_weight: style.bold.then_some(FontWeight::BOLD),
        font_style: style.italic.then_some(FontStyle::Italic),
        ..HighlightStyle::default()
    }
}

impl WindowView {
    /// 预览栏的正文：说明、图片、SVG、diff 或者文本的行，按读到的内容画；字号按配置的预览字号。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_preview_body(
        &self,
        preview: &Preview,
        width: f32,
        font: SharedString,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let font_size = cx.global::<AppConfig>().0.preview_font_size;
        match &preview.content {
            None => div().flex_1().into_any_element(),
            Some(Loaded::Note(note)) => {
                let text = match note {
                    Note::Binary => rust_i18n::t!("preview.binary").into_owned(),
                    Note::TooLarge => rust_i18n::t!("preview.too_large").into_owned(),
                    Note::Unreadable(err) => rust_i18n::t!("preview.unreadable", error = err).into_owned(),
                    Note::NoChanges => rust_i18n::t!("preview.diff.no_changes").into_owned(),
                    Note::NoContent => rust_i18n::t!("panel.no_content").into_owned(),
                    Note::DiffTooLarge => rust_i18n::t!("preview.diff.too_large").into_owned(),
                };
                panel_message(text.clone(), fg).id("preview-note").role(Role::Label).aria_label(text).into_any_element()
            }
            Some(Loaded::Image(image)) => div()
                .id("preview-image")
                .role(Role::Image)
                .aria_label(preview.name())
                .flex_1()
                .min_h_0()
                .p(px(12.))
                .flex()
                .justify_center()
                .items_start()
                .child(img(image.clone()).max_w_full().max_h_full())
                .into_any_element(),
            Some(Loaded::ImageDiff { old, new }) => {
                // 改之前在左、改之后在右；栏太窄、图片并排缩得比上下排还小时改成上下排。
                // 没有的那边写一句；图片还没解码出来时尺寸未知，先按左右排。
                let sizes: Vec<_> = [old, new]
                    .into_iter()
                    .flatten()
                    .filter_map(|image| image.clone().use_render_image(window, cx))
                    .map(|image| {
                        let size = image.size(0);
                        (size.width.0 as f32, size.height.0 as f32)
                    })
                    .collect();
                let height = f32::from(window.viewport_size().height)
                    - TITLEBAR_HEIGHT
                    - PANE_HEADER_HEIGHT
                    - status_bar::height(cx);
                let stacked = stack_images(width - 24., height - 24., &sizes);
                let side = |label: String, color: Rgb, image: &Option<std::sync::Arc<gpui::Image>>| {
                    div()
                        .id(SharedString::from(label.clone()))
                        .role(Role::Image)
                        .aria_label(label.clone())
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(IMAGE_GAP))
                        .child(div().h(px(IMAGE_LABEL_HEIGHT)).text_color(hsla(color)).child(label))
                        .child(match image {
                            Some(image) => div()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .flex()
                                .justify_center()
                                .child(img(image.clone()).max_w_full().max_h_full())
                                .into_any_element(),
                            None => div()
                                .text_color(hsla(fg).opacity(0.5))
                                .child(rust_i18n::t!("preview.diff.image_missing").into_owned())
                                .into_any_element(),
                        })
                };
                let body = div().flex_1().min_h_0().p(px(12.)).flex().gap(px(IMAGE_GAP));
                if stacked { body.flex_col() } else { body.items_start() }
                    .child(side(rust_i18n::t!("preview.diff.image_before").into_owned(), REMOVED, old))
                    .child(side(rust_i18n::t!("preview.diff.image_after").into_owned(), ADDED, new))
                    .into_any_element()
            }
            Some(Loaded::Svg(image)) => {
                // 宽了就按预览栏的宽度等比缩小；高了能上下滚。
                let size = image.size(0);
                let (w, h) =
                    (size.width.0 as f32 / SMOOTH_SVG_SCALE_FACTOR, size.height.0 as f32 / SMOOTH_SVG_SCALE_FACTOR);
                let fit = ((width - 24.) / w).min(1.);
                div()
                    .id("preview-svg")
                    .role(Role::Image)
                    .aria_label(preview.name())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(12.))
                    .flex()
                    .justify_center()
                    .items_start()
                    .child(
                        div()
                            .flex_none()
                            .relative()
                            .w(px(w * fit))
                            .h(px(h * fit))
                            .child(checkerboard())
                            .child(img(ImageSource::Render(image.clone())).absolute().size_full()),
                    )
                    .into_any_element()
            }
            Some(Loaded::Diff(content)) => self.render_diff_body(content, width, font, font_size, fg, bg, cx),
            Some(Loaded::Text { .. }) if self.markdown_shown(preview) => {
                self.render_markdown(preview, width, font, fg, bg, cx)
            }
            Some(Loaded::Text { lines, truncated, widest, wrap, .. }) => {
                let count = lines.len() + usize::from(*truncated);
                let digits = lines.len().to_string().len();
                // 行号一栏按位数定宽，等宽字体一个数字大约 0.6 个字号宽。
                let gutter = (digits as f32 * font_size * 0.62 + 16.).ceil();
                // 换行时文字能占的宽度：除去左边框、改动标记、行号和右边留的空。
                let wrapped = self.preview_wrap.then(|| {
                    let text_width = width - 1. - MARK_WIDTH - gutter - TEXT_RIGHT_PADDING - WRAP_SLACK;
                    wrap.get(text_width, font_size, &font, count, |ix| lines.get(ix).map(|line| shown_text(line)), cx)
                });
                preview.apply_reveal(wrapped.as_deref());
                let rows = wrapped.as_ref().map_or(count, |wrapped| wrapped.rows.len());
                let list = uniform_list(
                    "preview",
                    rows,
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        this.render_preview_rows(range, font_size, gutter, fg, bg, cx)
                    }),
                )
                .when(wrapped.is_none(), |list| {
                    list.with_width_from_item(Some(*widest))
                        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                })
                .track_scroll(&preview.scroll)
                .size_full()
                .py(px(BODY_PADDING))
                .font_family(font);
                let handle = preview.scroll.0.borrow().base_handle.clone();
                let fade = right_fade(&handle, bg);
                let mut changes: Vec<_> = preview
                    .marks
                    .iter()
                    .map(|(&number, &mark)| {
                        let line = (number as usize).saturating_sub(1).min(count.saturating_sub(1));
                        let rows = match &wrapped {
                            // 上面删了行的标在那一行的第一段上，加的、改的标满整行。
                            Some(wrapped) if mark == Mark::Removed => {
                                let start = wrapped.span(line).start;
                                start..start + 1
                            }
                            Some(wrapped) => wrapped.span(line),
                            None => line..line + 1,
                        };
                        let color = match mark {
                            Mark::Added => ADDED,
                            Mark::Modified => MODIFIED,
                            Mark::Removed => REMOVED,
                        };
                        (rows, hsla(color))
                    })
                    .collect();
                changes.sort_by_key(|(rows, _)| rows.start);
                let markers = row_markers(rows, changes);
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(list)
                    .children(fade)
                    .child(scrollbar("preview-scroll-y", handle.clone(), Axis::Vertical, hsla(fg)).markers(markers))
                    .child(scrollbar("preview-scroll-x", handle, Axis::Horizontal, hsla(fg)))
                    .into_any_element()
            }
        }
    }

    /// 预览栏的行，行高跟着字号 `font_size` 缩放。
    fn render_preview_rows(
        &self,
        range: Range<usize>,
        font_size: f32,
        gutter: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(preview) = self.preview() else {
            return Vec::new();
        };
        let Some(Loaded::Text { lines, highlights, wrap, .. }) = &preview.content else {
            return Vec::new();
        };
        let marks = &preview.marks;
        let palette = ansi_palette(cx);
        let selected = preview.selected_lines().unwrap_or(0..0);
        let selected_bg = hsla(bg.mix(RENAMED, 0.30));
        let dim = hsla(fg).opacity(0.4);
        let last = lines.len();
        let row_height = font_size + ROW_EXTRA_HEIGHT;
        // 换行时一项是折出来的一段：列表里的第几项、哪一行、这一行的哪一段、是不是第一段。
        let wrapped = self.preview_wrap.then(|| wrap.current()).flatten();
        let items: Vec<(usize, usize, Option<Range<usize>>, bool)> = match &wrapped {
            Some(wrapped) => range
                .filter_map(|row| {
                    let (ix, piece) = wrapped.rows.get(row)?;
                    Some((row, *ix, Some(piece.clone()), wrapped.starts_item(row)))
                })
                .collect(),
            None => range.map(|ix| (ix, ix, None, true)).collect(),
        };
        items
            .into_iter()
            .map(|(item, ix, piece, first)| {
                let row = div().flex_none().h(px(row_height)).w_full().flex().items_center().whitespace_nowrap();
                let Some(line) = lines.get(ix) else {
                    // 截断了的文件末尾多一行说明。
                    let note = rust_i18n::t!("preview.truncated", count = last).into_owned();
                    return row
                        .id(("preview-line", item))
                        .role(Role::Label)
                        .aria_label(note.clone())
                        .pl(px(gutter + MARK_WIDTH + 8.))
                        .italic()
                        .text_color(dim)
                        .child(note)
                        .into_any_element();
                };
                let number = u32::try_from(ix + 1).unwrap_or(u32::MAX);
                // 删在文件末尾的标记落在最后一行之后，挪到最后一行上。
                let mark = marks.get(&number).copied().or_else(|| {
                    (ix + 1 == last)
                        .then(|| marks.get(&(number + 1)).copied().filter(|m| *m == Mark::Removed))
                        .flatten()
                });
                // 折出来的后几段接着画加的、改的标记；上面删了行的只画在第一段顶上。
                let mark = mark.filter(|mark| first || *mark != Mark::Removed);
                let marker =
                    div().flex_none().w(px(MARK_WIDTH)).h_full().flex().flex_col().children(mark.map(|mark| {
                        let (color, height) = match mark {
                            Mark::Added => (ADDED, row_height),
                            Mark::Modified => (MODIFIED, row_height),
                            Mark::Removed => (REMOVED, REMOVED_MARK_HEIGHT),
                        };
                        div().w_full().h(px(height)).bg(hsla(color))
                    }));
                let spans = highlights.as_ref().and_then(|all| all.get(ix)).map_or(&[][..], Vec::as_slice);
                let shown = runode_preview::display_line(line, spans, MAX_COLUMNS);
                let runs: Vec<_> = shown
                    .spans
                    .iter()
                    .map(|span| (span.range.clone(), highlight_style(span.style, fg, &palette)))
                    .collect();
                let mut content = shown.text;
                if shown.cut {
                    content.push('…');
                }
                let (content, runs) = match &piece {
                    Some(piece) => segment(&content, &runs, piece),
                    None => (content, runs),
                };
                // 每行报成一段文字，辅助工具读得到看得见的这些行；按行选中的也报出来。
                row.id(("preview-line", item))
                    .role(Role::Label)
                    .aria_label(content.clone())
                    .aria_selected(selected.contains(&ix))
                    .when(selected.contains(&ix), |row| row.bg(selected_bg))
                    .child(marker)
                    .child(
                        div()
                            .flex_none()
                            .w(px(gutter))
                            .pr(px(10.))
                            .flex()
                            .justify_end()
                            .text_color(dim)
                            .when(first, |cell| cell.child(number.to_string())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .pr(px(TEXT_RIGHT_PADDING))
                            .text_color(hsla(fg))
                            .child(StyledText::new(content).with_highlights(runs)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_preview_line(ix, event.modifiers.shift, window, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        this.drag_preview_line(ix, event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| {
                            if let Some(preview) = this.preview_mut() {
                                preview.selecting = false;
                            }
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// 图片对比里两栏之间、标题和图片之间的空隙。
const IMAGE_GAP: f32 = 12.;
/// 图片对比里「改之前」「改之后」那一行的高度。
const IMAGE_LABEL_HEIGHT: f32 = 20.;

/// 宽 `width`、高 `height` 的地方放下 `sizes` 这几张图（原始像素尺寸，只缩不放）：上下排时缩得
/// 比左右排少就上下排。缩得最小的那张说了算。
fn stack_images(width: f32, height: f32, sizes: &[(f32, f32)]) -> bool {
    let fit = |w: f32, h: f32| {
        let h = h - IMAGE_LABEL_HEIGHT - IMAGE_GAP;
        sizes.iter().map(|&(iw, ih)| (w / iw).min(h / ih).min(1.)).fold(1., f32::min)
    };
    let half = |side: f32| (side - IMAGE_GAP) / 2.;
    fit(width, half(height)) > fit(half(width), height)
}

/// 图片底下的棋盘格：透明的地方看得出来，深色背景上黑色的线条也看得清。
fn checkerboard() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |bounds, _, window, _| {
            window.paint_quad(fill(bounds, hsla(CHECKER_LIGHT)));
            let cell = px(CHECKER_CELL);
            let (columns, rows) =
                ((bounds.size.width / cell).ceil() as usize, (bounds.size.height / cell).ceil() as usize);
            for row in 0..rows {
                for column in (row % 2..columns).step_by(2) {
                    let origin = bounds.origin + point(cell * column as f32, cell * row as f32);
                    let cell_bounds = Bounds::new(origin, size(cell, cell)).intersect(&bounds);
                    window.paint_quad(fill(cell_bounds, hsla(CHECKER_DARK)));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::stack_images;

    #[test]
    fn stacks_wide_images_in_a_narrow_tall_column() {
        // 横图放在窄高的栏里，上下排每张能宽一倍。
        assert!(stack_images(480., 900., &[(1280., 640.), (1280., 640.)]));
        // 竖图放在宽矮的地方，左右排。
        assert!(!stack_images(1600., 800., &[(1170., 2532.), (1170., 2532.)]));
        // 两边都放得下原大小时不用换，左右排。
        assert!(!stack_images(1600., 900., &[(100., 100.)]));
        // 还不知道尺寸时左右排。
        assert!(!stack_images(480., 900., &[]));
    }
}
