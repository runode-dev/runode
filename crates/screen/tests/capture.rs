//! 真采一块屏幕、真编 HEVC：要屏幕录制的授权，CI 上没有，所以 `#[ignore]`，在有授权的终端里用
//! `cargo test -p runode-screen -- --ignored` 跑。
#![cfg(target_os = "macos")]

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_screen::{Capture, Display, Event, Options};

/// 锁定第一帧是带三个参数集（VPS、SPS、PPS）的关键帧，手机才能从头解；要关键帧时下一帧就是。
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

    // 屏幕静止时没有新帧，就不会有关键帧；只确认要了以后来的第一帧是关键帧。
    capture.request_keyframe();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Frame(frame)) => {
                assert!(frame.keyframe);
                assert_eq!(frame.parameter_sets.len(), 3);
                break;
            }
            Ok(Event::Stopped(reason)) => panic!("capture stopped: {reason}"),
            Err(_) => {}
        }
    }
}

fn next_frame(rx: &mpsc::Receiver<Event>) -> runode_screen::VideoFrame {
    match rx.recv_timeout(Duration::from_secs(5)).expect("a frame") {
        Event::Frame(frame) => frame,
        Event::Stopped(reason) => panic!("capture stopped: {reason}"),
    }
}
