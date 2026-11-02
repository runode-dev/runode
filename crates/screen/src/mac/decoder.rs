//! VideoToolbox 的 HEVC 和 H.264 解码：把 `VideoFrame` 解成 IOSurface 支持的 `CVPixelBuffer`，像素格式是 NV12 全范围
//! （`kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`），GPUI 的 `surface` 元素只认这一种，能直接交给它画、
//! 不用拷像素。

use std::{
    ffi::c_void,
    io,
    ptr::{self, NonNull},
};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
};

use crate::{Codec, VideoFrame, nal};

/// 解出来的画面，GPUI 的 `surface` 元素直接收它。
pub type PixelBuffer = CFRetained<CVPixelBuffer>;

/// 解出来的一帧。
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub image: PixelBuffer,
}

/// 一路画面的解码器，只在一个线程上用；编码可以中途换，换了以后从关键帧开始解。
///
/// 从关键帧开始解：刚建好、`reset` 之后和解错一帧之后，关键帧以前的帧都丢掉，`decode` 返回 `Ok(None)`、
/// `needs_keyframe` 为真，调用方这时向采集端要关键帧。
pub struct Decoder {
    session: Option<Session>,
    /// 下一帧得是关键帧。
    need_keyframe: bool,
}

struct Session {
    decompression: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    /// 建 `format` 用的编码和参数集；关键帧带来的不一样（采集端换了编码、改了分辨率、切了屏）时重建会话。
    codec: Codec,
    parameter_sets: Vec<Vec<u8>>,
}

// SAFETY: VideoToolbox 的解码会话和格式描述可以在任意线程上用；`Decoder` 只交给一个线程。
unsafe impl Send for Decoder {}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self { session: None, need_keyframe: true }
    }

    /// 丢掉解到一半的状态，从下一个关键帧重新开始。
    pub fn reset(&mut self) {
        self.need_keyframe = true;
    }

    /// 在等关键帧：关键帧之前来的帧都丢掉。
    pub fn needs_keyframe(&self) -> bool {
        self.need_keyframe
    }

    /// 解一帧。还在等关键帧时返回 `Ok(None)`；解错时返回错误，之后同样等关键帧。
    pub fn decode(&mut self, frame: &VideoFrame) -> io::Result<Option<Decoded>> {
        if self.need_keyframe && !frame.keyframe {
            return Ok(None);
        }
        let result = self.decode_frame(frame);
        self.need_keyframe = result.is_err();
        result.map(Some)
    }

    fn decode_frame(&mut self, frame: &VideoFrame) -> io::Result<Decoded> {
        if frame.keyframe && !frame.parameter_sets.is_empty() {
            let current = self.session.as_ref().map(|session| (session.codec, &session.parameter_sets));
            if current != Some((frame.codec, &frame.parameter_sets)) {
                self.session = None;
                self.session = Some(Session::new(frame.codec, frame.parameter_sets.clone())?);
            }
        }
        let session = self
            .session
            .as_ref()
            .filter(|session| session.codec == frame.codec)
            .ok_or_else(|| io::Error::other("no parameter sets for this codec yet"))?;
        let sample = sample(&frame.data, &session.format)?;
        let mut output = Output { status: 0, image: None };
        let mut info = VTDecodeInfoFlags::empty();
        // SAFETY: 不开异步解码，输出回调在返回之前调完，`output` 在这之间一直有效；样本在调用期间有效。
        let status = unsafe {
            session.decompression.decode_frame(
                &sample,
                VTDecodeFrameFlags::empty(),
                ptr::from_mut(&mut output).cast(),
                &mut info,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!("cannot decode a frame ({status})")));
        }
        if output.status != 0 {
            return Err(io::Error::other(format!("the decoder failed on a frame ({})", output.status)));
        }
        let image = output.image.ok_or_else(|| io::Error::other("the decoder dropped a frame"))?;
        let width = CVPixelBufferGetWidth(&image) as u32;
        let height = CVPixelBufferGetHeight(&image) as u32;
        Ok(Decoded { width, height, image })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: 解码是同步的，这时没有在解的帧。
        unsafe { self.decompression.invalidate() };
    }
}

impl Session {
    fn new(codec: Codec, parameter_sets: Vec<Vec<u8>>) -> io::Result<Self> {
        let format = format(codec, &parameter_sets)?;
        // SAFETY: 读 CoreVideo 导出的常量。
        let (pixel_format, io_surface, metal) = unsafe {
            (
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferMetalCompatibilityKey,
            )
        };
        let empty = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let format_type = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange as i32);
        let attributes = CFDictionary::<CFString, CFType>::from_slices(
            &[pixel_format, io_surface, metal],
            &[format_type.as_ref(), empty.as_ref(), CFBoolean::new(true).as_ref()],
        );
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_decoded),
            decompressionOutputRefCon: ptr::null_mut(),
        };
        let mut session = ptr::null_mut();
        // SAFETY: 输出参数指向本地变量；回调记录在调用期间有效，VideoToolbox 会复制它。
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(attributes.as_opaque()),
                &callback,
                NonNull::from(&mut session),
            )
        };
        let decompression = match NonNull::new(session) {
            // SAFETY: Create 规则，返回的会话归调用方。
            Some(session) if status == 0 => unsafe { CFRetained::from_raw(session) },
            _ => return Err(io::Error::other(format!("cannot create the {codec:?} decoder ({status})"))),
        };
        Ok(Self { decompression, format, codec, parameter_sets })
    }
}

/// 一帧解完交回来的东西，经 `sourceFrameRefCon` 指着。
struct Output {
    status: i32,
    image: Option<CFRetained<CVPixelBuffer>>,
}

unsafe extern "C-unwind" fn on_decoded(
    _refcon: *mut c_void,
    frame: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: objc2_core_media::CMTime,
    _duration: objc2_core_media::CMTime,
) {
    // SAFETY: `frame` 是 `decode_frame` 给的 `Output`，同步解码时它在回调期间有效。
    let Some(output) = (unsafe { frame.cast::<Output>().as_mut() }) else { return };
    output.status = status;
    // SAFETY: 回调期间图像有效，这里自己再持有一份。
    output.image = NonNull::new(image).map(|image| unsafe { CFRetained::retain(image) });
}

/// 按参数集（HEVC 的 VPS、SPS、PPS，H.264 的 SPS、PPS）建格式描述。
fn format(codec: Codec, parameter_sets: &[Vec<u8>]) -> io::Result<CFRetained<CMFormatDescription>> {
    let pointers: Vec<NonNull<u8>> = parameter_sets
        .iter()
        .map(|set| NonNull::new(set.as_ptr().cast_mut()))
        .collect::<Option<_>>()
        .ok_or_else(|| io::Error::other("an empty parameter set"))?;
    let sizes: Vec<usize> = parameter_sets.iter().map(Vec::len).collect();
    let pointer_list =
        NonNull::new(pointers.as_ptr().cast_mut()).ok_or_else(|| io::Error::other("no parameter sets"))?;
    let size_list = NonNull::new(sizes.as_ptr().cast_mut()).ok_or_else(|| io::Error::other("no parameter sets"))?;
    let mut format = ptr::null();
    let out = NonNull::from(&mut format);
    let length_size = nal::LENGTH_SIZE as i32;
    // SAFETY: 指针和长度一一对应、在调用期间有效；输出参数指向本地变量。
    let status = unsafe {
        match codec {
            Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                pointers.len(),
                pointer_list,
                size_list,
                length_size,
                None,
                out,
            ),
            Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                pointers.len(),
                pointer_list,
                size_list,
                length_size,
                out,
            ),
        }
    };
    match NonNull::new(format.cast_mut()) {
        // SAFETY: Create 规则，返回的格式描述归调用方。
        Some(format) if status == 0 => Ok(unsafe { CFRetained::from_raw(format) }),
        _ => Err(io::Error::other(format!("bad {codec:?} parameter sets ({status})"))),
    }
}

/// 把一帧的数据复制进 `CMBlockBuffer`，包成解码器要的样本。
fn sample(data: &[u8], format: &CMFormatDescription) -> io::Result<CFRetained<CMSampleBuffer>> {
    let source = NonNull::new(data.as_ptr().cast_mut()).ok_or_else(|| io::Error::other("an empty frame"))?;
    let mut block = ptr::null_mut();
    // SAFETY: 让 CoreMedia 自己分配 `data.len()` 字节；输出参数指向本地变量。
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            ptr::null_mut(),
            data.len(),
            None,
            ptr::null(),
            0,
            data.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    let block = match NonNull::new(block) {
        // SAFETY: Create 规则，返回的块归调用方。
        Some(block) if status == 0 => unsafe { CFRetained::from_raw(block) },
        _ => return Err(io::Error::other(format!("cannot allocate a frame ({status})"))),
    };
    // SAFETY: 块刚分配了 `data.len()` 字节，源数据同样长。
    let status = unsafe { CMBlockBuffer::replace_data_bytes(source.cast(), &block, 0, data.len()) };
    if status != 0 {
        return Err(io::Error::other(format!("cannot copy a frame ({status})")));
    }
    let size = data.len();
    let mut sample = ptr::null_mut();
    // SAFETY: 一个样本，大小如实给出，不带时间；输出参数指向本地变量。
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(format),
            1,
            0,
            ptr::null(),
            1,
            &size,
            NonNull::from(&mut sample),
        )
    };
    match NonNull::new(sample) {
        // SAFETY: Create 规则，返回的样本归调用方。
        Some(sample) if status == 0 => Ok(unsafe { CFRetained::from_raw(sample) }),
        _ => Err(io::Error::other(format!("cannot wrap a frame ({status})"))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use objc2_core_media::CMTime;
    use objc2_core_video::{
        CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
        CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    };

    use super::*;
    use crate::{Event, mac::encoder::Encoder};

    /// 一张亮度处处是 `luma`（视频范围）、没有色彩的 NV12 画面。
    fn gray(width: usize, height: usize, luma: u8) -> CFRetained<CVPixelBuffer> {
        let mut image = ptr::null_mut();
        // SAFETY: 输出参数指向本地变量。
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                None,
                NonNull::from(&mut image),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: Create 规则，返回的缓冲归调用方。
        let image = unsafe { CFRetained::from_raw(NonNull::new(image).unwrap()) };
        fill(&image, |plane| if plane == 0 { luma } else { 128 }, CVPixelBufferLockFlags::empty());
        image
    }

    /// 锁住缓冲，每个平面交给 `f`（按平面号给出它整片的字节）。
    fn fill(image: &CVPixelBuffer, value: impl Fn(usize) -> u8, flags: CVPixelBufferLockFlags) {
        // SAFETY: 锁住期间按每个平面的行宽和行数写它自己的内存。
        unsafe {
            assert_eq!(CVPixelBufferLockBaseAddress(image, flags), 0);
            for plane in 0..2 {
                let len =
                    CVPixelBufferGetBytesPerRowOfPlane(image, plane) * CVPixelBufferGetHeightOfPlane(image, plane);
                let base = CVPixelBufferGetBaseAddressOfPlane(image, plane).cast::<u8>();
                std::slice::from_raw_parts_mut(base, len).fill(value(plane));
            }
            CVPixelBufferUnlockBaseAddress(image, flags);
        }
    }

    /// 解出画面左上角的亮度（全范围）。
    fn luma_at_origin(image: &CVPixelBuffer) -> u8 {
        // SAFETY: 只读锁住期间读第一个字节。
        unsafe {
            assert_eq!(CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly), 0);
            let value = *CVPixelBufferGetBaseAddressOfPlane(image, 0).cast::<u8>();
            CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly);
            value
        }
    }

    /// 编 `count` 帧同一张灰图，第一帧和要了关键帧的那帧是关键帧。
    fn encode(width: u32, height: u32, luma: u8, count: i64) -> Vec<VideoFrame> {
        encode_as(Codec::Hevc, width, height, luma, count)
    }

    fn encode_as(codec: Codec, width: u32, height: u32, luma: u8, count: i64) -> Vec<VideoFrame> {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&frames);
        let encoder = Encoder::new(
            width,
            height,
            30,
            4_000_000,
            &[codec],
            Arc::new(move |event| {
                if let Event::Frame(frame) = event {
                    sink.lock().unwrap().push(frame);
                }
            }),
        )
        .expect("a hardware HEVC encoder");
        let image = gray(width as usize, height as usize, luma);
        encoder.request_keyframe();
        for index in 0..count {
            // SAFETY: 时间按 1/30 秒一帧。
            encoder.encode(&image, unsafe { CMTime::new(index, 30) });
        }
        encoder.complete();
        drop(encoder);
        Arc::try_unwrap(frames).unwrap().into_inner().unwrap()
    }

    /// 锁定编出来的帧能解回同样大小、亮度对得上的 NV12 全范围画面（GPUI 只认这种格式）；关键帧之前的帧
    /// 丢掉；分辨率变了（参数集不同）时换新会话照样解。
    #[test]
    #[ignore = "needs a hardware HEVC encoder"]
    fn decodes_what_the_encoder_produces() {
        let frames = encode(640, 360, 180, 5);
        assert!(frames[0].keyframe);
        let mut decoder = Decoder::new();
        assert!(decoder.decode(&frames[1]).unwrap().is_none(), "a delta frame before any keyframe");
        for frame in &frames {
            let decoded = decoder.decode(frame).unwrap().expect("a decoded frame");
            assert_eq!((decoded.width, decoded.height), (640, 360));
            assert_eq!(CVPixelBufferGetPixelFormatType(&decoded.image), kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
            // 视频范围的 180 换成全范围约是 191。
            let luma = luma_at_origin(&decoded.image);
            assert!(luma.abs_diff(191) <= 8, "luma {luma}");
        }

        let resized = encode(320, 240, 60, 1);
        let decoded = decoder.decode(&resized[0]).unwrap().expect("a decoded frame");
        assert_eq!((decoded.width, decoded.height), (320, 240));
    }

    /// H.264 编出来的也解得回来，同一个解码器从 HEVC 换到 H.264 时重建会话。
    #[test]
    #[ignore = "needs a hardware encoder"]
    fn decodes_h264_after_hevc() {
        let mut decoder = Decoder::new();
        let hevc = encode(320, 240, 100, 2);
        decoder.decode(&hevc[0]).unwrap().expect("a decoded HEVC frame");
        let h264 = encode_as(Codec::H264, 640, 360, 180, 3);
        assert_eq!(h264[0].codec, Codec::H264);
        assert!(h264[0].keyframe);
        assert_eq!(h264[0].parameter_sets.len(), 2, "SPS and PPS");
        for frame in &h264 {
            let decoded = decoder.decode(frame).unwrap().expect("a decoded H.264 frame");
            assert_eq!((decoded.width, decoded.height), (640, 360));
        }
    }

    /// 坏数据报错，之后要等关键帧。
    #[test]
    #[ignore = "needs a hardware HEVC encoder"]
    fn waits_for_a_keyframe_after_an_error() {
        let frames = encode(320, 240, 100, 3);
        let mut decoder = Decoder::new();
        decoder.decode(&frames[0]).unwrap().expect("a decoded frame");
        let broken = VideoFrame { data: vec![0, 0, 0, 3, 0x02, 0x01, 0xff], ..frames[1].clone() };
        let _ = decoder.decode(&broken);
        decoder.reset();
        assert!(decoder.decode(&frames[2]).unwrap().is_none());
        assert!(decoder.decode(&frames[0]).unwrap().is_some());
    }
}
