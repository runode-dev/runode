//! 远程桌面：电脑编好的画面和手机交给电脑的输入。

use serde::{Deserialize, Serialize};

use crate::input::{Key, MouseButton};

/// 画面的编码。观看端在 `Start` 里列出自己解得了的，按想要的先后排；电脑用排在前面、又能低延迟编的那种。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    /// H.265。同样的码率下比 H.264 清楚，优先用它。
    #[default]
    Hevc,
    /// H.264：电脑的 HEVC 编码器做不到低延迟、观看端解不了 HEVC 时用。
    H264,
}

/// 编好的一帧。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    pub codec: Codec,
    /// 这一帧的 NAL 单元，每个前面是 4 字节的大端长度（不是起始码），可以直接交给 VideoToolbox 解码。
    pub data: Vec<u8>,
    /// 随机访问点，解码器能从这一帧开始解。
    pub keyframe: bool,
    /// 关键帧的参数集（不带长度前缀）：HEVC 是 VPS、SPS、PPS，H.264 是 SPS、PPS；别的帧是空的。
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
