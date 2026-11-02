//! 解码固定的一段 HEVC（`fixtures/red-blue.hevc`，macOS 的 VideoToolbox 编的 320x240 画面，左半纯红、右半
//! 纯蓝，四帧，第一帧是带参数集的关键帧）。Windows 上要装了「HEVC 视频扩展」才有解码器，CI 上没有，所以那边
//! `#[ignore]`，在装了的机器上用 `cargo test -p runode-screen --test decode -- --ignored` 跑。
#![cfg(any(target_os = "macos", windows))]

use runode_screen::{Codec, Decoder, VideoFrame};

/// 测试数据的写法：每帧 `u8 关键帧 | u8 参数集个数 | 每个参数集 u32 LE 长度 + 字节 | u32 LE 长度 + 帧数据`。
fn frames() -> Vec<VideoFrame> {
    fn take(data: &mut &[u8], len: usize) -> Vec<u8> {
        let (head, rest) = data.split_at(len);
        *data = rest;
        head.to_vec()
    }
    fn length(data: &mut &[u8]) -> usize {
        u32::from_le_bytes(take(data, 4).try_into().unwrap()) as usize
    }
    let mut data: &[u8] = include_bytes!("fixtures/red-blue.hevc");
    let mut frames = Vec::new();
    while !data.is_empty() {
        let keyframe = take(&mut data, 1)[0] == 1;
        let count = take(&mut data, 1)[0];
        let parameter_sets = (0..count)
            .map(|_| {
                let len = length(&mut data);
                take(&mut data, len)
            })
            .collect();
        let len = length(&mut data);
        let pts_us = frames.len() as i64 * 33_333;
        frames.push(VideoFrame { codec: Codec::Hevc, data: take(&mut data, len), keyframe, parameter_sets, pts_us });
    }
    frames
}

fn decoder() -> Decoder {
    Decoder::new().expect("an HEVC decoder (HEVC Video Extensions on Windows)")
}

/// 解出来的一帧在 (x, y) 处的颜色，(红, 绿, 蓝)。
#[cfg(target_os = "macos")]
fn color(decoded: &runode_screen::Decoded, x: usize, y: usize) -> (i32, i32, i32) {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    let image = &decoded.image;
    // SAFETY: 只读锁住期间按各平面的行宽读一个像素。
    let (luma, u, v) = unsafe {
        assert_eq!(CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly), 0);
        let luma = *CVPixelBufferGetBaseAddressOfPlane(image, 0)
            .cast::<u8>()
            .add(y * CVPixelBufferGetBytesPerRowOfPlane(image, 0) + x);
        let chroma = CVPixelBufferGetBaseAddressOfPlane(image, 1)
            .cast::<u8>()
            .add((y / 2) * CVPixelBufferGetBytesPerRowOfPlane(image, 1) + (x / 2) * 2);
        let (u, v) = (*chroma, *chroma.add(1));
        CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly);
        (luma, u, v)
    };
    // NV12 全范围按 BT.709 换回 RGB。
    let (c, u, v) = (f64::from(luma), f64::from(u) - 128., f64::from(v) - 128.);
    let r = c + 1.5748 * v;
    let g = c - 0.1873 * u - 0.4681 * v;
    let b = c + 1.8556 * u;
    (r as i32, g as i32, b as i32)
}

#[cfg(windows)]
fn color(decoded: &runode_screen::Decoded, x: usize, y: usize) -> (i32, i32, i32) {
    let at = (y * decoded.width as usize + x) * 4;
    let [b, g, r, _] = decoded.bgra[at..at + 4] else { unreachable!() };
    (i32::from(r), i32::from(g), i32::from(b))
}

/// 锁定每帧都解得出、尺寸对、左红右蓝：颜色的通道顺序和色彩矩阵错了都看得出来。
#[test]
#[cfg_attr(windows, ignore = "needs HEVC Video Extensions")]
fn decodes_the_fixture() {
    let frames = frames();
    assert_eq!(frames.len(), 4);
    assert!(frames[0].keyframe && frames[0].parameter_sets.len() == 3);
    let mut decoder = decoder();
    let mut decoded = 0;
    for frame in &frames {
        let Some(picture) = decoder.decode(frame).expect("decodes") else { continue };
        decoded += 1;
        assert_eq!((picture.width, picture.height), (320, 240));
        let (r, g, b) = color(&picture, 40, 120);
        assert!(r > 200 && g < 60 && b < 60, "left should be red: {r} {g} {b}");
        let (r, g, b) = color(&picture, 280, 120);
        assert!(r < 60 && g < 60 && b > 200, "right should be blue: {r} {g} {b}");
    }
    assert!(decoded >= 3, "only {decoded} frames came out");
}

/// 关键帧以前的帧丢掉，要求要关键帧；交了关键帧就不再要，最晚下一帧出画面（Windows 的解码器晚一帧出）。
#[test]
#[cfg_attr(windows, ignore = "needs HEVC Video Extensions")]
fn waits_for_the_first_keyframe() {
    let frames = frames();
    let mut decoder = decoder();
    assert!(decoder.decode(&frames[1]).unwrap().is_none());
    assert!(decoder.needs_keyframe());
    let first = decoder.decode(&frames[0]).unwrap();
    assert!(!decoder.needs_keyframe());
    assert!(first.is_some() || decoder.decode(&frames[1]).unwrap().is_some());
}
