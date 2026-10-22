//! 真采一块屏幕、真编 HEVC：要屏幕录制的授权，CI 上没有，所以 `#[ignore]`，在有授权的终端里用
//! `cargo test -p runode-screen -- --ignored` 跑。
#![cfg(target_os = "macos")]

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_screen::{Capture, Display, Event, Options};

/// 锁定第一帧是带三个参数集（VPS、SPS、PPS）的关键帧，手机才能从头解；要了关键帧，屏幕静止时也很快
/// 来一个。
#[test]
#[ignore = "needs the screen recording permission"]
fn first_frame_and_requested_frames_are_keyframes() {
    let display = Display::main();
    let options = Options { display: display.id, width: 1280, height: 720, fps: 60, bitrate: 8_000_000 };
    let (tx, rx) = mpsc::channel();
    let capture = Capture::start(&options, move |event| {
        let _ = tx.send(event);
    })
    .expect("start capturing");

    let first = next_frame(&rx);
    assert!(first.keyframe);
    assert_eq!(first.parameter_sets.len(), 3);

    // 要之前已经在编的帧可能先到，跳过。
    capture.request_keyframe();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let frame = next_frame(&rx);
        if frame.keyframe {
            assert_eq!(frame.parameter_sets.len(), 3);
            break;
        }
        assert!(Instant::now() < deadline, "no keyframe after asking for one");
    }
}

fn next_frame(rx: &mpsc::Receiver<Event>) -> runode_screen::VideoFrame {
    match rx.recv_timeout(Duration::from_secs(5)).expect("a frame") {
        Event::Frame(frame) => frame,
        Event::Stopped(reason) => panic!("capture stopped: {reason}"),
    }
}
