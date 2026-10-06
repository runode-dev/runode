//! app 里各个终端摆在哪：窗口、工作区、标签和标签里的分屏，回 `ClientMsg::Layout`。命令行据此
//! 按位置找终端（`left`、`tab:2`、`pane:3` 这类写法），列会话时标出每个会话在哪个窗口、标签里。
//!
//! 序号一律从 1 开始。窗口按打开的先后编号；工作区、标签按界面上的先后；分屏按标签里从左到右、
//! 从上到下的叶子顺序。

use serde::{Deserialize, Serialize};

use crate::SessionId;

/// 一个窗口。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowLayout {
    /// 窗口的序号，按打开的先后。
    pub index: u32,
    /// 是最前面的那个窗口。
    #[serde(default)]
    pub front: bool,
    pub workspaces: Vec<WorkspaceLayout>,
}

/// 窗口里的一个工作区。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    pub index: u32,
    /// 工作区在侧栏里显示的名字。
    #[serde(default)]
    pub name: Option<String>,
    /// 是窗口当前显示的工作区。
    #[serde(default)]
    pub active: bool,
    pub tabs: Vec<TabLayout>,
}

/// 工作区里的一个标签。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabLayout {
    pub index: u32,
    /// 是工作区当前显示的标签。
    #[serde(default)]
    pub active: bool,
    pub panes: Vec<PaneLayout>,
}

/// 标签里的一个分屏，即一个终端。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLayout {
    /// 分屏的序号，按叶子从左到右、从上到下的顺序。
    pub index: u32,
    /// 分屏里的会话。
    pub id: SessionId,
    pub rect: PaneRect,
    /// 是标签里有焦点的那个分屏。
    #[serde(default)]
    pub focused: bool,
}

/// 分屏在标签区域里的位置，整个标签区域按 0..1000 归一化，和窗口实际的像素大小无关；后台标签
/// 没画出来也一样算得出。按方向找相邻分屏时，分隔线占的那一点宽度不到几个单位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl PaneRect {
    /// 整个标签区域的边长。
    pub const EXTENT: u16 = 1000;
}
