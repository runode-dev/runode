//! 把录好的一段音转成文字：先用 VAD 切成一句一句、去掉静音，再逐句交给识别模型。

use std::path::Path;

use anyhow::Context;
use sherpa_onnx::{
    OfflineFireRedAsrModelConfig, OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

use crate::{
    SAMPLE_RATE, SpeechModel,
    models::{VAD, assets},
};

/// Silero VAD 一次要的样本数（16 kHz 时 32 毫秒）。
const VAD_WINDOW: usize = 512;

/// 加载好的识别模型。加载要几百毫秒到几秒，调用方加载一次反复用。
pub struct Recognizer {
    asr: OfflineRecognizer,
    vad: VoiceActivityDetector,
}

impl Recognizer {
    /// 从 `root` 下加载 `model`，文件要已经下载好（`is_installed`）。
    pub fn load(model: SpeechModel, root: &Path) -> anyhow::Result<Self> {
        let path = |name: &str| {
            let asset = assets(model).iter().find(|a| a.name == name).expect("model has the asset");
            Some(asset.path(root).to_string_lossy().into_owned())
        };
        let mut config = OfflineRecognizerConfig::default();
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
        config.model_config.num_threads = threads as i32;
        config.model_config.tokens = path("tokens.txt");
        match model {
            SpeechModel::SenseVoice => {
                config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                    model: path("model.int8.onnx"),
                    language: Some("auto".into()),
                    // 把「一百二十三」写成「123」，加标点。
                    use_itn: true,
                };
            }
            SpeechModel::FireRedAsr2 => {
                config.model_config.fire_red_asr = OfflineFireRedAsrModelConfig {
                    encoder: path("encoder.int8.onnx"),
                    decoder: path("decoder.int8.onnx"),
                };
            }
        }
        let asr = OfflineRecognizer::create(&config).context("cannot load the model")?;

        let vad = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(VAD.path(root).to_string_lossy().into_owned()),
                threshold: 0.5,
                min_silence_duration: 0.5,
                min_speech_duration: 0.25,
                window_size: VAD_WINDOW as i32,
                // 一句再长也在这里切开：FireRedASR2 一次最多认 60 秒。
                max_speech_duration: 20.,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            ..Default::default()
        };
        // 每喂一块就取走切好的句子，缓冲只要装得下最长的一句。
        let vad = VoiceActivityDetector::create(&vad, 30.).context("cannot load the VAD model")?;
        Ok(Self { asr, vad })
    }

    /// 识别一段 `SAMPLE_RATE` 的单声道音频；没有人声时是空字符串。
    pub fn transcribe(&mut self, samples: &[f32]) -> String {
        self.vad.reset();
        let mut text = String::new();
        for chunk in samples.chunks(VAD_WINDOW) {
            self.vad.accept_waveform(chunk);
            self.drain(&mut text);
        }
        self.vad.flush();
        self.drain(&mut text);
        text
    }

    fn drain(&self, text: &mut String) {
        while let Some(segment) = self.vad.front() {
            let stream = self.asr.create_stream();
            stream.accept_waveform(SAMPLE_RATE as i32, segment.samples());
            self.asr.decode(&stream);
            if let Some(result) = stream.get_result() {
                append(text, result.text.trim());
            }
            self.vad.pop();
        }
    }
}

/// 把一句接到前面的文字后面；两边都是西文时中间加空格，中文之间不加。
fn append(text: &mut String, sentence: &str) {
    let ascii_end = text.chars().next_back().is_some_and(|c| c.is_ascii_graphic());
    let ascii_start = sentence.chars().next().is_some_and(|c| c.is_ascii_graphic());
    if ascii_end && ascii_start {
        text.push(' ');
    }
    text.push_str(sentence);
}

#[cfg(test)]
mod tests {
    use super::append;

    #[test]
    fn spaces_only_between_latin_sentences() {
        let mut text = String::new();
        for sentence in ["Hello.", "World.", "你好。", "好的", "OK."] {
            append(&mut text, sentence);
        }
        assert_eq!(text, "Hello. World.你好。好的OK.");
    }
}
