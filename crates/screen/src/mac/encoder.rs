//! VideoToolbox 的硬件编码，按远程桌面的需要配成低延迟：不重排帧（没有 B 帧），不定期插关键帧，只在
//! `request_keyframe` 时出一帧，单帧的大小有上限（`BURST_WINDOW`）。编码按观看端给的先后挑：先找能开低延迟码率
//! 控制的，都开不了时再用普通的码率控制，因为普通的码率控制要攒几帧才出，延迟比换成 H.264 的画质损失更明显。
//! 编出的帧经 `Sink` 交出去，在 VideoToolbox 的线程上。

use std::{
    ffi::c_void,
    io,
    ptr::{self, NonNull},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoCodecType, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_DataRateLimits,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_ConstrainedHigh_AutoLevel, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use crate::{BURST_HEADROOM, BURST_WINDOW, Codec, Event, VideoFrame, nal};

pub(crate) type Sink = dyn Fn(Event) + Send + Sync;

/// 低延迟码率控制下交给编码器、还没编出来的帧最多这么多：有的机器交得多就在编码器里排得久，攒到几百毫秒。普通码率
/// 控制要攒几帧才出，不限，限了就一直等不到。
const MAX_IN_FLIGHT: u32 = 1;
/// 等编码器交出上一帧最多这么久，等不到也接着交，不至于卡死。
const IN_FLIGHT_WAIT: Duration = Duration::from_millis(250);

pub(crate) struct Encoder {
    session: CFRetained<VTCompressionSession>,
    /// 观看端要的平均码率和放没放大；改它和交给编码器在同一把锁里，两个线程同时改时编码器不会停在过时的码率上。
    rate: Mutex<Rate>,
    /// 编码回调的 refcon 指着它，所以装箱、比会话活得久（`Drop` 里先关会话）。
    shared: Box<Shared>,
}

/// 观看端要的平均码率（bit/s）和是不是按 `REFINE_BOOST` 倍放大着。
struct Rate {
    base: u32,
    boosted: bool,
}

/// 编码回调也要用的。
struct Shared {
    sink: Arc<Sink>,
    codec: Codec,
    /// 下一帧编成关键帧；硬要的那帧被编码器丢掉或编坏时回调再置上，免得要的关键帧一直不来。
    force_keyframe: AtomicBool,
    /// 交给编码器、还没经回调交回来的帧数，和最多能有几帧。
    in_flight: Mutex<u32>,
    max_in_flight: u32,
    returned: Condvar,
}

// SAFETY: VideoToolbox 的编码会话可以在任意线程上调用；`Shared` 本身是 Send + Sync。
unsafe impl Send for Encoder {}
// SAFETY: 同上。
unsafe impl Sync for Encoder {}

impl Encoder {
    /// 按 `codecs` 的先后挑编码建编码器，见模块的说明。
    pub(crate) fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        codecs: &[Codec],
        sink: Arc<Sink>,
    ) -> io::Result<Self> {
        let mut failures = Vec::new();
        for low_latency in [true, false] {
            for &codec in codecs {
                // 回调的 refcon 指着 `Shared`，编码确定了才建得出；建不成的会话没交出过帧，换一份没关系。
                let shared = Box::new(Shared {
                    sink: sink.clone(),
                    codec,
                    force_keyframe: AtomicBool::new(false),
                    in_flight: Mutex::new(0),
                    max_in_flight: if low_latency { MAX_IN_FLIGHT } else { u32::MAX },
                    returned: Condvar::new(),
                });
                let refcon = ptr::from_ref::<Shared>(&shared).cast_mut().cast::<c_void>();
                let session = match create(codec, width, height, refcon, low_latency) {
                    Ok(session) => session,
                    Err(status) => {
                        failures.push(format!("{codec:?} (low latency: {low_latency}, {status})"));
                        continue;
                    }
                };
                let encoder = Self { session, rate: Mutex::new(Rate { base: bitrate, boosted: false }), shared };
                // 建得出会话、配不上参数的也换下一种。
                if let Err(err) = encoder.configure(fps) {
                    failures.push(format!("{codec:?} (low latency: {low_latency}, {err})"));
                    continue;
                }
                if !failures.is_empty() {
                    tracing::info!(
                        "encoding the screen as {codec:?} (low latency: {low_latency}); unavailable: {}",
                        failures.join(", ")
                    );
                }
                return Ok(encoder);
            }
        }
        Err(io::Error::other(format!("cannot create a hardware encoder: {}", failures.join(", "))))
    }

    fn configure(&self, fps: u32) -> io::Result<()> {
        // SAFETY: 读 VideoToolbox 导出的常量。
        unsafe {
            self.set(kVTCompressionPropertyKey_RealTime, CFBoolean::new(true))?;
            self.set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false))?;
            let profiles: &[&CFString] = match self.shared.codec {
                Codec::Hevc => &[kVTProfileLevel_HEVC_Main_AutoLevel],
                // 低延迟码率控制只认受限的 High，普通码率控制时退到 High。
                Codec::H264 => &[kVTProfileLevel_H264_ConstrainedHigh_AutoLevel, kVTProfileLevel_H264_High_AutoLevel],
            };
            if !profiles.iter().any(|profile| self.set(kVTCompressionPropertyKey_ProfileLevel, profile).is_ok()) {
                tracing::debug!("the encoder takes none of its profiles, using its default");
            }
            self.set(kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(fps as i32))?;
        }
        self.apply_bitrate(&self.rate.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// 编码器选中的编码。
    pub(crate) fn codec(&self) -> Codec {
        self.shared.codec
    }

    fn set(&self, key: &CFString, value: &CFType) -> io::Result<()> {
        // SAFETY: 键是 VideoToolbox 的编码属性，值是它要的类型。
        let status = unsafe { VTSessionSetProperty(&self.session, key, Some(value)) };
        if status == 0 { Ok(()) } else { Err(io::Error::other(format!("cannot set {key} ({status})"))) }
    }

    /// 平均码率，单位 bit/s；编码中途可以改。
    pub(crate) fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        let mut rate = self.rate.lock().unwrap_or_else(PoisonError::into_inner);
        rate.base = bitrate;
        self.apply_bitrate(&rate)
    }

    /// 按 `REFINE_BOOST` 倍的码率编，画面静止时把最后一张补清楚（`Refiner`）；`false` 时回到原来的码率。
    pub(crate) fn boost(&self, on: bool) {
        let mut rate = self.rate.lock().unwrap_or_else(PoisonError::into_inner);
        if std::mem::replace(&mut rate.boosted, on) != on
            && let Err(err) = self.apply_bitrate(&rate)
        {
            tracing::debug!("cannot change the bitrate for refining: {err}");
        }
    }

    /// 把码率和单帧的上限交给编码器。
    fn apply_bitrate(&self, rate: &Rate) -> io::Result<()> {
        let bitrate = crate::boosted(rate.base, rate.boosted);
        let average = CFNumber::new_i32(bitrate.min(f64::from(i32::MAX)) as i32);
        // SAFETY: 读 VideoToolbox 导出的常量。
        self.set(unsafe { kVTCompressionPropertyKey_AverageBitRate }, &average)?;
        let window = BURST_WINDOW.as_secs_f64();
        let bytes = CFNumber::new_f64(bitrate / 8. * window * BURST_HEADROOM);
        let seconds = CFNumber::new_f64(window);
        let limits = CFArray::<CFNumber>::from_objects(&[&bytes, &seconds]);
        // 不是每种编码器都认单帧的上限，认不了的照平均码率编。
        // SAFETY: 读 VideoToolbox 导出的常量；值是字节数和秒数交替的数组。
        if let Err(err) = self.set(unsafe { kVTCompressionPropertyKey_DataRateLimits }, limits.as_opaque()) {
            tracing::debug!("the encoder takes no data rate limit: {err}");
        }
        Ok(())
    }

    /// 下一帧编成关键帧，并带上参数集。
    pub(crate) fn request_keyframe(&self) {
        self.shared.force_keyframe.store(true, Ordering::Relaxed);
    }

    /// 要了的关键帧还没交给编码器。
    pub(crate) fn keyframe_requested(&self) -> bool {
        self.shared.force_keyframe.load(Ordering::Relaxed)
    }

    /// 交一帧给编码器。编码器手里的帧到了上限（`MAX_IN_FLIGHT`）时先等它交出一帧，所以要在不怕阻塞的线程上调。
    pub(crate) fn encode(&self, image: &CVImageBuffer, pts: CMTime) {
        {
            let in_flight = self.shared.in_flight.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut in_flight, waited) = self
                .shared
                .returned
                .wait_timeout_while(in_flight, IN_FLIGHT_WAIT, |count| *count >= self.shared.max_in_flight)
                .unwrap_or_else(PoisonError::into_inner);
            // 等不到就当编码器把手里的帧弄丢了、不会再交回来，从这一帧重新数，不让少掉的回调把以后每帧都卡住。
            *in_flight = if waited.timed_out() { 1 } else { *in_flight + 1 };
        }
        let force = self.shared.force_keyframe.swap(false, Ordering::Relaxed);
        let options = force.then(|| {
            CFDictionary::<CFString, CFBoolean>::from_slices(
                // SAFETY: 读 VideoToolbox 导出的常量。
                &[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }],
                &[CFBoolean::new(true)],
            )
        });
        // 回调凭非空的 sourceFrameRefcon 认出硬要的关键帧，它只当标记、不解引用。
        let marker = if force { NonNull::<c_void>::dangling().as_ptr() } else { ptr::null_mut() };
        // SAFETY: 图像和选项在调用期间有效；编出的帧经建会话时给的回调交出。
        let status = unsafe {
            self.session.encode_frame(
                image,
                pts,
                kCMTimeInvalid,
                options.as_deref().map(AsRef::as_ref),
                marker,
                ptr::null_mut(),
            )
        };
        if status != 0 {
            tracing::warn!("cannot encode a screen frame ({status})");
            self.shared.returned_one();
            if force {
                self.shared.force_keyframe.store(true, Ordering::Relaxed);
            }
        }
    }

    /// 等交给编码器的帧都编完、经回调交出去再返回。
    pub(crate) fn complete(&self) {
        // SAFETY: 会话活着；时间戳无效表示全部。
        unsafe { self.session.complete_frames(kCMTimeInvalid) };
    }
}

impl Shared {
    /// 编码器交回了一帧（编好、丢掉或编坏）。
    fn returned_one(&self) {
        let mut in_flight = self.in_flight.lock().unwrap_or_else(PoisonError::into_inner);
        *in_flight = in_flight.saturating_sub(1);
        self.returned.notify_all();
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.complete();
        // SAFETY: 还在编的帧都交出去了，关会话以后回调不会再用到 `shared`。
        unsafe { self.session.invalidate() };
    }
}

fn create(
    codec: Codec,
    width: u32,
    height: u32,
    refcon: *mut c_void,
    low_latency: bool,
) -> Result<CFRetained<VTCompressionSession>, i32> {
    let codec_type: CMVideoCodecType = match codec {
        Codec::Hevc => kCMVideoCodecType_HEVC,
        Codec::H264 => kCMVideoCodecType_H264,
    };
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
            codec_type,
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
    frame: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: refcon 是 `Encoder::new` 给的，指着 `Encoder::shared`，会话关掉之前一直有效。
    let shared = unsafe { &*refcon.cast_const().cast::<Shared>() };
    shared.returned_one();
    if status != 0 || flags.contains(VTEncodeInfoFlags::FrameDropped) {
        if status != 0 {
            tracing::warn!("the encoder failed on a frame ({status})");
        }
        if !frame.is_null() {
            shared.force_keyframe.store(true, Ordering::Relaxed);
        }
        return;
    }
    // SAFETY: 回调期间样本有效。
    let Some(sample) = (unsafe { sample.as_ref() }) else { return };
    if let Some(frame) = video_frame(shared.codec, sample) {
        (shared.sink)(Event::Frame(frame));
    }
}

fn video_frame(codec: Codec, sample: &CMSampleBuffer) -> Option<VideoFrame> {
    // SAFETY: 读样本的数据和时间戳，复制到自己的缓冲里。
    unsafe {
        let block = sample.data_buffer()?;
        let mut data = vec![0u8; block.data_length()];
        let status = block.copy_data_bytes(0, data.len(), NonNull::new(data.as_mut_ptr())?.cast());
        if status != 0 {
            return None;
        }
        let keyframe = nal::is_keyframe(codec, &data);
        let parameter_sets = if keyframe { parameter_sets(codec, &*sample.format_description()?)? } else { Vec::new() };
        let pts_us = (sample.presentation_time_stamp().seconds() * 1e6) as i64;
        Some(VideoFrame { codec, data, keyframe, parameter_sets, pts_us })
    }
}

/// 关键帧的参数集，解码器要先拿它们建格式。
fn parameter_sets(codec: Codec, format: &CMFormatDescription) -> Option<Vec<Vec<u8>>> {
    let get = match codec {
        Codec::Hevc => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
        Codec::H264 => CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    };
    let (mut count, mut length_size) = (0usize, 0i32);
    // SAFETY: 只取个数和长度字段的字节数，输出参数指向本地变量。
    let status = unsafe { get(format, 0, ptr::null_mut(), ptr::null_mut(), &mut count, &mut length_size) };
    if status != 0 || length_size as usize != nal::LENGTH_SIZE {
        tracing::warn!("unexpected {codec:?} format (status {status}, length size {length_size})");
        return None;
    }
    (0..count)
        .map(|index| {
            let (mut bytes, mut len) = (ptr::null(), 0usize);
            // SAFETY: 返回的指针指着格式描述里的数据，在 `format` 活着时有效，这里马上复制。
            let status = unsafe { get(format, index, &mut bytes, &mut len, ptr::null_mut(), ptr::null_mut()) };
            (status == 0 && !bytes.is_null()).then(|| unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec())
        })
        .collect()
}
