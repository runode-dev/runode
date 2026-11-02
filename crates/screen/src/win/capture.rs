//! 用 DXGI 桌面复制采集一块屏幕，在单独的线程上：桌面复制给的画面不带鼠标指针，先拷进一张能用 GDI
//! 画的纹理、画上指针，再缩放、转成 NV12 交给 `Encoder`（`Convert`）。屏幕转过（竖着放）时桌面复制给的是
//! 没转的画面，先转成用户看到的方向（`Turner`）再画指针。屏幕和指针都没动时桌面复制不出新画面，
//! 静止时几乎不占带宽。
//!
//! 平常缩放和转换用显卡的视频处理器，交给显卡的硬件编码器；屏幕挂在没有视频功能的显卡上（没有显卡的虚拟机只有
//! 软件渲染器）或者显卡上没有编得了的硬件编码器时，把画面拷回内存，在 CPU 上缩放、转换（`nv12`），交给系统的
//! 软件编码器。

use std::{
    io,
    mem::ManuallyDrop,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, HMODULE, POINT, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_GDI_COMPATIBLE, D3D11_SDK_VERSION,
                D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CAPS, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION,
                D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_ROTATION,
                D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_180,
                D3D11_VIDEO_PROCESSOR_ROTATION_270, D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
                D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11Device,
                ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoDevice,
                ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
                ID3D11VideoProcessorOutputView,
            },
            Dxgi::{
                Common::{
                    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90,
                    DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
                },
                DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED,
                DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
                IDXGIOutputDuplication, IDXGIResource, IDXGISurface1,
            },
            Gdi::DeleteObject,
        },
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx},
        UI::WindowsAndMessaging::{
            CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, HICON, ICONINFO,
        },
    },
    core::Interface as _,
};

use super::{
    display::{self, Output},
    encoder::{Encoder, KeyframeRequest, Sink},
    other,
};
use crate::{
    Codec, Event, Options,
    nv12::{self, Area},
    refine::{REPEAT_DELAY, Refiner},
};

/// 编码器没收下的画面过这么久再试。
const RETRY_WAIT: Duration = Duration::from_millis(5);
/// 桌面复制断了（换到安全桌面、改了分辨率）以后隔这么久重连一次。
const REOPEN_WAIT: Duration = Duration::from_millis(250);
/// 轮流用的 NV12 纹理张数：交给编码器的那张编完之前不能再写。
const TARGETS: usize = 6;
/// 视频处理器输出的颜色：BT.709 的有限范围（`D3D11_VIDEO_PROCESSOR_COLOR_SPACE` 的 `YCbCr_Matrix` 和
/// `Nominal_Range` 两个位段），和编码器的格式一致。输入是全范围的 RGB，全零就是。
const OUTPUT_COLOR_SPACE: u32 = (1 << 2) | (1 << 4);

/// 开着的采集，丢掉时停下。
pub struct Capture {
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}

/// 采集线程和别处共用的。
struct State {
    encoder: Encoder,
    stop: AtomicBool,
}

impl Capture {
    /// 开始采集，`on_event` 在采集线程和编码器的线程上收到编好的帧和停下的原因。第一帧是带参数集的
    /// 关键帧。屏幕不存在时返回错误；硬件、软件的编码器都编不了观看端解得了的格式时，错误是
    /// `io::ErrorKind::Unsupported`。
    pub fn start(options: &Options, on_event: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Self> {
        let sink: Arc<Sink> = Arc::new(on_event);
        let options = options.clone();
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new().name("screen-capture".into()).spawn(move || {
            // SAFETY: 本线程进多线程套间，D3D11 和 MFT 的调用都在多线程套间里。
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            match Pipeline::open(&options, sink.clone()) {
                Ok(pipeline) => {
                    let _ = tx.send(Ok(pipeline.state.clone()));
                    pipeline.run(&*sink);
                }
                Err(err) => {
                    let _ = tx.send(Err(err));
                }
            }
        })?;
        match rx.recv() {
            Ok(Ok(state)) => Ok(Self { state, thread: Some(thread) }),
            Ok(Err(err)) => {
                let _ = thread.join();
                Err(err)
            }
            Err(_) => Err(io::Error::other("the capture thread ended")),
        }
    }

    /// 出一帧带参数集的关键帧：手机丢了帧、刚连上要从头解时用。屏幕静止时也出，重编最近一张画面。
    pub fn request_keyframe(&self) {
        self.state.encoder.request_keyframe();
    }

    /// 在 `on_event` 里要关键帧用的句柄，不让采集活下去。
    pub fn keyframes(&self) -> Keyframes {
        Keyframes(self.state.encoder.keyframe_request())
    }

    /// 改平均码率，单位 bit/s。
    pub fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        self.state.encoder.set_bitrate(bitrate)
    }

    /// 编码器选中的编码。
    pub fn codec(&self) -> Codec {
        self.state.encoder.codec()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 向采集要关键帧，编码器的线程上也能用：只持有编码器那边的弱引用。
#[derive(Clone)]
pub struct Keyframes(KeyframeRequest);

impl Keyframes {
    /// 出一帧带参数集的关键帧，屏幕静止时也出；采集已经丢掉时什么都不做。
    pub fn request(&self) {
        self.0.request();
    }
}

/// 采集线程上的一套东西：从桌面复制到编码器。
struct Pipeline {
    output: Output,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: Option<IDXGIOutputDuplication>,
    /// 屏幕的方向，开始采集时的。
    rotation: DXGI_MODE_ROTATION,
    /// 最近一张桌面画面，不带指针，桌面复制交来的方向：屏幕转了 90° 或 270° 时宽高和屏幕对调。
    desktop: ID3D11Texture2D,
    /// `desktop` 的宽高。
    raw: (u32, u32),
    /// 画上了指针的那张，用户看到的方向，能用 GDI 画。
    composed: ID3D11Texture2D,
    /// 屏幕转过时把 `desktop` 转进 `composed`；没转时直接拷。
    turner: Option<Turner>,
    convert: Convert,
    /// 拿到过桌面复制交来的真画面（`LastPresentTime` 不为零）。
    have_image: bool,
    /// 在画面上画指针（`Options::cursor`）。
    cursor: bool,
    state: Arc<State>,
    started: Instant,
    last_pts: i64,
}

impl Pipeline {
    fn open(options: &Options, sink: Arc<Sink>) -> io::Result<Self> {
        let output = display::find(options.display)?;
        // SAFETY: 读输出的描述。
        let rotation = unsafe { output.output.GetDesc() }.map_err(other)?.Rotation;
        let (width, height) = (options.width.max(2) & !1, options.height.max(2) & !1);
        let source = (
            (output.rect.right - output.rect.left).max(1) as u32,
            (output.rect.bottom - output.rect.top).max(1) as u32,
        );
        let raw = unrotated_size(source, rotation);
        let size = (width, height);
        // SAFETY: 以下都是 D3D11 的普通调用，对象都由 `Pipeline` 持有。
        unsafe {
            let (device, context, video_support) = create_device(&output.adapter)?;
            // 编码器在它自己的线程上也用这个设备。
            let _ = device.cast::<ID3D11Multithread>().map_err(other)?.SetMultithreadProtected(true);
            let adapter = output.adapter.GetDesc1().map_err(other)?.AdapterLuid;
            let (fps, bitrate) = (options.fps, options.bitrate);
            let hardware = if video_support {
                match Encoder::hardware(&device, adapter, size, fps, bitrate, &options.codecs, sink.clone()) {
                    Ok(encoder) => Some(encoder),
                    Err(err) if err.kind() == io::ErrorKind::Unsupported => {
                        tracing::info!("encoding on the CPU: {err}");
                        None
                    }
                    Err(err) => return Err(err),
                }
            } else {
                tracing::info!("encoding on the CPU: the display's graphics adapter has no video support");
                None
            };
            let encoder = match hardware {
                Some(encoder) => encoder,
                None => Encoder::software(size, fps, bitrate, &options.codecs, sink)?,
            };
            let desktop = texture(&device, raw, DXGI_FORMAT_B8G8R8A8_UNORM, 0)?;
            let composed =
                texture(&device, source, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32)?;
            let rate = DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 };
            let turner = match turn(rotation) {
                Some(turn) => {
                    let video = video_support
                        .then(|| Rotator::new(&device, &context, (&desktop, raw), (&composed, source), rate, turn));
                    Some(match video {
                        Some(Ok(rotator)) => Turner::Video(rotator),
                        failed => {
                            if let Some(Err(err)) = failed {
                                tracing::info!("turning the picture on the CPU: {err}");
                            }
                            Turner::Cpu(CpuRotator::new(&device, raw, quarters(rotation))?)
                        }
                    })
                }
                None => None,
            };
            let convert = if encoder.is_software() {
                Convert::Cpu(Cpu::new(&device, source, size)?)
            } else {
                Convert::Gpu(Gpu::new(&device, &context, &composed, source, size, rate)?)
            };
            // 锁着屏或停在 UAC 提示上时安全桌面复制不了，先不连，由 `next_frame` 隔一会儿重连。
            let duplication = match output.output.DuplicateOutput(&device) {
                Ok(duplication) => Some(duplication),
                Err(err) if err.code() == E_ACCESSDENIED => None,
                Err(err) => return Err(other(err)),
            };
            let state = Arc::new(State { encoder, stop: AtomicBool::new(false) });
            Ok(Self {
                output,
                device,
                context,
                duplication,
                rotation,
                desktop,
                raw,
                composed,
                turner,
                convert,
                have_image: false,
                cursor: options.cursor,
                state,
                started: Instant::now(),
                last_pts: -1,
            })
        }
    }

    fn run(mut self, sink: &Sink) {
        // 有没有拿到过画面、有没有还没交给编码器的画面、上次交出去的时刻。
        let (mut have_frame, mut pending) = (false, false);
        let mut last_encoded = Instant::now();
        let mut refiner = Refiner::default();
        // 编码器出错停了时它自己报过 `Event::Stopped`。
        while !self.state.stop.load(Ordering::Relaxed) && !self.state.encoder.stopped() {
            let wait = if pending { RETRY_WAIT } else { refiner.timeout(Instant::now()) };
            match self.next_frame(wait) {
                Ok(true) => {
                    have_frame = true;
                    pending = true;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!("screen capture stopped: {err}");
                    sink(Event::Stopped(err.to_string()));
                    return;
                }
            }
            let repeat =
                have_frame && self.state.encoder.keyframe_requested() && last_encoded.elapsed() >= REPEAT_DELAY;
            let refine = have_frame && !pending && refiner.wait(Instant::now()).is_some_and(|wait| wait.is_zero());
            if pending || repeat || refine {
                // 新画面回到原来的码率；屏幕停下以后补清楚时放大码率。
                self.state.encoder.boost(refine && !pending && !repeat);
                let encoded = self.encode();
                let now = Instant::now();
                if encoded {
                    // 重编的关键帧按原来的码率编，补清楚的画面又糊了，和新画面一样停下以后重新补。
                    refiner.encoded(now, !(pending || repeat));
                    pending = false;
                    last_encoded = now;
                } else if pending || repeat {
                    // 没收下的新画面和重编的关键帧过一会儿再交。
                    pending = true;
                } else {
                    // 补清楚的那帧交不进去也算补过，不然到点了一直重试、空转。
                    refiner.refined(now);
                }
            }
        }
    }

    /// 等最多 `wait` 拿一张新画面放进 `desktop`；屏幕或指针变了返回 true。桌面复制断了时重连，重连不上
    /// 先当没变，过一会儿再试（锁屏时一直这样）。
    fn next_frame(&mut self, wait: Duration) -> io::Result<bool> {
        let Some(duplication) = self.duplication.clone() else {
            thread::sleep(REOPEN_WAIT);
            self.reopen()?;
            return Ok(false);
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: 输出参数指向本地变量；拿到画面后拷完马上放掉。
        match unsafe { duplication.AcquireNextFrame(wait.as_millis() as u32, &mut info, &mut resource) } {
            Ok(()) => {
                if info.LastPresentTime != 0
                    && let Some(texture) = resource.and_then(|resource| resource.cast::<ID3D11Texture2D>().ok())
                {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    // SAFETY: 读纹理的描述，输出指向本地变量。
                    unsafe { texture.GetDesc(&mut desc) };
                    if (desc.Width, desc.Height) != self.raw {
                        // SAFETY: 拿到的画面放掉。
                        let _ = unsafe { duplication.ReleaseFrame() };
                        return Err(io::Error::other("the display changed its resolution or orientation"));
                    }
                    // SAFETY: 两张纹理尺寸和格式一样，都在这个设备上。
                    unsafe { self.context.CopyResource(&self.desktop, &texture) };
                }
                // SAFETY: 拿到的画面用完放掉，下一次才拿得到新的。
                let _ = unsafe { duplication.ReleaseFrame() };
                // 刚开始复制时交来的可能只有指针动了，画面还是空的，等第一张真画面来了才算。
                self.have_image |= info.LastPresentTime != 0;
                Ok(self.have_image && (info.LastPresentTime != 0 || info.LastMouseUpdateTime != 0))
            }
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => Ok(false),
            Err(err) if err.code() == DXGI_ERROR_ACCESS_LOST => {
                self.duplication = None;
                self.have_image = false;
                Ok(false)
            }
            Err(err) => Err(other(err)),
        }
    }

    /// 重连桌面复制。暂时连不上（锁屏、UAC 提示这类安全桌面，会话断开，切换显示模式途中）时先不管，
    /// 过一会儿再试；屏幕不在桌面上了、别的原因连不上时报错，不然手机一直停在最后一帧。屏幕的尺寸或方向
    /// 变了（改了分辨率、转了屏）时也报错：纹理和视频处理器是按原来的尺寸建的。
    fn reopen(&mut self) -> io::Result<()> {
        // SAFETY: 读输出的描述。
        let desc = unsafe { self.output.output.GetDesc() }.map_err(other)?;
        if !desc.AttachedToDesktop.as_bool() {
            return Err(io::Error::other("the display was disconnected"));
        }
        // SAFETY: 输出和设备都活着。
        let duplication = match unsafe { self.output.output.DuplicateOutput(&self.device) } {
            Ok(duplication) => duplication,
            Err(err)
                if [
                    E_ACCESSDENIED,
                    DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
                    DXGI_ERROR_SESSION_DISCONNECTED,
                    DXGI_ERROR_UNSUPPORTED,
                ]
                .contains(&err.code()) =>
            {
                tracing::debug!("cannot capture the screen yet: {err}");
                return Ok(());
            }
            Err(err) => return Err(other(err)),
        };
        let rect = desc.DesktopCoordinates;
        let size = |rect: RECT| (rect.right - rect.left, rect.bottom - rect.top);
        if size(rect) != size(self.output.rect) || desc.Rotation != self.rotation {
            return Err(io::Error::other("the display changed its resolution or orientation"));
        }
        self.output.rect = rect;
        self.duplication = Some(duplication);
        self.state.encoder.request_keyframe();
        Ok(())
    }

    /// 画上指针、转成 NV12 交给编码器；编码器没收下时返回 false。
    fn encode(&mut self) -> bool {
        // SAFETY: 纹理都在这个设备上。
        unsafe {
            match &mut self.turner {
                Some(turner) => {
                    if let Err(err) = turner.run(&self.context, &self.desktop, &self.composed) {
                        tracing::warn!("cannot turn a screen frame: {err}");
                        return true;
                    }
                }
                None => self.context.CopyResource(&self.composed, &self.desktop),
            }
        }
        if self.cursor {
            self.draw_cursor();
        }
        // 时间戳一直往后走，重编的画面排在上一帧之后。
        let pts = (self.started.elapsed().as_nanos() / 100) as i64;
        let pts = pts.max(self.last_pts + 1);
        let encoder = &self.state.encoder;
        let converted = match &mut self.convert {
            Convert::Gpu(gpu) => gpu.encode(encoder, pts),
            Convert::Cpu(cpu) => cpu.encode(&self.context, &self.composed, encoder, pts),
        };
        match converted {
            Ok(false) => false,
            Ok(true) => {
                self.last_pts = pts;
                true
            }
            Err(err) => {
                tracing::warn!("cannot convert a screen frame: {err}");
                true
            }
        }
    }

    /// 在 `composed` 上画出指针，位置按这块屏幕算；指针藏着或者不在这块屏幕上时不画。
    fn draw_cursor(&self) {
        let mut cursor = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
        // SAFETY: `cbSize` 已经填好，输出指向本地变量。
        if unsafe { GetCursorInfo(&mut cursor) }.is_err() || cursor.flags.0 & CURSOR_SHOWING.0 == 0 {
            return;
        }
        let rect = self.output.rect;
        let POINT { x, y } = cursor.ptScreenPos;
        if x < rect.left || x >= rect.right || y < rect.top || y >= rect.bottom {
            return;
        }
        let icon = HICON(cursor.hCursor.0);
        let mut info = ICONINFO::default();
        // SAFETY: 指针句柄来自系统；拿到的两张位图用完删掉。
        let hotspot = unsafe {
            if GetIconInfo(icon, &mut info).is_err() {
                return;
            }
            let _ = DeleteObject(info.hbmMask.into());
            if !info.hbmColor.is_invalid() {
                let _ = DeleteObject(info.hbmColor.into());
            }
            (info.xHotspot as i32, info.yHotspot as i32)
        };
        let Ok(surface) = self.composed.cast::<IDXGISurface1>() else { return };
        // SAFETY: 纹理建时带着 GDI 兼容的标志；画完放掉设备上下文，纹理才能接着给 D3D 用。
        unsafe {
            let Ok(dc) = surface.GetDC(false) else { return };
            let _ = DrawIconEx(dc, x - rect.left - hotspot.0, y - rect.top - hotspot.1, icon, 0, 0, 0, None, DI_NORMAL);
            let _ = surface.ReleaseDC(None);
        }
    }
}

/// 把屏幕转过的画面转回用户看到的方向：一个只做旋转的视频处理器，BGRA 进、BGRA 出。
struct Rotator {
    video: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    output: ID3D11VideoProcessorOutputView,
}

impl Rotator {
    /// 把 `raw`（纹理和宽高）按 `turn` 转进 `visible`。显卡的视频处理器不会转或者不能输出 BGRA 时返回错误。
    ///
    /// # Safety
    ///
    /// 纹理都在 `device` 上，它建时带着视频支持。
    unsafe fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        (raw, raw_size): (&ID3D11Texture2D, (u32, u32)),
        (visible, visible_size): (&ID3D11Texture2D, (u32, u32)),
        rate: DXGI_RATIONAL,
        turn: D3D11_VIDEO_PROCESSOR_ROTATION,
    ) -> io::Result<Self> {
        // SAFETY: 都是 D3D11 的普通调用，输出参数指向本地变量。
        unsafe {
            let video_device = device.cast::<ID3D11VideoDevice>().map_err(other)?;
            let video = context.cast::<ID3D11VideoContext>().map_err(other)?;
            let video_device = &video_device;
            let enumerator =
                video_device.CreateVideoProcessorEnumerator(&content(raw_size, visible_size, rate)).map_err(other)?;
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            enumerator.GetVideoProcessorCaps(&mut caps).map_err(other)?;
            let bgra = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM).map_err(other)?;
            if caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION.0 as u32 == 0
                || bgra & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0
            {
                return Err(io::Error::other("the graphics card cannot turn the picture of a rotated display"));
            }
            let processor = video_device.CreateVideoProcessor(&enumerator, 0).map_err(other)?;
            let input = input_view(video_device, &enumerator, raw)?;
            let output = output_view(video_device, &enumerator, visible)?;
            let full = RECT { left: 0, top: 0, right: visible_size.0 as i32, bottom: visible_size.1 as i32 };
            video.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            video.VideoProcessorSetStreamRotation(&processor, 0, true, turn);
            video.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full));
            video.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
            let rgb = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            video.VideoProcessorSetStreamColorSpace(&processor, 0, &rgb);
            video.VideoProcessorSetOutputColorSpace(&processor, &rgb);
            Ok(Self { video, processor, input, output })
        }
    }

    fn run(&self) -> windows::core::Result<()> {
        // SAFETY: 两个视图和处理器都建在 `video` 的设备上。
        unsafe { blit(&self.video, &self.processor, &self.input, &self.output) }
    }
}

/// 屏幕转过时把桌面复制交来的画面转进 `composed` 的办法。
enum Turner {
    Video(Rotator),
    /// 显卡没有视频功能或者视频处理器不会转时用。
    Cpu(CpuRotator),
}

impl Turner {
    fn run(
        &mut self,
        context: &ID3D11DeviceContext,
        desktop: &ID3D11Texture2D,
        composed: &ID3D11Texture2D,
    ) -> windows::core::Result<()> {
        match self {
            Self::Video(rotator) => rotator.run(),
            Self::Cpu(rotator) => rotator.run(context, desktop, composed),
        }
    }
}

/// 把桌面复制交来的画面拷回内存，在 CPU 上转好再传回显卡。
struct CpuRotator {
    /// CPU 读得到的一份桌面复制交来的画面。
    staging: ID3D11Texture2D,
    raw: (u32, u32),
    /// 顺时针转几个直角。
    quarters: u32,
    /// 转好的画面，行与行紧挨着。
    turned: Vec<u8>,
}

impl CpuRotator {
    fn new(device: &ID3D11Device, raw: (u32, u32), quarters: u32) -> io::Result<Self> {
        let turned = vec![0; raw.0 as usize * raw.1 as usize * 4];
        Ok(Self { staging: staging(device, raw)?, raw, quarters, turned })
    }

    /// 把 `desktop`（`raw` 大小）转进 `composed`（转后的大小），两张都在 `context` 的设备上。
    fn run(
        &mut self,
        context: &ID3D11DeviceContext,
        desktop: &ID3D11Texture2D,
        composed: &ID3D11Texture2D,
    ) -> windows::core::Result<()> {
        // SAFETY: `staging` 和 `desktop` 尺寸和格式一样；`turned` 正好是 `composed` 一整张的像素。
        unsafe {
            context.CopyResource(&self.staging, desktop);
            read(context, &self.staging, self.raw, |pixels, stride| {
                nv12::rotate_bgra(pixels, stride, self.raw, self.quarters, &mut self.turned);
            })?;
            let turned_width = if self.quarters % 2 == 1 { self.raw.1 } else { self.raw.0 };
            context.UpdateSubresource(composed, 0, None, self.turned.as_ptr().cast(), turned_width * 4, 0);
        }
        Ok(())
    }
}

/// 在 `adapter` 上建采集用的设备，返回设备、上下文和带没带视频支持。没有视频功能的（软件渲染器）上建不出带视频
/// 支持的，改建不带的。
fn create_device(adapter: &IDXGIAdapter1) -> io::Result<(ID3D11Device, ID3D11DeviceContext, bool)> {
    let create = |flags: D3D11_CREATE_DEVICE_FLAG| {
        let (mut device, mut context) = (None, None);
        // SAFETY: 输出参数指向本地变量。
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map(|()| (device, context))
    };
    // 驱动不支持视频时报的错不一定是 `DXGI_ERROR_UNSUPPORTED`，建不出就都改建不带的。
    let (created, video) = match create(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT) {
        Err(err) => {
            tracing::debug!("cannot create a D3D11 device with video support: {err}");
            (create(D3D11_CREATE_DEVICE_BGRA_SUPPORT), false)
        }
        created => (created, true),
    };
    let (device, context) = created.map_err(other)?;
    let device = device.ok_or_else(|| io::Error::other("no D3D11 device"))?;
    let context = context.ok_or_else(|| io::Error::other("no D3D11 context"))?;
    Ok((device, context, video))
}

/// 画好指针的画面怎么缩放、转成 NV12 交给编码器。
enum Convert {
    Gpu(Gpu),
    Cpu(Cpu),
}

/// 显卡的视频处理器把 `composed` 缩放、转进轮流用的 NV12 纹理，交给硬件编码器。
struct Gpu {
    video: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    targets: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next_target: usize,
}

impl Gpu {
    /// # Safety
    ///
    /// `composed`（`source` 大小）在 `device` 上，`device` 建时带着视频支持。
    unsafe fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        composed: &ID3D11Texture2D,
        source: (u32, u32),
        (width, height): (u32, u32),
        rate: DXGI_RATIONAL,
    ) -> io::Result<Self> {
        // SAFETY: 都是 D3D11 的普通调用，对象由返回的 `Gpu` 持有。
        unsafe {
            let video_device = device.cast::<ID3D11VideoDevice>().map_err(other)?;
            let video = context.cast::<ID3D11VideoContext>().map_err(other)?;
            let enumerator =
                video_device.CreateVideoProcessorEnumerator(&content(source, (width, height), rate)).map_err(other)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0).map_err(other)?;
            let input = input_view(&video_device, &enumerator, composed)?;
            let mut targets = Vec::with_capacity(TARGETS);
            for _ in 0..TARGETS {
                let target = texture(device, (width, height), DXGI_FORMAT_NV12, 0)?;
                let view = output_view(&video_device, &enumerator, &target)?;
                targets.push((target, view));
            }
            let dest = letterbox(source, (width, height));
            video.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            video.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dest));
            let full = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
            video.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
            video.VideoProcessorSetStreamColorSpace(&processor, 0, &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 });
            video.VideoProcessorSetOutputColorSpace(
                &processor,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: OUTPUT_COLOR_SPACE },
            );
            Ok(Self { video, processor, input, targets, next_target: 0 })
        }
    }

    /// 转好交给编码器，编码器没收下时返回 `Ok(false)`。
    fn encode(&mut self, encoder: &Encoder, pts: i64) -> windows::core::Result<bool> {
        let (target, view) = &self.targets[self.next_target];
        // SAFETY: 视图和处理器都建在 `video` 的设备上。
        unsafe { blit(&self.video, &self.processor, &self.input, view)? };
        if !encoder.encode_texture(target, pts) {
            return Ok(false);
        }
        self.next_target = (self.next_target + 1) % self.targets.len();
        Ok(true)
    }
}

/// 把 `composed` 拷回内存，在 CPU 上缩放、转成 NV12，交给软件编码器。
struct Cpu {
    /// CPU 读得到的一份 `composed`。
    staging: ID3D11Texture2D,
    source: (u32, u32),
    size: (u32, u32),
    area: Area,
}

impl Cpu {
    fn new(device: &ID3D11Device, source: (u32, u32), size: (u32, u32)) -> io::Result<Self> {
        Ok(Self { staging: staging(device, source)?, source, size, area: Area::letterbox(source, size) })
    }

    /// 转好交给编码器，编码器没收下时返回 `Ok(false)`。
    fn encode(
        &mut self,
        context: &ID3D11DeviceContext,
        composed: &ID3D11Texture2D,
        encoder: &Encoder,
        pts: i64,
    ) -> windows::core::Result<bool> {
        // SAFETY: 两张纹理尺寸和格式一样，都在 `context` 的设备上。
        unsafe { context.CopyResource(&self.staging, composed) };
        let (source, size, area) = (self.source, self.size, self.area);
        read(context, &self.staging, source, |pixels, stride| {
            encoder.encode_nv12(nv12::frame_len(size), pts, |out| {
                nv12::from_bgra(pixels, stride, source, out, size, area);
            })
        })
    }
}

/// 一张 `size` 大小的 BGRA 纹理，CPU 读得到（`read`）。
fn staging(device: &ID3D11Device, (width, height): (u32, u32)) -> io::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    // SAFETY: 描述是本地变量，输出参数指向本地变量。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(other)?;
    staging.ok_or_else(|| io::Error::other("no staging texture"))
}

/// 映射 `staging`（`size` 大小，`staging` 建的），把像素和每行的字节数交给 `use_pixels`，用完放掉映射。
fn read<T>(
    context: &ID3D11DeviceContext,
    staging: &ID3D11Texture2D,
    (width, height): (u32, u32),
    use_pixels: impl FnOnce(&[u8], usize) -> T,
) -> windows::core::Result<T> {
    // SAFETY: 映射出的内存在 Unmap 之前用完，长度不超过最后一行的末尾。
    unsafe {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let stride = mapped.RowPitch as usize;
        let len = stride * (height as usize - 1) + width as usize * 4;
        let used = use_pixels(std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len), stride);
        context.Unmap(staging, 0);
        Ok(used)
    }
}

/// 桌面复制交来的画面要怎么转才是用户看到的方向；没转过时为空。屏幕转了多少，视频处理器就照同样的角度转。
fn turn(rotation: DXGI_MODE_ROTATION) -> Option<D3D11_VIDEO_PROCESSOR_ROTATION> {
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_90),
        DXGI_MODE_ROTATION_ROTATE180 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_180),
        DXGI_MODE_ROTATION_ROTATE270 => Some(D3D11_VIDEO_PROCESSOR_ROTATION_270),
        _ => None,
    }
}

/// 桌面复制交来的画面要顺时针转几个直角才是用户看到的方向：屏幕转了多少就转多少。
fn quarters(rotation: DXGI_MODE_ROTATION) -> u32 {
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => 1,
        DXGI_MODE_ROTATION_ROTATE180 => 2,
        DXGI_MODE_ROTATION_ROTATE270 => 3,
        _ => 0,
    }
}

/// 用户看到的宽高是 `visible` 的屏幕，桌面复制交来的画面的宽高：转了 90° 或 270° 时对调。
fn unrotated_size(visible: (u32, u32), rotation: DXGI_MODE_ROTATION) -> (u32, u32) {
    if [DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE270].contains(&rotation) {
        (visible.1, visible.0)
    } else {
        visible
    }
}

/// 视频处理器的内容描述：逐行的画面，`input` 进、`output` 出，按 `rate` 的帧率。
fn content(input: (u32, u32), output: (u32, u32), rate: DXGI_RATIONAL) -> D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: rate,
        InputWidth: input.0,
        InputHeight: input.1,
        OutputFrameRate: rate,
        OutputWidth: output.0,
        OutputHeight: output.1,
        Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
    }
}

fn input_view(
    video_device: &ID3D11VideoDevice,
    enumerator: &ID3D11VideoProcessorEnumerator,
    texture: &ID3D11Texture2D,
) -> io::Result<ID3D11VideoProcessorInputView> {
    let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
        },
    };
    let mut view = None;
    // SAFETY: 描述是本地变量，输出参数指向本地变量。
    unsafe { video_device.CreateVideoProcessorInputView(texture, enumerator, &desc, Some(&mut view)) }
        .map_err(other)?;
    view.ok_or_else(|| io::Error::other("no video processor input view"))
}

fn output_view(
    video_device: &ID3D11VideoDevice,
    enumerator: &ID3D11VideoProcessorEnumerator,
    texture: &ID3D11Texture2D,
) -> io::Result<ID3D11VideoProcessorOutputView> {
    let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
    };
    let mut view = None;
    // SAFETY: 同上。
    unsafe { video_device.CreateVideoProcessorOutputView(texture, enumerator, &desc, Some(&mut view)) }
        .map_err(other)?;
    view.ok_or_else(|| io::Error::other("no video processor output view"))
}

/// 让 `processor` 把 `input` 处理进 `output`。
///
/// # Safety
///
/// 视图和处理器都是 `video` 那个设备上的。
unsafe fn blit(
    video: &ID3D11VideoContext,
    processor: &ID3D11VideoProcessor,
    input: &ID3D11VideoProcessorInputView,
    output: &ID3D11VideoProcessorOutputView,
) -> windows::core::Result<()> {
    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: true.into(),
        pInputSurface: ManuallyDrop::new(Some(input.clone())),
        ..Default::default()
    };
    // SAFETY: 见上；流的输入视图引用用完放掉。
    unsafe {
        let blitted = video.VideoProcessorBlt(processor, output, 0, std::slice::from_ref(&stream));
        ManuallyDrop::drop(&mut stream.pInputSurface);
        blitted
    }
}

/// 一张 `size` 大小、`format` 格式的纹理，能当视频处理器的输入输出。
fn texture(
    device: &ID3D11Device,
    (width, height): (u32, u32),
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    misc: u32,
) -> io::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: misc,
    };
    let mut texture = None;
    // SAFETY: 描述是本地变量，输出参数指向本地变量。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(other)?;
    texture.ok_or_else(|| io::Error::other("no texture"))
}

/// `Area::letterbox` 写成视频处理器要的矩形。
fn letterbox(source: (u32, u32), target: (u32, u32)) -> RECT {
    let area = Area::letterbox(source, target);
    let (left, top) = (area.left as i32, area.top as i32);
    RECT { left, top, right: left + area.width as i32, bottom: top + area.height as i32 }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, GetDC, ReleaseDC, SRCCOPY, SelectObject,
    };

    use super::*;

    /// 主屏当前的方向下，桌面复制交来的画面宽高是 `unrotated_size`，视频处理器按 `turn` 转过以后和屏幕上看到的
    /// （GDI 截的图）一样，转反了就对不上。要在登录着的桌面上、有视频功能的显卡上跑，把屏幕转到各个方向各跑一次：
    /// `cargo test -p runode-screen rotated_frames -- --ignored --nocapture`。
    #[test]
    #[ignore = "needs a logged-in desktop"]
    #[expect(clippy::print_stderr, reason = "手动跑的核对，各个转法的差距打出来看")]
    fn rotated_frames_match_the_screen() {
        let output = display::find(crate::Display::main().id).unwrap();
        // SAFETY: 都是 D3D11 和 DXGI 的普通调用，对象都在这个函数里。
        unsafe {
            let (mut device, mut context) = (None, None);
            D3D11CreateDevice(
                &output.adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
            let (device, context) = (device.unwrap(), context.unwrap());
            let shot = Shot::take(&output, &device, &context);
            let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
            let candidates: &[D3D11_VIDEO_PROCESSOR_ROTATION] = if shot.turned_upright() {
                &[D3D11_VIDEO_PROCESSOR_ROTATION(0), D3D11_VIDEO_PROCESSOR_ROTATION_180]
            } else {
                &[D3D11_VIDEO_PROCESSOR_ROTATION_90, D3D11_VIDEO_PROCESSOR_ROTATION_270]
            };
            let mut scores = Vec::new();
            for &candidate in candidates {
                let composed = texture(&device, shot.visible, DXGI_FORMAT_B8G8R8A8_UNORM, 0).unwrap();
                let rotator = Rotator::new(
                    &device,
                    &context,
                    (&shot.desktop, shot.raw),
                    (&composed, shot.visible),
                    rate,
                    candidate,
                )
                .unwrap();
                rotator.run().unwrap();
                let score = shot.score(&device, &context, &composed, &format!("video-{}", candidate.0));
                eprintln!("rotation {}, processor rotation {}: difference {score:.1}", shot.rotation.0, candidate.0);
                scores.push((candidate, score));
            }
            let wanted = turn(shot.rotation).unwrap_or(D3D11_VIDEO_PROCESSOR_ROTATION(0));
            assert_eq!(best(&scores), wanted, "rotation {}: {scores:?}", shot.rotation.0);
        }
    }

    /// 主屏当前的方向下，`CpuRotator` 按 `quarters` 转过以后和屏幕上看到的（GDI 截的图）一样。要在登录着的桌面上
    /// 跑，没有视频功能的显卡也行，把屏幕转到各个方向各跑一次：
    /// `cargo test -p runode-screen cpu_turned_frames -- --ignored --nocapture`。
    #[test]
    #[ignore = "needs a logged-in desktop"]
    #[expect(clippy::print_stderr, reason = "手动跑的核对，各个转法的差距打出来看")]
    fn cpu_turned_frames_match_the_screen() {
        let output = display::find(crate::Display::main().id).unwrap();
        // SAFETY: 都是 D3D11 和 DXGI 的普通调用，对象都在这个函数里。
        unsafe {
            let (device, context, _) = create_device(&output.adapter).unwrap();
            let shot = Shot::take(&output, &device, &context);
            let candidates: &[u32] = if shot.turned_upright() { &[0, 2] } else { &[1, 3] };
            let mut scores = Vec::new();
            for &candidate in candidates {
                let composed = texture(&device, shot.visible, DXGI_FORMAT_B8G8R8A8_UNORM, 0).unwrap();
                let mut rotator = CpuRotator::new(&device, shot.raw, candidate).unwrap();
                rotator.run(&context, &shot.desktop, &composed).unwrap();
                let score = shot.score(&device, &context, &composed, &format!("cpu-{candidate}"));
                eprintln!("rotation {}, quarters {candidate}: difference {score:.1}", shot.rotation.0);
                scores.push((candidate, score));
            }
            assert_eq!(best(&scores), quarters(shot.rotation), "rotation {}: {scores:?}", shot.rotation.0);
        }
    }

    /// 主屏同一时刻的两份画面：桌面复制交来的（没转的方向）和 GDI 截的（用户看到的方向）。
    struct Shot {
        rotation: DXGI_MODE_ROTATION,
        visible: (u32, u32),
        raw: (u32, u32),
        desktop: ID3D11Texture2D,
        screen: Vec<u8>,
    }

    impl Shot {
        /// # Safety
        ///
        /// `device` 建在 `output` 的显卡上，`context` 是它的。
        unsafe fn take(output: &Output, device: &ID3D11Device, context: &ID3D11DeviceContext) -> Self {
            // SAFETY: 都是 D3D11 和 DXGI 的普通调用，对象都在这个函数里或者交给返回值。
            unsafe {
                let rotation = output.output.GetDesc().unwrap().Rotation;
                let rect = output.rect;
                let visible = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);
                let duplication = output.output.DuplicateOutput(device).unwrap();
                let raw = unrotated_size(visible, rotation);
                let desktop = texture(device, raw, DXGI_FORMAT_B8G8R8A8_UNORM, 0).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                    let mut resource: Option<IDXGIResource> = None;
                    if duplication.AcquireNextFrame(500, &mut info, &mut resource).is_ok() {
                        let texture = resource.unwrap().cast::<ID3D11Texture2D>().unwrap();
                        let mut desc = D3D11_TEXTURE2D_DESC::default();
                        texture.GetDesc(&mut desc);
                        let fresh = info.LastPresentTime != 0;
                        if fresh {
                            assert_eq!((desc.Width, desc.Height), raw, "the duplicated frame's size");
                            context.CopyResource(&desktop, &texture);
                        }
                        duplication.ReleaseFrame().unwrap();
                        if fresh {
                            break;
                        }
                    }
                    assert!(Instant::now() < deadline, "no frame from the desktop duplication");
                }
                let screen = screen_pixels(rect, visible);
                if let Some(dir) = std::env::var_os("RUNODE_ROTATION_SHOTS") {
                    save_bmp(&std::path::Path::new(&dir).join(format!("screen-{}.bmp", rotation.0)), &screen, visible);
                }
                Self { rotation, visible, raw, desktop, screen }
            }
        }

        /// 桌面复制交来的画面和屏幕同向，只可能没转或者转了 180°；正方形的屏幕分不出来，当成转了直角。
        fn turned_upright(&self) -> bool {
            self.raw == self.visible && self.visible.0 != self.visible.1
        }

        /// `composed`（`visible` 大小）和屏幕差多少；设了 `RUNODE_ROTATION_SHOTS` 时存成 `name` 的图。
        ///
        /// # Safety
        ///
        /// `composed` 在 `device` 上。
        unsafe fn score(
            &self,
            device: &ID3D11Device,
            context: &ID3D11DeviceContext,
            composed: &ID3D11Texture2D,
            name: &str,
        ) -> f64 {
            // SAFETY: 调用方保证纹理在这个设备上。
            let turned = unsafe { texture_pixels(device, context, composed, self.visible) };
            if let Some(dir) = std::env::var_os("RUNODE_ROTATION_SHOTS") {
                let name = format!("turned-{}-{name}.bmp", self.rotation.0);
                save_bmp(&std::path::Path::new(&dir).join(name), &turned, self.visible);
            }
            difference(&turned, &self.screen, self.visible)
        }
    }

    /// 差距最小的那个转法；最好的和最差的要拉得开，不然屏幕上的东西太少，分不出来。
    fn best<T: Copy + std::fmt::Debug>(scores: &[(T, f64)]) -> T {
        let (best, best_score) = scores.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        let worst = scores.iter().map(|score| score.1).fold(0.0, f64::max);
        assert!(best_score * 3.0 < worst, "the candidates are too close to tell apart: {scores:?}");
        best
    }

    /// 屏幕上 `rect` 那一块，BGRA，自上而下。
    unsafe fn screen_pixels(rect: RECT, (width, height): (u32, u32)) -> Vec<u8> {
        // SAFETY: GDI 的普通调用，建的对象都在这里删掉。
        unsafe {
            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(memory, bitmap.into());
            BitBlt(memory, 0, 0, width as i32, height as i32, Some(screen), rect.left, rect.top, SRCCOPY | CAPTUREBLT)
                .unwrap();
            let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize).to_vec();
            SelectObject(memory, old);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            pixels
        }
    }

    /// 读回 `texture` 的像素，BGRA，自上而下。
    unsafe fn texture_pixels(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
        (width, height): (u32, u32),
    ) -> Vec<u8> {
        // SAFETY: D3D11 的普通调用；映射的内存按行距读完就解除映射。
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging)).unwrap();
            let staging = staging.unwrap();
            context.CopyResource(&staging, texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
            let mut pixels = Vec::with_capacity((width * height * 4) as usize);
            for row in 0..height as usize {
                let start = mapped.pData.cast::<u8>().add(row * mapped.RowPitch as usize);
                pixels.extend_from_slice(std::slice::from_raw_parts(start, width as usize * 4));
            }
            context.Unmap(&staging, 0);
            pixels
        }
    }

    /// 把 BGRA 图缩成四分之一存成 BMP，看转得对不对。
    fn save_bmp(path: &std::path::Path, pixels: &[u8], (width, height): (u32, u32)) {
        let (w, h) = (width / 4, height / 4);
        let mut rows = Vec::with_capacity((w * h * 4) as usize);
        // BMP 的行自下而上。
        for y in (0..h).rev() {
            for x in 0..w {
                let at = ((y * 4 * width + x * 4) * 4) as usize;
                rows.extend_from_slice(&pixels[at..at + 4]);
            }
        }
        let mut file = Vec::new();
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&(54 + rows.len() as u32).to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&(w as i32).to_le_bytes());
        file.extend_from_slice(&(h as i32).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&[0; 24]);
        file.extend_from_slice(&rows);
        std::fs::write(path, file).unwrap();
    }

    /// 两张同样大小的 BGRA 图分成 32×32 格，各格平均亮度差的平均。
    fn difference(a: &[u8], b: &[u8], (width, height): (u32, u32)) -> f64 {
        const CELLS: usize = 32;
        let (width, height) = (width as usize, height as usize);
        let cells = |pixels: &[u8]| {
            let mut sums = vec![(0.0f64, 0u32); CELLS * CELLS];
            for y in 0..height {
                for x in 0..width {
                    let at = (y * width + x) * 4;
                    let light = f64::from(pixels[at]) + f64::from(pixels[at + 1]) + f64::from(pixels[at + 2]);
                    let cell = &mut sums[(y * CELLS / height) * CELLS + x * CELLS / width];
                    cell.0 += light;
                    cell.1 += 1;
                }
            }
            sums.into_iter().map(|(sum, count)| sum / f64::from(count.max(1))).collect::<Vec<_>>()
        };
        let (a, b) = (cells(a), cells(b));
        a.iter().zip(&b).map(|(a, b)| (a - b).abs()).sum::<f64>() / a.len() as f64
    }

    #[test]
    fn rotated_displays_swap_the_duplicated_size() {
        assert_eq!(unrotated_size((1080, 1920), DXGI_MODE_ROTATION_ROTATE90), (1920, 1080));
        assert_eq!(unrotated_size((1080, 1920), DXGI_MODE_ROTATION_ROTATE270), (1920, 1080));
        assert_eq!(unrotated_size((1920, 1080), DXGI_MODE_ROTATION_ROTATE180), (1920, 1080));
        assert_eq!(unrotated_size((1920, 1080), DXGI_MODE_ROTATION(1)), (1920, 1080));
    }

    #[test]
    fn letterbox_keeps_the_aspect_ratio() {
        let rect = letterbox((2560, 1440), (1280, 720));
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (0, 0, 1280, 720));
        let rect = letterbox((1920, 1200), (1280, 720));
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (64, 0, 1216, 720));
    }
}
