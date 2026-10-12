//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。
//! 文件树的彩色类型图标和 agent 的 logo 数量多，单独列在 `file_icons::FILES` 和
//! `window::AGENT_LOGO_FILES`，`runode_ui` 的部件自己画的图标在 `icons::FILES`，这里一并查。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

use crate::{
    ui::{file_icons, icons},
    window,
};

/// 几处共用的图标在 `runode_ui` 里：展开、收起的箭头，行尾和菜单常用的按钮。
pub use crate::ui::icons::{
    CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, GLOBE_ICON, OPEN_FILE_ICON, PENCIL_ICON, PLAY_ICON, PLUS_ICON, REFRESH_ICON,
    SYNC_ICON, TERMINAL_ICON, TRASH_ICON,
};
/// GitHub Actions 页的标签图标，图标本身跟着页面在 `runode_github_actions` 里。
pub use runode_github_actions::WORKFLOW_ICON as ACTIONS_WORKFLOW_ICON;

icons! {
    FILES, "icons/", "../assets/icons/";
    /// 侧栏开关的图标。
    pub SIDEBAR_ICON = "sidebar",
    /// 右侧面板顶上 Git 和文件树标签的图标，以及标题栏右上角开关右侧面板的图标。
    pub GIT_ICON = "git",
    pub FILES_ICON = "files",
    pub PANEL_RIGHT_ICON = "panel-right",
    /// 收在终端区底部的分屏上展开它的箭头。
    pub CHEVRON_UP_ICON = "chevron-up",
    /// 文件树搜索框前面的图标。
    pub FILTER_ICON = "filter",
    /// 文件树「更多」菜单里用 VS Code 打开的图标。
    pub CODE_ICON = "code",
    /// Git 面板提交说明框里让 AI 写说明的按钮。
    pub SPARKLE_ICON = "sparkle",
    /// Git 面板里的按钮：分支、取消暂存、放弃改动、提交、更多，以及储藏的应用和弹出。
    pub BRANCH_ICON = "branch",
    pub MINUS_ICON = "minus",
    pub DISCARD_ICON = "discard",
    pub CHECK_ICON = "check",
    pub MORE_ICON = "more",
    pub STASH_APPLY_ICON = "stash-apply",
    pub STASH_POP_ICON = "stash-pop",
    /// Git 面板标题栏上切换改动的文件以树形式还是列表形式查看的按钮，画的是切过去以后的样子。
    pub VIEW_TREE_ICON = "view-tree",
    pub VIEW_LIST_ICON = "view-list",
    /// 预览栏里 diff 标签的图标，以及 diff 顶上跳到上一处、下一处改动的按钮。
    pub DIFF_ICON = "diff",
    pub ARROW_UP_ICON = "arrow-up",
    pub ARROW_DOWN_ICON = "arrow-down",
    /// 预览栏标签条右边的自动换行开关。
    pub WRAP_ICON = "wrap",
    /// Markdown 排版视图里 GitHub 提示块（`> [!NOTE]` 这类）标题前的图标。
    pub ALERT_NOTE_ICON = "alert-note",
    pub ALERT_TIP_ICON = "alert-tip",
    pub ALERT_IMPORTANT_ICON = "alert-important",
    pub ALERT_WARNING_ICON = "alert-warning",
    pub ALERT_CAUTION_ICON = "alert-caution",
    /// 卡片样式下分屏标题条右边向右、向下分屏，放大、还原和关闭分屏的按钮。
    pub SPLIT_RIGHT_ICON = "split-right",
    pub SPLIT_DOWN_ICON = "split-down",
    pub MAXIMIZE_ICON = "maximize",
    pub MINIMIZE_ICON = "minimize",
    pub CLOSE_ICON = "close",
    /// 新建工作区对话框里浏览文件夹的图标（从 URL 克隆用 `GLOBE_ICON`，创建新项目用 `PLUS_ICON`）。
    pub FOLDER_OPEN_ICON = "folder-open",
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
