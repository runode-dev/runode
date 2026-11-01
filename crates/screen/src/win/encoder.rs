//! Media Foundation 的硬件编码器（显卡的 MFT，异步工作），按远程桌面的需要配成低延迟：没有 B 帧，几乎不定期
//! 插关键帧，只在 `request_keyframe` 时出一帧，码率缓冲限在 `BURST_WINDOW` 那么长，单帧不会太大。编码按观看端
//! 给的先后挑显卡编得了的第一种。输入是采集设备上的 NV12 纹理；编出的帧在事件线程上经 `Sink` 交出去。

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
            IMFMediaEventGenerator, IMFMediaType, IMFShutdown, IMFTransform, METransformHaveOutput,
            METransformNeedInput, MF_E_NOTACCEPTING, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NONE,
            MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE,
            MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager,
            MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateSample, MFMediaType_Video, MFNominalRange_16_235,
            MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER,
            MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
            MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
            MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High,
            eAVEncH265VProfile_Main_420_8,
        },
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
            Variant::{VARIANT, VT_BOOL, VT_UI4},
        },
    },
    core::{GUID, Interface as _},
};

use super::other;
use crate::{BURST_HEADROOM, BURST_WINDOW, Codec, Event, REFINE_BOOST, VideoFrame, nal};

pub(crate) type Sink = dyn Fn(Event) + Send + Sync;

/// 编码器要下一帧时最多等它这么久，等不到就跳过这帧，留给下一轮。
const INPUT_WAIT: Duration = Duration::from_millis(50);

pub(crate) struct Encoder {
    mft: IMFTransform,
    api: ICodecAPI,
    /// 编码器用着采集的设备，要比编码器活得久。
    _manager: IMFDXGIDeviceManager,
    shared: Arc<Shared>,
    events: Option<JoinHandle<()>>,
    /// 一帧的时长，100 纳秒。
    duration: i64,
    /// 观看端要的平均码率（bit/s）和放没放大（按 `REFINE_BOOST` 倍）；改它和交给编码器在同一把锁里，两个线程同时
    /// 改时编码器不会停在过时的码率上。
    rate: Mutex<(u32, bool)>,
}

// SAFETY: 异步 MFT 和 ICodecAPI 可以在任意线程上调用（多线程套间），其余字段本身是 Send + Sync。
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

/// 事件线程拿着的编码器。
struct Worker {
    mft: IMFTransform,
    generator: IMFMediaEventGenerator,
    shared: Arc<Shared>,
}

// SAFETY: 同 `Encoder`。
unsafe impl Send for Worker {}

impl Encoder {
    /// 在 `adapter` 那块显卡上建编码器，用采集的 `device` 读输入的纹理；编码是 `codecs` 里显卡编得了的第一种。
    pub(crate) fn new(
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
            let mft = match hardware_encoder(adapter, codec) {
                Ok(mft) => mft,
                Err(err) => {
                    failures.push(format!("{codec:?}: {err}"));
                    continue;
                }
            };
            match configure(&mft, device, codec, size, fps, bitrate) {
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
        // SAFETY: 以下都是 Media Foundation 的普通调用，对象由返回的 `Encoder` 持有。
        unsafe {
            let started = mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0));
            if let Err(err) = started {
                shut_down(&mft);
                return Err(other(err));
            }
            let shared = Arc::new(Shared {
                sink,
                codec,
                need_input: Mutex::new(0),
                need_changed: Condvar::new(),
                stopping: AtomicBool::new(false),
                force_keyframe: AtomicBool::new(true),
                parameter_sets: Mutex::new(Vec::new()),
            });
            let worker = Worker { mft: mft.clone(), generator, shared: shared.clone() };
            let events = thread::Builder::new().name("screen-encoder".into()).spawn(move || worker.run())?;
            Ok(Self {
                mft,
                api,
                _manager: manager,
                shared,
                events: Some(events),
                duration: 10_000_000 / i64::from(fps.max(1)),
                rate: Mutex::new((bitrate, false)),
            })
        }
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

    /// 把 `texture`（NV12，采集设备上的）交给编码器，时间戳 `pts` 按 100 纳秒。编码器 `INPUT_WAIT` 内
    /// 不要输入时不交，返回 false。交出去的纹理在编完之前不能改。
    pub(crate) fn encode(&self, texture: &ID3D11Texture2D, pts: i64) -> bool {
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
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Relaxed);
        self.shared.need_changed.notify_all();
        // 关掉编码器，事件线程等着的 `GetEvent` 随即返回错误、结束。
        shut_down(&self.mft);
        if let Some(events) = self.events.take() {
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
                    // SAFETY: 换上编码器列出的第一种输出格式。
                    let changed = unsafe {
                        self.mft.GetOutputAvailableType(0, 0).and_then(|kind| self.mft.SetOutputType(0, &kind, 0))
                    };
                    if let Err(err) = changed {
                        tracing::warn!("cannot follow the encoder's new output format: {err}");
                        return;
                    }
                    // 新格式的参数集可能不同，关键帧没带时改从新格式里取。
                    self.shared.parameter_sets.lock().unwrap_or_else(PoisonError::into_inner).clear();
                }
                Err(err) => {
                    tracing::warn!("cannot take an encoded screen frame: {err}");
                    return;
                }
                Ok(()) => {
                    if let Some(frame) = sample.and_then(|sample| self.video_frame(&sample)) {
                        (self.shared.sink)(Event::Frame(frame));
                    }
                    return;
                }
            }
        }
    }

    fn video_frame(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> Option<VideoFrame> {
        // SAFETY: 锁住样本的数据复制出来再解锁；时间戳按 100 纳秒。
        let (bytes, pts) = unsafe {
            let buffer = sample.ConvertToContiguousBuffer().ok()?;
            let (mut data, mut len) = (ptr::null_mut(), 0u32);
            buffer.Lock(&mut data, None, Some(&mut len)).ok()?;
            let bytes = std::slice::from_raw_parts(data, len as usize).to_vec();
            let _ = buffer.Unlock();
            (bytes, sample.GetSampleTime().unwrap_or(0))
        };
        let codec = self.shared.codec;
        let (data, mut parameter_sets) = nal::from_annex_b(codec, &bytes);
        let keyframe = nal::is_keyframe(codec, &data);
        let mut known = self.shared.parameter_sets.lock().unwrap_or_else(PoisonError::into_inner);
        if !parameter_sets.is_empty() {
            *known = parameter_sets.clone();
        } else if keyframe {
            if known.is_empty() {
                *known = self.sequence_header();
            }
            parameter_sets = known.clone();
        }
        if keyframe && parameter_sets.is_empty() {
            tracing::warn!("the encoder gave a keyframe without parameter sets");
            self.shared.force_keyframe.store(true, Ordering::Relaxed);
            return None;
        }
        if !keyframe {
            parameter_sets.clear();
        }
        Some(VideoFrame { codec, data, keyframe, parameter_sets, pts_us: pts / 10 })
    }

    /// 输出格式里的参数集（`MF_MT_MPEG_SEQUENCE_HEADER`，Annex B 写法）。
    fn sequence_header(&self) -> Vec<Vec<u8>> {
        // SAFETY: 先问长度再取，缓冲按问到的长度分配。
        unsafe {
            let Ok(kind) = self.mft.GetOutputCurrentType(0) else { return Vec::new() };
            let Ok(len) = kind.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else { return Vec::new() };
            let mut blob = vec![0u8; len as usize];
            if kind.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).is_err() {
                return Vec::new();
            }
            nal::from_annex_b(self.shared.codec, &blob).1
        }
    }
}

/// 配好刚激活的编码器：异步解锁、交给它采集的设备、码率和低延迟这些设置、输入输出格式。返回要一直留着的设备管理器、
/// 设置接口和事件源。
fn configure(
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

/// 关掉不用了的异步编码器：不关的话它的事件队列和拿着的显卡设备一直不放。
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

/// `adapter` 那块显卡上编 `codec` 的硬件编码器：编码器读的是采集设备上的纹理，得在同一块显卡上。
fn hardware_encoder(adapter: LUID, codec: Codec) -> io::Result<IMFTransform> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
    let mut list: *mut Option<IMFActivate> = ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: 输出参数指向本地变量；返回的数组下面逐个接过引用后放掉。
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
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
    let wanted = luid_bytes(adapter);
    let on_adapter = |activate: &IMFActivate| {
        let mut luid = [0u8; 8];
        // SAFETY: 缓冲是本地数组，长度如实给出；没有这个属性（只有一块显卡时）也算。
        unsafe { activate.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut luid, None) }.map_or(true, |()| luid == wanted)
    };
    let activate = activates
        .iter()
        .find(|activate| on_adapter(activate))
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no hardware encoder on the display's GPU"))?;
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
