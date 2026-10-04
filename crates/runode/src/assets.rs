//! 编进二进制的界面资源：`svg()` 这类元素按路径向 GPUI 要文件，GPUI 再来这里取。

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// 侧栏开关的图标。
pub const SIDEBAR_ICON: &str = "icons/sidebar.svg";

const FILES: &[(&str, &[u8])] = &[(SIDEBAR_ICON, include_bytes!("../assets/icons/sidebar.svg"))];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES.iter().find(|(name, _)| *name == path).map(|(_, data)| Cow::Borrowed(*data)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(FILES.iter().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| (*name).into()).collect())
    }
}
