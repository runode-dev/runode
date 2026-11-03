//! 用 ScreenCaptureKit 采集一块屏幕，交给 `Encoder`。屏幕没变时 ScreenCaptureKit 不出带画面的帧，静止时几乎不占
//! 带宽；停下以后把最后一张补清楚（`Refiner`）。编码在自己的队列上，只编最新的一张：有的机器（Intel 的 Mac）交一帧
//! 给编码器要等几十毫秒，编不过来时中间的帧丢掉，帧率降下来，不让画面在采集的队列里越排越久。

use std::{
    io,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained, DispatchTime};
use objc2::{
    AllocAnyThread as _, DefinedClass as _, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMClock, CMSampleBuffer, CMTime};
use objc2_core_video::{CVImageBuffer, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput,
    SCStreamOutputType,
};

use super::{
    awake::KeepAwake,
    encoder::{Encoder, Sink},
};
use crate::{
    Codec, Event, Options,
    refine::{REPEAT_DELAY, Refiner},
};

/// 等 ScreenCaptureKit 的完成回调最多这么久。
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10);
/// `SCStreamConfiguration::queueDepth` 不该超过的帧数，ScreenCaptureKit 的文档这样要求。
const MAX_QUEUE_DEPTH: isize = 8;

/// 开着的采集，丢掉时停下。
pub struct Capture {
    stream: Retained<SCStream>,
    latest: Arc<Latest>,
    _output: Retained<Output>,
    /// 编码的队列（`Latest::queue`）；ScreenCaptureKit 交帧在 `_capture_queue` 上。
    queue: DispatchRetained<DispatchQueue>,
    _capture_queue: DispatchRetained<DispatchQueue>,
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
            Encoder::new(options.width, options.height, options.fps, options.bitrate, &options.codecs, sink.clone())?;
        encoder.request_keyframe();
        let queue = DispatchQueue::new("dev.runode.screen.encode", None);
        let capture_queue = DispatchQueue::new("dev.runode.screen", None);
        let latest = Arc::new(Latest {
            encoder,
            last: Mutex::new(None),
            stopped: AtomicBool::new(false),
            refiner: Mutex::new(Refiner::default()),
            refine_scheduled: AtomicBool::new(false),
            queue: queue.clone(),
            pending: Mutex::new(None),
            draining: AtomicBool::new(false),
        });
        let output = Output::new(Ivars { latest: latest.clone(), sink });
        // SAFETY: 以下都是 ScreenCaptureKit 的普通调用，对象都由 Retained 持有到 `Capture` 丢掉。
        unsafe {
            let filter =
                SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new());
            let config = SCStreamConfiguration::new();
            config.setWidth(options.width as usize);
            config.setHeight(options.height as usize);
            config.setMinimumFrameInterval(CMTime::new(1, options.fps.max(1) as i32));
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setShowsCursor(options.cursor);
            // `Latest` 一直留着一张，多要一张免得 ScreenCaptureKit 没有空的缓冲可写，但不过它的上限。
            config.setQueueDepth((config.queueDepth() + 1).min(MAX_QUEUE_DEPTH));
            let delegate = ProtocolObject::<dyn SCStreamDelegate>::from_ref(&*output);
            let stream =
                SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, Some(delegate));
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&capture_queue),
                )
                .map_err(|error| io::Error::other(error.localizedDescription().to_string()))?;
            let (tx, rx) = mpsc::channel();
            let done = RcBlock::new(move |error: *mut NSError| {
                let _ = tx.send(error_text(error));
            });
            stream.startCaptureWithCompletionHandler(Some(&done));
            wait(&rx, "start the screen capture")?;
            Ok(Self { stream, latest, _output: output, queue, _capture_queue: capture_queue, _awake: awake })
        }
    }

    /// 出一帧带参数集的关键帧：手机丢了帧、刚连上要从头解时用。屏幕静止时也出，重编最近一张画面。
    pub fn request_keyframe(&self) {
        self.latest.request_keyframe(&self.queue);
    }

    /// 在 `on_event` 里要关键帧用的句柄，不让采集活下去。
    pub fn keyframes(&self) -> Keyframes {
        Keyframes { latest: Arc::downgrade(&self.latest), queue: self.queue.clone() }
    }

    /// 改平均码率，单位 bit/s。
    pub fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        self.latest.encoder.set_bitrate(bitrate)
    }

    /// 编码器选中的编码。
    pub fn codec(&self) -> Codec {
        self.latest.encoder.codec()
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
        // `request_keyframe` 排着的重编不再做，正在做的等它做完，编码器里还没交出的帧也等它交出，
        // 丢掉以后才不再出帧。
        self.latest.stopped.store(true, Ordering::Relaxed);
        self.queue.exec_sync(|| {});
        self.latest.encoder.complete();
    }
}

/// 向采集要关键帧，编码回调里也能用：编码器最后一份要是在编码回调里丢掉，就在回调里等它交完帧，
/// 等不到，所以回调只持有弱引用，到编码的队列上才升成强引用。
#[derive(Clone)]
pub struct Keyframes {
    latest: Weak<Latest>,
    queue: DispatchRetained<DispatchQueue>,
}

impl Keyframes {
    /// 出一帧带参数集的关键帧，屏幕静止时也出；采集已经丢掉时什么都不做。
    pub fn request(&self) {
        let (latest, queue) = (self.latest.clone(), self.queue.clone());
        self.queue.exec_async(move || {
            if let Some(latest) = latest.upgrade() {
                latest.request_keyframe(&queue);
            }
        });
    }
}

/// 编码器和最近交给它的画面、时间戳；画面只在编码的队列（`queue`）上编。
struct Latest {
    encoder: Encoder,
    last: Mutex<Option<(CFRetained<CVImageBuffer>, CMTime)>>,
    stopped: AtomicBool,
    refiner: Mutex<Refiner>,
    /// 排着一次 `refine`，同时只排一次。
    refine_scheduled: AtomicBool,
    queue: DispatchRetained<DispatchQueue>,
    /// 采到了、还没编的最新一张；编码器忙时新来的顶掉它。
    pending: Mutex<Option<(CFRetained<CVImageBuffer>, CMTime)>>,
    /// 编码的队列上排着或正在跑 `drain`。
    draining: AtomicBool,
}

// SAFETY: CVPixelBuffer 可以在任意线程上读，这里只在锁里碰它；`Encoder` 本身是 Send + Sync。
unsafe impl Send for Latest {}
// SAFETY: 同上。
unsafe impl Sync for Latest {}

impl Latest {
    /// 硬要一帧关键帧，过 `REPEAT_DELAY` 还没出时重编最近一张。
    fn request_keyframe(self: &Arc<Self>, queue: &DispatchQueue) {
        self.encoder.request_keyframe();
        let latest = self.clone();
        let when = DispatchTime::try_from(REPEAT_DELAY).unwrap_or(DispatchTime::NOW);
        let _ = queue.after(when, move || latest.encode_again());
    }

    /// ScreenCaptureKit 交来一张新画面：记成最新的一张，编码的队列闲着时叫它来编。
    fn offer(self: &Arc<Self>, image: CFRetained<CVImageBuffer>, pts: CMTime) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some((image, pts));
        if !self.draining.swap(true, Ordering::AcqRel) {
            let latest = self.clone();
            self.queue.exec_async(move || latest.drain());
        }
    }

    /// 在编码的队列上把最新的一张编掉，编的时候又来了就接着编，直到没有新的。
    fn drain(self: &Arc<Self>) {
        loop {
            let next = self.pending.lock().unwrap_or_else(PoisonError::into_inner).take();
            match next {
                Some((image, pts)) => {
                    if !self.stopped.load(Ordering::Relaxed) {
                        self.encode(image, pts);
                    }
                }
                None => {
                    self.draining.store(false, Ordering::Release);
                    // 放下标志之前刚到的那张没人叫来编，这里接着编；别人已经又叫了就交给它。
                    let more = self.pending.lock().unwrap_or_else(PoisonError::into_inner).is_some();
                    if !more || self.draining.swap(true, Ordering::AcqRel) {
                        return;
                    }
                }
            }
        }
    }

    /// 编一张新画面，停下以后补清楚。
    fn encode(self: &Arc<Self>, image: CFRetained<CVImageBuffer>, pts: CMTime) {
        {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            let pts = match &*last {
                // SAFETY: 时间戳的算术。
                Some((_, previous)) => unsafe { pts.maximum(after(*previous)) },
                None => pts,
            };
            self.encoder.boost(false);
            self.encoder.encode(&image, pts);
            *last = Some((image, pts));
        }
        let now = Instant::now();
        let wait = {
            let mut refiner = self.refiner.lock().unwrap_or_else(PoisonError::into_inner);
            refiner.changed(now);
            refiner.wait(now)
        };
        if let Some(wait) = wait {
            self.schedule_refine(wait);
        }
    }

    /// 屏幕静止时 ScreenCaptureKit 不出新帧，要的关键帧还没出时把最近一张再编一次。
    fn encode_again(self: &Arc<Self>) {
        if self.stopped.load(Ordering::Relaxed) || !self.encoder.keyframe_requested() {
            return;
        }
        // 要关键帧多半是观看端丢了帧、网络不好，不按补清楚时放大的码率编。
        self.encoder.boost(false);
        self.encode_last();
        // 这一帧按原来的码率编，补清楚的画面又糊了，和新画面一样停下以后重新补。
        let now = Instant::now();
        let wait = {
            let mut refiner = self.refiner.lock().unwrap_or_else(PoisonError::into_inner);
            refiner.changed(now);
            refiner.wait(now)
        };
        if let Some(wait) = wait {
            self.schedule_refine(wait);
        }
    }

    /// 把最近一张再编一次，时间戳是现在：观看端按时间戳和到达的时刻估网络排队，重编的帧不能显得晚到了。
    fn encode_last(&self) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((image, pts)) = &mut *last {
            // SAFETY: 读主机时钟（ScreenCaptureKit 的时间戳也按它），时间戳的算术。
            *pts = unsafe { CMClock::host_time_clock().time().maximum(after(*pts)) };
            self.encoder.encode(image, *pts);
        }
    }

    /// 过 `wait` 在编码的队列上看要不要补一帧；已经排着一次时不再排，到时它自己按最新的情况再排。
    fn schedule_refine(self: &Arc<Self>, wait: Duration) {
        if self.refine_scheduled.swap(true, Ordering::Relaxed) {
            return;
        }
        let latest = self.clone();
        let when = DispatchTime::try_from(wait).unwrap_or(DispatchTime::NOW);
        let _ = self.queue.after(when, move || {
            latest.refine_scheduled.store(false, Ordering::Relaxed);
            latest.refine();
        });
    }

    /// 到了该补的时候按放大的码率把最近一张再编一次，再排下一次。
    fn refine(self: &Arc<Self>) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        let mut refiner = self.refiner.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(wait) = refiner.wait(now) else { return };
        if wait.is_zero() {
            self.encoder.boost(true);
            self.encode_last();
            refiner.refined(now);
        }
        match refiner.wait(now) {
            Some(wait) => {
                drop(refiner);
                self.schedule_refine(wait);
            }
            // 补够了，回到原来的码率。
            None => self.encoder.boost(false),
        }
    }
}

/// `pts` 之后一毫秒：编码器要时间戳一直往后走，重编的画面排在上一帧之后。
fn after(pts: CMTime) -> CMTime {
    // SAFETY: 时间戳的算术。
    unsafe { pts.add(CMTime::new(1, 1000)) }
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
        .map_err(|err| {
            crate::caused_error(
                crate::StopCause::NoScreenCapture,
                io::ErrorKind::PermissionDenied,
                format!("cannot list the displays: {err}"),
            )
        })?;
    // SAFETY: 读列表里的屏幕。
    let displays = unsafe { content.displays() };
    (0..displays.count())
        .map(|index| displays.objectAtIndex(index))
        // SAFETY: 读屏幕的编号。
        .find(|display| unsafe { display.displayID() } == id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no display {id}")))
}

struct Ivars {
    latest: Arc<Latest>,
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
                    self.ivars().latest.offer(image, sample.presentation_time_stamp());
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
