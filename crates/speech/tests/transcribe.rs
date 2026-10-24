//! 下载真模型、识别模型作者给的测试录音。要下几百 MB，平时不跑：
//! `RUNODE_SPEECH_MODELS=<目录> cargo test -p runode-speech -- --ignored`，目录留着下次复用。

use std::{path::PathBuf, process::Command, sync::atomic::AtomicBool};

use runode_speech::{Recognizer, SAMPLE_RATE, SpeechModel, download, is_installed};

fn models_dir() -> PathBuf {
    std::env::var_os("RUNODE_SPEECH_MODELS")
        .map_or_else(|| std::env::temp_dir().join("runode-speech-models"), PathBuf::from)
}

/// 读 16 kHz、16 位单声道的 WAV：跳过 44 字节的文件头。
fn read_wav(url: &str) -> Vec<f32> {
    let path = models_dir().join("zh.wav");
    if !path.is_file() {
        assert!(Command::new("curl").args(["-fsSL", "-o"]).arg(&path).arg(url).status().unwrap().success());
    }
    let bytes = std::fs::read(path).unwrap();
    bytes[44..].as_chunks::<2>().0.iter().map(|&b| f32::from(i16::from_le_bytes(b)) / 32768.).collect()
}

fn transcribe(model: SpeechModel) -> String {
    let root = models_dir();
    download(model, &root, false, &AtomicBool::new(false), &|_, _| {}).unwrap();
    assert!(is_installed(model, &root));
    let samples = read_wav(
        "https://huggingface.co/csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09/resolve/main/test_wavs/zh.wav",
    );
    assert!(samples.len() > SAMPLE_RATE as usize);
    Recognizer::load(model, &root).unwrap().transcribe(&samples)
}

#[test]
#[ignore = "downloads the model"]
fn sense_voice_transcribes_mandarin() {
    // 锁住标点和阿拉伯数字：只有原版 SenseVoice 在 `use_itn` 时带这两样，换模型时会掉。
    let text = transcribe(SpeechModel::SenseVoice);
    assert!(text.contains("早上9点至下午5点。"), "{text}");
}

#[test]
#[ignore = "downloads the model"]
fn fire_red_asr2_transcribes_mandarin() {
    let text = transcribe(SpeechModel::FireRedAsr2);
    assert!(text.contains("开放时间"), "{text}");
}
