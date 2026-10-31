//! Media Foundation 的 HEVC 解码：系统登记的同步解码器 MFT（Windows 10 上要装「HEVC 视频扩展」才有），
//! 开低延迟模式，来一帧出一帧；解出的 NV12 在内存里按 BT.709 有限范围换成 BGRA。

use std::{io, mem::ManuallyDrop, ptr};

use windows::{
    Win32::{
        Media::MediaFoundation::{
            CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer, IMFActivate, IMFMediaType, IMFSample, IMFTransform,
            MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
            MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MFCreateMediaType,
            MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
            MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoArea, MFVideoFormat_HEVC,
            MFVideoFormat_NV12,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
            Variant::{VARIANT, VT_BOOL},
        },
    },
    core::Interface as _,
};

use super::other;
use crate::{VideoFrame, nal};

/// 建解码器时先填的画面尺寸（宽在高 32 位，高在低 32 位）。
const PLACEHOLDER_SIZE: u64 = (1920 << 32) | 1080;

/// 解出来的一帧，BGRA，一行紧挨一行。
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// 一路 HEVC 画面的解码器，只在建它的线程上用（多线程套间）。
///
/// 从关键帧开始解：刚建好、`reset` 之后和解错一帧之后，关键帧以前的帧都丢掉，`decode` 返回 `Ok(None)`、
/// `needs_keyframe` 为真，调用方这时向采集端要关键帧。解码器攒着一帧、下一帧交进去时才出，所以 `decode`
/// 返回的是上一帧，第一个关键帧交进去时是 `Ok(None)`，这时 `needs_keyframe` 为假。
pub struct Decoder {
    mft: IMFTransform,
    /// 解码器自己不给输出样本时，每次取输出要备这么大的缓冲。
    output_size: Option<u32>,
    format: Option<Format>,
    need_keyframe: bool,
}

/// 输出的格式。
#[derive(Clone, Copy)]
struct Format {
    /// 解码器给的整幅画面（可能按块对齐，比画面大）。
    coded: (u32, u32),
    /// 要显示的那块。
    shown: (u32, u32),
}

// SAFETY: 解码器在多线程套间里建，只交给一个线程用。
unsafe impl Send for Decoder {}

impl Decoder {
    /// 建解码器；系统没有 HEVC 解码器时报 `Unsupported`。
    pub fn new() -> io::Result<Self> {
        // SAFETY: 本线程进多线程套间，之后解码器的调用都在这个线程上。
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        super::start_media_foundation();
        let mft = find_decoder()?;
        // SAFETY: 以下都是 Media Foundation 的普通调用，对象由返回的 `Decoder` 持有。
        unsafe {
            if let Ok(attributes) = mft.GetAttributes() {
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            if let Ok(codec) = mft.cast::<ICodecAPI>()
                && let Err(err) = codec.SetValue(&CODECAPI_AVLowLatencyMode, &boolean())
            {
                tracing::debug!("the HEVC decoder does not take the low latency setting: {err}");
            }
            let input = MFCreateMediaType().map_err(other)?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(other)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC).map_err(other)?;
            // 不给尺寸时解码器定不出输出格式、不肯开始收数据。先填一个占位的，第一帧来了它按参数集里的实际尺寸
            // 报格式变了（`drain` 跟着换）。
            input.SetUINT64(&MF_MT_FRAME_SIZE, PLACEHOLDER_SIZE).map_err(other)?;
            mft.SetInputType(0, &input, 0).map_err(failed("set the input type"))?;
        }
        let mut decoder = Self { mft, output_size: None, format: None, need_keyframe: true };
        decoder.choose_output()?;
        // SAFETY: 通知解码器开始收数据。
        unsafe {
            decoder.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(failed("begin streaming"))?;
            decoder.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(failed("start the stream"))?;
        }
        Ok(decoder)
    }

    /// 丢掉解到一半的状态，从下一个关键帧重新开始。
    pub fn reset(&mut self) {
        self.need_keyframe = true;
    }

    /// 在等关键帧：关键帧之前来的帧都丢掉。
    pub fn needs_keyframe(&self) -> bool {
        self.need_keyframe
    }

    /// 解一帧。还在等关键帧或者解码器还没出帧时返回 `Ok(None)`；解错时返回错误，之后等关键帧。
    pub fn decode(&mut self, frame: &VideoFrame) -> io::Result<Option<Decoded>> {
        if self.need_keyframe && !frame.keyframe {
            return Ok(None);
        }
        let result = self.decode_frame(frame);
        match &result {
            Ok(_) => self.need_keyframe = false,
            Err(_) => self.need_keyframe = true,
        }
        result
    }

    fn decode_frame(&mut self, frame: &VideoFrame) -> io::Result<Option<Decoded>> {
        let sets: &[Vec<u8>] = if frame.keyframe { &frame.parameter_sets } else { &[] };
        let sample = input_sample(&nal::to_annex_b(&frame.data, sets), frame.pts_us.saturating_mul(10))?;
        let mut latest = None;
        // SAFETY: 样本在调用期间有效，解码器要用就自己留一份引用。
        match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
            Err(err) if err.code() == MF_E_NOTACCEPTING => {
                // 先把攒着的取出来再交。
                latest = self.drain()?;
                // SAFETY: 同上。
                unsafe { self.mft.ProcessInput(0, &sample, 0) }.map_err(other)?;
            }
            result => result.map_err(other)?,
        }
        Ok(self.drain()?.or(latest))
    }

    /// 把解码器能出的帧都取出来，返回最后一帧。
    fn drain(&mut self) -> io::Result<Option<Decoded>> {
        let mut latest = None;
        loop {
            let provided = match self.output_size {
                Some(size) => Some(output_sample(size)?),
                None => None,
            };
            let mut buffer =
                MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(provided), ..Default::default() };
            let mut status = 0;
            // SAFETY: 缓冲是本地的；取完把样本和事件的引用接过来。
            let result = unsafe { self.mft.ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status) };
            // SAFETY: 两个字段是这里或 ProcessOutput 填的，接过所有权、只接一次。
            let (sample, _events) =
                unsafe { (ManuallyDrop::take(&mut buffer.pSample), ManuallyDrop::take(&mut buffer.pEvents)) };
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        latest = Some(self.picture(&sample)?);
                    }
                }
                Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(latest),
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.choose_output()?,
                Err(err) => return Err(other(err)),
            }
        }
    }

    /// 选 NV12 当输出格式，记下画面尺寸和要不要自己备输出缓冲。
    fn choose_output(&mut self) -> io::Result<()> {
        // SAFETY: 逐个看解码器列出的输出格式，选中的交回给它。
        unsafe {
            let mut index = 0;
            let kind = loop {
                let kind = self
                    .mft
                    .GetOutputAvailableType(0, index)
                    .map_err(|err| io::Error::other(format!("the HEVC decoder cannot output NV12: {err}")))?;
                if kind.GetGUID(&MF_MT_SUBTYPE).is_ok_and(|subtype| subtype == MFVideoFormat_NV12) {
                    break kind;
                }
                index += 1;
            };
            self.mft.SetOutputType(0, &kind, 0).map_err(failed("set the output type"))?;
            self.format = format(&kind);
            let info = self.mft.GetOutputStreamInfo(0).map_err(failed("read the output stream"))?;
            self.output_size =
                (info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0).then_some(info.cbSize.max(1));
        }
        Ok(())
    }

    /// 把解出的 NV12 样本换成 BGRA。
    fn picture(&self, sample: &IMFSample) -> io::Result<Decoded> {
        let format = self.format.ok_or_else(|| io::Error::other("the HEVC decoder gave no frame size"))?;
        // SAFETY: 锁住缓冲期间按它报的行宽读，读完解锁。
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().map_err(other)?;
            let (coded_w, coded_h) = (format.coded.0 as usize, format.coded.1 as usize);
            if let Ok(planar) = buffer.cast::<IMF2DBuffer>() {
                let (mut scan0, mut pitch) = (ptr::null_mut(), 0i32);
                planar.Lock2D(&mut scan0, &mut pitch).map_err(other)?;
                let pitch = pitch.unsigned_abs() as usize;
                let len = pitch * coded_h * 3 / 2;
                let bgra = nv12_to_bgra(std::slice::from_raw_parts(scan0, len), pitch, coded_h, format.shown);
                let _ = planar.Unlock2D();
                bgra.map(|bgra| Decoded { width: format.shown.0, height: format.shown.1, bgra })
            } else {
                let (mut data, mut len) = (ptr::null_mut(), 0u32);
                buffer.Lock(&mut data, None, Some(&mut len)).map_err(other)?;
                let bgra = nv12_to_bgra(std::slice::from_raw_parts(data, len as usize), coded_w, coded_h, format.shown);
                let _ = buffer.Unlock();
                bgra.map(|bgra| Decoded { width: format.shown.0, height: format.shown.1, bgra })
            }
        }
    }
}

/// 报错时带上是哪一步。
fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> io::Error {
    move |err| io::Error::other(format!("the HEVC decoder cannot {what}: {err}"))
}

/// 找系统里能解 HEVC 的同步解码器。
fn find_decoder() -> io::Result<IMFTransform> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
    let mut list: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: 输出参数指向本地变量；返回的数组下面逐个接过引用后放掉。
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            None,
            &mut list,
            &mut count,
        )
        .map_err(other)?;
    }
    // SAFETY: `list` 有 `count` 项，每项的引用接过来一次，数组本身用 CoTaskMemFree 放。
    let activates: Vec<IMFActivate> =
        unsafe { (0..count as usize).filter_map(|index| ptr::read(list.add(index))).collect() };
    // SAFETY: 同上。
    unsafe { CoTaskMemFree(Some(list.cast())) };
    let unsupported = || io::Error::new(io::ErrorKind::Unsupported, "no HEVC decoder on this system");
    activates
        .iter()
        // SAFETY: 建出解码器。
        .find_map(|activate| unsafe { activate.ActivateObject::<IMFTransform>() }.ok())
        .ok_or_else(unsupported)
}

/// 输出格式里的画面尺寸：整幅的和要显示的（有最小显示区域时取它）。
fn format(kind: &IMFMediaType) -> Option<Format> {
    // SAFETY: 读格式的属性，显示区域读进本地结构体。
    unsafe {
        let size = kind.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let coded = ((size >> 32) as u32, size as u32);
        let mut area = MFVideoArea::default();
        let bytes =
            std::slice::from_raw_parts_mut(ptr::from_mut(&mut area).cast::<u8>(), std::mem::size_of::<MFVideoArea>());
        let shown = match kind.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) {
            Ok(()) if area.Area.cx > 0 && area.Area.cy > 0 => {
                ((area.Area.cx as u32).min(coded.0), (area.Area.cy as u32).min(coded.1))
            }
            _ => coded,
        };
        Some(Format { coded, shown })
    }
}

fn input_sample(data: &[u8], time: i64) -> io::Result<IMFSample> {
    // SAFETY: 新建的缓冲锁住期间写满 `data.len()` 字节。
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32).map_err(other)?;
        let mut target = ptr::null_mut();
        buffer.Lock(&mut target, None, None).map_err(other)?;
        ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
        buffer.Unlock().map_err(other)?;
        buffer.SetCurrentLength(data.len() as u32).map_err(other)?;
        let sample = MFCreateSample().map_err(other)?;
        sample.AddBuffer(&buffer).map_err(other)?;
        sample.SetSampleTime(time).map_err(other)?;
        Ok(sample)
    }
}

fn output_sample(size: u32) -> io::Result<IMFSample> {
    // SAFETY: 新建一个带空缓冲的样本给解码器写。
    unsafe {
        let buffer = MFCreateMemoryBuffer(size).map_err(other)?;
        let sample = MFCreateSample().map_err(other)?;
        sample.AddBuffer(&buffer).map_err(other)?;
        Ok(sample)
    }
}

/// NV12（Y 平面 `rows` 行、每行 `pitch` 字节，后面紧跟交错的 UV 平面）左上角 `shown` 那块换成 BGRA，按
/// BT.709 有限范围。数据不够长时报错。
fn nv12_to_bgra(nv12: &[u8], pitch: usize, rows: usize, shown: (u32, u32)) -> io::Result<Vec<u8>> {
    let (width, height) = (shown.0 as usize, shown.1 as usize);
    let uv_start = pitch * rows;
    if width > pitch || height > rows || nv12.len() < uv_start + pitch * height.div_ceil(2) {
        return Err(io::Error::other("a decoded frame of the wrong size"));
    }
    let mut out = vec![0u8; width * height * 4];
    for (y, row) in out.chunks_exact_mut(width * 4).enumerate() {
        let luma = &nv12[y * pitch..][..width];
        let chroma = &nv12[uv_start + (y / 2) * pitch..][..width.next_multiple_of(2).min(pitch)];
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let c = 298 * (i32::from(luma[x]) - 16);
            let u = i32::from(chroma[x & !1]) - 128;
            let v = i32::from(chroma[(x & !1) + 1]) - 128;
            let clamp = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
            pixel[0] = clamp(c + 541 * u);
            pixel[1] = clamp(c - 55 * u - 136 * v);
            pixel[2] = clamp(c + 459 * v);
            pixel[3] = 255;
        }
    }
    Ok(out)
}

/// 真（`VARIANT_TRUE`）。
fn boolean() -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: 写的是 `VT_BOOL` 对应的那个字段。
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = windows::Win32::Foundation::VARIANT_TRUE;
    }
    variant
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 灰的、纯红的像素换出来对得上；画面比解码器给的整幅小时只换左上角那块。
    #[test]
    fn converts_nv12_to_bgra() {
        // 2x2 的画面放在每行 4 字节、4 行的缓冲里：亮度 16..235 映到 0..255，色度 128 是灰。
        let (pitch, rows) = (4, 4);
        let mut nv12 = vec![0u8; pitch * rows * 3 / 2];
        nv12[..pitch * rows].fill(235);
        nv12[pitch * rows..].fill(128);
        let bgra = nv12_to_bgra(&nv12, pitch, rows, (2, 2)).unwrap();
        assert_eq!(bgra.len(), 2 * 2 * 4);
        assert!(bgra.chunks(4).all(|pixel| pixel == [255, 255, 255, 255]));
        // BT.709 有限范围的纯红：Y 63，U 102，V 240。
        nv12[..pitch * rows].fill(63);
        for pair in nv12[pitch * rows..].chunks_mut(2) {
            pair.copy_from_slice(&[102, 240]);
        }
        let bgra = nv12_to_bgra(&nv12, pitch, rows, (2, 2)).unwrap();
        let [b, g, r, _] = bgra[..4] else { unreachable!() };
        assert!(r > 250 && g < 5 && b < 5, "{b} {g} {r}");
        assert!(nv12_to_bgra(&nv12[..10], pitch, rows, (2, 2)).is_err());
    }
}
