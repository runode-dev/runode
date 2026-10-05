//! 几何工具：整像素矩形，以及贝塞尔曲线、等宽线带、多边形内缩和圆环等路径运算。

use super::Shape;

/// 整像素矩形；两角顺序不限，面积或不透明度为零时不输出。
pub(super) fn rect(out: &mut Vec<Shape>, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    if x0 < x1 && y0 < y1 && alpha > 0 {
        out.push(Shape::Rect {
            x0,
            y0,
            x1,
            y1,
            alpha,
        });
    }
}

/// 把从 `points` 末点出发的三次贝塞尔曲线展平成折线追加进去。
pub(super) fn cubic(points: &mut Vec<[f32; 2]>, c1: [f32; 2], c2: [f32; 2], end: [f32; 2]) {
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
pub(super) fn band(points: &[[f32; 2]], d0: f32, d1: f32, out: &mut Vec<Shape>) {
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
    let mut polygon = offset(&pts, d0, false);
    polygon.extend(offset(&pts, d1, false).into_iter().rev());
    out.push(Shape::Polygon(polygon));
}

/// 把折线（`closed` 时为闭合多边形）的各边沿法向 `(-dy, dx)` 平移 `d`，转角处斜接。
/// 相邻点不能重合。
fn offset(pts: &[[f32; 2]], d: f32, closed: bool) -> Vec<[f32; 2]> {
    let n = pts.len();
    let edges = if closed { n } else { n - 1 };
    let normals: Vec<[f32; 2]> = (0..edges)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dy);
            [-dy / len, dx / len]
        })
        .collect();
    (0..n)
        .map(|i| {
            let n2 = normals[i.min(edges - 1)];
            let prev = if closed {
                Some((i + edges - 1) % edges)
            } else {
                i.checked_sub(1)
            };
            let m = match prev.map(|j| normals[j]) {
                Some(n1) => {
                    let k = 1. + n1[0] * n2[0] + n1[1] * n2[1];
                    // 接近折返时斜接点会飞得很远，退回用后一段的法向（相当于 miter limit）。
                    if k < 0.02 {
                        n2
                    } else {
                        [(n1[0] + n2[0]) / k, (n1[1] + n2[1]) / k]
                    }
                }
                None => n2,
            };
            [pts[i][0] + m[0] * d, pts[i][1] + m[1] * d]
        })
        .collect()
}

/// 闭合多边形向内收缩 `d`：按顶点绕向决定法向朝里还是朝外。
pub(super) fn inset(pts: &[[f32; 2]], d: f32) -> Vec<[f32; 2]> {
    let area2: f32 = (0..pts.len())
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    offset(pts, d * area2.signum(), true)
}

/// 外轮廓挖掉内轮廓：两个轮廓用一条来回重合的桥接边连成一个多边形，
/// GPUI 的路径默认按奇偶规则（`FillRule::EvenOdd`）填充，桥接边互相抵消，内轮廓成为洞。
pub(super) fn ring(mut outer: Vec<[f32; 2]>, inner: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    outer.push(outer[0]);
    outer.push(inner[0]);
    outer.extend(&inner[1..]);
    outer.push(inner[0]);
    outer
}

/// 圆周上均匀取 32 个点，最大径向误差约 0.5% 半径。
pub(super) fn circle(cx: f32, cy: f32, r: f32) -> Vec<[f32; 2]> {
    (0..32)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 32.;
            [cx + r * a.cos(), cy + r * a.sin()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{PathBuilder, point, px};

    #[test]
    fn hollow_shapes_tessellate_with_holes() {
        // GPUI 按奇偶规则剖分路径：环形多边形剖分后的面积应是外轮廓减去内轮廓。
        let outer = vec![[0., 0.], [0., 16.], [8., 0.]];
        let inner = inset(&outer, 1.);
        let area = |p: &[[f32; 2]]| {
            (0..p.len())
                .map(|i| p[i][0] * p[(i + 1) % p.len()][1] - p[(i + 1) % p.len()][0] * p[i][1])
                .sum::<f32>()
                .abs()
                / 2.
        };
        let expected = area(&outer) - area(&inner);
        let mut builder = PathBuilder::fill();
        let points: Vec<_> = ring(outer, inner)
            .iter()
            .map(|p| point(px(p[0]), px(p[1])))
            .collect();
        builder.add_polygon(&points, true);
        let path = builder.build().unwrap();
        let tessellated: f32 = path
            .vertices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|i| t[i].xy_position.map(f32::from));
                ((b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)).abs() / 2.
            })
            .sum();
        assert!(
            (tessellated - expected).abs() < 0.01,
            "{tessellated} != {expected}"
        );
    }
}
