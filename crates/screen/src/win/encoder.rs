//! Media Foundation 的视频编码器，按远程桌面的需要配成低延迟：没有 B 帧，几乎不定期插关键帧，只在
//! `request_keyframe` 时出一帧，码率缓冲限在 `BURST_WINDOW` 那么长，单帧不会太大。平常用显卡的硬件编码器（异步的
//! MFT），输入是采集设备上的 NV12 纹理，编出的帧在事件线程上经 `Sink` 交出去；没有能用的显卡时用系统的软件编码器
//! （同步的 MFT），输入是内存里的 NV12，在交帧的线程上编好、交出去。

use std::{
    io, ptr,
    sync::{
        Arc, Condvar, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{LUID, VARIANT_TRUE},
        Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D},
        Media::MediaFoundation::{
            CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
            CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame,
            CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer, IMFActivate, IMFDXGIDeviceManager,
            IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown, IMFTransform, METransformHaveOutput,
            METransformNeedInput, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
            MF_EVENT_FLAG_NONE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE,
            MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
            MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer,
            MFCreateSample, MFMediaType_Video, MFNominalRange_16_235, MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
            MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
            MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
            MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR,
            eAVEncH264VProfile_High, eAVEncH265VProfile_Main_420_8,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
            Variant::{VARIANT, VT_BOOL, VT_UI4},
        },
    },
    core::{GUID, Interface as _},
};

use super::other;
use crate::{BURST_HEADROOM, BURST_WINDOW, Codec, Event, REFINE_BOOST, VideoFrame, nal, nv12};

pub(crate) type Sink = dyn Fn(Event) + Send + Sync;

/// 编码器要下一帧时最多等它这么久，等不到就跳过这帧，留给下一轮。
const INPUT_WAIT: Duration = Duration::from_millis(50);

pub(crate) struct Encoder {
    mft: IMFTransform,
    api: ICodecAPI,
    shared: Arc<Shared>,
    kind: Kind,
    /// 一帧的时长，100 纳秒。
    duration: i64,
    /// 观看端要的平均码率（bit/s）和放没放大（按 `REFINE_BOOST` 倍）；改它和交给编码器在同一把锁里，两个线程同时
    /// 改时编码器不会停在过时的码率上。
    rate: Mutex<(u32, bool)>,
}

enum Kind {
    /// 显卡的异步编码器，输入 NV12 纹理（`encode_texture`）。
    Hardware {
        /// 编码器用着采集的设备，要比编码器活得久。
        _manager: IMFDXGIDeviceManager,
        events: Option<JoinHandle<()>>,
    },
    /// 系统的同步编码器，输入内存里的 NV12（`encode_nv12`）。同步的 MFT 不保证能在几个线程上同时调，编帧和改设置
    /// 都拿着 `calls`；锁里是取编好的帧时反复用的输出样本，编码器自己给样本时为空。
    Software { calls: Mutex<Option<IMFSample>> },
}

// SAFETY: 异步 MFT 和 ICodecAPI 可以在任意线程上调用（多线程套间）；同步 MFT 的调用都在 `Kind::Software` 的锁里，
// 一次只有一个线程调。其余字段本身是 Send + Sync。
unsafe impl Send for Encoder {}
// SAFETY: 同上。
unsafe impl Sync for Encoder {}

/// 事件线程也要用的。
struct Shared {
    sink: Arc<Sink>,
    codec: Codec,
    /// 编码器还要几帧输入（`METransformNeedInput` 来一次加一）。
    need_input: Mutex<u32>,
    need_changed: Condvar,
    stopping: AtomicBool,
    /// 下一帧编成关键帧。
    force_keyframe: AtomicBool,
    /// 上次的参数集，关键帧里没带时用它。
    parameter_sets: Mutex<Vec<Vec<u8>>>,
}

/// 硬件编码器的事件线程拿着的编码器。
struct Worker {
    mft: IMFTransform,
    generator: IMFMediaEventGenerator,
    shared: Arc<Shared>,
}

// SAFETY: 同 `Encoder`。
unsafe impl Send for Worker {}

impl Encoder {
    /// 在 `adapter` 那块显卡上建硬件编码器，用采集的 `device` 读输入的纹理；编码是 `codecs` 里显卡编得了的第一种。
    /// 显卡上没有编得了的编码器时错误是 `io::ErrorKind::Unsupported`。
    pub(crate) fn hardware(
        device: &ID3D11Device,
        adapter: LUID,
        size: (u32, u32),
        fps: u32,
        bitrate: u32,
        codecs: &[Codec],
        sink: Arc<Sink>,
    ) -> io::Result<Self> {
        super::start_media_foundation();
        // 激活得了、配不上参数（这个尺寸、这个档次不收）的也换下一种。
        let mut failures = Vec::new();
        let mut chosen = None;
        for &codec in codecs {
            let mft = match encoder(MFT_ENUM_FLAG_HARDWARE, Some(adapter), codec) {
                Ok(mft) => mft,
                Err(err) => {
                    failures.push(format!("{codec:?}: {err}"));
                    continue;
                }
            };
            match configure_hardware(&mft, device, codec, size, fps, bitrate) {
                Ok(parts) => {
                    chosen = Some((codec, (mft, parts)));
                    break;
                }
                Err(err) => {
                    shut_down(&mft);
                    failures.push(format!("{codec:?}: {err}"));
                }
            }
        }
        let Some((codec, (mft, (manager, api, generator)))) = chosen else {
            return Err(io::Error::new(io::ErrorKind::Unsupported, failures.join(", ")));
        };
        if !failures.is_empty() {
            tracing::info!("encoding the screen as {codec:?}; unavailable: {}", failures.join(", "));
        }
        start_streaming(&mft)?;
        let shared = Shared::new(sink, codec);
        let worker = Worker { mft: mft.clone(), generator, shared: shared.clone() };
        let events = thread::Builder::new().name("screen-encoder".into()).spawn(move || worker.run())?;
        Ok(Self {
            mft,
            api,
            shared,
            kind: Kind::Hardware { _manager: manager, events: Some(events) },
            duration: 10_000_000 / i64::from(fps.max(1)),
            rate: Mutex::new((bitrate, false)),
        })
    }

    /// 建系统的软件编码器，编 `codecs` 里编得了的一种：H.264 比 HEVC 省 CPU，观看端解得了 H.264 时先用它。
    /// 一种也编不了（比如没装媒体功能包的 Windows N 版）时错误是 `io::ErrorKind::Unsupported`。
    pub(crate) fn software(
        size: (u32, u32),
        fps: u32,
        bitrate: u32,
        codecs: &[Codec],
        sink: Arc<Sink>,
    ) -> io::Result<Self> {
        super::start_media_foundation();
        let mut order = codecs.to_vec();
        order.sort_by_key(|codec| *codec != Codec::H264);
        let mut failures = Vec::new();
        let mut chosen = None;
        for codec in order {
            let mft = match encoder(MFT_ENUM_FLAG_SYNCMFT, None, codec) {
                Ok(mft) => mft,
                Err(err) => {
                    failures.push(format!("{codec:?}: {err}"));
                    continue;
                }
            };
            match configure_software(&mft, codec, size, fps, bitrate) {
                Ok(parts) => {
                    chosen = Some((codec, (mft, parts)));
                    break;
                }
                Err(err) => failures.push(format!("{codec:?}: {err}")),
            }
        }
        let Some((codec, (mft, (api, output)))) = chosen else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("no software encoder: {}", failures.join(", ")),
            ));
        };
        tracing::info!("encoding the screen as {codec:?} on the CPU");
        start_streaming(&mft)?;
        Ok(Self {
            mft,
            api,
            shared: Shared::new(sink, codec),
            kind: Kind::Software { calls: Mutex::new(output) },
            duration: 10_000_000 / i64::from(fps.max(1)),
            rate: Mutex::new((bitrate, false)),
        })
    }

    /// 平均码率，单位 bit/s；编码中途可以改。
    pub(crate) fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        let mut rate = self.rate.lock().unwrap_or_else(PoisonError::into_inner);
        rate.0 = bitrate;
        self.apply_bitrate(*rate)
    }

    /// 按 `REFINE_BOOST` 倍的码率编，画面静止时把最后一张补清楚（`Refiner`）；`false` 时回到原来的码率。
    pub(crate) fn boost(&self, on: bool) {
        let mut rate = self.rate.lock().unwrap_or_else(PoisonError::into_inner);
        if std::mem::replace(&mut rate.1, on) != on
            && let Err(err) = self.apply_bitrate(*rate)
        {
            tracing::debug!("cannot change the bitrate for refining: {err}");
        }
    }

    /// 编码器选中的编码。
    pub(crate) fn codec(&self) -> Codec {
        self.shared.codec
    }

    fn apply_bitrate(&self, (base, boosted): (u32, bool)) -> io::Result<()> {
        let bitrate = if boosted { (f64::from(base) * REFINE_BOOST) as u32 } else { base };
        let _calls = match &self.kind {
            Kind::Software { calls } => Some(calls.lock().unwrap_or_else(PoisonError::into_inner)),
            Kind::Hardware { .. } => None,
        };
        // SAFETY: 键是编码器的设置，值是它要的类型。
        unsafe {
            if let Err(err) = self.api.SetValue(&CODECAPI_AVEncCommonBufferSize, &uint(buffer_size(bitrate))) {
                tracing::debug!("the encoder does not take a new buffer size: {err}");
            }
            self.api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &uint(bitrate)).map_err(other)
        }
    }

    /// 下一帧编成关键帧，并带上参数集。
    pub(crate) fn request_keyframe(&self) {
        self.shared.force_keyframe.store(true, Ordering::Relaxed);
    }

    /// 在编码器的线程上也能要关键帧的句柄，不让编码器活下去：不然编码器最后一份可能在它自己的线程上
    /// 丢掉，在那里等它自己的线程结束。
    pub(crate) fn keyframe_request(&self) -> KeyframeRequest {
        KeyframeRequest(Arc::downgrade(&self.shared))
    }

    /// 编码器出错停了，不会再收输入、出帧。
    pub(crate) fn stopped(&self) -> bool {
        self.shared.stopping.load(Ordering::Relaxed)
    }

    /// 要了的关键帧还没交给编码器。
    pub(crate) fn keyframe_requested(&self) -> bool {
        self.shared.force_keyframe.load(Ordering::Relaxed)
    }

    /// 用的是软件编码器，要 `encode_nv12`。
    pub(crate) fn is_software(&self) -> bool {
        matches!(self.kind, Kind::Software { .. })
    }

    /// 把 `texture`（NV12，采集设备上的）交给硬件编码器，时间戳 `pts` 按 100 纳秒。编码器 `INPUT_WAIT` 内
    /// 不要输入时不交，返回 false。交出去的纹理在编完之前不能改。
    pub(crate) fn encode_texture(&self, texture: &ID3D11Texture2D, pts: i64) -> bool {
        {
            let need = self.shared.need_input.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut need, _) = self
                .shared
                .need_changed
                .wait_timeout_while(need, INPUT_WAIT, |need| {
                    *need == 0 && !self.shared.stopping.load(Ordering::Relaxed)
                })
                .unwrap_or_else(PoisonError::into_inner);
            if *need == 0 {
                return false;
            }
            *need -= 1;
        }
        let force = self.shared.force_keyframe.swap(false, Ordering::Relaxed);
        let fed = self.feed(texture, pts, force);
        if let Err(err) = &fed {
            if err.code() != MF_E_NOTACCEPTING {
                tracing::warn!("cannot encode a screen frame: {err}");
            }
            *self.shared.need_input.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            if force {
                self.shared.force_keyframe.store(true, Ordering::Relaxed);
            }
        }
        fed.is_ok()
    }

    fn feed(&self, texture: &ID3D11Texture2D, pts: i64, force: bool) -> windows::core::Result<()> {
        // SAFETY: 纹理在调用期间有效，样本拿着它的引用，编码器用完才放。
        unsafe {
            if force && let Err(err) = self.api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &uint(1)) {
                tracing::warn!("cannot ask the encoder for a keyframe: {err}");
            }
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
            if let Ok(planar) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(planar.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(self.duration)?;
            self.mft.ProcessInput(0, &sample, 0)
        }
    }

    /// 让 `fill` 把一帧 NV12 写进 `len` 字节的输入缓冲，交给软件编码器编完，编好的帧在这个线程上交出去；时间戳
    /// `pts` 按 100 纳秒。编码器出错时报 `Event::Stopped`、返回 false，之后不再编。
    pub(crate) fn encode_nv12(&self, len: usize, pts: i64, fill: impl FnOnce(&mut [u8])) -> bool {
        let Kind::Software { calls } = &self.kind else { return false };
        if self.stopped() {
            return false;
        }
        let mut frames = Vec::new();
        let fed = {
            let mut output = calls.lock().unwrap_or_else(PoisonError::into_inner);
            let force = self.shared.force_keyframe.swap(false, Ordering::Relaxed);
            self.feed_memory(len, pts, fill, force, &mut output, &mut frames)
        };
        // 放了锁再交：`sink` 里改码率时要拿同一把锁。
        for frame in frames {
            (self.shared.sink)(Event::Frame(frame));
        }
        match fed {
            Ok(()) => true,
            Err(err) => {
                if !self.shared.stopping.swap(true, Ordering::Relaxed) {
                    (self.shared.sink)(Event::Stopped(format!("the encoder stopped: {err}")));
                }
                false
            }
        }
    }

    fn feed_memory(
        &self,
        len: usize,
        pts: i64,
        fill: impl FnOnce(&mut [u8]),
        force: bool,
        output: &mut Option<IMFSample>,
        frames: &mut Vec<VideoFrame>,
    ) -> windows::core::Result<()> {
        // SAFETY: 缓冲锁住时只写它自己的长度；样本交给编码器后由它持有引用。
        unsafe {
            if force && let Err(err) = self.api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &uint(1)) {
                tracing::warn!("cannot ask the encoder for a keyframe: {err}");
            }
            // 编码器可能还拿着前几帧的输入，每帧用新的缓冲。
            let buffer = MFCreateMemoryBuffer(len as u32)?;
            let (mut data, mut max) = (ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, Some(&mut max), None)?;
            fill(std::slice::from_raw_parts_mut(data, len.min(max as usize)));
            buffer.Unlock()?;
            buffer.SetCurrentLength(len as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts)?;
            sample.SetSampleDuration(self.duration)?;
            match self.mft.ProcessInput(0, &sample, 0) {
                Err(err) if err.code() == MF_E_NOTACCEPTING => {
                    self.drain(output, frames)?;
                    self.mft.ProcessInput(0, &sample, 0)?;
                }
                fed => fed?,
            }
        }
        self.drain(output, frames)
    }

    /// 把软件编码器手上编好的各帧取进 `frames`，直到它要下一帧输入。
    fn drain(&self, output: &mut Option<IMFSample>, frames: &mut Vec<VideoFrame>) -> windows::core::Result<()> {
        loop {
            if let Some(sample) = output.as_ref() {
                // SAFETY: 输出样本只有一个缓冲，是建它时放进去的；清空长度让编码器从头写。
                unsafe { sample.GetBufferByIndex(0)?.SetCurrentLength(0)? };
            }
            let mut buffer = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: std::mem::ManuallyDrop::new(output.clone()),
                ..Default::default()
            };
            let mut status = 0;
            // SAFETY: 编码器不自己给样本时用 `output`；取完把样本和事件的引用接过来。
            let result = unsafe { self.mft.ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status) };
            // SAFETY: 两个字段这里接过所有权、只接一次。
            let (sample, _events) = unsafe {
                (std::mem::ManuallyDrop::take(&mut buffer.pSample), std::mem::ManuallyDrop::take(&mut buffer.pEvents))
            };
            match result {
                Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.shared.follow_output_change(&self.mft)?;
                    if output.is_some() {
                        *output = Some(output_sample(&self.mft)?);
                    }
                }
                Err(err) => return Err(err),
                Ok(()) => frames.extend(sample.and_then(|sample| self.shared.video_frame(&self.mft, &sample))),
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Relaxed);
        self.shared.need_changed.notify_all();
        // 关掉编码器，事件线程等着的 `GetEvent` 随即返回错误、结束。
        shut_down(&self.mft);
        if let Kind::Hardware { events, .. } = &mut self.kind
            && let Some(events) = events.take()
        {
            let _ = events.join();
        }
    }
}

#[derive(Clone)]
pub(crate) struct KeyframeRequest(Weak<Shared>);

impl KeyframeRequest {
    /// 下一帧编成关键帧；编码器已经丢掉时什么都不做。
    pub(crate) fn request(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.force_keyframe.store(true, Ordering::Relaxed);
        }
    }
}

impl Worker {
    fn run(self) {
        // SAFETY: 本线程进多线程套间，MFT 的调用都在多线程套间里。
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        loop {
            // SAFETY: 编码器关掉时返回错误；事件带着失败的状态（显卡掉了之类）时编码器不会再出帧。
            let failed = match unsafe { self.generator.GetEvent(MF_EVENT_FLAG_NONE) } {
                Ok(event) => match unsafe { event.GetStatus() } {
                    Ok(status) if status.is_err() => Err(windows::core::Error::from(status)),
                    _ => Ok(event),
                },
                Err(err) => Err(err),
            };
            let event = match failed {
                Ok(event) => event,
                Err(err) => {
                    // 采集那边不再等编码器要输入，告诉手机停了。
                    if !self.shared.stopping.swap(true, Ordering::Relaxed) {
                        self.shared.need_changed.notify_all();
                        (self.shared.sink)(Event::Stopped(format!("the encoder stopped: {err}")));
                    }
                    return;
                }
            };
            // SAFETY: 读事件的类型。
            let kind = unsafe { event.GetType() }.unwrap_or(0);
            if kind == METransformNeedInput.0 as u32 {
                *self.shared.need_input.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                self.shared.need_changed.notify_all();
            } else if kind == METransformHaveOutput.0 as u32 {
                self.take_output();
            }
        }
    }

    /// 取一帧编好的，交出去；输出格式变了时换上编码器给的新格式再取。
    fn take_output(&self) {
        loop {
            let mut buffer = MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, ..Default::default() };
            let mut status = 0;
            // SAFETY: 编码器自己给样本（建它时查过），取完把样本和事件的引用接过来。
            let result = unsafe { self.mft.ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status) };
            // SAFETY: 两个字段是 ProcessOutput 填的，这里接过所有权、只接一次。
            let (sample, _events) = unsafe {
                (std::mem::ManuallyDrop::take(&mut buffer.pSample), std::mem::ManuallyDrop::take(&mut buffer.pEvents))
            };
            match result {
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    if let Err(err) = self.shared.follow_output_change(&self.mft) {
                        tracing::warn!("cannot follow the encoder's new output format: {err}");
                        return;
                    }
                }
                Err(err) => {
                    tracing::warn!("cannot take an encoded screen frame: {err}");
                    return;
                }
                Ok(()) => {
                    if let Some(frame) = sample.and_then(|sample| self.shared.video_frame(&self.mft, &sample)) {
                        (self.shared.sink)(Event::Frame(frame));
                    }
                    return;
                }
            }
        }
    }
}

impl Shared {
    fn new(sink: Arc<Sink>, codec: Codec) -> Arc<Self> {
        Arc::new(Self {
            sink,
            codec,
            need_input: Mutex::new(0),
            need_changed: Condvar::new(),
            stopping: AtomicBool::new(false),
            force_keyframe: AtomicBool::new(true),
            parameter_sets: Mutex::new(Vec::new()),
        })
    }

    /// 输出格式变了（`MF_E_TRANSFORM_STREAM_CHANGE`）：换上编码器列出的第一种。
    fn follow_output_change(&self, mft: &IMFTransform) -> windows::core::Result<()> {
        // SAFETY: 换上编码器自己列出的格式。
        unsafe { mft.GetOutputAvailableType(0, 0).and_then(|kind| mft.SetOutputType(0, &kind, 0))? };
        // 新格式的参数集可能不同，关键帧没带时改从新格式里取。
        self.parameter_sets.lock().unwrap_or_else(PoisonError::into_inner).clear();
        Ok(())
    }

    fn video_frame(&self, mft: &IMFTransform, sample: &IMFSample) -> Option<VideoFrame> {
        // SAFETY: 锁住样本的数据复制出来再解锁；时间戳按 100 纳秒。
        let (bytes, pts) = unsafe {
            let buffer = sample.ConvertToContiguousBuffer().ok()?;
            let (mut data, mut len) = (ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut len)).ok()?;
            let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
            let _ = buffer.Unlock();
            (bytes, sample.GetSampleTime().unwrap_or(0))
        };
        let codec = self.codec;
        let (data, mut parameter_sets) = nal::from_annex_b(codec, &bytes);
        let keyframe = nal::is_keyframe(codec, &data);
        let mut known = self.parameter_sets.lock().unwrap_or_else(PoisonError::into_inner);
        if !parameter_sets.is_empty() {
            *known = parameter_sets.clone();
        } else if keyframe {
            if known.is_empty() {
                *known = self.sequence_header(mft);
            }
            parameter_sets = known.clone();
        }
        if keyframe && parameter_sets.is_empty() {
            tracing::warn!("the encoder gave a keyframe without parameter sets");
            self.force_keyframe.store(true, Ordering::Relaxed);
            return None;
        }
        if !keyframe {
            parameter_sets.clear();
        }
        Some(VideoFrame { codec, data, keyframe, parameter_sets, pts_us: pts / 10 })
    }

    /// 输出格式里的参数集（`MF_MT_MPEG_SEQUENCE_HEADER`，Annex B 写法）。
    fn sequence_header(&self, mft: &IMFTransform) -> Vec<Vec<u8>> {
        // SAFETY: 先问长度再取，缓冲按问到的长度分配。
        unsafe {
            let Ok(kind) = mft.GetOutputCurrentType(0) else { return Vec::new() };
            let Ok(len) = kind.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else { return Vec::new() };
            let mut blob = vec![0u8; len as usize];
            if kind.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).is_err() {
                return Vec::new();
            }
            nal::from_annex_b(self.codec, &blob).1
        }
    }
}

/// 配好刚激活的硬件编码器：异步解锁、交给它采集的设备，再按 `configure`。返回要一直留着的设备管理器、设置接口和
/// 事件源。
fn configure_hardware(
    mft: &IMFTransform,
    device: &ID3D11Device,
    codec: Codec,
    size: (u32, u32),
    fps: u32,
    bitrate: u32,
) -> io::Result<(IMFDXGIDeviceManager, ICodecAPI, IMFMediaEventGenerator)> {
    // SAFETY: 以下都是 Media Foundation 的普通调用，建出的对象交给调用方持有。
    unsafe {
        let attributes = mft.GetAttributes().map_err(other)?;
        if attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
            return Err(io::Error::other("the encoder is not asynchronous"));
        }
        attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(other)?;
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(other)?;
        let manager = manager.ok_or_else(|| io::Error::other("no DXGI device manager"))?;
        manager.ResetDevice(device, token).map_err(other)?;
        mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize).map_err(other)?;
        let api = configure(mft, codec, size, fps, bitrate)?;
        let provides_samples = mft
            .GetOutputStreamInfo(0)
            .is_ok_and(|info| info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0);
        if !provides_samples {
            return Err(io::Error::other("the encoder does not allocate its output"));
        }
        let generator = mft.cast::<IMFMediaEventGenerator>().map_err(other)?;
        Ok((manager, api, generator))
    }
}

/// 配好刚激活的软件编码器，返回设置接口和取帧用的输出样本（编码器自己给样本时为空）。
fn configure_software(
    mft: &IMFTransform,
    codec: Codec,
    size: (u32, u32),
    fps: u32,
    bitrate: u32,
) -> io::Result<(ICodecAPI, Option<IMFSample>)> {
    let api = configure(mft, codec, size, fps, bitrate)?;
    // SAFETY: 读输出流的信息。
    let info = unsafe { mft.GetOutputStreamInfo(0) }.map_err(other)?;
    let output = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0 {
        None
    } else {
        Some(output_sample(mft).map_err(other)?)
    };
    Ok((api, output))
}

/// 编码器不自己给输出样本时取帧用的样本，缓冲按它要的大小，不说时按一帧没压缩的 NV12 算。
fn output_sample(mft: &IMFTransform) -> windows::core::Result<IMFSample> {
    // SAFETY: 都是 Media Foundation 的普通调用，建出的对象交给调用方。
    unsafe {
        let info = mft.GetOutputStreamInfo(0)?;
        let kind = mft.GetOutputCurrentType(0)?;
        let frame = kind.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let len = nv12::frame_len(((frame >> 32) as u32, frame as u32)) as u32;
        let buffer = MFCreateMemoryBuffer(info.cbSize.max(len))?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        Ok(sample)
    }
}

/// 两种编码器都要的设置：码率和低延迟这些，输入输出格式。返回设置接口。
fn configure(mft: &IMFTransform, codec: Codec, size: (u32, u32), fps: u32, bitrate: u32) -> io::Result<ICodecAPI> {
    // SAFETY: 以下都是 Media Foundation 的普通调用，建出的对象交给调用方持有。
    unsafe {
        // 不是每个编码器都认这些设置，认不了的照默认的编。
        let gop = fps.max(1).saturating_mul(3600);
        let api = mft.cast::<ICodecAPI>().map_err(other)?;
        for (key, value, what) in [
            (CODECAPI_AVEncCommonRateControlMode, uint(eAVEncCommonRateControlMode_CBR.0 as u32), "rate control"),
            (CODECAPI_AVEncCommonMeanBitRate, uint(bitrate), "bitrate"),
            (CODECAPI_AVEncCommonBufferSize, uint(buffer_size(bitrate)), "buffer size"),
            (CODECAPI_AVLowLatencyMode, boolean(), "low latency"),
            (CODECAPI_AVEncMPVGOPSize, uint(gop), "GOP size"),
            (CODECAPI_AVEncMPVDefaultBPictureCount, uint(0), "B frames"),
        ] {
            if let Err(err) = api.SetValue(&key, &value) {
                tracing::debug!("the {codec:?} encoder does not take its {what} setting: {err}");
            }
        }
        mft.SetOutputType(0, &media_type(subtype(codec), size, fps, Some((codec, bitrate)))?, 0).map_err(other)?;
        mft.SetInputType(0, &media_type(MFVideoFormat_NV12, size, fps, None)?, 0).map_err(other)?;
        Ok(api)
    }
}

/// 让配好的编码器开始收帧；开不起来时关掉它。
fn start_streaming(mft: &IMFTransform) -> io::Result<()> {
    // SAFETY: Media Foundation 的普通调用。
    let started = unsafe {
        mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
            .and_then(|()| mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
    };
    started.map_err(|err| {
        shut_down(mft);
        other(err)
    })
}

/// 关掉不用了的编码器：不关的话异步编码器的事件队列和拿着的显卡设备一直不放；同步编码器没有这个接口，不用关。
fn shut_down(mft: &IMFTransform) {
    // SAFETY: 之后不再用 `mft`。
    if let Ok(shutdown) = mft.cast::<IMFShutdown>()
        && let Err(err) = unsafe { shutdown.Shutdown() }
    {
        tracing::debug!("cannot shut the encoder down: {err}");
    }
}

/// 码率缓冲的大小，bit：`BURST_WINDOW` 那么长时间按 `BURST_HEADROOM` 倍的码率能发的量，单帧大不过它。
fn buffer_size(bitrate: u32) -> u32 {
    (f64::from(bitrate) * BURST_WINDOW.as_secs_f64() * BURST_HEADROOM) as u32
}

fn subtype(codec: Codec) -> GUID {
    match codec {
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::H264 => MFVideoFormat_H264,
    }
}

/// 按 `flags` 找编 `codec` 的编码器，激活排在最前的那个。给了 `adapter` 时只要那块显卡上的：硬件编码器读的是
/// 采集设备上的纹理，得在同一块显卡上。
fn encoder(flags: MFT_ENUM_FLAG, adapter: Option<LUID>, codec: Codec) -> io::Result<IMFTransform> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
    let mut list: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: 输出参数指向本地变量；返回的数组下面逐个接过引用后放掉。
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
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
    let wanted = adapter.map(luid_bytes);
    let on_adapter = |activate: &IMFActivate| {
        let Some(wanted) = wanted else { return true };
        let mut luid = [0u8; 8];
        // SAFETY: 缓冲是本地数组，长度如实给出；没有这个属性（只有一块显卡时）也算。
        unsafe { activate.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut luid, None) }.map_or(true, |()| luid == wanted)
    };
    let missing = if adapter.is_some() { "no hardware encoder on the display's GPU" } else { "no encoder" };
    let activate = activates
        .iter()
        .find(|activate| on_adapter(activate))
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, missing))?;
    // SAFETY: 建出编码器。
    unsafe { activate.ActivateObject::<IMFTransform>() }.map_err(other)
}

fn luid_bytes(luid: LUID) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&luid.LowPart.to_ne_bytes());
    bytes[4..].copy_from_slice(&luid.HighPart.to_ne_bytes());
    bytes
}

/// 视频格式：`subtype`、像素尺寸、帧率，逐行扫描，BT.709 的有限范围；编出的格式另带编码的档次和平均码率。
fn media_type(
    subtype: GUID,
    (width, height): (u32, u32),
    fps: u32,
    encoded: Option<(Codec, u32)>,
) -> io::Result<IMFMediaType> {
    // SAFETY: 新建的格式只在这里写。
    unsafe {
        let kind = MFCreateMediaType().map_err(other)?;
        kind.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(other)?;
        kind.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(other)?;
        kind.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height)).map_err(other)?;
        kind.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps.max(1)) << 32) | 1).map_err(other)?;
        kind.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(other)?;
        kind.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32).map_err(other)?;
        kind.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).map_err(other)?;
        if let Some((codec, bitrate)) = encoded {
            kind.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(other)?;
            let profile = match codec {
                Codec::Hevc => eAVEncH265VProfile_Main_420_8.0,
                Codec::H264 => eAVEncH264VProfile_High.0,
            };
            kind.SetUINT32(&MF_MT_MPEG2_PROFILE, profile as u32).map_err(other)?;
        }
        Ok(kind)
    }
}

fn uint(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: 写的是 `VT_UI4` 对应的那个字段。
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
    }
    variant
}

/// 真（`VARIANT_TRUE`）。
fn boolean() -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: 写的是 `VT_BOOL` 对应的那个字段。
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_TRUE;
    }
    variant
}
