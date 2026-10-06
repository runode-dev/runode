//! 不属于哪个具体功能、几处界面都用的 GPUI 部件和小工具：单行输入框（`text_field`）、多行输入框
//! （`text_area`）、滚动条（`scrollbar`）、悬停提示（`tooltip`）和按文件类型区分的图标（`file_icons`）。
//!
//! 依赖只能从功能模块指向这里：`ui` 不依赖 `terminal_view`、`workspace`、`session_host` 这些具体
//! 功能的模块，它们要的东西由调用方经参数或事件交过来。

pub mod file_icons;
pub mod scrollbar;
pub mod text_area;
pub mod text_field;
pub mod tooltip;
