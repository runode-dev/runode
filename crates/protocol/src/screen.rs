//! 远程桌面：看和操控整块屏幕的一条连接上说的话。连接上是 `frame` 的帧：控制消息是通道 0 的
//! `FrameKind::Control`，客户端发 `ScreenClientMsg`，电脑发 `ScreenHostMsg`；画面是
//! `FrameKind::Video`，一帧画面一个帧，载荷见 `encode_video`。
//!
//! 手机经远程访问连上来时，过了门禁（见 `remote`）发的第一帧是 `ScreenHello` 而不是
//! `ClientMsg::Hello`，这条连接就接到远程桌面上。电脑回 `Ready`（版本不同时回 `Incompatible` 后
//! 断开），客户端按里面的屏幕和授权发 `Start`，要采哪块屏幕在 `Start` 里指定，切屏就是再发一次。之后每要一次关键帧发一次 `Keyframe`（刚开始解、
//! 丢了帧），输入随时发 `Input`。
//!
//! 以后怎么改：和 `message` 一样，已有字段不改，新加的带 `#[serde(default)]`；改了含义时加
//! `SCREEN_VERSION`。

use serde::{Deserialize, Serialize};

pub use runode_types::screen::{ScreenInput, ScreenMods, VideoFrame};

/// 远程桌面这套消息和画面格式的版本，见 `ScreenClientMsg::ScreenHello`。
pub const SCREEN_VERSION: u32 = 2;

/// 客户端发给电脑的。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenClientMsg {
    /// 连接上的第一条。
    ScreenHello {
        version: u32,
    },
    /// 开始采集；采集中再发时按新的参数重新开始（比如手机转了屏）。
    Start {
        /// 采哪块屏幕，`ScreenDisplay::id`；不给时是主屏。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<u32>,
        /// 画面的像素尺寸。
        width: u32,
        height: u32,
        fps: u32,
        /// 平均码率，单位 bit/s。
        bitrate: u32,
    },
    /// 下一帧要关键帧。
    Keyframe,
    /// 改平均码率，单位 bit/s。
    Bitrate {
        bitrate: u32,
    },
    Input {
        input: ScreenInput,
    },
    /// 电脑锁着屏时用登录密码解锁：电脑按美式键盘把它敲进锁屏的密码框再回车，没锁时不敲。解没解开看随后
    /// 的 `Lock`；敲不了（有美式键盘打不出的字符）时回 `UnlockFailed`。
    Unlock {
        password: String,
    },
}

/// 电脑发给客户端的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenHostMsg {
    /// 连上后的第一条。`displays` 是开着的各块屏幕，主屏排第一。授权不够时采集（`screen_capture`）或
    /// 输入（`post_event`）办不了，要用户到电脑的系统设置里打开。
    Ready { displays: Vec<ScreenDisplay>, screen_capture: bool, post_event: bool },
    /// 客户端的 `SCREEN_VERSION` 和电脑的不同，电脑随后断开。
    Incompatible { version: u32 },
    /// 按 `Start` 开始采集了，在这之后的画面都是新的，第一帧是关键帧（开不起来时随后是 `Stopped`）；
    /// `display` 是采的那块屏幕，之后的输入落在它上面。
    Started { display: u32, width: u32, height: u32 },
    /// 采集停了或者开不起来，带原因；可以再发 `Start`。
    Stopped { reason: String },
    /// 电脑锁没锁屏：`Ready` 之后马上报一次，之后变了再报。
    Lock { locked: bool },
    /// `Unlock` 没敲成，带原因（给日志看）。
    UnlockFailed { reason: String },
}

/// 被采集的那块屏幕。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenDisplay {
    /// 电脑上这块屏幕的编号（`CGDirectDisplayID`），插拔以后可能变。
    pub id: u32,
    /// 点数。
    pub width: u32,
    pub height: u32,
    /// 像素数，Retina 屏是点数的两倍。
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// 关键帧的标志位。
const KEYFRAME: u8 = 1;

/// `FrameKind::Video` 的载荷：`u8 标志（第 0 位是关键帧）| i64 LE pts_us | u8 参数集个数 |
/// 每个参数集 u32 LE 长度和内容 | 帧的数据（到载荷结尾）`。
pub fn encode_video(frame: &VideoFrame) -> Vec<u8> {
    let sets: usize = frame.parameter_sets.iter().map(|set| 4 + set.len()).sum();
    let mut out = Vec::with_capacity(10 + sets + frame.data.len());
    out.push(if frame.keyframe { KEYFRAME } else { 0 });
    out.extend(frame.pts_us.to_le_bytes());
    out.push(frame.parameter_sets.len() as u8);
    for set in &frame.parameter_sets {
        out.extend((set.len() as u32).to_le_bytes());
        out.extend(set);
    }
    out.extend(&frame.data);
    out
}

/// 解 `encode_video` 的载荷，格式不对时返回 `None`。
pub fn decode_video(payload: &[u8]) -> Option<VideoFrame> {
    let (&flags, rest) = payload.split_first()?;
    let (pts, rest) = rest.split_first_chunk::<8>()?;
    let (&count, mut rest) = rest.split_first()?;
    let mut parameter_sets = Vec::with_capacity(count.into());
    for _ in 0..count {
        let (len, after) = rest.split_first_chunk::<4>()?;
        let len = u32::from_le_bytes(*len) as usize;
        parameter_sets.push(after.get(..len)?.to_vec());
        rest = &after[len..];
    }
    Some(VideoFrame {
        data: rest.to_vec(),
        keyframe: flags & KEYFRAME != 0,
        parameter_sets,
        pts_us: i64::from_le_bytes(*pts),
    })
}
