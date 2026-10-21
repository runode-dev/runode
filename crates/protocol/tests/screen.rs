//! 远程桌面的线上格式对着 `tests/fixtures/screen` 里的样例：样例照规格另外写出来，手机端的测试读同一批
//! 文件。测试失败时先看是不是改了线上格式，要改回来，不要改样例。

use std::path::Path;

use runode_protocol::screen::{
    ScreenClientMsg, ScreenDisplay, ScreenHostMsg, ScreenInput, ScreenMods, VideoFrame, decode_video, encode_video,
};
use runode_types::input::{Key, MouseButton};
use serde::{Serialize, de::DeserializeOwned};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/screen").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    text.trim_end_matches('\n').to_owned()
}

/// 样例读成 `expected`，`expected` 写出来和样例逐字节一样。
fn matches<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, expected: &T) {
    let text = fixture(name);
    let read: T = serde_json::from_str(&text).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert_eq!(&read, expected, "{name}");
    assert_eq!(serde_json::to_string(expected).unwrap(), text, "{name}");
}

fn hex(name: &str) -> Vec<u8> {
    let text = fixture(name);
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}

#[test]
fn client_messages_match_the_fixtures() {
    matches("screen_hello.json", &ScreenClientMsg::ScreenHello { version: 2 });
    matches(
        "start.json",
        &ScreenClientMsg::Start { display: Some(2), width: 2556, height: 1179, fps: 60, bitrate: 8_000_000 },
    );
    matches("keyframe.json", &ScreenClientMsg::Keyframe);
    matches("bitrate.json", &ScreenClientMsg::Bitrate { bitrate: 2_000_000 });
    let input = |input| ScreenClientMsg::Input { input };
    matches("input_mouse_move.json", &input(ScreenInput::MouseMove { x: 0.5, y: 0.25 }));
    let cmd = ScreenMods { cmd: true, ..ScreenMods::default() };
    let click = ScreenInput::MouseButton { button: MouseButton::Right, down: true, x: 0.75, y: 1.0, mods: cmd };
    matches("input_mouse_button.json", &input(click));
    matches("input_scroll.json", &input(ScreenInput::Scroll { dx: 0, dy: -120 }));
    let shift = ScreenMods { shift: true, ..ScreenMods::default() };
    matches("input_key.json", &input(ScreenInput::Key { key: Key::ArrowUp, mods: shift }));
    matches("input_text.json", &input(ScreenInput::Text { text: "你好 ok".into() }));
    matches("unlock.json", &ScreenClientMsg::Unlock { password: "p@ss word".into() });
}

#[test]
fn host_messages_match_the_fixtures() {
    let displays = vec![
        ScreenDisplay { id: 1, width: 1728, height: 1117, pixel_width: 3456, pixel_height: 2234 },
        ScreenDisplay { id: 2, width: 2560, height: 1440, pixel_width: 2560, pixel_height: 1440 },
    ];
    matches("ready.json", &ScreenHostMsg::Ready { displays, screen_capture: true, post_event: false });
    matches("incompatible.json", &ScreenHostMsg::Incompatible { version: 2 });
    matches("started.json", &ScreenHostMsg::Started { display: 2, width: 2556, height: 1652 });
    matches("stopped.json", &ScreenHostMsg::Stopped { reason: "display unplugged".into() });
    matches("lock.json", &ScreenHostMsg::Lock { locked: true });
    let reason = "the password has keys a US keyboard cannot type".into();
    matches("unlock_failed.json", &ScreenHostMsg::UnlockFailed { reason });
}

#[test]
fn video_payloads_match_the_fixtures() {
    let keyframe = VideoFrame {
        data: vec![0, 0, 0, 2, 0x26, 1],
        keyframe: true,
        parameter_sets: vec![vec![0x40, 1], vec![0x42], vec![0x44, 1, 2]],
        pts_us: -5,
    };
    let delta =
        VideoFrame { data: vec![0, 0, 0, 2, 2, 0x0a], keyframe: false, parameter_sets: Vec::new(), pts_us: 1000 };
    for (name, frame) in [("video_keyframe.hex", keyframe), ("video_delta.hex", delta)] {
        assert_eq!(encode_video(&frame), hex(name), "{name}");
        assert_eq!(decode_video(&hex(name)), Some(frame), "{name}");
    }
}

/// 参数集的长度越过载荷结尾时不读出界，整帧当作格式不对。
#[test]
fn truncated_parameter_sets_are_rejected() {
    let mut payload = vec![1];
    payload.extend(0i64.to_le_bytes());
    payload.push(1);
    payload.extend(9u32.to_le_bytes());
    payload.extend([1, 2]);
    assert_eq!(decode_video(&payload), None);
}
