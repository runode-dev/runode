//! `runode --screen`：远程桌面的采集进程，由宿主所在的进程按需拉起（`runode_host::launch_screen`）。
//! 在 `screen_socket_file` 上等连接，每条连接各自一份采集和输入，说 `runode_protocol::screen` 的话；
//! 持续 `IDLE_EXIT` 没有连接就退出，没人看时不占资源。采集和编码跑在这个进程里，崩了只断远程桌面，
//! 宿主里的终端会话不受影响。连接换了屏幕的显示模式（`SetMode`）时记在 `MODE_OWNERS` 里，最后一条换过它的
//! 连接断开时换回原来的。macOS 上这个进程退出时系统也会换回去；Windows 上不会，换过的屏幕另记进
//! `screen_modes_file`，由看守进程（`runode --screen-guard`，`guard`）等这个进程退出后照着换回去。观看端要画面改走
//! UDP（`Datagram`）时开一个 `runode_remote::datagram::Sender`，打通以后画面改经它发（`Outbox::route`）。
//! 连上它的一头在 `client`，经 ssh 接过来的在 `ssh`。

pub mod client;
pub mod ssh;

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Write as _},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    Frame, FrameKind, read_frame_limited,
    screen::{
        Codec, SCREEN_VERSION, ScreenClientMsg, ScreenDisplay, ScreenHostMsg, ScreenInput, ScreenMode, ScreenMods,
        encode_video,
    },
    write_frame,
};
use runode_remote::datagram::{self, Path};
use runode_screen::{Capture, Display, DisplayMode, Event, Injector, KeepAwake, Keyframes, Options};
use runode_types::input::Key;
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

/// 持续这么久没有连接就退出。拉起它的一方马上就连，所以不必等很久。
const IDLE_EXIT: Duration = Duration::from_secs(10);
/// 客户端发来的一帧最多这么大：只有控制消息，最长的是一段输入的文字。
const CONTROL_LIMIT: u32 = 1 << 20;
/// 这条连接上还没写出去的画面最多这么多帧，再来的丢掉。
const MAX_QUEUED: usize = 3;
/// 还没写出去的控制消息最多这么多条，再多就是客户端只发不收，断开它。
const MAX_QUEUED_CONTROL: usize = 64;
/// 客户端这么久一点都不收就断开它，写的线程不一直卡着。
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// 显示模式被换过的屏幕 → 换过它、还没断开也没换回去的连接（`NEXT_CONNECTION` 发的号）。
static MODE_OWNERS: Mutex<Option<HashMap<u32, HashSet<u64>>>> = Mutex::new(None);
static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(0);

/// 跑采集进程，返回进程的退出码。
pub fn run() -> i32 {
    // SAFETY: `main` 直接交到这里，还没开别的线程。
    let disclaimed = unsafe { runode_host::disclaim_responsibility() };
    // 宿主可能跑在 ssh 的登录或用户服务里，没有图形会话的环境，采集前接过来。
    // SAFETY: 同上，还没开别的线程。
    #[cfg(target_os = "linux")]
    let desktop = unsafe { runode_screen::adopt_session() };
    let dirs = runode_paths::Dirs::from_env();
    crate::logging::init_file(dirs.screen_log_file());
    if let Err(err) = disclaimed {
        tracing::warn!("the screen process stays under its launcher's responsible process: {err}");
    }
    // 占着 socket 却采不到屏幕，观看端只会一直黑屏：拉起它的一方该把它起到桌面所在的会话里。
    #[cfg(windows)]
    if runode_screen::in_service_session() {
        tracing::warn!("the screen process does not start in the service session, which has no screen");
        return 1;
    }
    #[cfg(target_os = "linux")]
    if !desktop {
        tracing::warn!("the screen process does not start: nobody is signed in to a desktop on this screen");
        return 1;
    }
    let (listener, socket, _lock) = match listen(&dirs) {
        Ok(listening) => listening,
        Err(err) => {
            // 多半是另一个采集进程已经在跑（两边同时拉起时由锁决出一个）。
            tracing::info!("the screen process does not start: {err}");
            return 1;
        }
    };
    // 上一个采集进程换过模式、没换回去就没了（看守进程也没办成）时，占着锁的这里替它换回去。
    #[cfg(windows)]
    restore_left(&dirs, None);
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
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let lock = options.open(lock)?;
    lock.try_lock().map_err(io::Error::from)?;
    let listener = runode_host::bind_fresh(&socket)?;
    // Windows 上 socket 文件不单独设权限，谁能连由所在的运行目录的权限决定。
    #[cfg(unix)]
    std::fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
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
        let counter = active.clone();
        let spawned = thread::Builder::new().name("screen-connection".into()).spawn(move || {
            if let Err(err) = connection(stream) {
                tracing::info!("screen connection ended: {err}");
            }
            counter.fetch_sub(1, Ordering::Relaxed);
        });
        if let Err(err) = spawned {
            tracing::warn!("cannot serve a screen connection: {err}");
            active.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// 等 `listener` 来连接最多 `timeout`。
#[cfg(unix)]
fn readable(listener: &UnixListener, timeout: Duration) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut fd = libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: 只等这一个描述符，`fd` 是本地变量。
    unsafe { libc::poll(&mut fd, 1, timeout.as_millis() as libc::c_int) > 0 }
}

/// 等 `listener` 来连接最多 `timeout`。
#[cfg(windows)]
fn readable(listener: &UnixListener, timeout: Duration) -> bool {
    use std::os::windows::io::AsRawSocket as _;
    use windows_sys::Win32::Networking::WinSock::{POLLRDNORM, WSAPOLLFD, WSAPoll};
    let mut fd = WSAPOLLFD { fd: listener.as_raw_socket() as usize, events: POLLRDNORM, revents: 0 };
    // SAFETY: 只等这一个 socket，`fd` 是本地变量。
    unsafe { WSAPoll(&mut fd, 1, timeout.as_millis() as i32) > 0 }
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
    // 一条连接里屏幕上的授权框只弹一次，没被允许就停在那里，等观看端重新连接再问。
    runode_screen::ask_again();
    // 连着时屏幕一直亮着：熄着的屏幕列不出来，也采不到。
    let _awake = KeepAwake::acquire();
    // 缺的授权当场请求：每个程序第一次请求时系统在这台电脑上弹框，并把它列进系统设置的「屏幕与系统录音」
    // 和「辅助功能」，用户打开开关就行，不用自己找程序加进去。
    let mut permissions = runode_screen::permissions();
    if !permissions.screen_capture || !permissions.post_event {
        permissions = runode_screen::request_permissions();
    }
    let modes = ModeOwner(NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed));
    let displays = Display::all();
    outbox.control(&ScreenHostMsg::Ready {
        displays: displays.iter().map(screen_display).collect(),
        screen_capture: permissions.screen_capture,
        post_event: permissions.post_event,
        datagram: true,
    });
    let _watch = watch(&outbox, permissions);
    let mut injector = Injector::new(displays[0].id);
    let mut capture: Option<Capture> = None;
    while let Some(message) = read_message(&mut reader)? {
        match message {
            ScreenClientMsg::ScreenHello { .. } => return Err(io::Error::other("a second screen hello")),
            ScreenClientMsg::Start { display, width, height, fps, bitrate, hide_cursor, codecs } => {
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
                    cursor: !hide_cursor,
                    codecs: if codecs.is_empty() { vec![Codec::Hevc] } else { codecs },
                };
                outbox.set_bitrate(options.bitrate);
                capture = start(&options, &outbox);
            }
            ScreenClientMsg::Keyframe => {
                if let Some(capture) = &capture {
                    capture.request_keyframe();
                }
            }
            ScreenClientMsg::Bitrate { bitrate } => {
                let bitrate = bitrate.clamp(100_000, 100_000_000);
                outbox.set_bitrate(bitrate);
                if let Some(capture) = &capture
                    && let Err(err) = capture.set_bitrate(bitrate)
                {
                    tracing::warn!("cannot change the bitrate: {err}");
                }
            }
            ScreenClientMsg::Datagram { key, port, addrs } => outbox.offer_datagram(&key, port, &addrs),
            ScreenClientMsg::Input { input } => injector.inject(&input),
            ScreenClientMsg::Unlock { password } => {
                if let Err(reason) = unlock(&mut injector, &password) {
                    tracing::info!("cannot unlock the screen: {reason}");
                    outbox.control(&ScreenHostMsg::UnlockFailed { reason: reason.into() });
                }
            }
            ScreenClientMsg::SetMode { display: id, mode } => {
                // 换了模式屏幕尺寸就变了，采集按旧尺寸开的，停下等客户端按新尺寸再要。
                drop(capture.take());
                if let Err(err) = modes.set(id, mode) {
                    tracing::warn!("cannot change the mode of display {id}: {err}");
                    outbox.control(&ScreenHostMsg::ModeFailed { reason: err.to_string() });
                }
                outbox.control(&ScreenHostMsg::Displays {
                    displays: Display::all().iter().map(screen_display).collect(),
                });
            }
        }
    }
    Ok(())
}

/// 一条连接在 `MODE_OWNERS` 里的号；丢掉时（连接断开）放开它换过的屏幕。
struct ModeOwner(u64);

impl ModeOwner {
    /// 把屏幕 `display` 换成 `mode`，为空时这条连接不再要它换过的模式：别的连接也不要了才换回原来的。
    fn set(&self, display: u32, mode: Option<ScreenMode>) -> io::Result<()> {
        let mut owners = MODE_OWNERS.lock().unwrap_or_else(PoisonError::into_inner);
        let owners = owners.get_or_insert_default();
        match mode {
            Some(mode) => {
                let mode = DisplayMode {
                    width: mode.width,
                    height: mode.height,
                    pixel_width: mode.pixel_width,
                    pixel_height: mode.pixel_height,
                };
                if !runode_screen::display_modes(display).contains(&mode) {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "the display has no such mode"));
                }
                runode_screen::set_display_mode(display, Some(&mode))?;
                owners.entry(display).or_default().insert(self.0);
                record(owners);
                Ok(())
            }
            None => release(owners, display, self.0),
        }
    }
}

impl Drop for ModeOwner {
    fn drop(&mut self) {
        let mut owners = MODE_OWNERS.lock().unwrap_or_else(PoisonError::into_inner);
        let owners = owners.get_or_insert_default();
        let held: Vec<u32> = owners
            .iter()
            .filter(|(_, connections)| connections.contains(&self.0))
            .map(|(display, _)| *display)
            .collect();
        for id in held {
            if let Err(err) = release(owners, id, self.0) {
                tracing::warn!("cannot restore the mode of display {id}: {err}");
            }
        }
    }
}

/// 连接 `connection` 不再要屏幕 `display` 换过的模式；没有别的连接要了就换回原来的。换不回去时屏幕留在
/// `owners` 里、没有主人：仍记在 `screen_modes_file` 里，哪条连接再放开它时再换一次。
fn release(owners: &mut HashMap<u32, HashSet<u64>>, display: u32, connection: u64) -> io::Result<()> {
    let Some(connections) = owners.get_mut(&display) else { return Ok(()) };
    connections.remove(&connection);
    if connections.is_empty() {
        runode_screen::set_display_mode(display, None)?;
        owners.remove(&display);
        record(owners);
    }
    Ok(())
}

/// 把换过模式的屏幕记进 `screen_modes_file`：第一行是这个进程的号，之后每行一块屏幕；都换回去了就删掉。第一次记
/// 时起看守进程。macOS 上进程退出时系统自己换回去，不用记。
#[cfg(windows)]
fn record(owners: &HashMap<u32, HashSet<u64>>) {
    let Some(path) = runode_paths::Dirs::from_env().screen_modes_file() else { return };
    if owners.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let mut text = format!("{}\n", std::process::id());
    for id in owners.keys() {
        text.push_str(&format!("{id}\n"));
    }
    if let Err(err) = std::fs::write(&path, text) {
        tracing::warn!("cannot record the changed display modes: {err}");
    }
    static GUARD: std::sync::Once = std::sync::Once::new();
    GUARD.call_once(spawn_guard);
}

#[cfg(not(windows))]
fn record(_: &HashMap<u32, HashSet<u64>>) {}

/// 起看守这个进程的 `runode --screen-guard <进程号>`。脱离这个进程所在的作业，这个进程被连着作业一起结束时它还在；
/// 作业不让脱离时照常起。
#[cfg(windows)]
fn spawn_guard() {
    use std::os::windows::process::CommandExt as _;
    use windows_sys::Win32::System::Threading::{CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return tracing::warn!("cannot find this program to guard the display modes: {err}"),
    };
    let spawn = |flags: u32| {
        std::process::Command::new(&exe)
            .args(["--screen-guard", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    if let Err(err) = spawn(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| spawn(CREATE_NO_WINDOW)) {
        tracing::warn!("cannot start the display mode guard: {err}");
    }
}

/// `runode --screen-guard <进程号>`：等采集进程 `pid` 退出，把它记着没换回去的屏幕换回系统设置里的模式。
#[cfg(windows)]
pub fn guard(pid: u32) -> i32 {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };
    // SAFETY: 只要等它结束的权限；句柄用完关掉。打不开时它已经退出了。
    unsafe {
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !process.is_null() {
            WaitForSingleObject(process, INFINITE);
            CloseHandle(process);
        }
    }
    restore_left(&runode_paths::Dirs::from_env(), Some(pid));
    0
}

/// 把 `screen_modes_file` 里记着的屏幕换回系统设置里的模式，再删掉它；给了 `owner` 时只在记它的是这个进程时才办，
/// 新起的采集进程记的不碰。
#[cfg(windows)]
fn restore_left(dirs: &runode_paths::Dirs, owner: Option<u32>) {
    let Some(path) = dirs.screen_modes_file() else { return };
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let mut lines = text.lines();
    let recorded = lines.next().and_then(|line| line.parse::<u32>().ok());
    if owner.is_some() && recorded != owner {
        return;
    }
    for id in lines.filter_map(|line| line.parse::<u32>().ok()) {
        if let Err(err) = runode_screen::reset_display_mode(id) {
            tracing::warn!("cannot restore the mode of display {id}: {err}");
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// 隔多久看一次锁没锁屏和输入授权。
const LOCK_POLL: Duration = Duration::from_secs(1);
/// 敲密码时两个键之间隔这么久，登录窗口来得及一个个收下。
const KEYSTROKE_GAP: Duration = Duration::from_millis(15);

/// 丢掉时停下 `watch` 的线程。
struct StopFlag(Arc<AtomicBool>);

impl Drop for StopFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// 一个线程每 `LOCK_POLL` 看一次锁没锁屏和输入授权：锁屏先报一次，之后变了再报；用户在电脑上打开或关掉了
/// 输入授权时报一次，观看端不用重连。`permissions` 是 `Ready` 里报的。
fn watch(outbox: &Outbox, permissions: runode_screen::Permissions) -> StopFlag {
    let stop = Arc::new(AtomicBool::new(false));
    let (outbox, stopped) = (outbox.clone(), stop.clone());
    let spawned = thread::Builder::new().name("screen-watch".into()).spawn(move || {
        let mut locked_before = None;
        let mut post_event_before = permissions.post_event;
        while !stopped.load(Ordering::Relaxed) {
            let locked = runode_screen::screen_locked();
            if locked_before != Some(locked) {
                outbox.control(&ScreenHostMsg::Lock { locked });
                locked_before = Some(locked);
            }
            let post_event = runode_screen::permissions().post_event;
            if post_event != post_event_before {
                outbox.control(&ScreenHostMsg::Permissions { screen_capture: permissions.screen_capture, post_event });
                post_event_before = post_event;
            }
            thread::sleep(LOCK_POLL);
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("cannot watch the screen lock and the input permission: {err}");
    }
    StopFlag(stop)
}

/// 把密码敲进锁屏的密码框再回车。没锁屏时什么都不敲；每敲一个键前都再看一眼，中途解开了（别人在电脑前
/// 解了）就停下，免得把密码敲进别的窗口。
fn unlock(injector: &mut Injector, password: &str) -> Result<(), &'static str> {
    // 锁屏界面在安全桌面上，普通进程往那里注入不了按键。
    if cfg!(windows) {
        return Err("the lock screen cannot be typed into on Windows");
    }
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

/// 读一条控制消息，读不懂的（更新的客户端多出来的）跳过；连接在两帧之间关了时返回 `None`。
fn read_message(reader: &mut impl io::Read) -> io::Result<Option<ScreenClientMsg>> {
    loop {
        let Some(frame) = read_frame_limited(reader, CONTROL_LIMIT).map_err(io::Error::other)? else {
            return Ok(None);
        };
        if frame.kind != FrameKind::Control {
            return Err(io::Error::other(format!("unexpected {:?} frame", frame.kind)));
        }
        match frame.message() {
            Ok(message) => return Ok(Some(message)),
            Err(err) => tracing::debug!("skipping an unknown screen message: {err}"),
        }
    }
}

fn screen_display(display: &Display) -> ScreenDisplay {
    let modes = runode_screen::display_modes(display.id)
        .into_iter()
        .map(|mode| ScreenMode {
            width: mode.width,
            height: mode.height,
            pixel_width: mode.pixel_width,
            pixel_height: mode.pixel_height,
        })
        .collect();
    ScreenDisplay {
        id: display.id,
        width: display.width,
        height: display.height,
        pixel_width: display.pixel_width,
        pixel_height: display.pixel_height,
        modes,
        changed: runode_screen::display_mode_changed(display.id),
    }
}

fn even(value: u32) -> u32 {
    value & !1
}

/// 按 `options` 开始采集，画面经 `outbox` 发出去；开不起来时告诉客户端原因。
fn start(options: &Options, outbox: &Outbox) -> Option<Capture> {
    // 丢帧时要向采集要关键帧，而回调在 `Capture` 建好之前就交出去了，所以经这里转一下。
    let handle: Arc<OnceLock<Keyframes>> = Arc::default();
    // 要了关键帧、`handle` 还没设上：设上以后补要一次。
    let owed = Arc::new(AtomicBool::new(false));
    let pacer = Mutex::new(Pacer::default());
    let sink = outbox.clone();
    let keyframes = handle.clone();
    let owing = owed.clone();
    // 先报开始，再开采集：第一帧在采集开好之前就可能编出来，客户端要先知道换了画面、才不会把新的关键帧
    // 当成旧画面的。开不起来时随后报 `Stopped`。
    outbox.control(&ScreenHostMsg::Started { display: options.display, width: options.width, height: options.height });
    let started = Capture::start(options, move |event| match event {
        Event::Frame(frame) => {
            let (route, switched) = sink.route();
            let (send, want_keyframe) = {
                let mut pacer = pacer.lock().unwrap_or_else(PoisonError::into_inner);
                // 换了路，另一条路上的第一帧得是关键帧。
                let restarted = switched && pacer.restart();
                let (send, want) = pacer.admit(frame.keyframe, sink.backlogged(route));
                (send, want || restarted)
            };
            // 交给路上时没收下（UDP 刚断、排满了）也算丢了一帧：后面的差分帧接不上，一直丢到关键帧。
            let lost = send && !sink.video(route, encode_video(&frame));
            let lost = lost && pacer.lock().unwrap_or_else(PoisonError::into_inner).restart();
            if want_keyframe || lost {
                owing.store(true, Ordering::SeqCst);
                if let Some(keyframes) = keyframes.get()
                    && owing.swap(false, Ordering::SeqCst)
                {
                    keyframes.request();
                }
            }
        }
        Event::Stopped(reason) => {
            sink.control(&ScreenHostMsg::Stopped { reason, signed_out: false, no_encoder: false });
        }
    });
    match started {
        Ok(capture) => {
            tracing::info!(
                "capturing display {} at {}x{} as {:?}",
                options.display,
                options.width,
                options.height,
                capture.codec()
            );
            let _ = handle.set(capture.keyframes());
            if owed.swap(false, Ordering::SeqCst) {
                capture.keyframes().request();
            }
            Some(capture)
        }
        Err(err) => {
            tracing::warn!("cannot start capturing: {err}");
            outbox.control(&ScreenHostMsg::Stopped {
                reason: err.to_string(),
                signed_out: err.kind() == io::ErrorKind::NotConnected,
                no_encoder: err.kind() == io::ErrorKind::Unsupported,
            });
            None
        }
    }
}

/// 发往客户端的帧都经一个线程按先后写出去；画面另外数着还没写出去的帧。画面打通了 UDP 时改经 `datagram` 发。
#[derive(Clone)]
struct Outbox {
    tx: mpsc::SyncSender<(FrameKind, Vec<u8>)>,
    queued_video: Arc<AtomicUsize>,
    stream: Arc<UnixStream>,
    datagram: Arc<Mutex<Datagram>>,
}

/// 画面走哪条路。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// 这条连接。
    Stream,
    Datagram,
}

/// 画面的 UDP 通道。
#[derive(Default)]
struct Datagram {
    sender: Option<datagram::Sender>,
    /// 这条连接已经开过通道：一条连接只开一次，同一把密钥再开一次通道 nonce 就从头重复了。
    offered: bool,
    /// 画面现在走 UDP。
    active: bool,
    /// 画面的平均码率，开通道时照它限速。
    bitrate: u32,
}

impl Outbox {
    /// 开写出去的线程。写失败、写超时或者控制消息积压过 `MAX_QUEUED_CONTROL` 时关掉连接，
    /// 读的一边随之结束。
    fn start(stream: UnixStream) -> io::Result<Self> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let stream = Arc::new(stream);
        let (tx, rx) = mpsc::sync_channel::<(FrameKind, Vec<u8>)>(MAX_QUEUED + MAX_QUEUED_CONTROL);
        let queued_video = Arc::new(AtomicUsize::new(0));
        let queued = queued_video.clone();
        let writing = stream.clone();
        thread::Builder::new().name("screen-writer".into()).spawn(move || {
            let mut writer = BufWriter::new(&*writing);
            for (kind, payload) in rx {
                let written =
                    write_frame(&mut writer, kind, 0, &payload).map_err(io::Error::other).and_then(|()| writer.flush());
                if kind == FrameKind::Video {
                    queued.fetch_sub(1, Ordering::Relaxed);
                }
                if let Err(err) = written {
                    tracing::debug!("cannot write to the screen client: {err}");
                    let _ = writing.shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
        })?;
        Ok(Self { tx, queued_video, stream, datagram: Arc::default() })
    }

    /// 客户端要画面改走 UDP：开通道往它给的地址打洞，回它这边的端口和地址。开不了时不回，画面照旧走这条连接。
    fn offer_datagram(&self, key: &str, port: u16, addrs: &[std::net::IpAddr]) {
        let Some(key) = datagram::Key::from_hex(key) else {
            tracing::info!("the screen client sent a bad datagram key");
            return;
        };
        let candidates = addrs.iter().map(|&addr| std::net::SocketAddr::new(addr, port)).collect();
        let mut state = self.datagram.lock().unwrap_or_else(PoisonError::into_inner);
        if std::mem::replace(&mut state.offered, true) {
            tracing::info!("the screen client asked for datagrams again, ignoring");
            return;
        }
        match datagram::Sender::start(&key, candidates, state.bitrate.max(1)) {
            Ok(sender) => {
                self.control(&ScreenHostMsg::Datagram { port: sender.port(), addrs: sender.candidates() });
                state.sender = Some(sender);
            }
            Err(err) => tracing::warn!("cannot open the remote desktop datagrams: {err}"),
        }
    }

    fn set_bitrate(&self, bitrate: u32) {
        let mut state = self.datagram.lock().unwrap_or_else(PoisonError::into_inner);
        state.bitrate = bitrate;
        if let Some(sender) = &state.sender {
            sender.set_bitrate(bitrate);
        }
    }

    /// 这一帧走哪条路，和是不是刚换了路。UDP 打通了改走它；没打通或断了先告诉客户端（`DatagramLost`），它好停下
    /// 收 UDP，画面回到或留在这条连接上，不再试 UDP。
    fn route(&self) -> (Route, bool) {
        let mut state = self.datagram.lock().unwrap_or_else(PoisonError::into_inner);
        let path = state.sender.as_ref().map(datagram::Sender::path);
        match path {
            Some(Path::Ready) => {
                let switched = !state.active;
                if switched {
                    tracing::info!("remote desktop video goes over datagrams");
                }
                state.active = true;
                (Route::Datagram, switched)
            }
            Some(Path::Failed) => {
                // 丢掉它要等它的线程停下（可能正发到一帧中间），不在采集回调里、拿着锁等。
                if let Some(sender) = state.sender.take() {
                    thread::spawn(move || drop(sender));
                }
                self.control(&ScreenHostMsg::DatagramLost);
                (Route::Stream, std::mem::take(&mut state.active))
            }
            Some(Path::Connecting) | None => (Route::Stream, false),
        }
    }

    fn control(&self, message: &ScreenHostMsg) {
        if let Ok(frame) = Frame::control(message) {
            self.send(FrameKind::Control, frame.payload);
        }
    }

    /// 发一帧画面，返回收下了没有。
    fn video(&self, route: Route, payload: Vec<u8>) -> bool {
        if route == Route::Datagram {
            let state = self.datagram.lock().unwrap_or_else(PoisonError::into_inner);
            return state.sender.as_ref().is_some_and(|sender| sender.send(payload));
        }
        self.queued_video.fetch_add(1, Ordering::Relaxed);
        let sent = self.send(FrameKind::Video, payload);
        if !sent {
            self.queued_video.fetch_sub(1, Ordering::Relaxed);
        }
        sent
    }

    /// 排进队列；画面有 `Pacer` 限着，队列满了只能是控制消息积压，断开连接。
    fn send(&self, kind: FrameKind, payload: Vec<u8>) -> bool {
        match self.tx.try_send((kind, payload)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::info!("the screen client does not read its messages, disconnecting");
                let _ = self.stream.shutdown(std::net::Shutdown::Both);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// 走 `route` 还没发出去的画面排满了没有：这条连接上排到 `MAX_QUEUED` 帧，UDP 上排到它的 `max_queued`。
    fn backlogged(&self, route: Route) -> bool {
        match route {
            Route::Stream => self.queued_video.load(Ordering::Relaxed) >= MAX_QUEUED,
            Route::Datagram => {
                let state = self.datagram.lock().unwrap_or_else(PoisonError::into_inner);
                state.sender.as_ref().is_some_and(|sender| sender.queued() >= sender.max_queued())
            }
        }
    }
}

/// 画面写不出去、排满了时丢帧：排着的都是过时的画面，发出去只会更慢。丢了一帧
/// 以后后面的帧都解不出来，所以一直丢到下一个关键帧，并向采集要一个。
#[derive(Default)]
struct Pacer {
    dropping: bool,
}

impl Pacer {
    /// 从下一个关键帧重新开始发：画面换了路，另一条路上的前几帧接不上。返回 true，要向采集要关键帧。
    fn restart(&mut self) -> bool {
        self.dropping = true;
        true
    }

    /// 返回这一帧发不发、要不要关键帧。
    fn admit(&mut self, keyframe: bool, backlogged: bool) -> (bool, bool) {
        if backlogged {
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
        assert_eq!(pacer.admit(false, false), (true, false));
        assert_eq!(pacer.admit(false, true), (false, true));
        assert_eq!(pacer.admit(false, true), (false, false));
        assert_eq!(pacer.admit(false, false), (false, false));
        assert_eq!(pacer.admit(true, true), (false, true));
        assert_eq!(pacer.admit(true, false), (true, false));
        assert_eq!(pacer.admit(false, false), (true, false));
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
        let start = ScreenClientMsg::Start {
            display: None,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate: 4_000_000,
            hide_cursor: false,
            codecs: Vec::new(),
        };
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
