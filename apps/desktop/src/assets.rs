//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。
//! 文件树的彩色类型图标和 agent 的 logo 数量多，单独列在 `file_icons::FILES` 和
//! `window::AGENT_LOGO_FILES`，`runode_ui` 的部件自己画的图标在 `icons::FILES`，GitHub Actions 页、预览栏、
//! 文件树和 Git 面板各自的图标在它们的 crate 里，这里一并查。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

use crate::{
    ui::{file_icons, icons},
    window,
};

/// 几处共用的图标在 `runode_ui` 里：展开、收起的箭头，行尾和菜单常用的按钮，和预览栏、Git 面板共用的那些。
pub use crate::ui::icons::{
    BRANCH_ICON, CHECK_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CLOSE_ICON, FOLDER_OPEN_ICON, GIT_ICON, GLOBE_ICON,
    MAXIMIZE_ICON, MINIMIZE_ICON, MINUS_ICON, PENCIL_ICON, PLAY_ICON, PLUS_ICON, REFRESH_ICON, SPARKLE_ICON,
    TERMINAL_ICON, TRASH_ICON,
};
/// GitHub Actions 页的标签图标，图标本身跟着页面在 `runode_github_actions` 里。
pub use runode_github_actions::WORKFLOW_ICON as ACTIONS_WORKFLOW_ICON;

icons! {
    FILES, "icons/", "../assets/icons/";
    /// 侧栏开关的图标。
    pub SIDEBAR_ICON = "sidebar",
    /// 右侧面板顶上文件树标签的图标（Git 标签的在 `runode_ui`），以及标题栏右上角开关右侧面板的图标。
    pub FILES_ICON = "files",
    pub PANEL_RIGHT_ICON = "panel-right",
    /// 收在终端区底部的分屏上展开它的箭头。
    pub CHEVRON_UP_ICON = "chevron-up",
    /// 卡片样式下分屏标题条右边向右、向下分屏的按钮（关闭、放大、还原的在 `runode_ui`）。
    pub SPLIT_RIGHT_ICON = "split-right",
    pub SPLIT_DOWN_ICON = "split-down",
    /// 卡片样式下标签图标叠里代表 shell 和普通程序的那块：深色底上的提示符。
    pub PROMPT_ICON = "prompt",
    /// 卡片样式下 agent 等你回答时的标记：像素画的问号。
    pub PIXEL_QUESTION_ICON = "pixel-question",
    /// 窗口底部状态栏上的防止休眠、内存、终端数和端口。
    pub COFFEE_ICON = "coffee",
    pub MEMORY_ICON = "memory",
    pub SHELL_ICON = "shell",
    pub PLUG_ICON = "plug",
    /// 侧栏顶上手机端入口和引导页的图标。
    pub PHONE_ICON = "phone",
    /// 设置页侧栏顶上的返回，以及各页的图标（终端、文件、agent、远程访问几页借用上面的）。
    pub ARROW_LEFT_ICON = "arrow-left",
    pub SETTINGS_ICON = "settings",
    pub SLIDERS_ICON = "sliders",
    pub PALETTE_ICON = "palette",
    pub KEYBOARD_ICON = "keyboard",
    /// 外观页配色的预览卡（跟着系统换、一个主题用 `PALETTE_ICON`）。
    pub MONITOR_ICON = "monitor",
}

fn all_files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    FILES
        .iter()
        .chain(icons::FILES)
        .chain(runode_github_actions::FILES)
        .chain(runode_preview_panel::FILES)
        .chain(runode_file_tree::FILES)
        .chain(runode_git_panel::FILES)
        .chain(file_icons::FILES)
        .chain(window::AGENT_LOGO_FILES)
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(all_files().find(|(name, _)| *name == path).map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(all_files().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| (*name).into()).collect())
    }
}
