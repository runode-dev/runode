//! 语音转文字：识别模型的清单和下载（`models`）、录麦克风（`Recorder`）、在本机识别（`Recognizer`），
//! 以及交给云端识别和用大模型润色（`cloud`）。

mod cloud;
mod http;
mod models;
mod recognizer;
mod recorder;

pub use cloud::{Cloud, Polish, polish, transcribe_cloud};
pub use models::{download, download_size, is_installed, remove};
pub use recognizer::Recognizer;
pub use recorder::{MAX_RECORDING_SECS, Recorder, Silent};
pub use runode_types::speech::{DictationCloud, SpeechModel};

/// 录音交给识别器时的采样率。
pub const SAMPLE_RATE: u32 = 16_000;
