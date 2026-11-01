//! OpenH264 的软件编码器，给没有能用的显卡时用：按屏幕内容的实时模式配，按码率控制。

use std::io;

use openh264::{
    OpenH264API, Timestamp,
    encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Profile, RateControlMode, UsageType, VuiConfig,
    },
    formats::YUVSource,
};

use super::encoder::{VideoEncoder, video_frame};
use crate::{BURST_HEADROOM, Codec, VideoFrame};

/// OpenH264 编得了的最大画面：横着的长边、短边，竖着的反过来。
const MAX_SIZE: (u32, u32) = (3840, 2160);
/// 交给码率控制的相邻两帧最多隔这么久（毫秒）：OpenH264 把隔了一秒以上的当成只隔一帧，久不动之后码率缓冲就
/// 放不空，接下来零星的改动被当成超码率跳过。
const MAX_RC_GAP_MS: u64 = 999;

/// `ENCODER_OPTION_BITRATE` 和 `ENCODER_OPTION_MAX_BITRATE`。
const OPTION_BITRATE: i32 = 5;
const OPTION_MAX_BITRATE: i32 = 6;
/// `SPATIAL_LAYER_ALL`。
const ALL_LAYERS: u32 = 4;

/// `SBitrateInfo`。
#[repr(C)]
struct BitrateInfo {
    layer: u32,
    bitrate: i32,
}

pub(crate) struct Software {
    encoder: Encoder,
    /// 从 NV12 拆出来的 I420，OpenH264 只收这种。
    i420: I420,
    bitrate: u32,
    /// 还没交给编码器的码率：编码器第一次编帧时才按尺寸建好，之前设的选项不算数。
    pending_rate: bool,
    parameter_sets: Vec<Vec<u8>>,
    /// 交给码率控制的时钟（毫秒）和上一帧的 `pts_us`：只在出帧时编，帧间隔不均匀，按真实时间放码率缓冲。
    rc_clock: u64,
    last_pts_us: Option<i64>,
}

impl Software {
    /// 画面 `size` 等比缩到 OpenH264 编得了的大小以内，宽高取偶数；本来就编得了时原样。
    pub(crate) fn fit(size: (u32, u32)) -> (u32, u32) {
        let (long, short) = (size.0.max(size.1), size.0.min(size.1));
        let scale = (f64::from(MAX_SIZE.0) / f64::from(long)).min(f64::from(MAX_SIZE.1) / f64::from(short));
        if scale >= 1.0 {
            return size;
        }
        let even = |len: u32| ((f64::from(len) * scale) as u32 & !1).max(2);
        (even(size.0), even(size.1))
    }

    /// `size` 要在 `fit` 给的大小以内。
    pub(crate) fn open(size: (u32, u32), fps: u32, bitrate: u32) -> io::Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Timestamp)
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps.max(1) as f32))
            // 不许跳帧时 OpenH264 管不住码率；跳过的帧不出码流，关键帧被跳过时采集再要一次。
            .skip_frames(true)
            .adaptive_quantization(false)
            .background_detection(false)
            .intra_frame_period(IntraFramePeriod::auto())
            .profile(Profile::Baseline)
            .vui(VuiConfig::bt709());
        let encoder = Encoder::with_api_config(OpenH264API::from_source(), config).map_err(other)?;
        Ok(Self {
            encoder,
            i420: I420::new(size),
            bitrate,
            pending_rate: true,
            parameter_sets: Vec::new(),
            rc_clock: 0,
            last_pts_us: None,
        })
    }

    fn apply_rate(&mut self) {
        let max = (f64::from(self.bitrate) * BURST_HEADROOM) as i32;
        for (option, bitrate) in [(OPTION_MAX_BITRATE, max), (OPTION_BITRATE, self.bitrate as i32)] {
            let mut info = BitrateInfo { layer: ALL_LAYERS, bitrate };
            // SAFETY: 两个选项都收 `SBitrateInfo`，`info` 在调用期间有效。
            let status = unsafe { self.encoder.raw_api().set_option(option, (&raw mut info).cast()) };
            if status != 0 {
                tracing::debug!("OpenH264 refused bitrate option {option}: {status}");
            }
        }
    }
}

impl VideoEncoder for Software {
    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn encode(&mut self, nv12: &[u8], keyframe: bool, pts_us: i64) -> io::Result<Option<VideoFrame>> {
        self.i420.fill(nv12);
        if keyframe {
            self.encoder.force_intra_frame();
        }
        let first = std::mem::take(&mut self.pending_rate);
        // 时间戳要一帧比一帧大，不然 OpenH264 当成刚过了一帧。
        let gap = self.last_pts_us.map_or(1, |last| u64::try_from((pts_us - last) / 1000).unwrap_or(0));
        self.last_pts_us = Some(pts_us);
        self.rc_clock += gap.clamp(1, MAX_RC_GAP_MS);
        let bytes = self.encoder.encode_at(&self.i420, Timestamp::from_millis(self.rc_clock)).map_err(other)?.to_vec();
        if first {
            self.apply_rate();
        }
        Ok(video_frame(Codec::H264, &bytes, &mut self.parameter_sets, pts_us))
    }

    fn set_bitrate(&mut self, bitrate: u32) -> io::Result<()> {
        self.bitrate = bitrate;
        if !self.pending_rate {
            self.apply_rate();
        }
        Ok(())
    }
}

/// 一帧 I420：亮度一个平面，两个色度平面横竖各减半。
struct I420 {
    size: (u32, u32),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl I420 {
    fn new((width, height): (u32, u32)) -> Self {
        let luma = width as usize * height as usize;
        Self { size: (width, height), y: vec![0; luma], u: vec![128; luma / 4], v: vec![128; luma / 4] }
    }

    /// 从同样大小的 NV12 拆出来。
    fn fill(&mut self, nv12: &[u8]) {
        let luma = self.y.len();
        self.y.copy_from_slice(&nv12[..luma]);
        for (i, pair) in nv12[luma..luma + luma / 2].as_chunks::<2>().0.iter().enumerate() {
            self.u[i] = pair[0];
            self.v[i] = pair[1];
        }
    }
}

impl YUVSource for I420 {
    fn dimensions(&self) -> (usize, usize) {
        (self.size.0 as usize, self.size.1 as usize)
    }

    fn strides(&self) -> (usize, usize, usize) {
        let width = self.size.0 as usize;
        (width, width / 2, width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.y
    }

    fn u(&self) -> &[u8] {
        &self.u
    }

    fn v(&self) -> &[u8] {
        &self.v
    }
}

fn other(err: openh264::Error) -> io::Error {
    io::Error::other(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_large_screens_into_what_openh264_encodes() {
        assert_eq!(Software::fit((3840, 2160)), (3840, 2160));
        assert_eq!(Software::fit((2160, 3840)), (2160, 3840));
        assert_eq!(Software::fit((5120, 1440)), (3840, 1080));
        assert_eq!(Software::fit((5120, 2880)), (3840, 2160));
        assert_eq!(Software::fit((1440, 5120)), (1080, 3840));
    }

    #[test]
    fn encodes_a_fitted_ultrawide_screen() {
        let size = Software::fit((5120, 1440));
        let mut encoder = Software::open(size, 30, 8_000_000).unwrap();
        let nv12 = vec![16; (size.0 * size.1 * 3 / 2) as usize];
        assert!(encoder.encode(&nv12, true, 0).unwrap().is_some());
    }

    /// 一张很费码率的关键帧之后，隔一阵才有的小改动照样编出来，不被当成超码率跳过。
    #[test]
    fn sparse_changes_after_a_large_frame_are_not_skipped() {
        let size = (1280, 720);
        let mut encoder = Software::open(size, 30, 1_000_000).unwrap();
        let luma = (size.0 * size.1) as usize;
        // 左上四分之一是噪点、其余是平的，关键帧要一秒的码率好几倍。
        let mut seed = 1u32;
        let mut nv12: Vec<u8> = (0..luma * 3 / 2)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let (x, y) = (i % size.0 as usize, i / size.0 as usize);
                if i < luma && x < 640 && y < 360 { (seed >> 16) as u8 } else { 100 }
            })
            .collect();
        assert!(encoder.encode(&nv12, true, 0).unwrap().is_some());
        for second in 1..=5 {
            nv12[400 * 1280 + second * 5000] ^= 0xff;
            let frame = encoder.encode(&nv12, false, second as i64 * 1_000_000).unwrap();
            assert!(frame.is_some(), "the change after {second}s was skipped");
        }
    }
}
