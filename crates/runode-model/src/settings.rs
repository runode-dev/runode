//! 终端状态机用到的设置：默认颜色、光标样式和 Option 键的用法。程序自己用转义序列设置的
//! 颜色和光标形状照旧优先，这些只是默认值，所以可以随时重新应用。

use crate::color::{Rgb, TerminalColor};

/// 配置的光标样式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    #[default]
    Block,
    BlockHollow,
    Bar,
    Underline,
}

/// macOS 上 Option 键当不当 Alt 用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionAsAlt {
    /// 不当 Alt，按出系统给的字符。
    #[default]
    False,
    True,
    /// 只有左边的当 Alt。
    Left,
    /// 只有右边的当 Alt。
    Right,
}

/// 交给终端的设置。
#[derive(Clone, Debug, PartialEq)]
pub struct TermSettings {
    pub background: Rgb,
    pub foreground: Rgb,
    /// 覆盖默认 256 色中的若干项。
    pub palette: Vec<(u8, Rgb)>,
    pub cursor_style: CursorStyle,
    /// `None` 表示默认闪烁，运行中的程序仍可改变。
    pub cursor_blink: Option<bool>,
    /// `None` 表示用前景色。
    pub cursor_color: Option<TerminalColor>,
    /// 实心块状光标下文字的颜色，`None` 表示用背景色。
    pub cursor_text: Option<TerminalColor>,
    /// `None` 表示按背景深浅用一种统一的蓝色。
    pub selection_background: Option<TerminalColor>,
    /// `None` 时：配了选区底色就取单元格的背景色（底色设成单元格前景色时就是反色），
    /// 没配就保持文字原来的颜色。
    pub selection_foreground: Option<TerminalColor>,
    /// 搜索匹配的背景色和文字色。
    pub search_background: TerminalColor,
    pub search_foreground: TerminalColor,
    /// 当前选中的那个搜索匹配的背景色和文字色。
    pub search_selected_background: TerminalColor,
    pub search_selected_foreground: TerminalColor,
    pub option_as_alt: OptionAsAlt,
}
