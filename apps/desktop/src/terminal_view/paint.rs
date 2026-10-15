//! 把一帧画出来：单元格尺寸、字形排版缓存，以及背景、文字、装饰线和光标的绘制。

use std::{collections::HashMap, rc::Rc, sync::LazyLock};

use gpui::{
    Bounds, ContentMask, FontStyle, FontWeight, Hsla, Pixels, Point, ShapedLine, SharedString, TextRun, Window, fill,
    point, px, size,
};
use runode_config::CellHeight;
use runode_types::{
    color::Rgb,
    frame::{Attrs, CursorShape, Frame},
};

use super::{Metrics, TerminalView, sprites};
use crate::ui::hsla;

/// 字体的下划线粗细（em 的比例），自绘字符的线宽由它算出。GPUI 不公开字体的下划线粗细，
/// 这里取 Hack 与 Menlo 的 post 表数值，两者都是 90/2048。
const UNDERLINE_THICKNESS_EM: f32 = 90. / 2048.;

/// 行高 `height` 对齐到缩放比例是 `scale` 的设备像素：先取整到整数点，再向上对齐到设备像素。缩放比例
/// 是小数（如 1.25）时整数点不落在设备像素上，一行行背景之间会有缝。
fn cell_height(height: f32, scale: f32) -> f32 {
    let snapped = (height * scale).round() / scale;
    // 减去一点，免得乘出来的浮点误差多进一个像素。
    (snapped.ceil() * scale - 1e-3).ceil() / scale
}

fn glyph_table(attrs: Attrs) -> usize {
    usize::from(attrs.bold) | usize::from(attrs.italic) << 1
}

impl TerminalView {
    pub(super) fn metrics(&mut self, window: &Window) -> Metrics {
        let scale = window.scale_factor();
        // 尺寸是按当时的缩放比例对齐到设备像素的，窗口换到另一块屏上要重算，不然背景块之间有缝。
        if let Some(metrics) = self.metrics.filter(|metrics| metrics.scale == scale) {
            return metrics;
        }
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&self.font);
        // 对齐到设备像素，相邻单元格背景之间才不会出现缝隙。
        let snap = |v: f32| (v * scale).round() / scale;
        let width = text_system
            .advance(font_id, self.font_size, 'M')
            .map_or(f32::from(self.font_size) * 0.6, |a| f32::from(a.width));
        let ascent = text_system.ascent(font_id, self.font_size);
        let descent = text_system.descent(font_id, self.font_size).abs();
        // 行高取字体本身的 ascent + descent，再按配置的 adjust-cell-height 增减。
        let natural = f32::from(ascent) + f32::from(descent);
        let height = match self.config.adjust_cell_height {
            Some(CellHeight::Pixels(delta)) => natural + delta,
            Some(CellHeight::Percent(percent)) => natural * (1. + percent / 100.),
            None => natural,
        }
        .max(1.);
        let metrics = Metrics { cell: size(px(snap(width)), px(cell_height(height, scale))), ascent, descent, scale };
        self.metrics = Some(metrics);
        metrics
    }

    pub(super) fn shape(&mut self, text: &str, attrs: Attrs, window: &Window) -> Rc<ShapedLine> {
        let table = glyph_table(attrs);
        if let Some(line) = self.glyphs[table].get(text) {
            return line.clone();
        }
        let mut font = self.font.clone();
        if attrs.bold {
            font.weight = FontWeight::BOLD;
        }
        if attrs.italic {
            font.style = FontStyle::Italic;
        }
        let line = Rc::new(window.text_system().shape_line(
            SharedString::from(text.to_owned()),
            self.font_size,
            &[TextRun {
                len: text.len(),
                font,
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ));
        // 只有异常输出才会让缓存无限增长；直接清空，不做逐项淘汰。
        if self.glyphs.iter().map(HashMap::len).sum::<usize>() > 8192 {
            self.glyphs.iter_mut().for_each(HashMap::clear);
        }
        self.glyphs[table].insert(text.to_owned(), line.clone());
        line
    }
}

/// `fg` 与 `bg` 的中间色，用于暗淡（SGR 2）文字。
pub(super) fn faint(fg: Rgb, bg: Rgb) -> Rgb {
    fg.mix(bg, 0.5)
}

/// sRGB 分量（0 到 255）对应的线性值，算相对亮度用；查表省得每帧每格都算乘方。
static LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f32 / 255.;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    })
});

/// WCAG 2.0 的相对亮度。
fn luminance(c: Rgb) -> f32 {
    0.2126 * LINEAR[usize::from(c.0)] + 0.7152 * LINEAR[usize::from(c.1)] + 0.0722 * LINEAR[usize::from(c.2)]
}

/// 两个相对亮度之间的对比度，1 到 21。
fn contrast(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `fg` 在 `bg` 上的对比度不到 `min`（配置项 `minimum-contrast`）时，把它往黑或白（在 `bg` 上
/// 对比度更高的那个）混，混到刚好够为止；混成纯黑、纯白还不够就用纯黑、纯白。Ghostty 直接换成
/// 黑白，这里按比例混、色相不变，深色配色的语法高亮放到浅色底上仍分得出颜色。
pub(super) fn with_contrast(fg: Rgb, bg: Rgb, min: f32) -> Rgb {
    let bg_luminance = luminance(bg);
    if contrast(luminance(fg), bg_luminance) >= min {
        return fg;
    }
    let target =
        if contrast(1., bg_luminance) >= contrast(0., bg_luminance) { Rgb(255, 255, 255) } else { Rgb(0, 0, 0) };
    // 越过 `bg` 的亮度之前对比度只会更低，之后随着混得越多越高，所以能二分找刚够的那一点。
    let (mut low, mut high) = (0., 1.);
    for _ in 0..8 {
        let mid = (low + high) / 2.;
        if contrast(luminance(fg.mix(target, mid)), bg_luminance) >= min {
            high = mid;
        } else {
            low = mid;
        }
    }
    fg.mix(target, high)
}

pub(super) fn paint_frame(
    view: &mut TerminalView,
    frame: &Frame,
    origin: Point<Pixels>,
    metrics: Metrics,
    focused: bool,
    window: &mut Window,
) {
    let cw = metrics.cell.width;
    let ch = metrics.cell.height;
    let grid = Bounds::new(origin, size(cw * f32::from(frame.cols), ch * f32::from(frame.rows)));
    // 平滑滚动时整屏往下错开不足一行，视口上面那一行（y 为 -1）从顶上露出一部分，最下面一行
    // 被网格的下边裁掉。指针换算也跟着错开。
    let origin = origin + point(px(0.), ch * frame.scroll_offset);
    view.grid_origin = origin;
    let cell_origin = |x: u16, y: i32| origin + point(cw * f32::from(x), ch * y as f32);
    let above = (!frame.above.is_empty()).then_some((-1, frame.above.as_slice()));
    let rows = || above.into_iter().chain((0..frame.rows).map(|y| (i32::from(y), frame.row(y))));

    let scale = window.scale_factor();
    let sprite_metrics =
        sprites::Metrics::new(f32::from(cw), f32::from(ch), f32::from(view.font_size) * UNDERLINE_THICKNESS_EM, scale);

    // 闪烁到灭的一半时不画光标；没有焦点时光标不闪，总画空心框。光标落在选区里时也不画，
    // 免得盖住那一格的选区颜色。
    let cursor_hidden = frame.cursor.is_some_and(|c| {
        (focused && !view.cursor_blink_visible && c.blinking)
            || frame.row(c.y).get(usize::from(c.x)).is_some_and(|cell| cell.selected)
    });
    // 有焦点时块状光标是实心的，光标下的字形改用光标文字色（默认背景色）画，保证仍然看得清。
    let filled_cursor = frame
        .cursor
        .filter(|c| focused && !cursor_hidden && c.shape == CursorShape::Block && view.marked_text.is_none());

    let mask = ContentMask { bounds: grid };
    window.with_content_mask(Some(mask), |window| {
        window.paint_layer(grid, |window| {
            // 背景：每行把同色的相邻单元格合并成一块画。
            for (y, row) in rows() {
                let mut x = 0usize;
                while x < row.len() {
                    let Some(bg) = row[x].bg else {
                        x += 1;
                        continue;
                    };
                    let start = x;
                    while x < row.len() && row[x].bg == Some(bg) {
                        x += 1;
                    }
                    window.paint_quad(fill(
                        Bounds::new(cell_origin(start as u16, y), size(cw * (x - start) as f32, ch)),
                        hsla(bg),
                    ));
                }
            }

            if let Some(cursor) = filled_cursor {
                let width = if cursor.wide { cw * 2. } else { cw };
                window.paint_quad(fill(
                    Bounds::new(cell_origin(cursor.x, i32::from(cursor.y)), size(width, ch)),
                    hsla(cursor.color),
                ));
            }

            // 字形和装饰线。
            let baseline = (ch - metrics.ascent - metrics.descent) / 2. + metrics.ascent;
            for (y, row) in rows() {
                for (x, cell) in row.iter().enumerate() {
                    let x = x as u16;
                    if cell.spacer {
                        continue;
                    }
                    let bg = cell.bg.unwrap_or(frame.background);
                    let mut fg = if cell.attrs.faint { faint(cell.fg, bg) } else { cell.fg };
                    let mut attrs = cell.attrs;
                    // 按着 ⌘ 悬停的链接加下划线。
                    if let (Some(link), Ok(y)) = (&view.hovered_link, u16::try_from(y))
                        && link.contains(x, y)
                    {
                        attrs.underline = true;
                    }
                    // 方框线、块元素、Powerline 等符号自绘，铺满单元格，不用字体字形。
                    let sprite = if cell.wide { None } else { sprites::shapes(&cell.text, sprite_metrics) };
                    // 字在底色上看不清时调深或调浅；自绘的符号拼的是图形，照 Ghostty 不调。
                    if sprite.is_none() {
                        fg = with_contrast(fg, bg, view.config.minimum_contrast);
                    }
                    if let Some(cursor) = filled_cursor.filter(|c| c.x == x && i32::from(c.y) == y) {
                        fg = cursor.text;
                    }
                    let position = cell_origin(x, y);
                    let width = if cell.wide { cw * 2. } else { cw };
                    if attrs.underline {
                        window.paint_quad(fill(
                            Bounds::new(position + point(px(0.), ch - px(2.)), size(width, px(1.))),
                            hsla(fg),
                        ));
                    }
                    if cell.attrs.strikethrough {
                        window.paint_quad(fill(
                            Bounds::new(position + point(px(0.), ch / 2.), size(width, px(1.))),
                            hsla(fg),
                        ));
                    }
                    if cell.text.is_empty() || cell.text == " " {
                        continue;
                    }
                    if let Some(shapes) = sprite {
                        sprites::paint(&shapes, position, scale, hsla(fg), window);
                        continue;
                    }
                    let line = view.shape(&cell.text, attrs, window);
                    paint_glyphs(&line, position + point(px(0.), baseline), hsla(fg), window);
                }
            }
        })
    });

    // 盖在文字上方的光标形状，以及输入法预编辑文本。
    let mut cursor_bounds = None;
    if let Some(cursor) = frame.cursor {
        let position = cell_origin(cursor.x, i32::from(cursor.y));
        let width = if cursor.wide { cw * 2. } else { cw };
        cursor_bounds = Some(Bounds::new(position, size(width, ch)));
        let color = hsla(cursor.color);
        window.with_content_mask(Some(mask), |window| {
            window.paint_layer(grid, |window| {
                if let Some(text) = view.marked_text.clone() {
                    let line = view.shape(&text, Attrs::default(), window);
                    let area = Bounds::new(position, size(line.width.max(cw), ch));
                    window.paint_quad(fill(area, hsla(frame.background)));
                    window.paint_quad(fill(
                        Bounds::new(position + point(px(0.), ch - px(2.)), size(area.size.width, px(1.))),
                        hsla(frame.foreground),
                    ));
                    let baseline = (ch - metrics.ascent - metrics.descent) / 2. + metrics.ascent;
                    paint_glyphs(&line, position + point(px(0.), baseline), hsla(frame.foreground), window);
                    return;
                }
                if cursor_hidden {
                    return;
                }
                // 竖条、下划线和空心框的线宽都是一个设备像素。
                let line = px(1. / scale);
                let quads: &[Bounds<Pixels>] = match (focused, cursor.shape) {
                    (true, CursorShape::Block) => &[],
                    // 骑在单元格左边线上，落在两个字符之间而不是贴着右边的字符；贴着可见区左边缘时
                    // 整根会被内容遮罩裁掉，改画在单元格里。可见区按眼下的内容遮罩算：横着裁掉了前几列时
                    // 它的左边缘在网格左边缘的右边。
                    (true, CursorShape::Bar) => {
                        let left = window.content_mask().bounds.origin.x;
                        let x = if position.x - line < left { position.x } else { position.x - line };
                        &[Bounds::new(point(x, position.y), size(line, ch))]
                    }
                    // 和文字下划线同一高度。
                    (true, CursorShape::Underline) => {
                        &[Bounds::new(position + point(px(0.), ch - px(2.)), size(width, line))]
                    }
                    // 没有焦点或明确要求空心时：只画轮廓。
                    _ => &[
                        Bounds::new(position, size(width, line)),
                        Bounds::new(position + point(px(0.), ch - line), size(width, line)),
                        Bounds::new(position, size(line, ch)),
                        Bounds::new(position + point(width - line, px(0.)), size(line, ch)),
                    ],
                };
                for quad in quads {
                    window.paint_quad(fill(*quad, color));
                }
            })
        });
    }
    view.cursor_bounds = cursor_bounds;
}

/// 在 `baseline_origin`（x 为单元格左边缘，y 在基线上）绘制一行已排版字形，
/// 不像 `ShapedLine::paint` 那样每次调用都新建一层。
pub(super) fn paint_glyphs(line: &ShapedLine, baseline_origin: Point<Pixels>, color: Hsla, window: &mut Window) {
    for run in &line.runs {
        for glyph in &run.glyphs {
            // 纵向的偏移照 GPUI 的写法加在基线上：组合附加符号之类排版时可能上下挪过。
            let position = baseline_origin + glyph.position;
            let painted = if glyph.is_emoji {
                window.paint_emoji(position, run.font_id, glyph.id, line.font_size)
            } else {
                window.paint_glyph(position, run.font_id, glyph.id, line.font_size, color)
            };
            if let Err(err) = painted {
                tracing::debug!("glyph paint failed: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: Rgb = Rgb(0xfb, 0xfa, 0xf7);
    const DARK: Rgb = Rgb(0x17, 0x16, 0x18);

    fn ratio(fg: Rgb, bg: Rgb) -> f32 {
        contrast(luminance(fg), luminance(bg))
    }

    #[test]
    fn cell_height_lands_on_device_pixels() {
        // 整数倍的缩放比例下和取整到整数点一样。
        assert_eq!(cell_height(16.3, 2.), 17.);
        assert_eq!(cell_height(16., 1.), 16.);
        for scale in [1.25, 1.5, 1.75, 1.1] {
            for height in [13.9, 16.5, 17., 20.2] {
                let device = cell_height(height, scale) * scale;
                assert!((device - device.round()).abs() < 1e-3, "{height} @ {scale}: {device}");
                assert!(cell_height(height, scale) >= height.floor(), "{height} @ {scale}");
            }
        }
        assert!((cell_height(16.5, 1.25) - 17.6).abs() < 1e-4);
    }

    #[test]
    fn readable_text_keeps_its_color() {
        assert_eq!(with_contrast(Rgb(0x2a, 0x27, 0x24), LIGHT, 3.), Rgb(0x2a, 0x27, 0x24));
        assert_eq!(with_contrast(Rgb(0xe6, 0xe1, 0xd8), DARK, 3.), Rgb(0xe6, 0xe1, 0xd8));
        // 1 是不调，和底色一样的字也照旧。
        assert_eq!(with_contrast(LIGHT, LIGHT, 1.), LIGHT);
    }

    #[test]
    fn dark_scheme_colors_on_a_light_background_get_just_enough_contrast() {
        // 深色配色（Monokai）的正文和青色关键字放到浅色底上。
        for fg in [Rgb(0xf8, 0xf8, 0xf2), Rgb(0x66, 0xd9, 0xef)] {
            let out = with_contrast(fg, LIGHT, 3.);
            assert!(ratio(out, LIGHT) >= 3., "{out:?}");
            // 刚够就停，不一路压成黑色。
            assert!(ratio(out, LIGHT) < 3.2, "{out:?}");
        }
        // 往黑色混是按比例缩，色相不变：青色仍是蓝绿多、红少。
        let cyan = with_contrast(Rgb(0x66, 0xd9, 0xef), LIGHT, 3.);
        assert!(cyan.2 > cyan.1 && cyan.1 > cyan.0, "{cyan:?}");
    }

    #[test]
    fn dark_text_on_a_dark_background_is_lightened() {
        let out = with_contrast(Rgb(0x39, 0x3a, 0x3d), DARK, 3.);
        assert!(ratio(out, DARK) >= 3., "{out:?}");
        assert!(luminance(out) > luminance(Rgb(0x39, 0x3a, 0x3d)));
    }

    #[test]
    fn unreachable_contrast_falls_back_to_black_or_white() {
        assert_eq!(with_contrast(Rgb(128, 128, 128), Rgb(128, 128, 128), 21.), Rgb(0, 0, 0));
        assert_eq!(with_contrast(DARK, DARK, 21.), Rgb(255, 255, 255));
    }
}
