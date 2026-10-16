//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。图标都在
//! `runode_ui`（`icons::all`），窗口里用的照旧从这里引。

use std::borrow::Cow;

use gpui::{App, AssetSource, SharedString};

use crate::ui::icons;

pub use crate::ui::icons::{
    ARROW_LEFT_ICON, BRANCH_ICON, CHECK_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CHEVRON_UP_ICON, CLOCK_ICON,
    CLOSE_ICON, COFFEE_ICON, FILES_ICON, FOLDER_OPEN_ICON, GIT_ICON, GLOBE_ICON, KEYBOARD_ICON, MAXIMIZE_ICON,
    MEMORY_ICON, MINIMIZE_ICON, MINUS_ICON, MONITOR_ICON, PALETTE_ICON, PANEL_RIGHT_ICON, PENCIL_ICON, PHONE_ICON,
    PIXEL_QUESTION_ICON, PLAY_ICON, PLUG_ICON, PLUS_ICON, PROMPT_ICON, REFRESH_ICON, SETTINGS_ICON, SHELL_ICON,
    SIDEBAR_ICON, SLIDERS_ICON, SPARKLE_ICON, SPLIT_DOWN_ICON, SPLIT_RIGHT_ICON, TERMINAL_ICON, TRASH_ICON,
    github_actions::WORKFLOW_ICON as ACTIONS_WORKFLOW_ICON,
};

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(icons::all().find(|(name, _)| *name == path).map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(icons::all().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| (*name).into()).collect())
    }
}

/// 随 app 带的终端字体：默认配置用的 Hack Nerd Font Mono（Nerd Fonts v3.5.1，MIT 和 Bitstream Vera
/// 许可，许可证和字体文件放在一起，打包时一起放进 app）。系统里没装它时也能用上，不至于退回 Menlo。
const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-Regular.ttf"),
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-Bold.ttf"),
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-Italic.ttf"),
    include_bytes!("../assets/fonts/HackNerdFontMono/HackNerdFontMono-BoldItalic.ttf"),
];

/// 把随 app 带的字体登记给 GPUI，要在第一次解析终端字体之前做。
pub fn install_fonts(cx: &App) {
    if let Err(err) = cx.text_system().add_fonts(FONTS.iter().map(|data| Cow::Borrowed(*data)).collect()) {
        tracing::warn!("failed to load bundled fonts: {err}");
    }
}
