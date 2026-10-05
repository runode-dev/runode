//! 编进二进制的界面资源：`svg()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// 侧栏开关的图标。
pub const SIDEBAR_ICON: &str = "icons/sidebar.svg";
/// 右上角改动栏和文件树开关的图标。
pub const CHANGES_ICON: &str = "icons/changes.svg";
pub const FILES_ICON: &str = "icons/files.svg";
/// 改动栏和文件树里展开、收起的箭头，以及文件树里目录和文件的图标。
pub const CHEVRON_RIGHT_ICON: &str = "icons/chevron-right.svg";
pub const CHEVRON_DOWN_ICON: &str = "icons/chevron-down.svg";
pub const FOLDER_ICON: &str = "icons/folder.svg";
pub const FILE_ICON: &str = "icons/file.svg";

const FILES: &[(&str, &[u8])] = &[
    (SIDEBAR_ICON, include_bytes!("../assets/icons/sidebar.svg")),
    (CHANGES_ICON, include_bytes!("../assets/icons/changes.svg")),
    (FILES_ICON, include_bytes!("../assets/icons/files.svg")),
    (CHEVRON_RIGHT_ICON, include_bytes!("../assets/icons/chevron-right.svg")),
    (CHEVRON_DOWN_ICON, include_bytes!("../assets/icons/chevron-down.svg")),
    (FOLDER_ICON, include_bytes!("../assets/icons/folder.svg")),
    (FILE_ICON, include_bytes!("../assets/icons/file.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES.iter().find(|(name, _)| *name == path).map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(FILES.iter().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| (*name).into()).collect())
    }
}
