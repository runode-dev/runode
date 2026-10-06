//! 方框线 U+2500–U+257F：按上、右、下、左四个方向的线样式拼出细线、粗线、双线、虚线、圆角和斜线。

use super::{
    Metrics, Shape,
    geometry::{band, cubic, rect},
};

/// 每个方向线的样式（无、细、粗、双线），2 位一组按上、右、下、左打包。
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

pub(super) fn box_drawing(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let light = m.thickness as i32;
    let heavy = light * 2;
    match cp {
        // 虚线：(段数, 线宽, 期望间隙)；间隙至少 4 像素，太窄时虚线看着像实线。
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

/// 由四个方向线段组成的字符：各方向的线段从单元格边缘画到中心，
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
        if left == D || right == D { h_double_bottom } else { h_light_bottom }
    } else if left == N && right == N {
        h_light_bottom
    } else {
        h_light_top
    };
    let down_top = if left == H || right == H {
        h_heavy_top
    } else if left != right || up == down {
        if left == D || right == D { h_double_top } else { h_light_top }
    } else if left == N && right == N {
        h_light_top
    } else {
        h_light_bottom
    };
    let left_right = if up == H || down == H {
        v_heavy_right
    } else if up != down || left == right {
        if up == D || down == D { v_double_right } else { v_light_right }
    } else if up == N && down == N {
        v_light_right
    } else {
        v_light_left
    };
    let right_left = if up == H || down == H {
        v_heavy_left
    } else if up != down || right == left {
        if up == D || down == D { v_double_left } else { v_light_left }
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

/// 横向虚线：左右各留半个间隙，横向平铺时间隔均匀；
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

/// 纵向虚线：从顶部开始，整个间隙留在底部，和上下的实线字符相接时不出现半截间隙。
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

/// 圆角：从单元格边缘沿细线中心线走到离中心 r 处，
/// 用控制点系数 0.25 的三次贝塞尔拐到另一条边，再以细线宽度描边（平头端点）。
/// `dx`、`dy` 为 ±1，表示圆弧向右/左、向下/上伸出单元格。
pub(super) fn arc(m: Metrics, dx: f32, dy: f32, out: &mut Vec<Shape>) {
    let (w, h, t) = (m.width as f32, m.height as f32, m.thickness as f32);
    let cx = ((m.width as i32 - m.thickness as i32).max(0) / 2) as f32 + t / 2.;
    let cy = ((m.height as i32 - m.thickness as i32).max(0) / 2) as f32 + t / 2.;
    let r = w.min(h) / 2.;
    let s = 0.25;
    let (edge_x, edge_y) = (if dx > 0. { w } else { 0. }, if dy > 0. { h } else { 0. });
    // 尺寸为奇数时圆弧端点可能越出单元格边缘半个像素，连到边缘的直线就会掉头，
    // 自相重叠的带子按奇偶规则填充会挖出洞。掉头那段本来就被圆弧盖住，
    // 所以这时直接从圆弧端点起止。
    let mut points = Vec::new();
    if dy * (edge_y - cy) > r {
        points.push([cx, edge_y]);
    }
    points.push([cx, cy + dy * r]);
    cubic(&mut points, [cx, cy + dy * s * r], [cx + dx * s * r, cy], [cx + dx * r, cy]);
    if dx * (edge_x - cx) > r {
        points.push([edge_x, cy]);
    }
    band(&points, -t / 2., t / 2., out);
}

/// 斜线：两端按斜率略微伸出单元格，相邻单元格的斜线才能连上。
pub(super) fn diagonal(m: Metrics, rising: bool, out: &mut Vec<Shape>) {
    let (w, h, t) = (m.width as f32, m.height as f32, m.thickness as f32);
    let sx = (w / h).min(1.) * 0.5;
    let sy = (h / w).min(1.) * 0.5;
    let points = if rising { [[w + sx, -sy], [-sx, h + sy]] } else { [[-sx, -sy], [w + sx, h + sy]] };
    band(&points, -t / 2., t / 2., out);
}

#[cfg(test)]
mod tests {
    use crate::terminal_view::sprites::testing::{art, metrics, raster};

    #[test]
    fn box_lines() {
        let m = metrics(5, 7, 1);
        assert_eq!(art('─', m), [".....", ".....", ".....", "#####", ".....", ".....", "....."].join("\n"));
        assert_eq!(art('│', m), ["..#.."; 7].join("\n"));
        assert_eq!(art('┼', m), ["..#..", "..#..", "..#..", "#####", "..#..", "..#..", "..#.."].join("\n"));
        assert_eq!(art('═', m), [".....", ".....", "#####", ".....", "#####", ".....", "....."].join("\n"));
        assert_eq!(art('║', m), [".#.#."; 7].join("\n"));
        assert_eq!(art('╬', m), [".#.#.", ".#.#.", "##.##", ".....", "##.##", ".#.#.", ".#.#."].join("\n"));
    }

    #[test]
    fn heavy_corner_joins_flush() {
        // 粗线拐角：横线从竖线左缘开始，竖线从横线上缘开始，拐角处是实心方块。
        let m = metrics(6, 8, 1);
        assert_eq!(
            art('┏', m),
            ["......", "......", "......", "..####", "..####", "..##..", "..##..", "..##.."].join("\n")
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
        let col: String = raster('┆', metrics(8, 12, 1)).iter().map(|r| if r[3] == 0 { '.' } else { '#' }).collect();
        assert_eq!(col, "##..##..##..");
    }
}
