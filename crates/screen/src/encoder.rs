//! VideoToolbox 的硬件 HEVC 编码，按远程桌面的需要配成低延迟：不重排帧（没有 B 帧），不定期插
//! 关键帧，只在 `request_keyframe` 时出一帧。编出的帧经 `Sink` 交出去，在 VideoToolbox 的线程上。

use std::{
    ffi::c_void,
    io,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMTimeInvalid,
    kCMVideoCodecType_HEVC,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use crate::{Event, VideoFrame, nal};

pub(crate) type Sink = dyn Fn(Event) + Send + Sync;

pub(crate) struct Encoder {
    session: CFRetained<VTCompressionSession>,
    /// 编码回调的 refcon 指着它，所以装箱、比会话活得久（`Drop` 里先关会话）。
    _sink: Box<Arc<Sink>>,
    force_keyframe: AtomicBool,
}

// SAFETY: VideoToolbox 的编码会话可以在任意线程上调用；`sink` 本身是 Send + Sync。
unsafe impl Send for Encoder {}
// SAFETY: 同上。
unsafe impl Sync for Encoder {}

impl Encoder {
    pub(crate) fn new(width: u32, height: u32, fps: u32, bitrate: u32, sink: Arc<Sink>) -> io::Result<Self> {
        let sink = Box::new(sink);
        let refcon = ptr::from_ref::<Arc<Sink>>(&sink).cast_mut().cast::<c_void>();
        // 低延迟码率控制不是每种编码器都支持，建不了就退回普通的码率控制，仍然要硬件编码。
        let session = create(width, height, refcon, true).or_else(|status| {
            tracing::warn!("low-latency HEVC encoding is unavailable ({status}), using the normal rate control");
            create(width, height, refcon, false)
        });
        let session =
            session.map_err(|status| io::Error::other(format!("cannot create the HEVC encoder ({status})")))?;
        let encoder = Self { session, _sink: sink, force_keyframe: AtomicBool::new(false) };
        // SAFETY: 读 VideoToolbox 导出的常量。
        unsafe {
            encoder.set(kVTCompressionPropertyKey_RealTime, CFBoolean::new(true))?;
            encoder.set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false))?;
            encoder.set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel)?;
            encoder.set(kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(fps as i32))?;
        }
        encoder.set_bitrate(bitrate)?;
        Ok(encoder)
    }

    fn set(&self, key: &CFString, value: &CFType) -> io::Result<()> {
        // SAFETY: 键是 VideoToolbox 的编码属性，值是它要的类型。
        let status = unsafe { VTSessionSetProperty(&self.session, key, Some(value)) };
        if status == 0 { Ok(()) } else { Err(io::Error::other(format!("cannot set {key} ({status})"))) }
    }

    /// 平均码率，单位 bit/s；编码中途可以改。
    pub(crate) fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        let bitrate = CFNumber::new_i32(bitrate.min(i32::MAX as u32) as i32);
        // SAFETY: 读 VideoToolbox 导出的常量。
        self.set(unsafe { kVTCompressionPropertyKey_AverageBitRate }, &bitrate)
    }

    /// 下一帧编成关键帧，并带上参数集。
    pub(crate) fn request_keyframe(&self) {
        self.force_keyframe.store(true, Ordering::Relaxed);
    }

    /// 要了的关键帧还没交给编码器。
    pub(crate) fn keyframe_requested(&self) -> bool {
        self.force_keyframe.load(Ordering::Relaxed)
    }

    pub(crate) fn encode(&self, image: &CVImageBuffer, pts: CMTime) {
        let force = self.force_keyframe.swap(false, Ordering::Relaxed);
        let options = force.then(|| {
            CFDictionary::<CFString, CFBoolean>::from_slices(
                // SAFETY: 读 VideoToolbox 导出的常量。
                &[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }],
                &[CFBoolean::new(true)],
            )
        });
        // SAFETY: 图像和选项在调用期间有效；编出的帧经建会话时给的回调交出。
        let status = unsafe {
            self.session.encode_frame(
                image,
                pts,
                kCMTimeInvalid,
                options.as_deref().map(AsRef::as_ref),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 {
            tracing::warn!("cannot encode a screen frame ({status})");
            if force {
                self.force_keyframe.store(true, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: 等还在编的帧交出去再关会话，之后回调不会再用到 `sink`。
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}

fn create(
    width: u32,
    height: u32,
    refcon: *mut c_void,
    low_latency: bool,
) -> Result<CFRetained<VTCompressionSession>, i32> {
    // SAFETY: 读 VideoToolbox 导出的常量。
    let spec_keys = unsafe {
        [
            kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
        ]
    };
    let keys = if low_latency { &spec_keys[..] } else { &spec_keys[..1] };
    let values = vec![CFBoolean::new(true); keys.len()];
    let spec = CFDictionary::<CFString, CFBoolean>::from_slices(keys, &values);
    let mut session = ptr::null_mut();
    // SAFETY: 输出参数指向本地变量；refcon 指着的 `Arc<Sink>` 由调用方保证比会话活得久。
    let status = unsafe {
        VTCompressionSession::create(
            None,
            width as i32,
            height as i32,
            kCMVideoCodecType_HEVC,
            Some(spec.as_ref()),
            None,
            None,
            Some(on_encoded),
            refcon,
            NonNull::from(&mut session),
        )
    };
    match NonNull::new(session) {
        // SAFETY: Create 规则，返回的会话归调用方。
        Some(session) if status == 0 => Ok(unsafe { CFRetained::from_raw(session) }),
        _ => Err(status),
    }
}

unsafe extern "C-unwind" fn on_encoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: refcon 是 `Encoder::new` 给的，指着 `Encoder::_sink`，会话关掉之前一直有效。
    let sink = unsafe { &*refcon.cast_const().cast::<Arc<Sink>>() };
    if status != 0 {
        tracing::warn!("the HEVC encoder failed on a frame ({status})");
        return;
    }
    if flags.contains(VTEncodeInfoFlags::FrameDropped) {
        return;
    }
    // SAFETY: 回调期间样本有效。
    let Some(sample) = (unsafe { sample.as_ref() }) else { return };
    if let Some(frame) = frame(sample) {
        sink(Event::Frame(frame));
    }
}

fn frame(sample: &CMSampleBuffer) -> Option<VideoFrame> {
    // SAFETY: 读样本的数据和时间戳，复制到自己的缓冲里。
    unsafe {
        let block = sample.data_buffer()?;
        let mut data = vec![0u8; block.data_length()];
        let status = block.copy_data_bytes(0, data.len(), NonNull::new(data.as_mut_ptr())?.cast());
        if status != 0 {
            return None;
        }
        let keyframe = nal::is_keyframe(&data);
        let parameter_sets = if keyframe { parameter_sets(&*sample.format_description()?)? } else { Vec::new() };
        let pts_us = (sample.presentation_time_stamp().seconds() * 1e6) as i64;
        Some(VideoFrame { data, keyframe, parameter_sets, pts_us })
    }
}

/// 关键帧的 VPS、SPS、PPS，解码器要先拿它们建格式。
fn parameter_sets(format: &CMFormatDescription) -> Option<Vec<Vec<u8>>> {
    let (mut count, mut length_size) = (0usize, 0i32);
    // SAFETY: 只取个数和长度字段的字节数，输出参数指向本地变量。
    let status = unsafe {
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            format,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut length_size,
        )
    };
    if status != 0 || length_size as usize != nal::LENGTH_SIZE {
        tracing::warn!("unexpected HEVC format (status {status}, length size {length_size})");
        return None;
    }
    (0..count)
        .map(|index| {
            let (mut bytes, mut len) = (ptr::null(), 0usize);
            // SAFETY: 返回的指针指着格式描述里的数据，在 `format` 活着时有效，这里马上复制。
            let status = unsafe {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &mut bytes,
                    &mut len,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            (status == 0 && !bytes.is_null()).then(|| unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec())
        })
        .collect()
}
