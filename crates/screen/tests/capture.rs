//! 真采一块屏幕、真编 HEVC 和 H.264：macOS 上要屏幕录制的授权，Windows 上要在登录着的桌面上跑（没有能用的显卡时
//! 走 CPU 和软件编码器），Linux 上要有图形会话（`Xvfb` 起的也行，设好 `DISPLAY`），CI 上都没有，所以 `#[ignore]`，
//! 在桌面的终端里用 `cargo test -p runode-screen --test capture -- --ignored --test-threads=1` 跑：Windows 上一个
//! 进程不能同时复制同一块屏幕两次。

use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_screen::{Capture, Codec, Display, Event, Options};

/// 锁定第一帧是带三个参数集（VPS、SPS、PPS）的关键帧，手机才能从头解；要了关键帧，屏幕静止时也很快
/// 来一个。Linux 上没有显卡时编不了 HEVC，不测。
#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "needs the screen recording permission"]
fn first_frame_and_requested_frames_are_keyframes() {
    keyframes(Codec::Hevc, 3);
}

/// H.264 的关键帧带 SPS、PPS 两个参数集。
#[test]
#[ignore = "needs the screen recording permission"]
fn h264_keyframes_carry_their_parameter_sets() {
    keyframes(Codec::H264, 2);
}

fn keyframes(codec: Codec, parameter_sets: usize) {
    let display = Display::main();
    let options = Options {
        display: display.id,
        width: 1280,
        height: 720,
        fps: 60,
        bitrate: 8_000_000,
        cursor: true,
        codecs: vec![codec],
    };
    let (tx, rx) = mpsc::channel();
    let capture = Capture::start(&options, move |event| {
        let _ = tx.send(event);
    })
    .expect("start capturing");

    assert_eq!(capture.codec(), codec);
    let first = next_frame(&rx);
    assert!(first.keyframe);
    assert_eq!(first.parameter_sets.len(), parameter_sets);

    // 要之前已经在编的帧可能先到，跳过。
    capture.request_keyframe();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let frame = next_frame(&rx);
        if frame.keyframe {
            assert_eq!(frame.parameter_sets.len(), parameter_sets);
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
