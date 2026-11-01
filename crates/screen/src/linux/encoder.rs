//! 编码器：先试显卡的硬件编码器（`vaapi`），建不成时用 OpenH264 在 CPU 上编（`software`）。都按远程桌面的需要
//! 配成低延迟：没有 B 帧，不定期插关键帧，只在要的时候出一帧。输入是 `nv12` 换好的整帧 NV12，在采集线程上同步地编。

use std::io;

use crate::{Codec, VideoFrame, nal};

use super::{software::Software, vaapi::Vaapi};

pub(crate) trait VideoEncoder: Send {
    fn codec(&self) -> Codec;

    /// 编一帧 NV12，大小是建编码器时的。`keyframe` 时编成带参数集的关键帧。这次没出帧时为 `None`。
    fn encode(&mut self, nv12: &[u8], keyframe: bool, pts_us: i64) -> io::Result<Option<VideoFrame>>;

    /// 改平均码率，单位 bit/s，下一帧起生效。
    fn set_bitrate(&mut self, bitrate: u32) -> io::Result<()>;
}

/// 建编码器，连同它编的画面大小：`codecs` 里观看端想要的排前面，显卡编得了的第一种；显卡编不了时观看端解得了
/// H.264 就用软件的，画面太大时等比缩小（`Software::fit`）。都不行时错误是 `io::ErrorKind::Unsupported`。
pub(crate) fn open(
    size: (u32, u32),
    fps: u32,
    bitrate: u32,
    codecs: &[Codec],
) -> io::Result<(Box<dyn VideoEncoder>, (u32, u32))> {
    match Vaapi::open(size, fps, bitrate, codecs) {
        Ok(encoder) => return Ok((Box::new(encoder), size)),
        Err(err) => tracing::info!("no VA-API encoder, encoding on the CPU: {err}"),
    }
    if !codecs.contains(&Codec::H264) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the viewer cannot decode H.264, the only codec without a GPU",
        ));
    }
    let fitted = Software::fit(size);
    if fitted != size {
        tracing::info!("OpenH264 cannot encode {size:?}, encoding {fitted:?}");
    }
    Ok((Box::new(Software::open(fitted, fps, bitrate)?), fitted))
}

/// 一帧 Annex B 码流换成 `VideoFrame`。关键帧里没带参数集时补上 `known` 里记着的，带了就记下来；关键帧怎么也
/// 拿不到参数集时为 `None`，调用方要再要一帧关键帧。
pub(crate) fn video_frame(codec: Codec, annex_b: &[u8], known: &mut Vec<Vec<u8>>, pts_us: i64) -> Option<VideoFrame> {
    let (data, mut parameter_sets) = nal::from_annex_b(codec, annex_b);
    if data.is_empty() {
        return None;
    }
    let keyframe = nal::is_keyframe(codec, &data);
    if !parameter_sets.is_empty() {
        known.clone_from(&parameter_sets);
    } else if keyframe {
        parameter_sets.clone_from(known);
    }
    if keyframe && parameter_sets.is_empty() {
        tracing::warn!("the encoder gave a keyframe without parameter sets");
        return None;
    }
    if !keyframe {
        parameter_sets.clear();
    }
    Some(VideoFrame { codec, data, keyframe, parameter_sets, pts_us })
}
