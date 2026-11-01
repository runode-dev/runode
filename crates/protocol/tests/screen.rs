//! 远程桌面的线上格式对着 `tests/fixtures/screen` 里的样例：样例照规格另外写出来，手机端的测试读同一批
//! 文件。测试失败时先看是不是改了线上格式，要改回来，不要改样例。

use std::path::Path;

use runode_protocol::screen::{
    Codec, ScreenClientMsg, ScreenDisplay, ScreenHostMsg, ScreenInput, ScreenMode, ScreenMods, VideoFrame,
    decode_video, encode_video,
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
        &ScreenClientMsg::Start {
            display: Some(2),
            width: 2556,
            height: 1179,
            fps: 60,
            bitrate: 8_000_000,
            hide_cursor: false,
            codecs: Vec::new(),
        },
    );
    matches(
        "start_hide_cursor.json",
        &ScreenClientMsg::Start {
            display: None,
            width: 1280,
            height: 720,
            fps: 60,
            bitrate: 4_000_000,
            hide_cursor: true,
            codecs: Vec::new(),
        },
    );
    matches(
        "start_codecs.json",
        &ScreenClientMsg::Start {
            display: None,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate: 6_000_000,
            hide_cursor: true,
            codecs: vec![Codec::Hevc, Codec::H264],
        },
    );
    matches(
        "datagram_offer.json",
        &ScreenClientMsg::Datagram {
            key: "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".into(),
            port: 50123,
            addrs: vec!["10.0.0.5".parse().unwrap(), "fd00::5".parse().unwrap()],
        },
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
    let mode = ScreenMode { width: 1440, height: 900, pixel_width: 2880, pixel_height: 1800 };
    matches("set_mode.json", &ScreenClientMsg::SetMode { display: 1, mode: Some(mode) });
    matches("set_mode_default.json", &ScreenClientMsg::SetMode { display: 1, mode: None });
}

#[test]
fn host_messages_match_the_fixtures() {
    let display = |id, width, height, pixel_width, pixel_height| ScreenDisplay {
        id,
        width,
        height,
        pixel_width,
        pixel_height,
        modes: Vec::new(),
        changed: false,
    };
    let displays = vec![display(1, 1728, 1117, 3456, 2234), display(2, 2560, 1440, 2560, 1440)];
    let ready = |datagram| ScreenHostMsg::Ready {
        displays: displays.clone(),
        screen_capture: true,
        post_event: false,
        datagram,
    };
    matches("ready.json", &ready(false));
    matches("ready_datagram.json", &ready(true));
    matches("incompatible.json", &ScreenHostMsg::Incompatible { version: 2 });
    matches("started.json", &ScreenHostMsg::Started { display: 2, width: 2556, height: 1652 });
    matches("stopped.json", &ScreenHostMsg::Stopped { reason: "display unplugged".into(), signed_out: false });
    let reason = "nobody is signed in on this computer's screen".into();
    matches("stopped_signed_out.json", &ScreenHostMsg::Stopped { reason, signed_out: true });
    matches("lock.json", &ScreenHostMsg::Lock { locked: true });
    matches("permissions.json", &ScreenHostMsg::Permissions { screen_capture: true, post_event: true });
    let reason = "the password has keys a US keyboard cannot type".into();
    matches("unlock_failed.json", &ScreenHostMsg::UnlockFailed { reason });
    let modes = vec![
        ScreenMode { width: 2560, height: 1440, pixel_width: 2560, pixel_height: 1440 },
        ScreenMode { width: 1920, height: 1080, pixel_width: 1920, pixel_height: 1080 },
    ];
    let changed = ScreenDisplay { modes, changed: true, ..display(2, 1920, 1080, 1920, 1080) };
    matches("displays.json", &ScreenHostMsg::Displays { displays: vec![changed] });
    matches("mode_failed.json", &ScreenHostMsg::ModeFailed { reason: "the display refused the mode".into() });
    matches(
        "datagram_accept.json",
        &ScreenHostMsg::Datagram { port: 61000, addrs: vec!["10.0.0.20".parse().unwrap()] },
    );
    matches("datagram_lost.json", &ScreenHostMsg::DatagramLost);
}

/// 更新的客户端多列的编码跳过，认得的照收。
#[test]
fn unknown_codecs_are_skipped() {
    let text = r#"{"type":"start","width":8,"height":8,"fps":60,"bitrate":1,"codecs":["av1","h264"]}"#;
    let start: ScreenClientMsg = serde_json::from_str(text).unwrap();
    assert!(matches!(start, ScreenClientMsg::Start { codecs, .. } if codecs == [Codec::H264]));
}

#[test]
fn video_payloads_match_the_fixtures() {
    let keyframe = VideoFrame {
        codec: Codec::Hevc,
        data: vec![0, 0, 0, 2, 0x26, 1],
        keyframe: true,
        parameter_sets: vec![vec![0x40, 1], vec![0x42], vec![0x44, 1, 2]],
        pts_us: -5,
    };
    let delta = VideoFrame {
        codec: Codec::Hevc,
        data: vec![0, 0, 0, 2, 2, 0x0a],
        keyframe: false,
        parameter_sets: Vec::new(),
        pts_us: 1000,
    };
    let h264 = VideoFrame {
        codec: Codec::H264,
        data: vec![0, 0, 0, 2, 0x65, 0x88],
        keyframe: true,
        parameter_sets: vec![vec![0x67, 0x64], vec![0x68, 0xee]],
        pts_us: 7,
    };
    for (name, frame) in [("video_keyframe.hex", keyframe), ("video_delta.hex", delta), ("video_h264.hex", h264)] {
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
