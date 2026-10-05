//! git 分支符号 U+F5D0–U+F60D：横竖线、圆角和节点圆圈拼成的提交图。

use super::{
    Metrics, Shape,
    box_drawing::arc,
    geometry::{circle, rect, ring},
};

/// U+F5D0–U+F5ED 由哪些部件组成：横线、竖线和四个方向的圆角（同方框线 ╭╮╰╯）。
const BRANCH_H: u8 = 1;
const BRANCH_V: u8 = 2;
const BRANCH_DR: u8 = 4;
const BRANCH_DL: u8 = 8;
const BRANCH_UR: u8 = 16;
const BRANCH_UL: u8 = 32;
#[rustfmt::skip]
const BRANCH_PARTS: [u8; 0x1e] = [
    BRANCH_H, BRANCH_V, 0, 0, 0, 0, BRANCH_DR, BRANCH_DL, BRANCH_UR, BRANCH_UL,
    BRANCH_V | BRANCH_UR, BRANCH_V | BRANCH_DR, BRANCH_UR | BRANCH_DR,
    BRANCH_V | BRANCH_UL, BRANCH_V | BRANCH_DL, BRANCH_UL | BRANCH_DL,
    BRANCH_H | BRANCH_DL, BRANCH_H | BRANCH_DR, BRANCH_DR | BRANCH_DL,
    BRANCH_H | BRANCH_UL, BRANCH_H | BRANCH_UR, BRANCH_UR | BRANCH_UL,
    BRANCH_V | BRANCH_UL | BRANCH_UR, BRANCH_V | BRANCH_DL | BRANCH_DR,
    BRANCH_H | BRANCH_DL | BRANCH_UL, BRANCH_H | BRANCH_UR | BRANCH_DR,
    BRANCH_V | BRANCH_UL | BRANCH_DR, BRANCH_V | BRANCH_UR | BRANCH_DL,
    BRANCH_H | BRANCH_UL | BRANCH_DR, BRANCH_H | BRANCH_UR | BRANCH_DL,
];

/// U+F5EE–U+F60D 的节点每两个一组（先实心后空心），这里是每组连出去的边：
/// 1 上、2 右、4 下、8 左。
const BRANCH_NODES: [u8; 16] = [0, 2, 8, 10, 4, 1, 5, 6, 12, 3, 9, 7, 13, 14, 11, 15];

/// git 分支符号：直线、圆角、渐隐线和带连线的节点，用来画提交图。
pub(super) fn branch(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    let t = m.thickness as i32;
    let (h_top, v_left) = ((h - t).max(0) / 2, (w - t).max(0) / 2);
    match cp {
        0xf5d2 => fade(m, false, true, out),
        0xf5d3 => fade(m, false, false, out),
        0xf5d4 => fade(m, true, true, out),
        0xf5d5 => fade(m, true, false, out),
        0xf5d0..=0xf5ed => {
            let parts = BRANCH_PARTS[(cp - 0xf5d0) as usize];
            if parts & BRANCH_H != 0 {
                rect(out, 0, h_top, w, h_top + t, 0xff);
            }
            if parts & BRANCH_V != 0 {
                rect(out, v_left, 0, v_left + t, h, 0xff);
            }
            for (bit, dx, dy) in
                [(BRANCH_DR, 1., 1.), (BRANCH_DL, -1., 1.), (BRANCH_UR, 1., -1.), (BRANCH_UL, -1., -1.)]
            {
                if parts & bit != 0 {
                    arc(m, dx, dy, out);
                }
            }
        }
        _ => {
            let i = cp - 0xf5ee;
            branch_node(m, BRANCH_NODES[(i / 2) as usize], i.is_multiple_of(2), out);
        }
    }
}

/// 分支节点：圆心落在细线中心上，半径取到最近单元格边的距离，
/// 连出去的线从圆周画到单元格边缘。
fn branch_node(m: Metrics, edges: u8, filled: bool, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    let t = m.thickness as i32;
    let (h_top, v_left) = ((h - t).max(0) / 2, (w - t).max(0) / 2);
    let (tf, wf, hf) = (t as f32, w as f32, h as f32);
    let (cx, cy) = (v_left as f32 + tf / 2., h_top as f32 + tf / 2.);
    let r = cx.min(cy).min(wf - cx).min(hf - cy);
    if edges & 1 != 0 {
        rect(out, v_left, 0, v_left + t, (cy - r + tf / 2.).ceil() as i32, 0xff);
    }
    if edges & 2 != 0 {
        rect(out, (cx + r - tf / 2.).floor() as i32, h_top, w, h_top + t, 0xff);
    }
    if edges & 4 != 0 {
        rect(out, v_left, (cy + r - tf / 2.).floor() as i32, v_left + t, h, 0xff);
    }
    if edges & 8 != 0 {
        rect(out, 0, h_top, (cx - r + tf / 2.).ceil() as i32, h_top + t, 0xff);
    }
    // 空心节点是半径 r - t/2、线宽 t 的圆环；r 不足一个线宽时就是实心圆。
    let shape = if filled || r <= tf { circle(cx, cy, r) } else { ring(circle(cx, cy, r), circle(cx, cy, r - tf)) };
    out.push(Shape::Polygon(shape));
}

/// 渐隐线：沿线逐像素改变不透明度，`toward_end` 时从起点的
/// 不透明渐隐到右端或底端，否则从透明渐显。
fn fade(m: Metrics, vertical: bool, toward_end: bool, out: &mut Vec<Shape>) {
    let (w, h) = (m.width as i32, m.height as i32);
    let t = m.thickness as i32;
    let (across, along) = if vertical { ((w - t).max(0) / 2, h) } else { ((h - t).max(0) / 2, w) };
    let step = 255. / along as f32;
    for i in 0..along {
        let alpha = if toward_end { 255. - step * i as f32 } else { step * i as f32 };
        let alpha = alpha.round() as u8;
        if vertical {
            rect(out, across, i, across + t, i + 1, alpha);
        } else {
            rect(out, i, across, i + 1, across + t, alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sprites::testing::{metrics, raster};

    #[test]
    fn branch_symbols() {
        let m = metrics(10, 20, 2);
        // 圆心在 (5, 10)，半径 5。
        let hollow = raster('\u{f5ef}', m);
        assert_eq!(hollow[10][5], 0, "空心节点中间是空的");
        assert!(hollow[10][0] == 0xff && hollow[10][9] == 0xff, "圆环");
        assert_eq!(raster('\u{f5ee}', m)[10][5], 0xff, "实心节点");
        let cross = raster('\u{f60d}', m);
        assert!(cross[0][4] == 0xff && cross[19][4] == 0xff, "上下连到边");
        assert!(cross[9][0] == 0xff && cross[9][9] == 0xff, "左右连到边");
        let fade = raster('\u{f5d2}', m);
        assert!(fade[9].windows(2).all(|p| p[0] > p[1]) && fade[9][0] == 0xff, "向右渐隐");
    }
}
