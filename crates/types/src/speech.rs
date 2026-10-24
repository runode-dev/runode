//! 语音转文字用哪个识别模型（配置项 `dictation-model`）和本机没有模型时交给哪家云端识别
//! （`dictation-cloud`）；配置里写它们的名字（`name`）。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SpeechModel {
    /// SenseVoice-Small：中、英、日、韩、粤，快，带标点。
    #[default]
    SenseVoice,
    /// FireRedASR2-AED：中英，中文最准，也最大。
    FireRedAsr2,
}

impl SpeechModel {
    pub const ALL: [Self; 2] = [Self::SenseVoice, Self::FireRedAsr2];

    pub fn name(self) -> &'static str {
        match self {
            Self::SenseVoice => "sense-voice",
            Self::FireRedAsr2 => "fire-red-asr2",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.name() == name)
    }
}

/// 本机识别不了（模型没下载、识别出错）时交给哪家云端识别。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DictationCloud {
    /// 不交给云端。
    #[default]
    None,
    /// OpenAI 兼容的 `/audio/transcriptions`。
    OpenAi,
    /// 火山引擎豆包的录音文件识别极速版。
    Doubao,
}

impl DictationCloud {
    pub const ALL: [Self; 3] = [Self::None, Self::OpenAi, Self::Doubao];

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::OpenAi => "openai",
            Self::Doubao => "doubao",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cloud| cloud.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::{DictationCloud, SpeechModel};

    #[test]
    fn names_round_trip() {
        for model in SpeechModel::ALL {
            assert_eq!(SpeechModel::from_name(model.name()), Some(model));
        }
        assert_eq!(SpeechModel::from_name("whisper"), None);
    }

    #[test]
    fn cloud_names_round_trip() {
        for cloud in DictationCloud::ALL {
            assert_eq!(DictationCloud::from_name(cloud.name()), Some(cloud));
        }
    }
}
