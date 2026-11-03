//! 远程桌面：看和操控整块屏幕的一条连接上说的话。连接上是 `frame` 的帧：控制消息是通道 0 的
//! `FrameKind::Control`，客户端发 `ScreenClientMsg`，电脑发 `ScreenHostMsg`；画面是
//! `FrameKind::Video`，一帧画面一个帧，载荷见 `encode_video`。
//!
//! 手机经远程访问连上来时，过了门禁（见 `remote`）发的第一帧是 `ScreenHello` 而不是
//! `ClientMsg::Hello`，这条连接就接到远程桌面上。电脑回 `Ready`（版本不同时回 `Incompatible` 后
//! 断开），客户端按里面的屏幕和授权发 `Start`，要采哪块屏幕在 `Start` 里指定，切屏就是再发一次。之后每要一次关键帧发一次 `Keyframe`（刚开始解、
//! 丢了帧），输入随时发 `Input`。要换屏幕的显示模式时发 `SetMode`，电脑换好后回 `Displays`，客户端按新的尺寸再发
//! `Start`；连接断开时电脑把换过的屏幕换回原来的模式。
//!
//! 画面可以改走 UDP：`Ready` 里 `datagram` 为真时客户端可以发 `Datagram` 给出密钥和自己的地址，电脑回 `Datagram`
//! 给出它的，两边互相打通以后电脑把画面改发到 UDP 上，控制消息和输入仍走这条连接；UDP 不通了电脑先发
//! `DatagramLost`，之后的画面回到这条连接上。`datagram` 为假的电脑不认得 `Datagram`，收到会断开连接。
//!
//! 以后怎么改：和 `message` 一样，已有字段不改，新加的带 `#[serde(default)]`；改了含义时加
//! `SCREEN_VERSION`。

use serde::{Deserialize, Serialize};

use std::net::IpAddr;

pub use runode_types::screen::{Codec, ScreenInput, ScreenMods, StopCause, VideoFrame};

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
        /// 画面里不画电脑的鼠标指针：客户端自己有指针、点哪儿就是哪儿时（桌面 app）不要画面里再出一个跟不上的。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        hide_cursor: bool,
        /// 客户端解得了的编码，想要的排前面；为空时只有 HEVC。不认得的编码跳过。
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "known_codecs")]
        codecs: Vec<Codec>,
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
    /// 把屏幕 `display` 换成显示模式 `mode`（`ScreenDisplay::modes` 里的一个），为空时换回原来的。电脑停下这条
    /// 连接的采集，换好后回 `Displays`，换不成时先回 `ModeFailed`。这条连接断开时电脑自己换回原来的。
    SetMode {
        display: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<ScreenMode>,
    },
    /// 想让画面改走 UDP：`key` 是 64 位十六进制的 32 字节密钥，两个方向的包都用它加密，每条连接都要新随机生成、
    /// 不能复用（每条连接的包计数都从零起，复用密钥就复用了 nonce）；`port` 和 `addrs` 是客户端收包的端口和本机的
    /// 地址，电脑往这些地址打洞。电脑回 `Datagram` 才算答应。
    Datagram {
        key: String,
        port: u16,
        addrs: Vec<IpAddr>,
    },
}

/// 读编码的列表，跳过不认得的（更新的客户端多列的编码）。
fn known_codecs<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Codec>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Listed {
        Known(Codec),
        Unknown(serde::de::IgnoredAny),
    }
    let listed = Vec::<Listed>::deserialize(deserializer)?;
    Ok(listed
        .into_iter()
        .filter_map(|codec| match codec {
            Listed::Known(codec) => Some(codec),
            Listed::Unknown(_) => None,
        })
        .collect())
}

/// 电脑发给客户端的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenHostMsg {
    /// 连上后的第一条。`displays` 是开着的各块屏幕，主屏排第一。授权不够时采集（`screen_capture`）或
    /// 输入（`post_event`）办不了，要用户到电脑的系统设置里打开。
    Ready {
        displays: Vec<ScreenDisplay>,
        screen_capture: bool,
        post_event: bool,
        /// 认得客户端的 `Datagram`，画面能改走 UDP。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        datagram: bool,
    },
    /// 客户端的 `SCREEN_VERSION` 和电脑的不同，电脑随后断开。
    Incompatible { version: u32 },
    /// 按 `Start` 开始采集了，在这之后的画面都是新的，第一帧是关键帧（开不起来时随后是 `Stopped`）；
    /// `display` 是采的那块屏幕，之后的输入落在它上面。
    Started { display: u32, width: u32, height: u32 },
    /// 采集停了或者开不起来，带原因；可以再发 `Start`。电脑认得出原因时给出 `cause`，客户端照它给人看说明，
    /// `reason` 是电脑上的错误原文。`signed_out` 说电脑的屏幕上没有能看的桌面，有人登录后重新连上就能看；
    /// `no_encoder` 说电脑上建不出能编客户端解得了的格式的编码器，重连也看不了。给了 `cause` 时两个标志和它
    /// 对得上（`StopCause::waits_for_sign_in` 的类带 `signed_out`，`NoEncoder` 带 `no_encoder`），不认得
    /// `cause` 的客户端照标志办。
    Stopped {
        reason: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        signed_out: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        no_encoder: bool,
        /// 不认得的种类（更新的电脑多分的类）读成空。
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "known_cause")]
        cause: Option<StopCause>,
    },
    /// 电脑锁没锁屏：`Ready` 之后马上报一次，之后变了再报。
    Lock { locked: bool },
    /// 用户在电脑上打开或关掉了输入授权：不用重连就能操作，关掉后操作不了；`post_event` 的含义和 `Ready`
    /// 里的一样。`screen_capture` 是 `Ready` 时的值，屏幕录制的授权要重新连接才认。
    Permissions { screen_capture: bool, post_event: bool },
    /// `Unlock` 没敲成，带原因（给日志看）。
    UnlockFailed { reason: String },
    /// 回 `SetMode`：现在的各块屏幕，和 `Ready` 里的一样。这条连接的采集已经停了，要按新的尺寸再发 `Start`。
    Displays { displays: Vec<ScreenDisplay> },
    /// `SetMode` 没换成，带原因。
    ModeFailed { reason: String },
    /// 回 `Datagram`：电脑收包的端口和本机的地址。两边打通以后画面改走 UDP。
    Datagram { port: u16, addrs: Vec<IpAddr> },
    /// UDP 没打通或者不通了，之后的画面走这条连接，换路后的第一帧是关键帧。这条连接不再试 UDP。
    DatagramLost,
}

impl ScreenHostMsg {
    /// 带原因 `reason` 的 `Stopped`，`signed_out`、`no_encoder` 按 `cause` 填。
    pub fn stopped(reason: String, cause: Option<StopCause>) -> Self {
        Self::Stopped {
            reason,
            signed_out: cause.is_some_and(StopCause::waits_for_sign_in),
            no_encoder: cause == Some(StopCause::NoEncoder),
            cause,
        }
    }
}

/// 读 `Stopped::cause`，不认得的种类当成没给。
fn known_cause<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<StopCause>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Given {
        Known(StopCause),
        Unknown(serde::de::IgnoredAny),
    }
    Ok(match Option::<Given>::deserialize(deserializer)? {
        Some(Given::Known(cause)) => Some(cause),
        Some(Given::Unknown(_)) | None => None,
    })
}

/// 被采集的那块屏幕。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenDisplay {
    /// 电脑上这块屏幕的编号（`CGDirectDisplayID`），插拔以后可能变。
    pub id: u32,
    /// 点数。
    pub width: u32,
    pub height: u32,
    /// 像素数，Retina 屏是点数的两倍。
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// 能换成的显示模式，排版尺寸从大到小；换不了模式的电脑不给。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ScreenMode>,
    /// 显示模式被远程桌面换过，最后一条换过它的连接断开时换回原来的。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub changed: bool,
}

/// 屏幕的一种显示模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScreenMode {
    /// 按它排版的尺寸：Mac 上是点数，Windows 上是像素数。
    pub width: u32,
    pub height: u32,
    /// 像素数。
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// 关键帧的标志位。
pub const KEYFRAME: u8 = 1;
/// H.264 的标志位，没有时是 HEVC。客户端在 `Start` 里列了 H.264 才会有。
const H264: u8 = 2;

/// `FrameKind::Video` 的载荷：`u8 标志（第 0 位是关键帧，第 1 位是 H.264）| i64 LE pts_us | u8 参数集个数 |
/// 每个参数集 u32 LE 长度和内容 | 帧的数据（到载荷结尾）`。
pub fn encode_video(frame: &VideoFrame) -> Vec<u8> {
    let sets: usize = frame.parameter_sets.iter().map(|set| 4 + set.len()).sum();
    let mut out = Vec::with_capacity(10 + sets + frame.data.len());
    let codec = if frame.codec == Codec::H264 { H264 } else { 0 };
    out.push(codec | if frame.keyframe { KEYFRAME } else { 0 });
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
        codec: if flags & H264 != 0 { Codec::H264 } else { Codec::Hevc },
        data: rest.to_vec(),
        keyframe: flags & KEYFRAME != 0,
        parameter_sets,
        pts_us: i64::from_le_bytes(*pts),
    })
}
