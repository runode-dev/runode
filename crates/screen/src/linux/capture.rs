//! 采集的主循环，各后端共用：后端（`Source`）交来 BGRx 的整屏画面，画面变了才缩放、转成 NV12（`nv12`），交给
//! 编码器（`encoder`）。屏幕不变时不编，停下一会儿后把最后一张补清楚（`Refiner`）；要了关键帧时屏幕静止也重编
//! 最近一张。帧率按 `Options::fps` 限，后端不用自己限。

use std::{
    io,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use super::encoder::{self, VideoEncoder};
use crate::{
    Codec, Event, Options,
    nv12::{self, Area},
    refine::Refiner,
};

/// 等新画面最多这么久就回来看一眼要不要停、要不要重编。
const FRAME_WAIT: Duration = Duration::from_millis(100);

/// 后端交来的一张画面：BGRx（每像素四字节，蓝绿红，第四个字节不管），每行 `stride` 字节。
pub(crate) struct Picture<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) stride: usize,
    pub(crate) size: (u32, u32),
}

/// 一个后端的屏幕画面。
pub(crate) trait Source: Send {
    /// 最多等 `timeout`，屏幕（连同画上的指针）变了时为真，新画面由 `picture` 取。出错时采集停下，错误交给观看端。
    fn wait(&mut self, timeout: Duration) -> io::Result<bool>;

    /// 最近一张画面；还没有时为空。
    fn picture(&self) -> Option<Picture<'_>>;
}

/// 开着的采集，丢掉时停下。
pub struct Capture {
    control: Arc<Control>,
    codec: Codec,
    thread: Option<JoinHandle<()>>,
}

/// 采集线程和别处共用的。
struct Control {
    stop: AtomicBool,
    keyframe: AtomicBool,
    /// 观看端要的平均码率（bit/s），`rate_changed` 时采集线程交给编码器。
    bitrate: AtomicU32,
    rate_changed: AtomicBool,
}

impl Capture {
    /// 开始采集，`on_event` 在采集线程上收到编好的帧和停下的原因。第一帧是带参数集的关键帧。屏幕不存在、后端
    /// 开不起来时返回错误；编码器编不了观看端解得了的格式时，错误是 `io::ErrorKind::Unsupported`。
    pub fn start(options: &Options, on_event: impl Fn(Event) + Send + Sync + 'static) -> io::Result<Self> {
        let control = Arc::new(Control {
            stop: AtomicBool::new(false),
            keyframe: AtomicBool::new(true),
            bitrate: AtomicU32::new(options.bitrate),
            rate_changed: AtomicBool::new(false),
        });
        let options = options.clone();
        let (tx, rx) = mpsc::channel();
        let shared = control.clone();
        let thread = thread::Builder::new().name("screen-capture".into()).spawn(move || {
            let opened = super::open_source(options.display, options.cursor).and_then(|source| {
                let size = (options.width.max(2) & !1, options.height.max(2) & !1);
                let (encoder, size) = encoder::open(size, options.fps, options.bitrate, &options.codecs)?;
                Ok((source, encoder, size))
            });
            match opened {
                Ok((source, encoder, size)) => {
                    let _ = tx.send(Ok(encoder.codec()));
                    let pipeline = Pipeline { source, encoder, size, fps: options.fps.max(1), control: shared };
                    pipeline.run(&on_event);
                }
                Err(err) => {
                    let _ = tx.send(Err(err));
                }
            }
        })?;
        match rx.recv() {
            Ok(Ok(codec)) => Ok(Self { control, codec, thread: Some(thread) }),
            Ok(Err(err)) => {
                let _ = thread.join();
                Err(err)
            }
            Err(_) => Err(io::Error::other("the capture thread ended")),
        }
    }

    /// 出一帧带参数集的关键帧：手机丢了帧、刚连上要从头解时用。屏幕静止时也出，重编最近一张画面。
    pub fn request_keyframe(&self) {
        self.control.keyframe.store(true, Ordering::Relaxed);
    }

    /// 在 `on_event` 里要关键帧用的句柄，不让采集活下去。
    pub fn keyframes(&self) -> Keyframes {
        Keyframes(Arc::downgrade(&self.control))
    }

    /// 改平均码率，单位 bit/s。
    pub fn set_bitrate(&self, bitrate: u32) -> io::Result<()> {
        self.control.bitrate.store(bitrate, Ordering::Relaxed);
        self.control.rate_changed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// 编码器选中的编码。
    pub fn codec(&self) -> Codec {
        self.codec
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 向采集要关键帧，采集线程上也能用：只持有弱引用。
#[derive(Clone)]
pub struct Keyframes(Weak<Control>);

impl Keyframes {
    /// 出一帧带参数集的关键帧，屏幕静止时也出；采集已经丢掉时什么都不做。
    pub fn request(&self) {
        if let Some(control) = self.0.upgrade() {
            control.keyframe.store(true, Ordering::Relaxed);
        }
    }
}

struct Pipeline {
    source: Box<dyn Source>,
    encoder: Box<dyn VideoEncoder>,
    /// 编出的画面的尺寸。
    size: (u32, u32),
    fps: u32,
    control: Arc<Control>,
}

impl Pipeline {
    fn run(mut self, sink: &dyn Fn(Event)) {
        let started = Instant::now();
        let interval = Duration::from_secs(1) / self.fps;
        let mut nv12 = vec![0u8; nv12::frame_len(self.size)];
        let mut area: Option<(Area, (u32, u32))> = None;
        let mut have_frame = false;
        let mut refiner = Refiner::default();
        let mut boosted = false;
        let mut last_encoded = started - interval;
        // 上一轮开始取画面的时候：不等画面变就交回来的来源（X11），画面没变时也按帧率限着取。
        let mut last_poll = started - interval;
        while !self.control.stop.load(Ordering::Relaxed) {
            // 帧率上限：离上一帧、上一轮不到一帧的时长就先等着。
            let since = last_encoded.max(last_poll).elapsed();
            if since < interval {
                thread::sleep(interval - since);
            }
            last_poll = Instant::now();
            let refine_in = refiner.wait(Instant::now());
            let timeout = refine_in.map_or(FRAME_WAIT, |wait| wait.min(FRAME_WAIT));
            let changed = match self.source.wait(timeout) {
                Ok(changed) => changed,
                Err(err) => {
                    sink(Event::Stopped(err.to_string()));
                    return;
                }
            };
            if changed && let Some(picture) = self.source.picture() {
                let fitted = match area {
                    Some((area, source)) if source == picture.size => area,
                    _ => {
                        let fitted = Area::letterbox(picture.size, self.size);
                        area = Some((fitted, picture.size));
                        fitted
                    }
                };
                nv12::from_bgra(picture.data, picture.stride, picture.size, &mut nv12, self.size, fitted);
                have_frame = true;
            }
            let keyframe = self.control.keyframe.load(Ordering::Relaxed);
            let refine = have_frame && !changed && refiner.wait(Instant::now()).is_some_and(|wait| wait.is_zero());
            if !have_frame || !(changed || keyframe || refine) {
                continue;
            }
            // 读到这里之后才来的请求也算进这一帧，不在后面清掉时丢了。
            let keyframe = self.control.keyframe.swap(false, Ordering::Relaxed) || keyframe;
            let boost = refine && !changed && !keyframe;
            let rate_changed = self.control.rate_changed.swap(false, Ordering::Relaxed);
            if rate_changed || boost != boosted {
                let bitrate = self.control.bitrate.load(Ordering::Relaxed);
                let bitrate = crate::boosted(bitrate, boost) as u32;
                if let Err(err) = self.encoder.set_bitrate(bitrate) {
                    tracing::debug!("cannot change the bitrate: {err}");
                }
                boosted = boost;
            }
            let now = Instant::now();
            let pts_us = now.duration_since(started).as_micros() as i64;
            match self.encoder.encode(&nv12, keyframe, pts_us) {
                Ok(Some(frame)) => sink(Event::Frame(frame)),
                Ok(None) if keyframe => self.control.keyframe.store(true, Ordering::Relaxed),
                Ok(None) => {}
                Err(err) => {
                    sink(Event::Stopped(format!("the encoder failed: {err}")));
                    return;
                }
            }
            last_encoded = now;
            if boost {
                refiner.refined(now);
            } else {
                refiner.changed(now);
            }
        }
    }
}
