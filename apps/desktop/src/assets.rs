//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。
//! 文件树的彩色类型图标数量多，单独列在 `file_icons::FILES`，这里一并查。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

use crate::ui::file_icons;

/// 侧栏开关的图标。
pub const SIDEBAR_ICON: &str = "icons/sidebar.svg";
/// 右上角 Git 面板和文件树开关的图标。
pub const GIT_ICON: &str = "icons/git.svg";
pub const FILES_ICON: &str = "icons/files.svg";
/// Git 面板和文件树里展开、收起的箭头。
pub const CHEVRON_RIGHT_ICON: &str = "icons/chevron-right.svg";
pub const CHEVRON_DOWN_ICON: &str = "icons/chevron-down.svg";
/// 文件树标题栏上显示、隐藏被 git 忽略的文件的开关。
pub const EYE_ICON: &str = "icons/eye.svg";
pub const EYE_OFF_ICON: &str = "icons/eye-off.svg";
/// 文件树标题栏上新建文件、新建文件夹和全部收起的按钮。
pub const NEW_FILE_ICON: &str = "icons/new-file.svg";
pub const NEW_FOLDER_ICON: &str = "icons/new-folder.svg";
pub const COLLAPSE_ALL_ICON: &str = "icons/collapse-all.svg";
/// Git 面板里的按钮：分支、暂存、取消暂存、放弃改动、提交、刷新、更多、同步、打开文件，以及储藏的
/// 应用、弹出和删除。
pub const BRANCH_ICON: &str = "icons/branch.svg";
pub const PLUS_ICON: &str = "icons/plus.svg";
pub const MINUS_ICON: &str = "icons/minus.svg";
pub const DISCARD_ICON: &str = "icons/discard.svg";
pub const CHECK_ICON: &str = "icons/check.svg";
pub const REFRESH_ICON: &str = "icons/refresh.svg";
pub const MORE_ICON: &str = "icons/more.svg";
pub const SYNC_ICON: &str = "icons/sync.svg";
pub const OPEN_FILE_ICON: &str = "icons/open-file.svg";
pub const STASH_APPLY_ICON: &str = "icons/stash-apply.svg";
pub const STASH_POP_ICON: &str = "icons/stash-pop.svg";
pub const TRASH_ICON: &str = "icons/trash.svg";
/// Git 面板标题栏上切换改动的文件以树形式还是列表形式查看的按钮，画的是切过去以后的样子。
pub const VIEW_TREE_ICON: &str = "icons/view-tree.svg";
pub const VIEW_LIST_ICON: &str = "icons/view-list.svg";
/// 预览栏里 diff 标签的图标，以及 diff 顶上跳到上一处、下一处改动的按钮。
pub const DIFF_ICON: &str = "icons/diff.svg";
pub const ARROW_UP_ICON: &str = "icons/arrow-up.svg";
pub const ARROW_DOWN_ICON: &str = "icons/arrow-down.svg";

const FILES: &[(&str, &[u8])] = &[
    (SIDEBAR_ICON, include_bytes!("../assets/icons/sidebar.svg")),
    (GIT_ICON, include_bytes!("../assets/icons/git.svg")),
    (FILES_ICON, include_bytes!("../assets/icons/files.svg")),
    (CHEVRON_RIGHT_ICON, include_bytes!("../assets/icons/chevron-right.svg")),
    (CHEVRON_DOWN_ICON, include_bytes!("../assets/icons/chevron-down.svg")),
    (EYE_ICON, include_bytes!("../assets/icons/eye.svg")),
    (EYE_OFF_ICON, include_bytes!("../assets/icons/eye-off.svg")),
    (NEW_FILE_ICON, include_bytes!("../assets/icons/new-file.svg")),
    (NEW_FOLDER_ICON, include_bytes!("../assets/icons/new-folder.svg")),
    (COLLAPSE_ALL_ICON, include_bytes!("../assets/icons/collapse-all.svg")),
    (BRANCH_ICON, include_bytes!("../assets/icons/branch.svg")),
    (PLUS_ICON, include_bytes!("../assets/icons/plus.svg")),
    (MINUS_ICON, include_bytes!("../assets/icons/minus.svg")),
    (DISCARD_ICON, include_bytes!("../assets/icons/discard.svg")),
    (CHECK_ICON, include_bytes!("../assets/icons/check.svg")),
    (REFRESH_ICON, include_bytes!("../assets/icons/refresh.svg")),
    (MORE_ICON, include_bytes!("../assets/icons/more.svg")),
    (SYNC_ICON, include_bytes!("../assets/icons/sync.svg")),
    (OPEN_FILE_ICON, include_bytes!("../assets/icons/open-file.svg")),
    (STASH_APPLY_ICON, include_bytes!("../assets/icons/stash-apply.svg")),
    (STASH_POP_ICON, include_bytes!("../assets/icons/stash-pop.svg")),
    (TRASH_ICON, include_bytes!("../assets/icons/trash.svg")),
    (VIEW_TREE_ICON, include_bytes!("../assets/icons/view-tree.svg")),
    (VIEW_LIST_ICON, include_bytes!("../assets/icons/view-list.svg")),
    (DIFF_ICON, include_bytes!("../assets/icons/diff.svg")),
    (ARROW_UP_ICON, include_bytes!("../assets/icons/arrow-up.svg")),
    (ARROW_DOWN_ICON, include_bytes!("../assets/icons/arrow-down.svg")),
];

fn all_files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    FILES.iter().chain(file_icons::FILES)
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
