//! 自绘字符：方框线、块元素、盲文、六分块与八分块、几何三角、Powerline 和 git 分支符号，
//! 按单元格像素尺寸直接生成几何图形，不用字体字形，让这些字符精确铺满单元格、相邻单元格无缝拼接。
//!
//! 几何层（`shapes` 及其下的函数）不依赖 GPUI：按设备像素算出单元格内的图元，
//! 坐标原点在单元格左上角。绘制层 `paint` 把图元换算回逻辑像素交给 GPUI。
//! 各类字符的几何分在子模块里：方框线（`box_drawing`）、块元素和盲文等（`blocks`）、
//! Powerline（`powerline`）、git 分支符号（`branch`），它们共用的路径运算在 `geometry`。

mod blocks;
mod box_drawing;
mod branch;
mod geometry;
mod powerline;
#[cfg(test)]
mod testing;

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, fill, point, px};

use blocks::{block, braille, corner_triangle, mosaic};
use box_drawing::box_drawing;
use branch::branch;
use powerline::powerline;

/// 单元格尺寸和线宽，单位都是设备像素。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub width: u32,
    pub height: u32,
    /// 细线宽度；粗线是它的两倍。
    pub thickness: u32,
}

impl Metrics {
    /// 单元格宽高和下划线粗细都是逻辑像素。线宽取
    /// `max(1, ceil(下划线粗细))`，在设备像素上取整，Retina 下也是整像素宽、不发虚。
    pub fn new(cell_width: f32, cell_height: f32, underline_thickness: f32, scale: f32) -> Self {
        Self {
            width: (cell_width * scale).round() as u32,
            height: (cell_height * scale).round() as u32,
            thickness: (underline_thickness * scale).ceil().max(1.) as u32,
        }
    }
}

/// 设备像素坐标下的图元。
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// 整像素对齐的矩形，`alpha` 是前景色的不透明度（0xff 为实心）。
    Rect { x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8 },
    /// 实心填充的闭合简单多边形，边缘由 GPUI 抗锯齿。
    Polygon(Vec<[f32; 2]>),
}

/// 单元格文本是单个受支持码点时返回它的图元，否则返回 `None`，交给字体绘制。
pub fn shapes(text: &str, m: Metrics) -> Option<Vec<Shape>> {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let cp = c as u32;
    let mut out = Vec::new();
    match cp {
        0x2500..=0x257f => box_drawing(cp, m, &mut out),
        0x2580..=0x259f => block(cp, m, &mut out),
        0x25e2..=0x25e5 | 0x25f8..=0x25fa | 0x25ff => corner_triangle(cp, m, &mut out),
        0x2800..=0x28ff => braille(cp, m, &mut out),
        0x1fb00..=0x1fb3b | 0x1cd00..=0x1cde5 | 0x1cea0 | 0x1cea3 | 0x1cea8 | 0x1ceab | 0x1fbe6 | 0x1fbe7 => {
            mosaic(cp, m, &mut out)
        }
        0xe0b0..=0xe0bf | 0xe0d2 | 0xe0d4 => powerline(cp, m, &mut out),
        0xf5d0..=0xf60d => branch(cp, m, &mut out),
        _ => return None,
    }
    Some(out)
}

// ---- 绘制 ----

/// 把 `shapes` 画在左上角为 `origin` 的单元格里。原点先对齐到设备像素，
/// 整像素矩形才能和设备像素网格重合。
pub fn paint(shapes: &[Shape], origin: Point<Pixels>, scale: f32, color: Hsla, window: &mut Window) {
    let ox = (f32::from(origin.x) * scale).round();
    let oy = (f32::from(origin.y) * scale).round();
    let at = |x: f32, y: f32| point(px((ox + x) / scale), px((oy + y) / scale));
    for shape in shapes {
        match shape {
            Shape::Rect { x0, y0, x1, y1, alpha } => window.paint_quad(fill(
                Bounds::from_corners(at(*x0 as f32, *y0 as f32), at(*x1 as f32, *y1 as f32)),
                color.opacity(f32::from(*alpha) / 255.),
            )),
            Shape::Polygon(points) => {
                let points: Vec<_> = points.iter().map(|p| at(p[0], p[1])).collect();
                let mut builder = PathBuilder::fill();
                builder.add_polygon(&points, true);
                match builder.build() {
                    Ok(path) => window.paint_path(path, color),
                    Err(err) => tracing::debug!("sprite path failed: {err}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::{metrics, raster};

    #[test]
    fn line_width_rounds_up_to_device_pixels() {
        // Hack 与 Menlo 的下划线粗细都是 90/2048 em；13px 下 Retina 为 2 设备像素、1x 为 1。
        let underline = 13. * 90. / 2048.;
        assert_eq!(Metrics::new(8., 16., underline, 2.).thickness, 2);
        assert_eq!(Metrics::new(8., 16., underline, 1.).thickness, 1);
        assert_eq!(Metrics::new(7.8, 16., underline, 2.).width, 16);
    }

    #[test]
    fn unsupported_text_falls_back_to_font() {
        let m = metrics(8, 16, 1);
        assert_eq!(shapes("a", m), None);
        assert_eq!(shapes("─━", m), None);
        assert_eq!(shapes("\u{e0c0}", m), None);
        assert_eq!(shapes("\u{25a0}", m), None);
    }

    #[test]
    fn every_codepoint_draws_something() {
        // 全部自绘范围。
        let codepoints: Vec<u32> = (0x2500..=0x259f)
            .chain(0x2800..=0x28ff)
            .chain(0x1fb00..=0x1fb3b)
            .chain(0x1cd00..=0x1cde5)
            .chain([0x1cea0, 0x1cea3, 0x1cea8, 0x1ceab, 0x1fbe6, 0x1fbe7])
            .chain(0xe0b0..=0xe0bf)
            .chain([0xe0d2, 0xe0d4])
            .chain((0x25e2..=0x25e5).chain(0x25f8..=0x25fa).chain([0x25ff]))
            .chain(0xf5d0..=0xf60d)
            .collect();
        let m = metrics(8, 16, 1);
        let drawable = (0..=0x1ffff).filter_map(char::from_u32).filter(|c| shapes(&c.to_string(), m).is_some()).count();
        assert_eq!(drawable, codepoints.len(), "自绘范围多出了别的码点");
        // 空白盲文 U+2800 本来就什么都不画。
        for cp in codepoints.into_iter().filter(|&cp| cp != 0x2800) {
            let c = char::from_u32(cp).unwrap();
            for (w, h, t) in [(8, 16, 1), (9, 17, 1), (11, 21, 2), (16, 32, 2), (18, 36, 4)] {
                let m = metrics(w, h, t);
                let shapes = shapes(&c.to_string(), m).unwrap();
                assert!(!shapes.is_empty(), "U+{cp:04X} {w}x{h}+{t} 没有图元");
                for shape in &shapes {
                    if let Shape::Polygon(p) = shape {
                        assert!(p.len() >= 3 && p.iter().flatten().all(|v| v.is_finite()), "U+{cp:04X} 多边形无效");
                    }
                }
                assert!(raster(c, m).iter().flatten().any(|&v| v > 0), "U+{cp:04X} {w}x{h}+{t} 画出来是空的");
            }
            // 极小的单元格只要求不 panic。
            for (w, h, t) in [(1, 1, 1), (2, 3, 1), (3, 2, 4)] {
                let _ = raster(c, metrics(w, h, t));
            }
        }
    }
}
