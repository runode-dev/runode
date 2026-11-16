//! 手机把图片、视频交给电脑的一条连接上说的话。连接上是 `frame` 的帧：控制消息是通道 0 的
//! `FrameKind::Control`，手机发 `MediaClientMsg`，电脑发 `MediaHostMsg`；文件的内容是通道 0 的
//! `FrameKind::Media`，一帧的载荷最多 `MAX_MEDIA_FRAME` 字节。
//!
//! 手机经远程访问连上来时，过了门禁发的第一帧是 `MediaHello`，这条连接就接到电脑收文件的地方。电脑回
//! `Ready`；版本不同时回 `Incompatible`，随后断开。
//!
//! 每个文件：手机发 `Media { len }`，接着发若干 `FrameKind::Media` 帧，载荷加起来正好 `len` 字节。电脑认得
//! 格式并存好后回 `Saved { path }`（文件在电脑上的绝对路径），存不了回 `Failed { reason }`。之后连接可以接着
//! 发下一个文件。
//!
//! - `len` 为 0 或超过 `MAX_MEDIA_BYTES` 时电脑回 `Failed`，随后断开。
//! - 格式认不出时电脑回 `Failed`，连接照旧，可以接着发下一个。
//! - 一帧超过 `MAX_MEDIA_FRAME`、载荷加起来多出 `len`、发到一半来了别的帧时，电脑直接断开。
//! - 手机不要了就直接断开，电脑删掉没收完的文件。
//!
//! 电脑看文件头认格式：PNG、JPEG、GIF、WebP，以及 ISO BMFF 的视频（偏移 4 处是 `ftyp`）：主品牌是 `qt  `
//! 的存成 `.mov`，别的存成 `.mp4`。电脑不转码，视频要多大由手机压好再发。
//!
//! 兼容：已有的字段和取值不改，新加的字段带 `#[serde(default)]`，旧的一端读得懂；要改已有消息的含义时加
//! `MEDIA_VERSION`。

use serde::{Deserialize, Serialize};

/// 这套消息的版本，手机在 `MediaClientMsg::MediaHello` 里报自己的。
pub const MEDIA_VERSION: u32 = 1;

/// 一个文件最多这么多字节（200 MiB），超了电脑回 `Failed` 后断开。
pub const MAX_MEDIA_BYTES: u32 = 200 << 20;

/// 一帧 `FrameKind::Media` 的载荷最多这么多字节（1 MiB），超了电脑断开。
pub const MAX_MEDIA_FRAME: u32 = 1 << 20;

/// 手机发给电脑的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaClientMsg {
    /// 连接上的第一条。
    MediaHello { version: u32 },
    /// 要发一个文件了，随后的 `FrameKind::Media` 帧的载荷加起来正好 `len` 字节。
    Media { len: u32 },
}

/// 电脑发给手机的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaHostMsg {
    /// 连上后的第一条。
    Ready,
    /// 手机的 `MEDIA_VERSION` 和电脑的不同，电脑随后断开。
    Incompatible { version: u32 },
    /// 存好了，`path` 是文件在电脑上的绝对路径。
    Saved { path: String },
    /// 存不了，带原因（给人看的英文）。
    Failed { reason: String },
}
