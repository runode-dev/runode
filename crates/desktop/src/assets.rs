//! 编进二进制的界面资源：`svg()`、`img()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。
//! 文件树的彩色类型图标数量多，单独列在 `file_icons::FILES`，这里一并查。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

use crate::file_icons;

/// 侧栏开关的图标。
pub const SIDEBAR_ICON: &str = "icons/sidebar.svg";
/// 右上角改动栏和文件树开关的图标。
pub const CHANGES_ICON: &str = "icons/changes.svg";
pub const FILES_ICON: &str = "icons/files.svg";
/// 改动栏和文件树里展开、收起的箭头。
pub const CHEVRON_RIGHT_ICON: &str = "icons/chevron-right.svg";
pub const CHEVRON_DOWN_ICON: &str = "icons/chevron-down.svg";
/// 文件树标题栏上显示、隐藏被 git 忽略的文件的开关。
pub const EYE_ICON: &str = "icons/eye.svg";
pub const EYE_OFF_ICON: &str = "icons/eye-off.svg";
/// 文件树标题栏上新建文件、新建文件夹和全部收起的按钮。
pub const NEW_FILE_ICON: &str = "icons/new-file.svg";
pub const NEW_FOLDER_ICON: &str = "icons/new-folder.svg";
pub const COLLAPSE_ALL_ICON: &str = "icons/collapse-all.svg";

const FILES: &[(&str, &[u8])] = &[
    (SIDEBAR_ICON, include_bytes!("../assets/icons/sidebar.svg")),
    (CHANGES_ICON, include_bytes!("../assets/icons/changes.svg")),
    (FILES_ICON, include_bytes!("../assets/icons/files.svg")),
    (CHEVRON_RIGHT_ICON, include_bytes!("../assets/icons/chevron-right.svg")),
    (CHEVRON_DOWN_ICON, include_bytes!("../assets/icons/chevron-down.svg")),
    (EYE_ICON, include_bytes!("../assets/icons/eye.svg")),
    (EYE_OFF_ICON, include_bytes!("../assets/icons/eye-off.svg")),
    (NEW_FILE_ICON, include_bytes!("../assets/icons/new-file.svg")),
    (NEW_FOLDER_ICON, include_bytes!("../assets/icons/new-folder.svg")),
    (COLLAPSE_ALL_ICON, include_bytes!("../assets/icons/collapse-all.svg")),
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
