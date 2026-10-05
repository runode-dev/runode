//! 把一帧画出来：单元格尺寸、字形排版缓存，以及背景、文字、装饰线、光标和灰字建议的绘制。

use std::{collections::HashMap, rc::Rc};

use gpui::{
    Bounds, ContentMask, FontStyle, FontWeight, Hsla, Pixels, Point, ShapedLine, SharedString, TextRun, Window,
    fill, point, px, size,
};
use runode_config::CellHeight;
use runode_model::{
    color::Rgb,
    frame::{Attrs, CursorShape, Frame},
};

use super::{Metrics, TerminalView, hsla};
use crate::sprites;

/// 字体的下划线粗细（em 的比例），自绘字符的线宽由它算出。GPUI 不公开字体的下划线粗细，
/// 这里取 Hack 与 Menlo 的 post 表数值，两者都是 90/2048。
const UNDERLINE_THICKNESS_EM: f32 = 90. / 2048.;

fn glyph_table(attrs: Attrs) -> usize {
    usize::from(attrs.bold) | usize::from(attrs.italic) << 1
}

impl TerminalView {
    pub(super) fn metrics(&mut self, window: &Window) -> Metrics {
        if let Some(metrics) = self.metrics {
            return metrics;
        }
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&self.font);
        let scale = window.scale_factor();
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
        let metrics = Metrics {
            cell: size(px(snap(width)), px(snap(height).ceil())),
            ascent,
            descent,
        };
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
    let grid = Bounds::new(
        origin,
        size(cw * f32::from(frame.cols), ch * f32::from(frame.rows)),
    );
    // 平滑滚动时整屏往下错开不足一行，视口上面那一行（y 为 -1）从顶上露出一部分，最下面一行
    // 被网格的下边裁掉。指针换算也跟着错开。
    let origin = origin + point(px(0.), ch * frame.scroll_offset);
    view.grid_origin = origin;
    let cell_origin = |x: u16, y: i32| origin + point(cw * f32::from(x), ch * y as f32);
    let above = (!frame.above.is_empty()).then_some((-1, frame.above.as_slice()));
    let rows = || above.into_iter().chain((0..frame.rows).map(|y| (i32::from(y), frame.row(y))));

    let scale = window.scale_factor();
    let sprite_metrics = sprites::Metrics::new(
        f32::from(cw),
        f32::from(ch),
        f32::from(view.font_size) * UNDERLINE_THICKNESS_EM,
        scale,
    );

    // 闪烁到灭的一半时不画光标；没有焦点时光标不闪，总画空心框。光标落在选区里时也不画，
    // 免得盖住那一格的选区颜色。
    let cursor_hidden = frame.cursor.is_some_and(|c| {
        (focused && !view.cursor_blink_visible && c.blinking)
            || frame.row(c.y).get(usize::from(c.x)).is_some_and(|cell| cell.selected)
    });
    // 有焦点时块状光标是实心的，光标下的字形改用光标文字色（默认背景色）画，保证仍然看得清。
    let filled_cursor = frame.cursor.filter(|c| {
        focused && !cursor_hidden && c.shape == CursorShape::Block && view.marked_text.is_none()
    });
    let suggestion = view.visible_suggestion().map(|s| s.rest.clone());

    let mask = ContentMask { bounds: grid };
    window.with_content_mask(Some(mask), |window| window.paint_layer(grid, |window| {
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
                    Bounds::new(
                        cell_origin(start as u16, y),
                        size(cw * (x - start) as f32, ch),
                    ),
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
                let mut fg = if cell.attrs.faint {
                    faint(cell.fg, bg)
                } else {
                    cell.fg
                };
                if let Some(cursor) = filled_cursor.filter(|c| c.x == x && i32::from(c.y) == y) {
                    fg = cursor.text;
                }
                let position = cell_origin(x, y);
                let width = if cell.wide { cw * 2. } else { cw };
                if cell.attrs.underline {
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
                // 方框线、块元素、Powerline 等符号自绘，铺满单元格，不用字体字形。
                if !cell.wide
                    && let Some(shapes) = sprites::shapes(&cell.text, sprite_metrics)
                {
                    sprites::paint(&shapes, position, scale, hsla(fg), window);
                    continue;
                }
                let line = view.shape(&cell.text, cell.attrs, window);
                paint_glyphs(&line, position + point(px(0.), baseline), hsla(fg), window);
            }
        }

        // 灰字建议：从光标处往右逐字画，不写进屏幕；画到行尾为止，不折行。落在实心光标下的
        // 那个字和普通文字一样改用光标文字色。
        if let (Some(cursor), Some(rest)) = (frame.cursor, suggestion.as_deref()) {
            let dim = faint(frame.foreground, frame.background);
            let mut x = cursor.x;
            for c in rest.chars() {
                let width = u16::from(runode_term::cell_width(c));
                if width == 0 {
                    continue;
                }
                if x + width > frame.cols {
                    break;
                }
                let color = match filled_cursor {
                    Some(filled) if filled.x == x && filled.y == cursor.y => filled.text,
                    _ => dim,
                };
                if c != ' ' {
                    let mut buf = [0; 4];
                    let line = view.shape(c.encode_utf8(&mut buf), Attrs::default(), window);
                    let position = cell_origin(x, i32::from(cursor.y));
                    paint_glyphs(&line, position + point(px(0.), baseline), hsla(color), window);
                }
                x += width;
            }
        }
    }));

    // 盖在文字上方的光标形状，以及输入法预编辑文本。
    let mut cursor_bounds = None;
    if let Some(cursor) = frame.cursor {
        let position = cell_origin(cursor.x, i32::from(cursor.y));
        let width = if cursor.wide { cw * 2. } else { cw };
        cursor_bounds = Some(Bounds::new(position, size(width, ch)));
        let color = hsla(cursor.color);
        window.with_content_mask(Some(mask), |window| window.paint_layer(grid, |window| {
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
                // 骑在单元格左边线上，落在两个字符之间而不是贴着右边的字符。
                (true, CursorShape::Bar) => &[Bounds::new(position - point(line, px(0.)), size(line, ch))],
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
        }));
    }
    view.cursor_bounds = cursor_bounds;
}

/// 在 `baseline_origin`（x 为单元格左边缘，y 在基线上）绘制一行已排版字形，
/// 不像 `ShapedLine::paint` 那样每次调用都新建一层。
pub(super) fn paint_glyphs(line: &ShapedLine, baseline_origin: Point<Pixels>, color: Hsla, window: &mut Window) {
    for run in &line.runs {
        for glyph in &run.glyphs {
            let position = baseline_origin + point(glyph.position.x, px(0.));
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
