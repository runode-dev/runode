//! 手机把录音交给电脑识别的一条连接上说的话。连接上是 `frame` 的帧：控制消息是通道 0 的
//! `FrameKind::Control`，手机发 `SpeechClientMsg`，电脑发 `SpeechHostMsg`；录音是通道 0 的
//! `FrameKind::Audio`，载荷是 `AUDIO_SAMPLE_RATE` 的单声道 16 位小端 PCM，边录边发，一帧多长不限。
//!
//! 手机经远程访问连上来时，过了门禁发的第一帧是 `SpeechHello`，这条连接就接到电脑的
//! 识别上。电脑回 `Ready`（版本不同时回 `Incompatible` 后断开）。手机录完发 `Finish`，电脑识别完回
//! `Text` 或 `Failed` 后断开。一条连接识别一段话；不要了就直接断开。
//!
//! 以后怎么改：和 `message` 一样，已有字段不改，新加的带 `#[serde(default)]`；改了含义时加
//! `SPEECH_VERSION`。

use serde::{Deserialize, Serialize};

/// 这套消息和录音格式的版本，手机在 `SpeechClientMsg::SpeechHello` 里报自己的。
pub const SPEECH_VERSION: u32 = 1;

/// `FrameKind::Audio` 的采样率。
pub const AUDIO_SAMPLE_RATE: u32 = 16_000;

/// 一段录音最长这么多秒，超了电脑回 `Failed`。
pub const MAX_AUDIO_SECS: u32 = 120;

/// 手机发给电脑的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpeechClientMsg {
    /// 连接上的第一条。
    SpeechHello { version: u32 },
    /// 录完了，识别之前发来的录音。
    Finish,
}

/// 电脑发给手机的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpeechHostMsg {
    /// 连上后的第一条。`installed` 为假时电脑还没下载它设置的识别模型，识别只会失败，要用户到电脑的
    /// 设置里下载。
    Ready { installed: bool },
    /// 手机的 `SPEECH_VERSION` 和电脑的不同，电脑随后断开。
    Incompatible { version: u32 },
    /// 识别出的文字；没听出话来时是空的。
    Text { text: String },
    /// 识别不了，带原因（给人看的英文）。
    Failed { reason: String },
}
