//! 不属于哪个具体功能、几处界面都用的 GPUI 部件和小工具：单行输入框（`text_field`）、多行输入框
//! （`text_area`）、滚动条（`scrollbar`）、悬停提示（`tooltip`）、系统声音（`sound`）、按文件类型区分的图标（`file_icons`）、
//! 让 Option 组合键先走快捷键的输入处理（`input_handler`）、
//! 复制粘贴这类共用的编辑动作（`actions`），把调色板的颜色换成 GPUI 颜色的 `hsla`，以及把家目录写成
//! `~` 显示路径的 `display_dir`。
//!
//! 依赖只能从功能模块指向这里：`ui` 不依赖 `terminal_view`、`window`、`host_client` 这些具体
//! 功能的模块，它们要的东西由调用方经参数或事件交过来。

pub mod actions;
pub mod file_icons;
pub mod input_handler;
pub mod scrollbar;
pub mod sound;
pub mod text_area;
pub mod text_field;
pub mod tooltip;

use std::path::Path;

use gpui::{Hsla, rgb};
use runode_shared_types::color::Rgb;

/// 调色板里的颜色换成 GPUI 画图用的颜色。
pub fn hsla(color: Rgb) -> Hsla {
    rgb(color.to_u32()).into()
}

/// 给人看的路径，家目录写成 `~`。
pub fn display_dir(dir: &Path) -> String {
    if let Some(home) = runode_paths::Dirs::from_env().home
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}
