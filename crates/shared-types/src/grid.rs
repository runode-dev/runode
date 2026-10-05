//! 终端网格的尺寸、网格里的位置和视口的滚动方式。

/// 网格的行列数和单元格的像素尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
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
