//! `runode --screen`：远程桌面的采集进程，由宿主所在的进程按需拉起（`runode_host::launch_screen`）。
//! 在 `screen_socket_file` 上等连接，每条连接各自一份采集和输入，说 `runode_protocol::screen` 的话；
//! 持续 `IDLE_EXIT` 没有连接就退出，没人看时不占资源。采集和编码跑在这个进程里，崩了只断远程桌面，
//! 宿主里的终端会话不受影响。

use std::{
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Write as _},
    os::{
        fd::AsRawFd as _,
        unix::{
            fs::{OpenOptionsExt as _, PermissionsExt as _},
            net::{UnixListener, UnixStream},
        },
    },
    sync::{
        Arc, Mutex, OnceLock, PoisonError, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    Frame, FrameKind, read_frame_limited,
    screen::{SCREEN_VERSION, ScreenClientMsg, ScreenDisplay, ScreenHostMsg, ScreenInput, ScreenMods, encode_video},
    write_frame,
};
use runode_screen::{Capture, Display, Event, Injector, KeepAwake, Options};
use runode_types::input::Key;

/// 持续这么久没有连接就退出。拉起它的一方马上就连，所以不必等很久。
const IDLE_EXIT: Duration = Duration::from_secs(10);
/// 客户端发来的一帧最多这么大：只有控制消息，最长的是一段输入的文字。
const CONTROL_LIMIT: u32 = 1 << 20;
/// 还没写出去的画面最多这么多帧，再来的丢掉，见 `Pacer`。
const MAX_QUEUED: usize = 3;

/// 跑采集进程，返回进程的退出码。
pub fn run() -> i32 {
    // SAFETY: `main` 直接交到这里，还没开别的线程。
    let disclaimed = unsafe { runode_host::disclaim_responsibility() };
    let dirs = runode_paths::Dirs::from_env();
    crate::host_process::init_logging(dirs.screen_log_file());
    if let Err(err) = disclaimed {
        tracing::warn!("the screen process stays under its launcher's responsible process: {err}");
    }
    let (listener, socket, _lock) = match listen(&dirs) {
        Ok(listening) => listening,
        Err(err) => {
            // 多半是另一个采集进程已经在跑（两边同时拉起时由锁决出一个）。
            tracing::info!("the screen process does not start: {err}");
            return 1;
        }
    };
    tracing::info!("screen process {} listening on {}", std::process::id(), socket.display());
    serve(&listener);
    // 还锁着时删掉 socket 文件：放开锁以后路径可能已经归新的采集进程了。
    let _ = std::fs::remove_file(&socket);
    tracing::info!("screen process {} exits", std::process::id());
    0
}

/// 锁住 `screen_lock_file`，在 `screen_socket_file` 上开监听。返回的锁文件丢掉时放开锁。
fn listen(dirs: &runode_paths::Dirs) -> io::Result<(UnixListener, std::path::PathBuf, File)> {
    dirs.create_runtime_dir()?;
    let socket = dirs.screen_socket_file().ok_or_else(|| io::Error::other("the socket path is too long"))?;
    let lock = dirs.screen_lock_file().ok_or_else(|| io::Error::other("no place for the lock"))?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(lock)?;
    // SAFETY: 描述符来自上面打开的文件，在这次调用期间一直有效。
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error());
    }
    match std::fs::remove_file(&socket) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, socket, lock))
}

/// 接连接，每条一个线程，没有连接持续 `IDLE_EXIT` 时返回。
fn serve(listener: &UnixListener) {
    let active = Arc::new(AtomicUsize::new(0));
    let mut idle_since = Instant::now();
    loop {
        if active.load(Ordering::Relaxed) > 0 {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= IDLE_EXIT {
            return;
        }
        if !readable(listener, Duration::from_secs(1)) {
            continue;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(err) => {
                tracing::warn!("cannot accept a screen connection: {err}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if !runode_host::same_user(&stream) {
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let active = active.clone();
        let spawned = thread::Builder::new().name("screen-connection".into()).spawn(move || {
            if let Err(err) = connection(stream) {
                tracing::info!("screen connection ended: {err}");
            }
            active.fetch_sub(1, Ordering::Relaxed);
        });
        if let Err(err) = spawned {
            tracing::warn!("cannot serve a screen connection: {err}");
        }
    }
}

/// 等 `listener` 来连接最多 `timeout`。
fn readable(listener: &UnixListener, timeout: Duration) -> bool {
    let mut fd = libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: 只等这一个描述符，`fd` 是本地变量。
    unsafe { libc::poll(&mut fd, 1, timeout.as_millis() as libc::c_int) > 0 }
}

/// 服务一条连接，到对面断开或者说错话为止。第一帧要是 `ScreenHello`。
fn connection(stream: UnixStream) -> io::Result<()> {
    let mut reader = BufReader::new(&stream);
    let outbox = Outbox::start(stream.try_clone()?)?;
    match read_message(&mut reader)? {
        Some(ScreenClientMsg::ScreenHello { version }) if version == SCREEN_VERSION => {}
        Some(ScreenClientMsg::ScreenHello { version }) => {
            outbox.control(&ScreenHostMsg::Incompatible { version: SCREEN_VERSION });
            return Err(io::Error::other(format!("the client speaks screen version {version}")));
        }
        _ => return Err(io::Error::other("the first frame is not a screen hello")),
    }
    // 连着时屏幕一直亮着：熄着的屏幕列不出来，也采不到。
    let _awake = KeepAwake::acquire();
    let permissions = runode_screen::permissions();
    let displays = Display::all();
    outbox.control(&ScreenHostMsg::Ready {
        displays: displays.iter().map(screen_display).collect(),
        screen_capture: permissions.screen_capture,
        post_event: permissions.post_event,
    });
    let _watch = watch_lock(&outbox);
    let mut injector = Injector::new(displays[0].id);
    let mut capture: Option<Arc<Capture>> = None;
    while let Some(message) = read_message(&mut reader)? {
        match message {
            ScreenClientMsg::ScreenHello { .. } => return Err(io::Error::other("a second screen hello")),
            ScreenClientMsg::Start { display, width, height, fps, bitrate } => {
                // 先停旧的，同一块屏幕不同时采两份。
                drop(capture.take());
                // 屏幕可能插拔过，按现在开着的找；要的那块不在了就采主屏。
                let displays = Display::all();
                let display = displays.iter().find(|candidate| Some(candidate.id) == display).unwrap_or(&displays[0]);
                injector = Injector::new(display.id);
                let options = Options {
                    display: display.id,
                    width: even(width.clamp(16, display.pixel_width.max(16))),
                    height: even(height.clamp(16, display.pixel_height.max(16))),
                    fps: fps.clamp(1, 120),
                    bitrate: bitrate.clamp(100_000, 100_000_000),
                };
                capture = start(&options, &outbox);
            }
            ScreenClientMsg::Keyframe => {
                if let Some(capture) = &capture {
                    capture.request_keyframe();
                }
            }
            ScreenClientMsg::Bitrate { bitrate } => {
                if let Some(capture) = &capture
                    && let Err(err) = capture.set_bitrate(bitrate.clamp(100_000, 100_000_000))
                {
                    tracing::warn!("cannot change the bitrate: {err}");
                }
            }
            ScreenClientMsg::Input { input } => injector.inject(&input),
            ScreenClientMsg::Unlock { password } => {
                if let Err(reason) = unlock(&mut injector, &password) {
                    tracing::info!("cannot unlock the screen: {reason}");
                    outbox.control(&ScreenHostMsg::UnlockFailed { reason: reason.into() });
                }
            }
        }
    }
    Ok(())
}

/// 隔多久看一次锁没锁屏。
const LOCK_POLL: Duration = Duration::from_secs(1);
/// 敲密码时两个键之间隔这么久，登录窗口来得及一个个收下。
const KEYSTROKE_GAP: Duration = Duration::from_millis(15);

/// 丢掉时停下 `watch_lock` 的线程。
struct StopFlag(Arc<AtomicBool>);

impl Drop for StopFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// 一个线程每 `LOCK_POLL` 看一次锁没锁屏，先报一次，之后变了再报。
fn watch_lock(outbox: &Outbox) -> StopFlag {
    let stop = Arc::new(AtomicBool::new(false));
    let (outbox, stopped) = (outbox.clone(), stop.clone());
    let spawned = thread::Builder::new().name("screen-lock".into()).spawn(move || {
        let mut last = None;
        while !stopped.load(Ordering::Relaxed) {
            let locked = runode_screen::screen_locked();
            if last != Some(locked) {
                outbox.control(&ScreenHostMsg::Lock { locked });
                last = Some(locked);
            }
            thread::sleep(LOCK_POLL);
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cannot watch the screen lock: {err}");
    }
    StopFlag(stop)
}

/// 把密码敲进锁屏的密码框再回车。没锁屏时什么都不敲；每敲一个键前都再看一眼，中途解开了（别人在电脑前
/// 解了）就停下，免得把密码敲进别的窗口。
fn unlock(injector: &mut Injector, password: &str) -> Result<(), &'static str> {
    let strokes = runode_screen::key_strokes(password).ok_or("the password has keys a US keyboard cannot type")?;
    for (key, shift) in strokes.into_iter().chain([(Key::Enter, false)]) {
        if !runode_screen::screen_locked() {
            return Ok(());
        }
        injector.inject(&ScreenInput::Key { key, mods: ScreenMods { shift, ..ScreenMods::default() } });
        thread::sleep(KEYSTROKE_GAP);
    }
    Ok(())
}

/// 读一条控制消息；连接在两帧之间关了时返回 `None`。
fn read_message(reader: &mut impl io::Read) -> io::Result<Option<ScreenClientMsg>> {
    let Some(frame) = read_frame_limited(reader, CONTROL_LIMIT).map_err(io::Error::other)? else { return Ok(None) };
    if frame.kind != FrameKind::Control {
        return Err(io::Error::other(format!("unexpected {:?} frame", frame.kind)));
    }
    frame.message().map(Some).map_err(io::Error::other)
}

fn screen_display(display: &Display) -> ScreenDisplay {
    ScreenDisplay {
        id: display.id,
        width: display.width,
        height: display.height,
        pixel_width: display.pixel_width,
        pixel_height: display.pixel_height,
    }
}

fn even(value: u32) -> u32 {
    value & !1
}

/// 按 `options` 开始采集，画面经 `outbox` 发出去；开不起来时告诉客户端原因。
fn start(options: &Options, outbox: &Outbox) -> Option<Arc<Capture>> {
    // 丢帧时要向采集要关键帧，而回调在 `Capture` 建好之前就交出去了，所以经这里转一下。
    let handle: Arc<OnceLock<Weak<Capture>>> = Arc::default();
    let pacer = Mutex::new(Pacer::default());
    let sink = outbox.clone();
    let keyframes = handle.clone();
    // 先报开始，再开采集：第一帧在采集开好之前就可能编出来，客户端要先知道换了画面、才不会把新的关键帧
    // 当成旧画面的。开不起来时随后报 `Stopped`。
    outbox.control(&ScreenHostMsg::Started { display: options.display, width: options.width, height: options.height });
    let started = Capture::start(options, move |event| match event {
        Event::Frame(frame) => {
            let (send, want_keyframe) =
                pacer.lock().unwrap_or_else(PoisonError::into_inner).admit(frame.keyframe, sink.queued_video());
            if want_keyframe && let Some(capture) = keyframes.get().and_then(Weak::upgrade) {
                capture.request_keyframe();
            }
            if send {
                sink.video(encode_video(&frame));
            }
        }
        Event::Stopped(reason) => sink.control(&ScreenHostMsg::Stopped { reason }),
    });
    match started {
        Ok(capture) => {
            let capture = Arc::new(capture);
            let _ = handle.set(Arc::downgrade(&capture));
            Some(capture)
        }
        Err(err) => {
            tracing::warn!("cannot start capturing: {err}");
            outbox.control(&ScreenHostMsg::Stopped { reason: err.to_string() });
            None
        }
    }
}

/// 发往客户端的帧都经一个线程按先后写出去；画面另外数着还没写出去的帧，见 `Pacer`。
#[derive(Clone)]
struct Outbox {
    tx: mpsc::Sender<(FrameKind, Vec<u8>)>,
    queued_video: Arc<AtomicUsize>,
}

impl Outbox {
    /// 开写出去的线程。写失败时关掉连接，读的一边随之结束。
    fn start(stream: UnixStream) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<(FrameKind, Vec<u8>)>();
        let queued_video = Arc::new(AtomicUsize::new(0));
        let queued = queued_video.clone();
        thread::Builder::new().name("screen-writer".into()).spawn(move || {
            let mut writer = BufWriter::new(&stream);
            for (kind, payload) in rx {
                let written =
                    write_frame(&mut writer, kind, 0, &payload).map_err(io::Error::other).and_then(|()| writer.flush());
                if kind == FrameKind::Video {
                    queued.fetch_sub(1, Ordering::Relaxed);
                }
                if let Err(err) = written {
                    tracing::debug!("cannot write to the screen client: {err}");
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
        })?;
        Ok(Self { tx, queued_video })
    }

    fn control(&self, message: &ScreenHostMsg) {
        if let Ok(frame) = Frame::control(message) {
            let _ = self.tx.send((FrameKind::Control, frame.payload));
        }
    }

    fn video(&self, payload: Vec<u8>) {
        self.queued_video.fetch_add(1, Ordering::Relaxed);
        if self.tx.send((FrameKind::Video, payload)).is_err() {
            self.queued_video.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn queued_video(&self) -> usize {
        self.queued_video.load(Ordering::Relaxed)
    }
}

/// 画面写不出去、排队超过 `MAX_QUEUED` 时丢帧：排着的都是过时的画面，发出去只会更慢。丢了一帧
/// 以后后面的帧都解不出来，所以一直丢到下一个关键帧，并向采集要一个。
#[derive(Default)]
struct Pacer {
    dropping: bool,
}

impl Pacer {
    /// 返回这一帧发不发、要不要关键帧。
    fn admit(&mut self, keyframe: bool, queued: usize) -> (bool, bool) {
        if queued >= MAX_QUEUED {
            // 丢掉的是关键帧时再要一个，不然要等的关键帧永远不来。
            let want = !self.dropping || keyframe;
            self.dropping = true;
            return (false, want);
        }
        if self.dropping && !keyframe {
            return (false, false);
        }
        self.dropping = false;
        (true, false)
    }
}

#[cfg(test)]
mod tests {
    use runode_protocol::{read_frame, screen::decode_video};

    use super::*;

    #[test]
    fn pacer_drops_until_the_next_keyframe() {
        let mut pacer = Pacer::default();
        assert_eq!(pacer.admit(false, 0), (true, false));
        assert_eq!(pacer.admit(false, MAX_QUEUED), (false, true));
        assert_eq!(pacer.admit(false, MAX_QUEUED), (false, false));
        assert_eq!(pacer.admit(false, 0), (false, false));
        assert_eq!(pacer.admit(true, MAX_QUEUED), (false, true));
        assert_eq!(pacer.admit(true, 0), (true, false));
        assert_eq!(pacer.admit(false, 0), (true, false));
    }

    /// 走一遍真的采集：握手后先收到 `Ready`，`Start` 以后收到 `Started` 和带参数集的关键帧。
    #[test]
    #[ignore = "needs the screen recording permission"]
    fn serves_a_capture() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || connection(theirs));
        let mut reader = BufReader::new(&ours);
        let hello = Frame::control(&ScreenClientMsg::ScreenHello { version: SCREEN_VERSION }).unwrap();
        write_frame(&mut &ours, hello.kind, 0, &hello.payload).unwrap();
        let ready: ScreenHostMsg = read_frame(&mut reader).unwrap().unwrap().message().unwrap();
        assert!(matches!(ready, ScreenHostMsg::Ready { screen_capture: true, .. }), "{ready:?}");
        let start = ScreenClientMsg::Start { display: None, width: 1280, height: 720, fps: 30, bitrate: 4_000_000 };
        let frame = Frame::control(&start).unwrap();
        write_frame(&mut &ours, frame.kind, 0, &frame.payload).unwrap();
        // 锁屏状态的报告随时会插进来，跳过。
        let mut next = || loop {
            let frame = read_frame(&mut reader).unwrap().unwrap();
            if frame.kind != FrameKind::Control
                || !matches!(frame.message::<ScreenHostMsg>(), Ok(ScreenHostMsg::Lock { .. }))
            {
                return frame;
            }
        };
        let started: ScreenHostMsg = next().message().unwrap();
        assert!(matches!(started, ScreenHostMsg::Started { width: 1280, height: 720, .. }), "{started:?}");
        let video = next();
        assert_eq!(video.kind, FrameKind::Video);
        let video = decode_video(&video.payload).unwrap();
        assert!(video.keyframe);
        assert_eq!(video.parameter_sets.len(), 3);
    }
}
