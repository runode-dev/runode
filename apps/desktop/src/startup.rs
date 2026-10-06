//! 启动路径上的计时点。`RUST_LOG=runode::startup=info` 时，每过一个节点按 INFO 级打一条
//! `node=… ms=…`，`ms` 是距进程启动的毫秒数（保留一位小数），供性能测量脚本分解启动时间；
//! 默认不打。
//!
//! 进程启动的时刻取内核记的（`proc_pidinfo` 的 `PROC_PIDTBSDINFO`，精确到微秒），所以 `main`
//! 那个点也含 dyld 加载、静态初始化这些进 `main` 之前的时间；取不到时从 `main` 起算，`main`
//! 记为 0。
//!
//! 没开时开销很小：`begin` 只读一次单调时钟和墙上时钟，之后每个点先问 tracing 这个 target
//! 开没开（调用点的判断结果有缓存），没开就返回；只出现一次的点（`Once`）问过一次后只剩一次
//! 原子读。

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

/// 这些计时点的 tracing target。默认的日志过滤把它关掉，见 `main`。
pub const TARGET: &str = "runode::startup";

/// `main` 第一行记下的时刻：单调时钟用来量之后的间隔，墙上时钟用来和内核记的进程启动时刻对齐。
struct Origin {
    instant: Instant,
    wall: SystemTime,
}

static ORIGIN: OnceLock<Origin> = OnceLock::new();

/// 进 `main` 之前花的时间，第一次打点时才去问内核。
static BEFORE_MAIN: OnceLock<Duration> = OnceLock::new();

/// 在 `main` 第一行调用，记下起点。只有第一次调用算数。
pub fn begin() {
    let _ = ORIGIN.set(Origin { instant: Instant::now(), wall: SystemTime::now() });
}

/// 打出 `main` 入口那个点。日志在 `main` 里装好以后才能打，所以起点由 `begin` 先记下，这里补打。
pub fn mark_main() {
    if let Some(origin) = ORIGIN.get() {
        emit("main", origin.instant);
    }
}

/// 打出名为 `node` 的点，时刻为现在。
pub fn mark(node: &'static str) {
    emit(node, Instant::now());
}

/// 只打一次的点，比如第一次绘制：之后再调用只剩一次原子读。没开时第一次调用后也不再问。
pub struct Once(AtomicBool);

impl Once {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// `happened` 为真时打出 `node` 并记下已打过；为假时下次再看。`happened` 只在这个点开着、
    /// 还没打过时才调用，可以放稍贵的判断（比如扫一遍屏幕）。
    pub fn mark_when(&self, node: &'static str, happened: impl FnOnce() -> bool) {
        if self.0.load(Ordering::Relaxed) {
            return;
        }
        if !enabled() {
            self.0.store(true, Ordering::Relaxed);
            return;
        }
        if happened() && !self.0.swap(true, Ordering::Relaxed) {
            mark(node);
        }
    }
}

fn enabled() -> bool {
    tracing::enabled!(target: TARGET, tracing::Level::INFO)
}

fn emit(node: &'static str, at: Instant) {
    if !enabled() {
        return;
    }
    let Some(origin) = ORIGIN.get() else {
        return;
    };
    let before_main = *BEFORE_MAIN
        .get_or_init(|| process_start().and_then(|start| origin.wall.duration_since(start).ok()).unwrap_or_default());
    let since_start = before_main + at.saturating_duration_since(origin.instant);
    let ms = (since_start.as_secs_f64() * 1e4).round() / 10.;
    tracing::info!(target: TARGET, node, ms);
}

/// 内核记的本进程启动时刻（fork 或 posix_spawn 的那一刻）。
#[cfg(target_os = "macos")]
fn process_start() -> Option<SystemTime> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: 缓冲区正好是一个 `proc_bsdinfo` 的大小，内核最多写这么多字节。
    let written =
        unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    if written != size {
        return None;
    }
    // SAFETY: 内核写满了整个结构体；结构体本身也已清零，全是整数字段，哪个值都合法。
    let info = unsafe { info.assume_init() };
    let since_epoch = Duration::from_secs(info.pbi_start_tvsec) + Duration::from_micros(info.pbi_start_tvusec);
    Some(SystemTime::UNIX_EPOCH + since_epoch)
}

#[cfg(not(target_os = "macos"))]
fn process_start() -> Option<SystemTime> {
    None
}
