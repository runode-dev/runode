//! runode 各部分共用的纯数据：终端画面的快照、网格的尺寸和坐标、分屏布局、agent 的状态、
//! shell 报告的名字、终端设置、默认配色和输入事件。
//!
//! 这里只有数据和不依赖外部状态的计算，只用标准库和 serde；界面、终端状态机和 PTY 都不碰。

pub mod agent;
pub mod color;
pub mod frame;
pub mod grid;
pub mod input;
pub mod pane;
pub mod settings;
pub mod shell;
pub mod theme;
