//! 配置的内容：各项设置和默认值。

use std::path::PathBuf;

use runode_shared_types::{
    color::{Rgb, TerminalColor},
    settings::{CursorStyle, OptionAsAlt, TermSettings},
    shell::IntegrationMode,
};

use crate::keybind::Action;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellHeight {
    Pixels(f32),
    Percent(f32),
}

/// 文件树里单击还是双击文件在预览栏打开。单击打开时开成临时标签，双击固定下来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewClick {
    Single,
    Double,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// 依次尝试的字体族，第一个能解析的生效。
    pub font_family: Vec<String>,
    pub font_size: f32,
    pub adjust_cell_height: Option<CellHeight>,
    /// (左, 右)
    pub window_padding_x: (f32, f32),
    /// (上, 下)
    pub window_padding_y: (f32, f32),
    /// 右侧文件树的字号。
    pub file_tree_font_size: f32,
    pub file_tree_preview_click: PreviewClick,
    /// 预览栏的字号。
    pub preview_font_size: f32,
    pub cursor_style: CursorStyle,
    /// `None` 表示默认闪烁，运行中的程序仍可改变。
    pub cursor_style_blink: Option<bool>,
    pub background: Rgb,
    pub foreground: Rgb,
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
    /// 覆盖默认 256 色中的若干项。
    pub palette: Vec<(u8, Rgb)>,
    pub macos_option_as_alt: OptionAsAlt,
    pub shell_integration: IntegrationMode,
    /// 在 shell 提示符上输入时，按命令历史在光标后用灰字给出建议；关掉时也不读写命令历史。
    pub command_suggestions: bool,
    /// 按 Tab 时由 runode 弹出补全菜单（命令名，以及有规格的命令的参数）；关掉时 Tab 总是交给 shell。
    pub command_completions: bool,
    /// 界面语言，是 locales 里的某个语言标签；`None` 表示跟随系统。
    pub language: Option<String>,
    /// 叠在默认快捷键上的 `keybind`，按出现顺序；只认 runode 自己的配置文件。
    pub keybinds: Vec<Keybind>,
    /// 本次读到的全部文件（含主题和 config-file 引入的），供热重载监视。
    pub sources: Vec<PathBuf>,
    /// 加载时系统是否为深色外观，`theme = light:A,dark:B` 据此选了其中一个。
    pub dark: bool,
}

impl Default for Config {
    fn default() -> Self {
        // 交给终端的那部分沿用 `TermSettings` 的默认值，只在一处写。
        let term = TermSettings::default();
        Self {
            font_family: vec!["Hack Nerd Font Mono".into()],
            font_size: 14.,
            adjust_cell_height: None,
            window_padding_x: (2., 2.),
            window_padding_y: (0., 6.),
            file_tree_font_size: 13.,
            file_tree_preview_click: PreviewClick::Single,
            preview_font_size: 13.,
            cursor_style: term.cursor_style,
            cursor_style_blink: term.cursor_blink,
            background: term.background,
            foreground: term.foreground,
            cursor_color: term.cursor_color,
            cursor_text: term.cursor_text,
            selection_background: term.selection_background,
            selection_foreground: term.selection_foreground,
            search_background: term.search_background,
            search_foreground: term.search_foreground,
            search_selected_background: term.search_selected_background,
            search_selected_foreground: term.search_selected_foreground,
            palette: term.palette,
            macos_option_as_alt: term.option_as_alt,
            shell_integration: IntegrationMode::Detect,
            command_suggestions: true,
            command_completions: true,
            language: None,
            keybinds: Vec::new(),
            sources: Vec::new(),
            dark: true,
        }
    }
}

/// 一条 `keybind`，由 `keybind::parse` 解析并校验。
#[derive(Clone, Debug, PartialEq)]
pub enum Keybind {
    /// `keybind = clear`：去掉此前的全部绑定，包括默认的。
    Clear,
    /// `触发键=unbind`，触发键已转成 GPUI 的写法。
    Unbind(String),
    /// `触发键=动作`，动作已解析好。
    Bind { keys: String, action: Action },
}

impl Config {
    /// 交给终端的那部分设置。
    pub fn term_settings(&self) -> TermSettings {
        TermSettings {
            background: self.background,
            foreground: self.foreground,
            palette: self.palette.clone(),
            cursor_style: self.cursor_style,
            cursor_blink: self.cursor_style_blink,
            cursor_color: self.cursor_color,
            cursor_text: self.cursor_text,
            selection_background: self.selection_background,
            selection_foreground: self.selection_foreground,
            search_background: self.search_background,
            search_foreground: self.search_foreground,
            search_selected_background: self.search_selected_background,
            search_selected_foreground: self.search_selected_foreground,
            option_as_alt: self.macos_option_as_alt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_gives_the_default_term_settings() {
        assert_eq!(Config::default().term_settings(), TermSettings::default());
    }
}
