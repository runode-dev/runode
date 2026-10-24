//! 手机把录音交给电脑识别的线上格式对着 `tests/fixtures/speech` 里的样例：手机端的测试读同一批文件。
//! 测试失败时先看是不是改了线上格式，要改回来，不要改样例。

use std::path::Path;

use runode_protocol::{
    Frame, FrameKind, read_frame,
    speech::{SPEECH_VERSION, SpeechClientMsg, SpeechHostMsg},
    write_frame,
};
use serde::{Serialize, de::DeserializeOwned};

/// 样例读成 `expected`，`expected` 写出来和样例逐字节一样。
fn matches<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, expected: &T) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/speech").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let text = text.trim_end_matches('\n');
    let read: T = serde_json::from_str(text).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert_eq!(&read, expected, "{name}");
    assert_eq!(serde_json::to_string(expected).unwrap(), text, "{name}");
}

#[test]
fn messages_match_the_fixtures() {
    matches("speech_hello.json", &SpeechClientMsg::SpeechHello { version: SPEECH_VERSION });
    matches("finish.json", &SpeechClientMsg::Finish);
    matches("ready.json", &SpeechHostMsg::Ready { installed: true });
    matches("incompatible.json", &SpeechHostMsg::Incompatible { version: 1 });
    matches("text.json", &SpeechHostMsg::Text { text: "打开 README.md 看看".into() });
    matches("failed.json", &SpeechHostMsg::Failed { reason: "no speech model downloaded".into() });
}

#[test]
fn audio_frames_have_kind_five() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, FrameKind::Audio, 0, &[1, 0, 0xff, 0x7f]).unwrap();
    assert_eq!(bytes[4], 5);
    let frame = read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(frame, Frame { kind: FrameKind::Audio, channel: 0, payload: vec![1, 0, 0xff, 0x7f] });
}
