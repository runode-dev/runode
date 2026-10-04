//! 自绘字符：方框线 U+2500–U+257F、块元素 U+2580–U+259F 和 Powerline 符号，
//! 移植自 Ghostty 的精灵字体（sprite font），让这些字符精确铺满单元格、相邻单元格无缝拼接。
//!
//! 几何层（`shapes` 及其下的函数）不依赖 GPUI：按设备像素算出单元格内的图元，
//! 坐标原点在单元格左上角。绘制层 `paint` 把图元换算回逻辑像素交给 GPUI。

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Point, Window, fill, point, px};

/// 单元格尺寸和线宽，单位都是设备像素。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub width: u32,
    pub height: u32,
    /// 细线宽度，对应 Ghostty 的 `box_thickness`。
    pub thickness: u32,
}

impl Metrics {
    /// 单元格宽高和下划线粗细都是逻辑像素。线宽按 Ghostty 的算法取
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
    Rect {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        alpha: u8,
    },
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
        0xe0b0..=0xe0bf | 0xe0d2 | 0xe0d4 => powerline(cp, m, &mut out),
        _ => return None,
    }
    Some(out)
}

// ---- 方框线 ----

/// 每个方向线的样式，2 位一组按上、右、下、左打包，同 Ghostty 的 `Lines`。
const N: u8 = 0;
const L: u8 = 1;
const H: u8 = 2;
const D: u8 = 3;

const fn l(up: u8, right: u8, down: u8, left: u8) -> u8 {
    up | right << 2 | down << 4 | left << 6
}

/// U+2500–U+257F 中由四个方向的线段组成的字符；0 表示虚线、圆角、斜线等另行绘制的字符。
#[rustfmt::skip]
const LINES: [u8; 0x80] = [
    l(N,L,N,L), l(N,H,N,H), l(L,N,L,N), l(H,N,H,N), 0, 0, 0, 0,
    0, 0, 0, 0, l(N,L,L,N), l(N,H,L,N), l(N,L,H,N), l(N,H,H,N),
    l(N,N,L,L), l(N,N,L,H), l(N,N,H,L), l(N,N,H,H), l(L,L,N,N), l(L,H,N,N), l(H,L,N,N), l(H,H,N,N),
    l(L,N,N,L), l(L,N,N,H), l(H,N,N,L), l(H,N,N,H), l(L,L,L,N), l(L,H,L,N), l(H,L,L,N), l(L,L,H,N),
    l(H,L,H,N), l(H,H,L,N), l(L,H,H,N), l(H,H,H,N), l(L,N,L,L), l(L,N,L,H), l(H,N,L,L), l(L,N,H,L),
    l(H,N,H,L), l(H,N,L,H), l(L,N,H,H), l(H,N,H,H), l(N,L,L,L), l(N,L,L,H), l(N,H,L,L), l(N,H,L,H),
    l(N,L,H,L), l(N,L,H,H), l(N,H,H,L), l(N,H,H,H), l(L,L,N,L), l(L,L,N,H), l(L,H,N,L), l(L,H,N,H),
    l(H,L,N,L), l(H,L,N,H), l(H,H,N,L), l(H,H,N,H), l(L,L,L,L), l(L,L,L,H), l(L,H,L,L), l(L,H,L,H),
    l(H,L,L,L), l(L,L,H,L), l(H,L,H,L), l(H,L,L,H), l(H,H,L,L), l(L,L,H,H), l(L,H,H,L), l(H,H,L,H),
    l(L,H,H,H), l(H,L,H,H), l(H,H,H,L), l(H,H,H,H), 0, 0, 0, 0,
    l(N,D,N,D), l(D,N,D,N), l(N,D,L,N), l(N,L,D,N), l(N,D,D,N), l(N,N,L,D), l(N,N,D,L), l(N,N,D,D),
    l(L,D,N,N), l(D,L,N,N), l(D,D,N,N), l(L,N,N,D), l(D,N,N,L), l(D,N,N,D), l(L,D,L,N), l(D,L,D,N),
    l(D,D,D,N), l(L,N,L,D), l(D,N,D,L), l(D,N,D,D), l(N,D,L,D), l(N,L,D,L), l(N,D,D,D), l(L,D,N,D),
    l(D,L,N,L), l(D,D,N,D), l(L,D,L,D), l(D,L,D,L), l(D,D,D,D), 0, 0, 0,
    0, 0, 0, 0, l(N,N,N,L), l(L,N,N,N), l(N,L,N,N), l(N,N,L,N),
    l(N,N,N,H), l(H,N,N,N), l(N,H,N,N), l(N,N,H,N), l(N,H,N,L), l(L,N,H,N), l(N,L,N,H), l(H,N,L,N),
];

fn box_drawing(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let light = m.thickness as i32;
    let heavy = light * 2;
    match cp {
        // 虚线：(数量, 线宽, 期望间隙)，取值同 Ghostty 的 `draw2500_257F`。
        0x2504 => dash_h(m, 3, light, light.max(4), out),
        0x2505 => dash_h(m, 3, heavy, light.max(4), out),
        0x2506 => dash_v(m, 3, light, light.max(4), out),
        0x2507 => dash_v(m, 3, heavy, light.max(4), out),
        0x2508 => dash_h(m, 4, light, light.max(4), out),
        0x2509 => dash_h(m, 4, heavy, light.max(4), out),
        0x250a => dash_v(m, 4, light, light.max(4), out),
        0x250b => dash_v(m, 4, heavy, light.max(4), out),
        0x254c => dash_h(m, 2, light, light, out),
        0x254d => dash_h(m, 2, heavy, heavy, out),
        0x254e => dash_v(m, 2, light, heavy, out),
        0x254f => dash_v(m, 2, heavy, heavy, out),
        // ╭╮╯╰：参数是圆弧朝哪个横向、纵向伸出去。
        0x256d => arc(m, 1., 1., out),
        0x256e => arc(m, -1., 1., out),
        0x256f => arc(m, -1., -1., out),
        0x2570 => arc(m, 1., -1., out),
        0x2571 => diagonal(m, true, out),
        0x2572 => diagonal(m, false, out),
        0x2573 => {
            diagonal(m, true, out);
            diagonal(m, false, out);
        }
        _ => {
            let packed = LINES[(cp - 0x2500) as usize];
            lines(m, [0, 2, 4, 6].map(|shift| packed >> shift & 3), out);
        }
    }
}

/// 整像素矩形；两角顺序不限，面积为零时不输出。
fn rect(out: &mut Vec<Shape>, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    if x0 < x1 && y0 < y1 {
        out.push(Shape::Rect {
            x0,
            y0,
            x1,
            y1,
            alpha,
        });
    }
}

/// 移植 Ghostty 的 `linesChar`：各方向的线段从单元格边缘画到中心，
/// 交汇处按相邻线的粗细和是否双线决定伸进中心多少，保证接缝无缺口、无凸起。
fn lines(m: Metrics, [up, right, down, left]: [u8; 4], out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    let light = m.thickness as i32;
    let heavy = light * 2;

    let h_light_top = (h - light).max(0) / 2;
    let h_light_bottom = h_light_top + light;
    let h_heavy_top = (h - heavy).max(0) / 2;
    let h_heavy_bottom = h_heavy_top + heavy;
    let h_double_top = (h_light_top - light).max(0);
    let h_double_bottom = h_light_bottom + light;

    let v_light_left = (w - light).max(0) / 2;
    let v_light_right = v_light_left + light;
    let v_heavy_left = (w - heavy).max(0) / 2;
    let v_heavy_right = v_heavy_left + heavy;
    let v_double_left = (v_light_left - light).max(0);
    let v_double_right = v_light_right + light;

    let up_bottom = if left == H || right == H {
        h_heavy_bottom
    } else if left != right || down == up {
        if left == D || right == D {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if left == N && right == N {
        h_light_bottom
    } else {
        h_light_top
    };
    let down_top = if left == H || right == H {
        h_heavy_top
    } else if left != right || up == down {
        if left == D || right == D {
            h_double_top
        } else {
            h_light_top
        }
    } else if left == N && right == N {
        h_light_top
    } else {
        h_light_bottom
    };
    let left_right = if up == H || down == H {
        v_heavy_right
    } else if up != down || left == right {
        if up == D || down == D {
            v_double_right
        } else {
            v_light_right
        }
    } else if up == N && down == N {
        v_light_right
    } else {
        v_light_left
    };
    let right_left = if up == H || down == H {
        v_heavy_left
    } else if up != down || right == left {
        if up == D || down == D {
            v_double_left
        } else {
            v_light_left
        }
    } else if up == N && down == N {
        v_light_left
    } else {
        v_light_right
    };

    match up {
        L => rect(out, v_light_left, 0, v_light_right, up_bottom, 0xff),
        H => rect(out, v_heavy_left, 0, v_heavy_right, up_bottom, 0xff),
        D => {
            let left_bottom = if left == D { h_light_top } else { up_bottom };
            let right_bottom = if right == D { h_light_top } else { up_bottom };
            rect(out, v_double_left, 0, v_light_left, left_bottom, 0xff);
            rect(out, v_light_right, 0, v_double_right, right_bottom, 0xff);
        }
        _ => {}
    }
    match right {
        L => rect(out, right_left, h_light_top, w, h_light_bottom, 0xff),
        H => rect(out, right_left, h_heavy_top, w, h_heavy_bottom, 0xff),
        D => {
            let top_left = if up == D { v_light_right } else { right_left };
            let bottom_left = if down == D { v_light_right } else { right_left };
            rect(out, top_left, h_double_top, w, h_light_top, 0xff);
            rect(out, bottom_left, h_light_bottom, w, h_double_bottom, 0xff);
        }
        _ => {}
    }
    match down {
        L => rect(out, v_light_left, down_top, v_light_right, h, 0xff),
        H => rect(out, v_heavy_left, down_top, v_heavy_right, h, 0xff),
        D => {
            let left_top = if left == D { h_light_bottom } else { down_top };
            let right_top = if right == D { h_light_bottom } else { down_top };
            rect(out, v_double_left, left_top, v_light_left, h, 0xff);
            rect(out, v_light_right, right_top, v_double_right, h, 0xff);
        }
        _ => {}
    }
    match left {
        L => rect(out, 0, h_light_top, left_right, h_light_bottom, 0xff),
        H => rect(out, 0, h_heavy_top, left_right, h_heavy_bottom, 0xff),
        D => {
            let top_right = if up == D { v_light_left } else { left_right };
            let bottom_right = if down == D { v_light_left } else { left_right };
            rect(out, 0, h_double_top, top_right, h_light_top, 0xff);
            rect(out, 0, h_light_bottom, bottom_right, h_double_bottom, 0xff);
        }
        _ => {}
    }
}

/// 横向虚线，移植 Ghostty 的 `dashHorizontal`：左右各留半个间隙，横向平铺时间隔均匀；
/// 除不尽的像素逐个分给各段，而不是加到间隙里。
fn dash_h(m: Metrics, count: i32, thick: i32, desired_gap: i32, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    if w < count * 2 {
        let light = m.thickness as i32;
        let y = (h - light).max(0) / 2;
        return rect(out, 0, y, w, y + light, 0xff);
    }
    let gap = desired_gap.min(w / (2 * count));
    let total_dash = w - count * gap;
    let (dash, mut extra) = (total_dash / count, total_dash % count);
    let y = (h - thick).max(0) / 2;
    let mut x = gap / 2;
    for _ in 0..count {
        let mut x1 = x + dash;
        if extra > 0 {
            extra -= 1;
            x1 += 1;
        }
        rect(out, x, y, x1, y + thick, 0xff);
        x = x1 + gap;
    }
}

/// 纵向虚线，移植 Ghostty 的 `dashVertical`：从顶部开始，整个间隙留在底部。
fn dash_v(m: Metrics, count: i32, thick: i32, desired_gap: i32, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    if h < count * 2 {
        let light = m.thickness as i32;
        let x = (w - light).max(0) / 2;
        return rect(out, x, 0, x + light, h, 0xff);
    }
    let gap = desired_gap.min(h / (2 * count));
    let total_dash = h - count * gap;
    let (dash, mut extra) = (total_dash / count, total_dash % count);
    let x = (w - thick).max(0) / 2;
    let mut y = 0;
    for _ in 0..count {
        let mut y1 = y + dash;
        if extra > 0 {
            extra -= 1;
            y1 += 1;
        }
        rect(out, x, y, x + thick, y1, 0xff);
        y = y1 + gap;
    }
}

/// 圆角，移植 Ghostty 的 `arc`：从单元格边缘沿细线中心线走到离中心 r 处，
/// 用控制点系数 0.25 的三次贝塞尔拐到另一条边，再以细线宽度描边（平头端点）。
/// `dx`、`dy` 为 ±1，表示圆弧向右/左、向下/上伸出单元格。
fn arc(m: Metrics, dx: f32, dy: f32, out: &mut Vec<Shape>) {
    let (w, h, t) = (m.width as f32, m.height as f32, m.thickness as f32);
    let cx = ((m.width as i32 - m.thickness as i32).max(0) / 2) as f32 + t / 2.;
    let cy = ((m.height as i32 - m.thickness as i32).max(0) / 2) as f32 + t / 2.;
    let r = w.min(h) / 2.;
    let s = 0.25;
    let mut points = vec![[cx, if dy > 0. { h } else { 0. }], [cx, cy + dy * r]];
    cubic(
        &mut points,
        [cx, cy + dy * s * r],
        [cx + dx * s * r, cy],
        [cx + dx * r, cy],
    );
    points.push([if dx > 0. { w } else { 0. }, cy]);
    band(&points, -t / 2., t / 2., out);
}

/// 斜线，移植 Ghostty 的 `lightDiagonal*`：两端按斜率略微伸出单元格，相邻单元格的斜线才能连上。
fn diagonal(m: Metrics, rising: bool, out: &mut Vec<Shape>) {
    let (w, h, t) = (m.width as f32, m.height as f32, m.thickness as f32);
    let sx = (w / h).min(1.) * 0.5;
    let sy = (h / w).min(1.) * 0.5;
    let points = if rising {
        [[w + sx, -sy], [-sx, h + sy]]
    } else {
        [[-sx, -sy], [w + sx, h + sy]]
    };
    band(&points, -t / 2., t / 2., out);
}

// ---- 块元素 ----

#[derive(Clone, Copy)]
enum Align {
    Upper,
    Lower,
    Left,
    Right,
}

/// 象限块 U+2596–U+259F 的组成：1 左上、2 右上、4 左下、8 右下。
const QUADRANTS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];

fn block(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    match cp {
        0x2580 => block_rect(m, Align::Upper, 1., 0.5, out),
        // ▁▂▃▄▅▆▇：下方 1/8 到 7/8。
        0x2581..=0x2587 => block_rect(m, Align::Lower, 1., (cp - 0x2580) as f32 / 8., out),
        0x2588 => rect(out, 0, 0, w, h, 0xff),
        // ▉▊▋▌▍▎▏：左侧 7/8 到 1/8。
        0x2589..=0x258f => block_rect(m, Align::Left, (0x2590 - cp) as f32 / 8., 1., out),
        0x2590 => block_rect(m, Align::Right, 0.5, 1., out),
        // ░▒▓：和 Ghostty 一样用前景色加透明度铺满，不画点阵。
        0x2591..=0x2593 => rect(out, 0, 0, w, h, (cp - 0x2590) as u8 * 0x40),
        0x2594 => block_rect(m, Align::Upper, 1., 0.125, out),
        0x2595 => block_rect(m, Align::Right, 0.125, 1., out),
        _ => {
            let quads = QUADRANTS[(cp - 0x2596) as usize];
            // 中线位置同 Ghostty 的 `Fraction.min`/`max`：两半各自取整，奇数尺寸时不留缝。
            let (x_mid0, x_mid1) = (
                w - (w as f32 * 0.5).round() as i32,
                (w as f32 * 0.5).round() as i32,
            );
            let (y_mid0, y_mid1) = (
                h - (h as f32 * 0.5).round() as i32,
                (h as f32 * 0.5).round() as i32,
            );
            for (bit, x0, x1, y0, y1) in [
                (1, 0, x_mid1, 0, y_mid1),
                (2, x_mid0, w, 0, y_mid1),
                (4, 0, x_mid1, y_mid0, h),
                (8, x_mid0, w, y_mid0, h),
            ] {
                if quads & bit != 0 {
                    rect(out, x0, y0, x1, y1, 0xff);
                }
            }
        }
    }
}

/// 移植 Ghostty 的 `blockShade`：宽高按比例取整，再按对齐方式贴边或居中。
fn block_rect(m: Metrics, align: Align, fw: f32, fh: f32, out: &mut Vec<Shape>) {
    let (cw, ch) = (m.width as i32, m.height as i32);
    let w = (cw as f32 * fw).round() as i32;
    let h = (ch as f32 * fh).round() as i32;
    let (x, y) = match align {
        Align::Upper => ((cw - w) / 2, 0),
        Align::Lower => ((cw - w) / 2, ch - h),
        Align::Left => (0, (ch - h) / 2),
        Align::Right => (cw - w, (ch - h) / 2),
    };
    rect(out, x, y, x + w, y + h, 0xff);
}

// ---- Powerline ----

/// Ghostty `powerline.zig` 覆盖的码点：U+E0B0–U+E0BF 的三角、细线箭头、半圆和斜线，
/// 以及 U+E0D2、U+E0D4。
fn powerline(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let (w, h, t) = (m.width as f32, m.height as f32, m.thickness as f32);
    // 右边的符号都是左边那个水平翻转。
    let start = out.len();
    let mirrored = matches!(cp, 0xe0b3 | 0xe0b6 | 0xe0b7 | 0xe0d4);
    match cp {
        0xe0b0 => out.push(Shape::Polygon(vec![[0., 0.], [w, h / 2.], [0., h]])),
        0xe0b2 => out.push(Shape::Polygon(vec![[w, 0.], [0., h / 2.], [w, h]])),
        0xe0b8 => out.push(Shape::Polygon(vec![[0., 0.], [w, h], [0., h]])),
        0xe0ba => out.push(Shape::Polygon(vec![[w, 0.], [w, h], [0., h]])),
        0xe0bc => out.push(Shape::Polygon(vec![[0., 0.], [w, 0.], [0., h]])),
        0xe0be => out.push(Shape::Polygon(vec![[0., 0.], [w, 0.], [w, h]])),
        0xe0b9 | 0xe0bf => diagonal(m, false, out),
        0xe0bb | 0xe0bd => diagonal(m, true, out),
        // 细线箭头：折线描边，转角斜接。
        0xe0b1 | 0xe0b3 => band(&[[0., 0.], [w, h / 2.], [0., h]], -t / 2., t / 2., out),
        // 实心半圆，半径 min(w, h/2)，贴左边全高。
        0xe0b4 | 0xe0b6 => out.push(Shape::Polygon(half_circle(w, h))),
        // 半圆轮廓：只往内侧描边，外缘与实心半圆重合。
        0xe0b5 | 0xe0b7 => band(&half_circle(w, h), 0., t, out),
        0xe0d2 | 0xe0d4 => {
            out.push(Shape::Polygon(vec![
                [0., 0.],
                [w, 0.],
                [w / 2., h / 2. - t / 2.],
                [0., h / 2. - t / 2.],
            ]));
            out.push(Shape::Polygon(vec![
                [0., h],
                [w, h],
                [w / 2., h / 2. + t / 2.],
                [0., h / 2. + t / 2.],
            ]));
        }
        _ => unreachable!("powerline 码点范围在 shapes 里已经限定"),
    }
    if mirrored {
        for shape in &mut out[start..] {
            if let Shape::Polygon(points) = shape {
                points.iter_mut().for_each(|p| p[0] = w - p[0]);
            }
        }
    }
}

/// 从左上角经右侧圆弧到左下角的半圆轮廓（不含闭合的左边），圆弧用四分之一圆的三次贝塞尔近似。
fn half_circle(w: f32, h: f32) -> Vec<[f32; 2]> {
    let c = (std::f32::consts::SQRT_2 - 1.) * 4. / 3.;
    let r = w.min(h / 2.);
    let mut points = vec![[0., 0.]];
    cubic(&mut points, [r * c, 0.], [r, r - r * c], [r, r]);
    points.push([r, h - r]);
    cubic(&mut points, [r, h - r + r * c], [r * c, h], [0., h]);
    points
}

// ---- 路径工具 ----

/// 把从 `points` 末点出发的三次贝塞尔曲线展平成折线追加进去。
fn cubic(points: &mut Vec<[f32; 2]>, c1: [f32; 2], c2: [f32; 2], end: [f32; 2]) {
    const SEGMENTS: usize = 16;
    let p0 = *points.last().expect("曲线需要起点");
    for i in 1..=SEGMENTS {
        let t = i as f32 / SEGMENTS as f32;
        let u = 1. - t;
        let (a, b, c, d) = (u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t);
        points.push([
            a * p0[0] + b * c1[0] + c * c2[0] + d * end[0],
            a * p0[1] + b * c1[1] + c * c2[1] + d * end[1],
        ]);
    }
}

/// 沿折线铺一条带子：两侧分别是向法向 `(-dy, dx)` 偏移 `d0` 和 `d1` 的折线，转角斜接，
/// 两端平头。`d0 = -t/2, d1 = t/2` 即居中描边；`0..t` 即只向法向一侧描边。
fn band(points: &[[f32; 2]], d0: f32, d1: f32, out: &mut Vec<Shape>) {
    let mut pts: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for &p in points {
        if pts
            .last()
            .is_none_or(|q| (p[0] - q[0]).hypot(p[1] - q[1]) > 1e-3)
        {
            pts.push(p);
        }
    }
    if pts.len() < 2 {
        return;
    }
    let normals: Vec<[f32; 2]> = pts
        .windows(2)
        .map(|s| {
            let (dx, dy) = (s[1][0] - s[0][0], s[1][1] - s[0][1]);
            let len = dx.hypot(dy);
            [-dy / len, dx / len]
        })
        .collect();
    let miter = |i: usize| -> [f32; 2] {
        let n2 = normals[i.min(normals.len() - 1)];
        let Some(&n1) = i.checked_sub(1).and_then(|j| normals.get(j)) else {
            return n2;
        };
        let k = 1. + n1[0] * n2[0] + n1[1] * n2[1];
        // 接近折返时斜接点会飞得很远，退回用后一段的法向（相当于 miter limit）。
        if k < 0.02 {
            return n2;
        }
        [(n1[0] + n2[0]) / k, (n1[1] + n2[1]) / k]
    };
    let side = |d: f32, i: usize| {
        let m = miter(i);
        [pts[i][0] + m[0] * d, pts[i][1] + m[1] * d]
    };
    let mut polygon: Vec<[f32; 2]> = (0..pts.len()).map(|i| side(d0, i)).collect();
    polygon.extend((0..pts.len()).rev().map(|i| side(d1, i)));
    out.push(Shape::Polygon(polygon));
}

// ---- 绘制 ----

/// 把 `shapes` 画在左上角为 `origin` 的单元格里。原点先对齐到设备像素，
/// 整像素矩形才能和设备像素网格重合。
pub fn paint(
    shapes: &[Shape],
    origin: Point<Pixels>,
    scale: f32,
    color: Hsla,
    window: &mut Window,
) {
    let ox = (f32::from(origin.x) * scale).round();
    let oy = (f32::from(origin.y) * scale).round();
    let at = |x: f32, y: f32| point(px((ox + x) / scale), px((oy + y) / scale));
    for shape in shapes {
        match shape {
            Shape::Rect {
                x0,
                y0,
                x1,
                y1,
                alpha,
            } => window.paint_quad(fill(
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

    fn metrics(width: u32, height: u32, thickness: u32) -> Metrics {
        Metrics {
            width,
            height,
            thickness,
        }
    }

    /// 在每个像素中心采样，把图元画进 `width × height` 的覆盖度网格（0–255）。
    fn raster(c: char, m: Metrics) -> Vec<Vec<u8>> {
        let shapes = shapes(&c.to_string(), m).expect("应当是自绘字符");
        let mut grid = vec![vec![0u8; m.width as usize]; m.height as usize];
        for (y, row) in grid.iter_mut().enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                for shape in &shapes {
                    let a = match shape {
                        Shape::Rect {
                            x0,
                            y0,
                            x1,
                            y1,
                            alpha,
                        } => {
                            let inside = (*x0..*x1).contains(&(x as i32))
                                && (*y0..*y1).contains(&(y as i32));
                            if inside { *alpha } else { 0 }
                        }
                        Shape::Polygon(p) => {
                            // 奇偶规则判断像素中心是否在多边形内。
                            let mut inside = false;
                            for i in 0..p.len() {
                                let (a, b) = (p[i], p[(i + 1) % p.len()]);
                                if (a[1] > py) != (b[1] > py)
                                    && px < a[0] + (py - a[1]) / (b[1] - a[1]) * (b[0] - a[0])
                                {
                                    inside = !inside;
                                }
                            }
                            if inside { 0xff } else { 0 }
                        }
                    };
                    *v = (*v).max(a);
                }
            }
        }
        grid
    }

    fn art(c: char, m: Metrics) -> String {
        raster(c, m)
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&v| if v == 0 { '.' } else { '#' })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn line_width_follows_ghostty() {
        // Hack 与 Menlo 的下划线粗细都是 90/2048 em；13px 下 Retina 为 2 设备像素、1x 为 1。
        let underline = 13. * 90. / 2048.;
        assert_eq!(Metrics::new(8., 16., underline, 2.).thickness, 2);
        assert_eq!(Metrics::new(8., 16., underline, 1.).thickness, 1);
        assert_eq!(Metrics::new(7.8, 16., underline, 2.).width, 16);
    }

    #[test]
    fn box_lines() {
        let m = metrics(5, 7, 1);
        assert_eq!(
            art('─', m),
            [
                ".....", ".....", ".....", "#####", ".....", ".....", "....."
            ]
            .join("\n")
        );
        assert_eq!(art('│', m), ["..#.."; 7].join("\n"));
        assert_eq!(
            art('┼', m),
            [
                "..#..", "..#..", "..#..", "#####", "..#..", "..#..", "..#.."
            ]
            .join("\n")
        );
        assert_eq!(
            art('═', m),
            [
                ".....", ".....", "#####", ".....", "#####", ".....", "....."
            ]
            .join("\n")
        );
        assert_eq!(art('║', m), [".#.#."; 7].join("\n"));
        assert_eq!(
            art('╬', m),
            [
                ".#.#.", ".#.#.", "##.##", ".....", "##.##", ".#.#.", ".#.#."
            ]
            .join("\n")
        );
    }

    #[test]
    fn heavy_corner_joins_flush() {
        // 粗线拐角：横线从竖线左缘开始，竖线从横线上缘开始，拐角处是实心方块。
        let m = metrics(6, 8, 1);
        assert_eq!(
            art('┏', m),
            [
                "......", "......", "......", "..####", "..####", "..##..", "..##..", "..##.."
            ]
            .join("\n")
        );
    }

    #[test]
    fn rounded_corner_enters_from_right_and_bottom() {
        let m = metrics(10, 20, 2);
        let g = raster('╭', m);
        // 中心线在 x = 4..6、y = 9..11。
        assert!(g[19][4] == 0xff && g[19][5] == 0xff, "下边中点有线");
        assert!(g[9][9] == 0xff && g[10][9] == 0xff, "右边中点有线");
        assert!(g[0].iter().all(|&v| v == 0), "上边没有线");
        assert!(g.iter().all(|row| row[0] == 0), "左边没有线");
        assert_eq!(g[9][4], 0, "拐角被圆弧切掉");
    }

    #[test]
    fn dashes_tile_evenly() {
        let m = metrics(12, 8, 1);
        // 三段：间隙 2（左右各半个），每段 (12 - 6) / 3 = 2。
        assert_eq!(art('┄', m).lines().nth(3), Some(".##..##..##."));
        let col: String = raster('┆', metrics(8, 12, 1))
            .iter()
            .map(|r| if r[3] == 0 { '.' } else { '#' })
            .collect();
        assert_eq!(col, "##..##..##..");
    }

    #[test]
    fn blocks() {
        let m = metrics(4, 8, 1);
        assert!(raster('█', m).iter().flatten().all(|&v| v == 0xff));
        let upper = raster('▀', m);
        assert!(upper[..4].iter().flatten().all(|&v| v == 0xff));
        assert!(upper[4..].iter().flatten().all(|&v| v == 0));
        assert_eq!(
            art('▂', m),
            [
                "....", "....", "....", "....", "....", "....", "####", "####"
            ]
            .join("\n")
        );
        assert_eq!(art('▐', m), ["..##"; 8].join("\n"));
        assert_eq!(
            art('▚', m),
            [
                "##..", "##..", "##..", "##..", "..##", "..##", "..##", "..##"
            ]
            .join("\n")
        );
    }

    #[test]
    fn shades_use_ghostty_alpha() {
        let m = metrics(4, 8, 1);
        for (c, alpha) in [('░', 0x40), ('▒', 0x80), ('▓', 0xc0)] {
            assert_eq!(
                shapes(&c.to_string(), m),
                Some(vec![Shape::Rect {
                    x0: 0,
                    y0: 0,
                    x1: 4,
                    y1: 8,
                    alpha
                }])
            );
        }
    }

    #[test]
    fn powerline_triangle_and_half_circle() {
        let m = metrics(8, 16, 1);
        assert_eq!(
            shapes("\u{e0b0}", m),
            Some(vec![Shape::Polygon(vec![[0., 0.], [8., 8.], [0., 16.]])]),
            "从左边全高收到右边中点"
        );
        let tri = raster('\u{e0b0}', m);
        assert!(tri[1..15].iter().all(|row| row[0] == 0xff), "左边全高");
        assert!(
            tri[7][6] == 0xff && tri[8][6] == 0xff,
            "尖端附近只剩中间两行"
        );
        assert!(tri[5][6] == 0 && tri[10][6] == 0 && tri[0][7] == 0 && tri[15][7] == 0);
        let left = raster('\u{e0b2}', m);
        assert!(left[1..15].iter().all(|row| row[7] == 0xff), "镜像后贴右边");

        let half = raster('\u{e0b4}', m);
        assert!(half.iter().all(|row| row[0] == 0xff), "贴左边全高");
        assert!(half[0][7] == 0 && half[15][7] == 0, "右上、右下角空");
        assert!(half[7][7] == 0xff && half[8][7] == 0xff, "中段铺到半径处");
        let outline = raster('\u{e0b5}', m);
        assert!(outline[8][7] == 0xff && outline[8][3] == 0, "轮廓只描外缘");
        assert!(
            outline[0][0] == 0xff && outline[15][0] == 0xff,
            "上下端贴着左上、左下角"
        );
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
        let codepoints = (0x2500..=0x259f)
            .chain(0xe0b0..=0xe0bf)
            .chain([0xe0d2, 0xe0d4]);
        for cp in codepoints {
            let c = char::from_u32(cp).unwrap();
            for (w, h, t) in [
                (8, 16, 1),
                (9, 17, 1),
                (11, 21, 2),
                (16, 32, 2),
                (18, 36, 4),
            ] {
                let m = metrics(w, h, t);
                let shapes = shapes(&c.to_string(), m).unwrap();
                assert!(!shapes.is_empty(), "U+{cp:04X} {w}x{h}+{t} 没有图元");
                for shape in &shapes {
                    if let Shape::Polygon(p) = shape {
                        assert!(
                            p.len() >= 3 && p.iter().flatten().all(|v| v.is_finite()),
                            "U+{cp:04X} 多边形无效"
                        );
                    }
                }
                assert!(
                    raster(c, m).iter().flatten().any(|&v| v > 0),
                    "U+{cp:04X} {w}x{h}+{t} 画出来是空的"
                );
            }
            // 极小的单元格只要求不 panic。
            for (w, h, t) in [(1, 1, 1), (2, 3, 1), (3, 2, 4)] {
                let _ = raster(c, metrics(w, h, t));
            }
        }
    }
}
