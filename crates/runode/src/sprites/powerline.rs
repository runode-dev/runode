//! Powerline 分隔符：三角、细线箭头、半圆和斜线。

use super::{
    Metrics, Shape,
    box_drawing::diagonal,
    geometry::{band, cubic},
};

/// Powerline 分隔符：U+E0B0–U+E0BF 的三角、细线箭头、半圆和斜线，
/// 以及 U+E0D2、U+E0D4。
pub(super) fn powerline(cp: u32, m: Metrics, out: &mut Vec<Shape>) {
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

#[cfg(test)]
mod tests {
    use crate::sprites::{Shape, shapes, testing::{metrics, raster}};

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
}
