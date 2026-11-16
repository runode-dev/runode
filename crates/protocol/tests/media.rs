//! 手机把图片、视频交给电脑的线上格式对着 `tests/fixtures/media` 里的样例：手机端的测试读同一批文件。
//! 测试失败时先看是不是改了线上格式，要改回来，不要改样例。

use std::path::Path;

use runode_protocol::{
    Frame, FrameKind,
    media::{MEDIA_VERSION, MediaClientMsg, MediaHostMsg},
    read_frame, write_frame,
};
use serde::{Serialize, de::DeserializeOwned};

/// 样例读成 `expected`，`expected` 写出来和样例逐字节一样。
fn matches<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, expected: &T) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let text = text.trim_end_matches('\n');
    let read: T = serde_json::from_str(text).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert_eq!(&read, expected, "{name}");
    assert_eq!(serde_json::to_string(expected).unwrap(), text, "{name}");
}

#[test]
fn messages_match_the_fixtures() {
    matches("media_hello.json", &MediaClientMsg::MediaHello { version: MEDIA_VERSION });
    matches("media.json", &MediaClientMsg::Media { len: 245_760 });
    matches("ready.json", &MediaHostMsg::Ready);
    matches("incompatible.json", &MediaHostMsg::Incompatible { version: 1 });
    matches("saved.json", &MediaHostMsg::Saved { path: "/Users/ethan/Library/Caches/runode/images/截图 1.png".into() });
    matches("failed.json", &MediaHostMsg::Failed { reason: "the file is larger than 200 MiB".into() });
}

#[test]
fn media_frames_have_kind_six() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, FrameKind::Media, 0, &[0x89, b'P', b'N', b'G']).unwrap();
    assert_eq!(bytes[4], 6);
    let frame = read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(frame, Frame { kind: FrameKind::Media, channel: 0, payload: vec![0x89, b'P', b'N', b'G'] });
}
