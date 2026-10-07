//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。
//! 文件树的彩色类型图标和 agent 的 logo 数量多，单独列在 `file_icons::FILES` 和
//! `window::AGENT_LOGO_FILES`，这里一并查。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

use crate::{
    ui::file_icons::{self, icons},
    window,
};

icons! {
    FILES, "icons/", "../assets/icons/";
    /// 侧栏开关的图标。
    pub SIDEBAR_ICON = "sidebar",
    /// 右上角 Git 面板和文件树开关的图标。
    pub GIT_ICON = "git",
    pub FILES_ICON = "files",
    /// Git 面板和文件树里展开、收起的箭头。
    pub CHEVRON_RIGHT_ICON = "chevron-right",
    pub CHEVRON_DOWN_ICON = "chevron-down",
    /// 文件树标题栏上显示、隐藏被 git 忽略的文件的开关。
    pub EYE_ICON = "eye",
    pub EYE_OFF_ICON = "eye-off",
    /// 文件树标题栏上新建文件、新建文件夹和全部收起的按钮。
    pub NEW_FILE_ICON = "new-file",
    pub NEW_FOLDER_ICON = "new-folder",
    pub COLLAPSE_ALL_ICON = "collapse-all",
    /// Git 面板里的按钮：分支、暂存、取消暂存、放弃改动、提交、刷新、更多、同步、打开文件，以及储藏的
    /// 应用、弹出和删除。
    pub BRANCH_ICON = "branch",
    pub PLUS_ICON = "plus",
    pub MINUS_ICON = "minus",
    pub DISCARD_ICON = "discard",
    pub CHECK_ICON = "check",
    pub REFRESH_ICON = "refresh",
    pub MORE_ICON = "more",
    pub SYNC_ICON = "sync",
    pub OPEN_FILE_ICON = "open-file",
    pub STASH_APPLY_ICON = "stash-apply",
    pub STASH_POP_ICON = "stash-pop",
    pub TRASH_ICON = "trash",
    /// Git 面板标题栏上切换改动的文件以树形式还是列表形式查看的按钮，画的是切过去以后的样子。
    pub VIEW_TREE_ICON = "view-tree",
    pub VIEW_LIST_ICON = "view-list",
    /// 预览栏里 diff 标签的图标，以及 diff 顶上跳到上一处、下一处改动的按钮。
    pub DIFF_ICON = "diff",
    pub ARROW_UP_ICON = "arrow-up",
    pub ARROW_DOWN_ICON = "arrow-down",
    /// 卡片样式下标题栏左边这台机器的图标：笔记本或者台式机。
    pub LAPTOP_ICON = "laptop",
    pub DESKTOP_ICON = "desktop",
    /// 卡片样式下分屏标题条上的图标：前台不是 agent 时的终端图标，以及右边向右、向下分屏，放大、
    /// 还原和关闭分屏的按钮。
    pub TERMINAL_ICON = "terminal",
    pub SPLIT_RIGHT_ICON = "split-right",
    pub SPLIT_DOWN_ICON = "split-down",
    pub MAXIMIZE_ICON = "maximize",
    pub MINIMIZE_ICON = "minimize",
    pub CLOSE_ICON = "close",
    /// 卡片样式下标签图标叠里代表 shell 和普通程序的那块：深色底上的提示符。
    pub PROMPT_ICON = "prompt",
    /// 卡片样式下 agent 等你回答时的标记：像素画的问号。
    pub PIXEL_QUESTION_ICON = "pixel-question",
}

fn all_files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    FILES.iter().chain(file_icons::FILES).chain(window::AGENT_LOGO_FILES)
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
