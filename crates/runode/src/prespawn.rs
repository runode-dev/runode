//! 启动时提前拉起第一个终端的 shell。shell 读完启动配置要几十毫秒，放在后台和 GPUI
//! 初始化、建窗口同时进行，等视图建好时提示符多半已经输出，第一帧就能画出来。
//!
//! 伪终端的行列数得在窗口量出来之前定下，所以沿用上次启动时第一个终端量到的尺寸，连同
//! 影响尺寸的那几项配置记在缓存目录里。配置变了或者还没有记录时不提前启动，照常在建视图
//! 时启动；记下的尺寸和这次量到的不符（比如换了显示器）也不要紧，第一次布局时照常调整。

use std::{
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use futures::channel::mpsc::UnboundedReceiver;

use crate::{
    config::Config,
    pty::{GridSize, Pty, PtyEvent},
};

/// 提前启动好的 shell，以及启动时用的尺寸。
pub struct Prespawned {
    pub size: GridSize,
    pub pty: Pty,
    pub rx: UnboundedReceiver<PtyEvent>,
}

static PENDING: Mutex<Option<JoinHandle<Option<Prespawned>>>> = Mutex::new(None);

/// 在后台线程里读记下的尺寸并启动 shell，不耽误主线程初始化 GPUI。
pub fn start() {
    let spawned = thread::Builder::new().name("prespawn".into()).spawn(|| {
        // 配置有问题时由主线程加载配置时报告，这里不重复。
        let config = tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            Config::load(true)
        });
        let size = recorded(&key(&config))?;
        match Pty::spawn(size, None, None, config.shell_integration) {
            Ok((pty, rx)) => Some(Prespawned { size, pty, rx }),
            Err(err) => {
                tracing::warn!("failed to start the shell early: {err:#}");
                None
            }
        }
    });
    match spawned {
        Ok(handle) => *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle),
        Err(err) => tracing::warn!("failed to start the prespawn thread: {err}"),
    }
}

/// 取走提前启动的 shell；后台线程还没做完时等它。只有第一次调用可能取到。
pub fn take() -> Option<Prespawned> {
    let handle = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()?;
    handle.join().ok().flatten()
}

/// 记下启动时第一个终端量到的尺寸，供下次启动用。只有进程里的第一次调用算数，
/// 那时布局的正是启动时的那个终端；和已有记录一样时不写。
pub fn remember(config: &Config, size: GridSize) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let key = key(config);
    // 读写文件不放在画第一帧的路上。
    let _ = thread::Builder::new().name("prespawn-record".into()).spawn(move || {
        if recorded(&key) == Some(size) {
            return;
        }
        let Some(path) = record_path() else {
            return;
        };
        let text = format!(
            "{key}\n{} {} {} {}\n",
            size.cols, size.rows, size.cell_width_px, size.cell_height_px
        );
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, text));
        if let Err(err) = written {
            tracing::debug!("failed to record the terminal size: {err}");
        }
    });
}

/// 决定第一个终端行列数的配置，加上版本号：新版本可能改了窗口布局。
fn key(config: &Config) -> String {
    format!(
        "{} {:?}",
        env!("CARGO_PKG_VERSION"),
        (
            &config.font_family,
            config.font_size,
            config.adjust_cell_height,
            config.window_padding_x,
            config.window_padding_y,
        )
    )
}

/// 按 `key` 记下的尺寸；没有记录或配置已经变了时为 `None`。
fn recorded(key: &str) -> Option<GridSize> {
    let text = std::fs::read_to_string(record_path()?).ok()?;
    let (recorded_key, size) = text.split_once('\n')?;
    if recorded_key != key {
        return None;
    }
    let mut values = size.split_whitespace().map(|v| v.parse().ok());
    let mut next = || values.next().flatten();
    Some(GridSize {
        cols: next()?,
        rows: next()?,
        cell_width_px: next()?,
        cell_height_px: next()?,
    })
}

fn record_path() -> Option<PathBuf> {
    runode_dirs::Dirs::from_env().prespawn_size_file()
}
