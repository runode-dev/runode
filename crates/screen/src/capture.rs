//! 用 ScreenCaptureKit 采集一块屏幕，每帧交给 `Encoder`。屏幕没变时 ScreenCaptureKit 不出带画面的
//! 帧，静止时几乎不占带宽。

use std::{
    io,
    sync::{Arc, mpsc},
    time::Duration,
};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    AllocAnyThread as _, DefinedClass as _, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput,
    SCStreamOutputType,
};

use crate::{
    Event,
    awake::KeepAwake,
    encoder::{Encoder, Sink},
};

/// 等 ScreenCaptureKit 的完成回调最多这么久。
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);

/// 怎么采集。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// 采哪块屏幕，见 `Display`。
    pub display: u32,
    /// 画面的像素尺寸；和屏幕的宽高比不同时 ScreenCaptureKit 留黑边。
    pub width: u32,
    pub height: u32,
    /// 帧率上限。
    pub fps: u32,
    /// 平均码率，单位 bit/s。
    pub bitrate: u32,
}

/// 开着的采集，丢掉时停下。
pub struct Capture {
    stream: Retained<SCStream>,
    encoder: Arc<Encoder>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
    _awake: KeepAwake,
}

// SAFETY: SCStream 的方法可以在任意线程上调用；`Encoder` 本身是 Send + Sync，其余字段只为持有。
unsafe impl Send for Capture {}
// SAFETY: 同上。
unsafe impl Sync for Capture {}

impl Capture {
    /// 开始采集，`on_event` 在 ScreenCaptureKit 和 VideoToolbox 的线程上收到编好的帧和停下的原因。
    /// 第一帧是带参数集的关键帧。没有屏幕录制的授权、屏幕不存在时返回错误。
    pub fn start(options: &Options, on_event: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Self> {
        let sink: Arc<Sink> = Arc::new(on_event);
        let awake = KeepAwake::acquire();
        let display = display(options.display)?;
        let encoder =
            Arc::new(Encoder::new(options.width, options.height, options.fps, options.bitrate, sink.clone())?);
        encoder.request_keyframe();
        let output = Output::new(Ivars { encoder: encoder.clone(), sink });
        let queue = DispatchQueue::new("dev.runode.screen", None);
        // SAFETY: 以下都是 ScreenCaptureKit 的普通调用，对象都由 Retained 持有到 `Capture` 丢掉。
        unsafe {
            let filter =
                SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new());
            let config = SCStreamConfiguration::new();
            config.setWidth(options.width as usize);
            config.setHeight(options.height as usize);
            config.setMinimumFrameInterval(CMTime::new(1, options.fps.max(1) as i32));
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setShowsCursor(true);
            let delegate = ProtocolObject::<dyn SCStreamDelegate>::from_ref(&*output);
            let stream =
                SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(delegate));
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|error| io::Error::other(error.localizedDescription().to_string()))?;
            let (tx, rx) = mpsc::channel();
            let done = RcBlock::new(move |error: *mut NSError| {
                let _ = tx.send(error_text(error));
            });
            stream.startCaptureWithCompletionHandler(Some(&done));
            wait(&rx, "start the screen capture")?;
            Ok(Self { stream, encoder, _output: output, _queue: queue, _awake: awake })
        }
    }

    /// 下一帧编成带参数集的关键帧：手机丢了帧、刚连上要从头解时用。
    pub fn request_keyframe(&self) {
        self.encoder.request_keyframe();
    }

    /// 改平均码率，单位 bit/s。
    pub fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        self.encoder.set_bitrate(bitrate)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send(error_text(error));
        });
        // SAFETY: 停下这条流；等它停好，丢掉以后 `on_event` 不再收到帧。
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&done)) };
        if let Err(err) = wait(&rx, "stop the screen capture") {
            tracing::warn!("{err}");
        }
    }
}

fn error_text(error: *mut NSError) -> Result<(), String> {
    // SAFETY: 回调给的错误要么为空，要么在回调期间有效。
    match unsafe { error.as_ref() } {
        Some(error) => Err(format!("{} ({})", error.localizedDescription(), error.code())),
        None => Ok(()),
    }
}

fn wait(rx: &mpsc::Receiver<Result<(), String>>, what: &str) -> io::Result<()> {
    match rx.recv_timeout(CALLBACK_TIMEOUT) {
        Ok(result) => result.map_err(|err| io::Error::other(format!("cannot {what}: {err}"))),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, format!("cannot {what}: no answer"))),
    }
}

/// 按 `CGDirectDisplayID` 找 ScreenCaptureKit 的屏幕；问它时系统核对屏幕录制的授权。
fn display(id: u32) -> io::Result<Retained<SCDisplay>> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: 回调给的内容要么为空，要么有效，这里多持有一份。
        let content = unsafe { Retained::retain(content) };
        let _ = tx.send(content.ok_or_else(|| error_text(error).err().unwrap_or_default()));
    });
    // 只列屏上的窗口：列全部窗口要几百毫秒，这里只要屏幕。
    // SAFETY: ScreenCaptureKit 自己复制回调块。
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &handler,
        )
    };
    let content = rx
        .recv_timeout(CALLBACK_TIMEOUT)
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "cannot list the displays: no answer"))?
        .map_err(|err| io::Error::new(io::ErrorKind::PermissionDenied, format!("cannot list the displays: {err}")))?;
    // SAFETY: 读列表里的屏幕。
    let displays = unsafe { content.displays() };
    (0..displays.count())
        .map(|index| displays.objectAtIndex(index))
        // SAFETY: 读屏幕的编号。
        .find(|display| unsafe { display.displayID() } == id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no display {id}")))
}

struct Ivars {
    encoder: Arc<Encoder>,
    sink: Arc<Sink>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // SAFETY: 回调期间样本有效；屏幕没变时样本不带画面，跳过。
            unsafe {
                if let Some(image) = sample.image_buffer() {
                    self.ivars().encoder.encode(&image, sample.presentation_time_stamp());
                }
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            (self.ivars().sink)(Event::Stopped(format!("{} ({})", error.localizedDescription(), error.code())));
        }
    }
);

impl Output {
    fn new(ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        // SAFETY: NSObject 的 init。
        unsafe { msg_send![super(this), init] }
    }
}
