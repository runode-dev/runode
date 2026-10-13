//! 终端网格的尺寸、网格里的位置和视口的滚动方式。

/// 网格的行列数和单元格的像素尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

/// 程序先后两次用 CSI 8 t 要求改窗口尺寸（行数、列数，0 是那一维不变），合起来等于一次要求什么：
/// 后一次给了的维度用后一次的，没给的沿用前一次的。宿主和界面都只留最近一次要求，合并时用它。
pub fn merge_window_resize(earlier: Option<(u16, u16)>, later: (u16, u16)) -> (u16, u16) {
    let (rows, columns) = earlier.unwrap_or_default();
    (if later.0 == 0 { rows } else { later.0 }, if later.1 == 0 { columns } else { later.1 })
}

/// 指针在网格里的位置，以单元格为单位并带小数。拖到网格外时可以为负，也可以超出行列数。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridPoint {
    pub x: f32,
    pub y: f32,
}

/// `Session::scroll_viewport` 的滚动方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewportScroll {
    Top,
    Bottom,
    Page(isize),
}

#[cfg(test)]
mod tests {
    use super::merge_window_resize;

    #[test]
    fn a_later_resize_keeps_the_dimensions_it_leaves_out() {
        assert_eq!(merge_window_resize(None, (0, 120)), (0, 120));
        assert_eq!(merge_window_resize(Some((30, 100)), (0, 120)), (30, 120));
        assert_eq!(merge_window_resize(Some((30, 100)), (40, 0)), (40, 100));
        assert_eq!(merge_window_resize(Some((30, 100)), (24, 80)), (24, 80));
    }
}
