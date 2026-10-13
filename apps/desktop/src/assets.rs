//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。图标都在
//! `runode_ui`（`icons::all`），窗口里用的照旧从这里引。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

use crate::ui::icons;

pub use crate::ui::icons::{
    ARROW_LEFT_ICON, BRANCH_ICON, CHECK_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CHEVRON_UP_ICON, CLOSE_ICON,
    COFFEE_ICON, FILES_ICON, FOLDER_OPEN_ICON, GIT_ICON, GLOBE_ICON, KEYBOARD_ICON, MAXIMIZE_ICON, MEMORY_ICON,
    MINIMIZE_ICON, MINUS_ICON, MONITOR_ICON, PALETTE_ICON, PANEL_RIGHT_ICON, PENCIL_ICON, PHONE_ICON,
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
