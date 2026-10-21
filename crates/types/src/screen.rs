//! 远程桌面：电脑编好的画面和手机交给电脑的输入。

use serde::{Deserialize, Serialize};

use crate::input::{Key, MouseButton};

/// 编好的一帧 HEVC。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    /// 这一帧的 NAL 单元，每个前面是 4 字节的大端长度（不是起始码），可以直接交给 VideoToolbox 解码。
    pub data: Vec<u8>,
    /// 随机访问点，解码器能从这一帧开始解。
    pub keyframe: bool,
    /// 关键帧的 VPS、SPS、PPS（不带长度前缀），别的帧是空的。
    pub parameter_sets: Vec<Vec<u8>>,
    /// 采集时刻，微秒，只用来比先后和算间隔。
    pub pts_us: i64,
}

/// 按着的修饰键。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenMods {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    /// Command 键（Windows 键）。
    #[serde(default)]
    pub cmd: bool,
}

/// 一次输入。坐标是被采集的那块屏幕上的比例，左上角是 (0, 0)，右下角是 (1, 1)，和画面的分辨率无关。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenInput {
    /// 指针挪到这里；按着键时是拖动。
    MouseMove { x: f64, y: f64 },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: f64,
        y: f64,
        #[serde(default)]
        mods: ScreenMods,
    },
    /// 滚动的点数，和手指拖动的方向一致：正的是内容往下、往右走。
    Scroll { dx: i32, dy: i32 },
    /// 按一下再松开。
    ///
    /// shortcut: 不能按住连发，也不能单独按住修饰键，接硬件键盘时拆成按下和松开
    Key {
        key: Key,
        #[serde(default)]
        mods: ScreenMods,
    },
    /// 输入一段文字，不管键盘布局；输入法上屏的字和 `Key::Unidentified` 的字符走这里。
    Text { text: String },
}
