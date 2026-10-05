//! 块元素、象限块、盲文、六分块与八分块，以及几何三角：都是按单元格比例填充的矩形或多边形。

use super::{
    Metrics, Shape,
    geometry::{inset, rect, ring},
};

#[derive(Clone, Copy)]
enum Align {
    Upper,
    Lower,
    Left,
    Right,
}

/// 象限块 U+2596–U+259F 的组成：1 左上、2 右上、4 左下、8 右下。
const QUADRANTS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];

pub(super) fn block(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    match cp {
        0x2580 => block_rect(m, Align::Upper, 1., 0.5, out),
        // ▁▂▃▄▅▆▇：下方 1/8 到 7/8。
        0x2581..=0x2587 => block_rect(m, Align::Lower, 1., (cp - 0x2580) as f32 / 8., out),
        0x2588 => rect(out, 0, 0, w, h, 0xff),
        // ▉▊▋▌▍▎▏：左侧 7/8 到 1/8。
        0x2589..=0x258f => block_rect(m, Align::Left, (0x2590 - cp) as f32 / 8., 1., out),
        0x2590 => block_rect(m, Align::Right, 0.5, 1., out),
        // ░▒▓：用前景色加 1/4、1/2、3/4 的不透明度铺满，不画点阵，相邻单元格才不会出现花纹。
        0x2591..=0x2593 => rect(out, 0, 0, w, h, (cp - 0x2590) as u8 * 0x40),
        0x2594 => block_rect(m, Align::Upper, 1., 0.125, out),
        0x2595 => block_rect(m, Align::Right, 0.125, 1., out),
        _ => grid(m, 2, QUADRANTS[(cp - 0x2596) as usize].into(), out),
    }
}

/// 八分块 U+1CD00–U+1CDE5 按码点顺序恰好是 0–255 中去掉下面这些组合后的升序排列
/// （它们已有别的码点，如空格、`█`、象限块和半块）。
const OCTANT_SKIPPED: [u8; 26] = [
    0x00, 0x01, 0x02, 0x03, 0x05, 0x0a, 0x0f, 0x14, 0x28, 0x3f, 0x40, 0x50, 0x55, 0x5a, 0x5f, 0x80,
    0xa0, 0xa5, 0xaa, 0xaf, 0xc0, 0xf0, 0xf5, 0xfa, 0xfc, 0xff,
];

/// 六分块、八分块，以及几个四分之一块（U+1CEA0 等）和居中的半块（U+1FBE6/7）。
pub(super) fn mosaic(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    match cp {
        // 六分块：按码点顺序排列，跳过与半块、全块重复的组合。
        0x1fb00..=0x1fb3b => {
            let i = cp - 0x1fb00;
            grid(m, 3, i + i / 0x14 + 1, out);
        }
        0x1cd00..=0x1cde5 => {
            let mask = (0..=255u8)
                .filter(|b| !OCTANT_SKIPPED.contains(b))
                .nth((cp - 0x1cd00) as usize)
                .unwrap_or(0);
            grid(m, 4, mask.into(), out);
        }
        0x1cea0 => fill_frac(m, [0.5, 1.], [0.75, 1.], out),
        0x1cea3 => fill_frac(m, [0., 0.5], [0.75, 1.], out),
        0x1cea8 => fill_frac(m, [0., 0.5], [0., 0.25], out),
        0x1ceab => fill_frac(m, [0.5, 1.], [0., 0.25], out),
        0x1fbe6 => block_rect(m, Align::Left, 0.5, 0.5, out),
        _ => block_rect(m, Align::Right, 0.5, 0.5, out),
    }
}

/// 2 列 × `rows` 行的格子，`mask` 按行优先每格一位（象限块、六分块、八分块共用）。
fn grid(m: Metrics, rows: u32, mask: u32, out: &mut Vec<Shape>) {
    for i in (0..2 * rows).filter(|i| mask >> i & 1 != 0) {
        let (col, row) = (f64::from(i % 2), f64::from(i / 2));
        let rows = f64::from(rows);
        fill_frac(
            m,
            [col / 2., (col + 1.) / 2.],
            [row / rows, (row + 1.) / rows],
            out,
        );
    }
}

/// 按比例填充单元格的一块：起点按从另一端量的
/// 互补比例取整，终点直接取整，奇数尺寸下相邻两块正好拼满、不留缝也不重叠。
fn fill_frac(m: Metrics, [x0, x1]: [f64; 2], [y0, y1]: [f64; 2], out: &mut Vec<Shape>) {
    let lo = |f: f64, size: u32| (f64::from(size) - ((1. - f) * f64::from(size)).round()) as i32;
    let hi = |f: f64, size: u32| (f * f64::from(size)).round() as i32;
    rect(
        out,
        lo(x0, m.width),
        lo(y0, m.height),
        hi(x1, m.width),
        hi(y1, m.height),
        0xff,
    );
}

/// 盲文：点是 w×w 的整像素方块，剩余像素依次分给
/// 点宽、边距、点距，保证各种单元格尺寸下点阵都均匀。码点低 8 位依次是左列上三点、
/// 右列上三点、左下、右下。
pub(super) fn braille(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let (width, height) = (m.width as i32, m.height as i32);
    let mut w = (width / 4).min(height / 8);
    let (mut x_spacing, mut y_spacing) = (width / 4, height / 8);
    let (mut x_margin, mut y_margin) = (x_spacing / 2, y_spacing / 2);
    let mut x_left = width - 2 * x_margin - x_spacing - 2 * w;
    let mut y_left = height - 2 * y_margin - 3 * y_spacing - 4 * w;
    if x_left >= 2 && y_left >= 4 && w == 0 {
        (w, x_left, y_left) = (w + 1, x_left - 2, y_left - 4);
    }
    if x_left >= 2 && x_margin == 0 {
        (x_margin, x_left) = (1, x_left - 2);
    }
    if y_left >= 2 && y_margin == 0 {
        (y_margin, y_left) = (1, y_left - 2);
    }
    if x_left >= 1 {
        (x_spacing, x_left) = (x_spacing + 1, x_left - 1);
    }
    if y_left >= 3 {
        (y_spacing, y_left) = (y_spacing + 1, y_left - 3);
    }
    if x_left >= 2 {
        (x_margin, x_left) = (x_margin + 1, x_left - 2);
    }
    if y_left >= 2 {
        (y_margin, y_left) = (y_margin + 1, y_left - 2);
    }
    if x_left >= 2 && y_left >= 4 {
        w += 1;
    }
    let dots = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for (bit, (col, row)) in dots.into_iter().enumerate() {
        if cp >> bit & 1 != 0 {
            let x = x_margin + col * (w + x_spacing);
            let y = y_margin + row * (w + y_spacing);
            rect(out, x, y, x + w, y + w, 0xff);
        }
    }
}

/// ◢◣◤◥ 实心直角三角形和 ◸◹◺◿ 空心三角形；空心的只向内描边，外缘与实心的重合。
pub(super) fn corner_triangle(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let (w, h, t) = (m.width as f32, m.height as f32, m.thickness as f32);
    let points = match cp {
        0x25e4 | 0x25f8 => vec![[0., 0.], [0., h], [w, 0.]],
        0x25e5 | 0x25f9 => vec![[0., 0.], [w, h], [w, 0.]],
        0x25e3 | 0x25fa => vec![[0., 0.], [0., h], [w, h]],
        _ => vec![[0., h], [w, h], [w, 0.]],
    };
    if cp <= 0x25e5 {
        out.push(Shape::Polygon(points));
    } else {
        let inner = inset(&points, t);
        out.push(Shape::Polygon(ring(points, inner)));
    }
}

/// 块元素：宽高按比例取整，再按对齐方式贴边或居中。
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

#[cfg(test)]
mod tests {
    use crate::sprites::{Shape, shapes, testing::{art, metrics, raster}};

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
    fn shades_are_translucent_fills() {
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
    fn braille_and_mosaics() {
        let m = metrics(8, 16, 1);
        let full = raster('⣿', m);
        let dots = shapes("⣿", m).unwrap();
        assert_eq!(dots.len(), 8);
        assert!(
            dots.iter()
                .all(|d| matches!(d, Shape::Rect { x0, x1, .. } if x1 - x0 == 2))
        );
        let one = raster('⠁', m);
        assert_eq!(
            one.iter().flatten().filter(|&&v| v > 0).count(),
            4,
            "只有一个 2×2 的点"
        );
        assert!(
            one[..4].iter().all(|row| row[4..].iter().all(|&v| v == 0)),
            "点在左上"
        );
        assert!(
            full[12..].iter().any(|row| row[4..].iter().any(|&v| v > 0)),
            "8 点盲文有右下点"
        );

        let m = metrics(4, 6, 1);
        // 🬀：左上六分之一；八分块 U+1CD00 是 OCTANT-3（第二行左格）。
        assert_eq!(
            art('\u{1fb00}', m),
            ["##..", "##..", "....", "....", "....", "...."].join("\n")
        );
        let m = metrics(4, 8, 1);
        assert_eq!(
            art('\u{1cd00}', m),
            [
                "....", "....", "##..", "##..", "....", "....", "....", "...."
            ]
            .join("\n")
        );
        assert_eq!(
            art('\u{1cea0}', m),
            [
                "....", "....", "....", "....", "....", "....", "..##", "..##"
            ]
            .join("\n")
        );
        assert_eq!(
            art('\u{1fbe7}', m),
            [
                "....", "....", "..##", "..##", "..##", "..##", "....", "...."
            ]
            .join("\n")
        );
    }

    #[test]
    fn corner_triangles() {
        let m = metrics(8, 16, 1);
        let solid = raster('◢', m);
        assert!(
            solid[15][7] == 0xff && solid[0][0] == 0 && solid[15][0] == 0xff && solid[2][7] == 0xff
        );
        assert_eq!(solid[2][2], 0, "左上空");
        let hollow = raster('◸', m);
        assert!(hollow[0].iter().all(|&v| v == 0xff), "上边描边");
        assert!(hollow[..14].iter().all(|row| row[0] == 0xff), "左边描边");
        assert_eq!(hollow[4][2], 0, "内部挖空");
        assert_eq!(hollow[15][7], 0, "外部为空");
    }
}
