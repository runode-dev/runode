//! 渲染器要画的终端画面。

use crate::color::Rgb;

/// 影响字形排版或装饰的文字属性。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// 一个网格单元格，颜色已解析为具体值。
#[derive(Clone, Debug, Default)]
pub struct Cell {
    /// 字素簇；空白单元格为空串。
    pub text: String,
    pub fg: Rgb,
    /// `None` 表示不画背景，透出帧背景色。
    pub bg: Option<Rgb>,
    pub attrs: Attrs,
    /// 宽字符，同时占用下一个单元格。
    pub wide: bool,
    /// 宽字符的后半格，这里什么都不画。
    pub spacer: bool,
    /// 在选区里。
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    BlockHollow,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub color: Rgb,
    /// 实心块状光标下文字的颜色。
    pub text: Rgb,
    /// 光标落在宽字符上，跨两个单元格。
    pub wide: bool,
    /// 终端要求光标闪烁（DECSCUSR 或 DEC 模式 12）。
    pub blinking: bool,
}

/// 渲染器要画的内容：视口的一份副本，与终端状态机分离，绘制时不碰 VT。
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// 视口上面紧挨着的一行，平滑滚动时露出它的一部分；视口已在回滚历史最顶上（或备用屏幕
    /// 没有历史）时为空。
    pub above: Vec<Cell>,
    /// 整屏往下错开的行数，0 到 1 之间，露出 `above` 的下半部分；`above` 为空时总是 0。
    pub scroll_offset: f32,
    /// 按行优先存放，共 `cols * rows` 个单元格。
    pub cells: Vec<Cell>,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Option<Cursor>,
}

impl Frame {
    pub fn row(&self, y: u16) -> &[Cell] {
        let start = usize::from(y) * usize::from(self.cols);
        &self.cells[start..start + usize::from(self.cols)]
    }
}
