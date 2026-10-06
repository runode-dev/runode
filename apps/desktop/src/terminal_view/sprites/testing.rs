//! 测试共用的工具：构造单元格尺寸，把图元栅格化成覆盖度网格或字符画。

use super::{Metrics, Shape, shapes};

pub(super) fn metrics(width: u32, height: u32, thickness: u32) -> Metrics {
    Metrics { width, height, thickness }
}

/// 在每个像素中心采样，把图元画进 `width × height` 的覆盖度网格（0–255）。
pub(super) fn raster(c: char, m: Metrics) -> Vec<Vec<u8>> {
    let shapes = shapes(&c.to_string(), m).expect("应当是自绘字符");
    let mut grid = vec![vec![0u8; m.width as usize]; m.height as usize];
    for (y, row) in grid.iter_mut().enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            for shape in &shapes {
                let a = match shape {
                    Shape::Rect { x0, y0, x1, y1, alpha } => {
                        let inside = (*x0..*x1).contains(&(x as i32)) && (*y0..*y1).contains(&(y as i32));
                        if inside { *alpha } else { 0 }
                    }
                    Shape::Polygon(p) => {
                        // 奇偶规则判断像素中心是否在多边形内。
                        let mut inside = false;
                        for i in 0..p.len() {
                            let (a, b) = (p[i], p[(i + 1) % p.len()]);
                            if (a[1] > py) != (b[1] > py) && px < a[0] + (py - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
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

pub(super) fn art(c: char, m: Metrics) -> String {
    raster(c, m)
        .iter()
        .map(|row| row.iter().map(|&v| if v == 0 { '.' } else { '#' }).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
