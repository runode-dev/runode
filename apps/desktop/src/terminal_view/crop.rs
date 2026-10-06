//! 会话的尺寸归别的前端管时 VT 怎么画进视图。宿主按 owner 的视图改会话的尺寸（见
//! `HostMsg::SizeOwner`），这边的 VT 可能比视图大，也可能比视图小。左对齐、跟着光标：VT 比视图大时
//! 裁掉右边和离光标远的行，倾向底部对齐（提示符和正在打的命令多在下面），裁掉内容的那几边渐隐；
//! VT 比视图小时空出来的地方用较暗的底色留白。这里只算 VT 的哪一块画进视图、指针落在 VT 的哪里，
//! 画由 `TerminalElement` 做。

use gpui::{Pixels, Point, Size, point};
use runode_shared_types::{color::Rgb, grid::GridSize};

/// VT 里画在视图左上角的那一格：左边 `col` 列、上面 `row` 行裁掉了。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Crop {
    pub(super) col: u16,
    pub(super) row: u16,
}

/// 视图的哪几边裁掉了 VT 的内容，那几边画渐隐。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Edges {
    pub(super) left: bool,
    pub(super) right: bool,
    pub(super) top: bool,
    pub(super) bottom: bool,
}

impl Crop {
    /// `vt` 行列的 VT 画进 `view` 行列的视图时从哪一格画起。`cursor` 是光标在 VT 视口里的位置（列、行），
    /// 不显示光标时为 `None`。`prev` 是上一帧的：光标还在视图里就不动，免得画面跟着光标一格一格地跳；
    /// 光标出了视图就挪到刚好看得见它。没有上一帧时列从最左边起；行让光标下面还露出视图的三分之一
    /// （放得下的话），光标下面多半是空的或者只有一两行状态栏；没有光标时贴底。VT 在某个方向上不比
    /// 视图大时这个方向从头画，另一头留白。
    pub(super) fn follow(vt: GridSize, view: GridSize, cursor: Option<(u16, u16)>, prev: Option<Crop>) -> Crop {
        let col = axis(vt.cols, view.cols, cursor.map(|(x, _)| x), prev.map(|prev| prev.col), 0);
        let fresh_row = match cursor {
            Some((_, y)) => {
                (u32::from(y) + 1 + u32::from(view.rows.saturating_sub(1) / 3)).saturating_sub(u32::from(view.rows))
            }
            None => u32::MAX,
        };
        let row = axis(vt.rows, view.rows, cursor.map(|(_, y)| y), prev.map(|prev| prev.row), fresh_row);
        Crop { col, row }
    }

    /// 这样画时视图的哪几边裁掉了内容。
    pub(super) fn edges(self, vt: GridSize, view: GridSize) -> Edges {
        Edges {
            left: self.col > 0,
            right: u32::from(self.col) + u32::from(view.cols) < u32::from(vt.cols),
            top: self.row > 0,
            bottom: u32::from(self.row) + u32::from(view.rows) < u32::from(vt.rows),
        }
    }

    /// 视图左上角在窗口里的位置 `origin` 换成 VT 第 0 列第 0 行在窗口里的位置：画单元格、把指针换成
    /// VT 里的位置（`grid_point_at`）都从它算，裁掉的那几列几行落在视图外面。
    pub(super) fn vt_origin(self, origin: Point<Pixels>, cell: Size<Pixels>) -> Point<Pixels> {
        origin - point(cell.width * f32::from(self.col), cell.height * f32::from(self.row))
    }
}

/// 一个方向上从第几格画起，见 `Crop::follow`。`fresh` 是没有上一帧时想从哪里起，超出范围的取最远的。
fn axis(vt: u16, view: u16, cursor: Option<u16>, prev: Option<u16>, fresh: u32) -> u16 {
    let (vt, view) = (u32::from(vt), u32::from(view));
    let last = vt.saturating_sub(view);
    if last == 0 {
        return 0;
    }
    let start = prev.map_or(fresh, u32::from).min(last);
    let start = match cursor.map(u32::from) {
        Some(cursor) if cursor < start => cursor,
        Some(cursor) if cursor >= start + view => (cursor + 1 - view).min(last),
        _ => start,
    };
    // `start` 不超过 `last`，`last` 小于 `vt`，放得进 u16。
    start as u16
}

/// VT 比视图小时空出来那块的底色：比终端背景暗一些；背景本来就接近黑色、暗不下去时改成稍亮一点，
/// 好让人看出 VT 到哪里为止。
pub(super) fn pad_color(background: Rgb, foreground: Rgb) -> Rgb {
    let Rgb(r, g, b) = background;
    let luma = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    if luma < 24. { background.mix(foreground, 0.06) } else { background.mix(Rgb(0, 0, 0), 0.25) }
}

#[cfg(test)]
mod tests {
    use gpui::{px, size};

    use super::*;
    use crate::terminal_view::input::grid_point_at;

    fn grid(cols: u16, rows: u16) -> GridSize {
        GridSize { cols, rows, cell_width_px: 8, cell_height_px: 16 }
    }

    const VIEW: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };

    #[test]
    fn a_vt_that_fits_is_drawn_from_the_top_left() {
        for vt in [VIEW, grid(60, 20), grid(80, 10), grid(40, 24)] {
            for cursor in [None, Some((0, 0)), Some((vt.cols - 1, vt.rows - 1))] {
                let crop = Crop::follow(vt, VIEW, cursor, None);
                assert_eq!(crop, Crop::default(), "{vt:?} {cursor:?}");
                assert_eq!(crop.edges(vt, VIEW), Edges::default());
            }
        }
        // 上一帧留下的偏移在 VT 变小以后不再用。
        assert_eq!(Crop::follow(grid(60, 20), VIEW, None, Some(Crop { col: 30, row: 10 })), Crop::default());
    }

    #[test]
    fn a_taller_vt_follows_the_cursor_and_leans_to_the_bottom() {
        let vt = grid(80, 50);
        // 光标在底：贴底，上面裁掉。
        let bottom = Crop::follow(vt, VIEW, Some((2, 49)), None);
        assert_eq!(bottom, Crop { col: 0, row: 26 });
        assert_eq!(bottom.edges(vt, VIEW), Edges { top: true, ..Edges::default() });
        // 光标在中间：光标下面露出三分之一的视图，上下都裁。
        let middle = Crop::follow(vt, VIEW, Some((0, 25)), None);
        assert_eq!(middle, Crop { col: 0, row: 25 + 1 + 7 - 24 });
        assert_eq!(middle.edges(vt, VIEW), Edges { top: true, bottom: true, ..Edges::default() });
        // 光标在顶（刚清过屏的 shell）：从头画，下面裁掉。
        let top = Crop::follow(vt, VIEW, Some((0, 1)), None);
        assert_eq!(top, Crop::default());
        assert_eq!(top.edges(vt, VIEW), Edges { bottom: true, ..Edges::default() });
        // 不显示光标：贴底。
        assert_eq!(Crop::follow(vt, VIEW, None, None), Crop { col: 0, row: 26 });
    }

    #[test]
    fn the_crop_stays_put_while_the_cursor_is_in_view() {
        let vt = grid(80, 50);
        let prev = Crop { col: 0, row: 10 };
        // 光标在视图里移动：不动。
        for y in [10, 20, 33] {
            assert_eq!(Crop::follow(vt, VIEW, Some((0, y)), Some(prev)), prev);
        }
        // 光标出了下边：挪到刚好看得见它（在最后一行）。
        assert_eq!(Crop::follow(vt, VIEW, Some((0, 34)), Some(prev)), Crop { col: 0, row: 11 });
        // 出了上边：它在第一行。
        assert_eq!(Crop::follow(vt, VIEW, Some((0, 4)), Some(prev)), Crop { col: 0, row: 4 });
        // 没有光标：不动；超出范围的（VT 变矮了）收回来。
        assert_eq!(Crop::follow(vt, VIEW, None, Some(prev)), prev);
        assert_eq!(Crop::follow(grid(80, 30), VIEW, None, Some(prev)), Crop { col: 0, row: 6 });
    }

    #[test]
    fn a_wider_vt_is_left_aligned_unless_the_cursor_is_off_to_the_right() {
        let vt = grid(120, 24);
        let left = Crop::follow(vt, VIEW, Some((10, 5)), None);
        assert_eq!(left, Crop::default());
        assert_eq!(left.edges(vt, VIEW), Edges { right: true, ..Edges::default() });
        // 光标在视图右边外面：挪到刚好看得见它，左右都裁。
        let right = Crop::follow(vt, VIEW, Some((100, 5)), None);
        assert_eq!(right, Crop { col: 21, row: 0 });
        assert_eq!(right.edges(vt, VIEW), Edges { left: true, right: true, ..Edges::default() });
        // 光标在最后一列：贴右边，右边不再裁。
        let end = Crop::follow(vt, VIEW, Some((119, 5)), None);
        assert_eq!(end, Crop { col: 40, row: 0 });
        assert_eq!(end.edges(vt, VIEW), Edges { left: true, ..Edges::default() });
        // 光标回到左边：跟回去。
        assert_eq!(Crop::follow(vt, VIEW, Some((3, 5)), Some(end)), Crop { col: 3, row: 0 });
    }

    #[test]
    fn wider_but_shorter_crops_one_way_and_pads_the_other() {
        let vt = grid(120, 10);
        let crop = Crop::follow(vt, VIEW, Some((90, 9)), None);
        assert_eq!(crop, Crop { col: 11, row: 0 });
        assert_eq!(crop.edges(vt, VIEW), Edges { left: true, right: true, ..Edges::default() });
    }

    #[test]
    fn pointers_map_to_the_cell_of_the_vt_under_them() {
        let cell = size(px(8.), px(16.));
        let origin = Point { x: px(100.), y: px(50.) };
        // 视图左上角往里两格半、一行半：没裁时就是 VT 的这一格。
        let position = origin + point(px(20.), px(24.));
        let at = grid_point_at(position, Crop::default().vt_origin(origin, cell), cell);
        assert_eq!((at.x, at.y), (2.5, 1.5));
        // 左边裁掉 3 列、上面 10 行：同一个位置落在 VT 的 (5.5, 11.5)。
        let crop = Crop { col: 3, row: 10 };
        let at = grid_point_at(position, crop.vt_origin(origin, cell), cell);
        assert_eq!((at.x, at.y), (5.5, 11.5));
        // 视图左上角那一格是 VT 里画在那里的那一格。
        let at = grid_point_at(origin, crop.vt_origin(origin, cell), cell);
        assert_eq!((at.x, at.y), (3., 10.));
    }

    #[test]
    fn the_padding_is_darker_but_still_visible_on_black() {
        let white = Rgb(255, 255, 255);
        let Rgb(r, _, _) = pad_color(Rgb(40, 40, 40), white);
        assert!(r < 40);
        let Rgb(r, _, _) = pad_color(Rgb(250, 250, 250), Rgb(0, 0, 0));
        assert!(r < 250);
        assert_ne!(pad_color(Rgb(0, 0, 0), white), Rgb(0, 0, 0));
    }
}
