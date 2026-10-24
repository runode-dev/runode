//! 用 DXGI 桌面复制采集一块屏幕，在单独的线程上：桌面复制给的画面不带鼠标指针，先拷进一张能用 GDI
//! 画的纹理、画上指针，再用 D3D11 的视频处理器缩放、转成 NV12 交给 `Encoder`。屏幕和指针都没动时
//! 桌面复制不出新画面，静止时几乎不占带宽。

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
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_RESOURCE_MISC_GDI_COMPATIBLE, D3D11_SDK_VERSION,
                D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_COLOR_SPACE,
                D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
                D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
                D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
                ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
                ID3D11VideoProcessorInputView, ID3D11VideoProcessorOutputView,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC},
                DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIOutputDuplication,
                IDXGIResource, IDXGISurface1,
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
use crate::{Event, Options};

/// 等新画面最多这么久就回来看一眼要不要停、要不要重编。
const FRAME_WAIT: Duration = Duration::from_millis(100);
/// 编码器没收下的画面过这么久再试。
const RETRY_WAIT: Duration = Duration::from_millis(5);
/// 要了关键帧这么久还没有新画面把它带出来，就重编最近一张。
const REPEAT_DELAY: Duration = Duration::from_millis(100);
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
    /// 关键帧。屏幕不存在、显卡上没有硬件 HEVC 编码器时返回错误。
    pub fn start(options: &Options, on_event: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Self> {
        let sink: Arc<Sink> = Arc::new(on_event);
        let options = *options;
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
    /// 最近一张桌面画面，不带指针。
    desktop: ID3D11Texture2D,
    /// 画上了指针的那张，能用 GDI 画。
    composed: ID3D11Texture2D,
    video: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    targets: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next_target: usize,
    /// 拿到过桌面复制交来的真画面（`LastPresentTime` 不为零）。
    have_image: bool,
    state: Arc<State>,
    started: Instant,
    last_pts: i64,
}

impl Pipeline {
    fn open(options: &Options, sink: Arc<Sink>) -> io::Result<Self> {
        let output = display::find(options.display)?;
        let (width, height) = (options.width.max(2) & !1, options.height.max(2) & !1);
        let source = (
            (output.rect.right - output.rect.left).max(1) as u32,
            (output.rect.bottom - output.rect.top).max(1) as u32,
        );
        // SAFETY: 以下都是 D3D11 的普通调用，对象都由 `Pipeline` 持有。
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
            .map_err(other)?;
            let device = device.ok_or_else(|| io::Error::other("no D3D11 device"))?;
            let context = context.ok_or_else(|| io::Error::other("no D3D11 context"))?;
            // 编码器在它自己的线程上也用这个设备。
            let _ = device.cast::<ID3D11Multithread>().map_err(other)?.SetMultithreadProtected(true);
            let adapter = output.adapter.GetDesc1().map_err(other)?.AdapterLuid;
            let encoder = Encoder::new(&device, adapter, (width, height), options.fps, options.bitrate, sink)?;
            let bgra = |misc: u32| -> io::Result<ID3D11Texture2D> {
                texture(&device, source, DXGI_FORMAT_B8G8R8A8_UNORM, misc)
            };
            let desktop = bgra(0)?;
            let composed = bgra(D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32)?;
            let video_device = device.cast::<ID3D11VideoDevice>().map_err(other)?;
            let video = context.cast::<ID3D11VideoContext>().map_err(other)?;
            let rate = DXGI_RATIONAL { Numerator: options.fps.max(1), Denominator: 1 };
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: source.0,
                InputHeight: source.1,
                OutputFrameRate: rate,
                OutputWidth: width,
                OutputHeight: height,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator: ID3D11VideoProcessorEnumerator =
                video_device.CreateVideoProcessorEnumerator(&content).map_err(other)?;
            let processor = video_device.CreateVideoProcessor(&enumerator, 0).map_err(other)?;
            let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                },
            };
            let mut input = None;
            video_device
                .CreateVideoProcessorInputView(&composed, &enumerator, &input_desc, Some(&mut input))
                .map_err(other)?;
            let input = input.ok_or_else(|| io::Error::other("no video processor input view"))?;
            let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut targets = Vec::with_capacity(TARGETS);
            for _ in 0..TARGETS {
                let target = texture(&device, (width, height), DXGI_FORMAT_NV12, 0)?;
                let mut view = None;
                video_device
                    .CreateVideoProcessorOutputView(&target, &enumerator, &output_desc, Some(&mut view))
                    .map_err(other)?;
                targets.push((target, view.ok_or_else(|| io::Error::other("no video processor output view"))?));
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
                desktop,
                composed,
                video,
                processor,
                input,
                targets,
                next_target: 0,
                have_image: false,
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
        // 编码器出错停了时它自己报过 `Event::Stopped`。
        while !self.state.stop.load(Ordering::Relaxed) && !self.state.encoder.stopped() {
            let wait = if pending { RETRY_WAIT } else { FRAME_WAIT };
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
            if pending || repeat {
                if self.encode() {
                    pending = false;
                    last_encoded = Instant::now();
                } else {
                    pending = true;
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

    /// 重连桌面复制，连不上时先不管，过一会儿再试。屏幕的尺寸变了（改了分辨率、转了屏）时报错：
    /// 纹理和视频处理器是按原来的尺寸建的。
    fn reopen(&mut self) -> io::Result<()> {
        // SAFETY: 输出和设备都活着。
        let duplication = match unsafe { self.output.output.DuplicateOutput(&self.device) } {
            Ok(duplication) => duplication,
            Err(err) => {
                tracing::debug!("cannot capture the screen yet: {err}");
                return Ok(());
            }
        };
        // SAFETY: 读输出的描述。
        let rect = unsafe { self.output.output.GetDesc() }.map_err(other)?.DesktopCoordinates;
        let size = |rect: RECT| (rect.right - rect.left, rect.bottom - rect.top);
        if size(rect) != size(self.output.rect) {
            return Err(io::Error::other("the display changed its resolution"));
        }
        self.output.rect = rect;
        self.duplication = Some(duplication);
        self.state.encoder.request_keyframe();
        Ok(())
    }

    /// 画上指针、转成 NV12 交给编码器；编码器没收下时返回 false。
    fn encode(&mut self) -> bool {
        let (target, view) = self.targets[self.next_target].clone();
        // SAFETY: 纹理和视图都在这个设备上；流的输入视图引用用完放掉。
        unsafe {
            self.context.CopyResource(&self.composed, &self.desktop);
            self.draw_cursor();
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(self.input.clone())),
                ..Default::default()
            };
            let blitted = self.video.VideoProcessorBlt(&self.processor, &view, 0, std::slice::from_ref(&stream));
            ManuallyDrop::drop(&mut stream.pInputSurface);
            if let Err(err) = blitted {
                tracing::warn!("cannot convert a screen frame: {err}");
                return true;
            }
        }
        // 时间戳一直往后走，重编的画面排在上一帧之后。
        let pts = (self.started.elapsed().as_nanos() / 100) as i64;
        let pts = pts.max(self.last_pts + 1);
        if !self.state.encoder.encode(&target, pts) {
            return false;
        }
        self.last_pts = pts;
        self.next_target = (self.next_target + 1) % self.targets.len();
        true
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

/// 把 `source` 等比缩放后居中放进 `target` 的位置，宽高比不同时两边留黑边。
fn letterbox(source: (u32, u32), target: (u32, u32)) -> RECT {
    let (sw, sh) = (f64::from(source.0), f64::from(source.1));
    let (tw, th) = (f64::from(target.0), f64::from(target.1));
    let scale = (tw / sw).min(th / sh);
    let (w, h) = ((sw * scale).round() as i32, (sh * scale).round() as i32);
    let (left, top) = ((target.0 as i32 - w) / 2, (target.1 as i32 - h) / 2);
    RECT { left, top, right: left + w, bottom: top + h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_keeps_the_aspect_ratio() {
        let rect = letterbox((2560, 1440), (1280, 720));
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (0, 0, 1280, 720));
        let rect = letterbox((1920, 1200), (1280, 720));
        assert_eq!((rect.left, rect.top, rect.right, rect.bottom), (64, 0, 1216, 720));
    }
}
