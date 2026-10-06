//! `runode --host`：终端宿主单独一个进程跑，由 app 拉起（见 `runode_host::launch`），app 退出后
//! 会话照常跑着。只开宿主的 socket 等前端连上来，没有会话也没有连接、持续 `IDLE_EXIT` 后自己退出，
//! 收到 `Shutdown` 时结束所有会话后退出。不碰 GPUI、配置、提前拉起 shell 和通知：主题、要不要记
//! 命令历史由连上来的桌面告诉它。
//!
//! `runode --host --take-over`（`take_over`）是升级时新版本的 app 拉起的新宿主：先接手 socket 上
//! 旧宿主的会话和 socket，再照常跑。

use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    os::fd::FromRawFd as _,
    sync::Mutex,
    time::{Duration, Instant},
};

use runode_host::{BuildId, Host, STATUS_FD, TakeOverError, TakeOverOptions};

use crate::session_host::{HandoffFailure, HandoffStatus};

/// 没有会话也没有连接，持续这么久就退出。
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// 日志超过这么大时，启动时清空重写。
const LOG_LIMIT: u64 = 8 << 20;

/// 跑宿主，返回进程的退出码。
pub fn run() -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    init_logging(&dirs);
    let host = prepare(&dirs);
    let listened = dirs.create_runtime_dir().map_err(anyhow::Error::from).and_then(|_| {
        let socket = dirs.host_socket_file().ok_or_else(|| anyhow::anyhow!("the socket path is too long"))?;
        let lock = dirs.host_lock_file().ok_or_else(|| anyhow::anyhow!("no place for the host lock"))?;
        host.listen(&socket, &lock)?;
        tracing::info!("host {} listening on {}", std::process::id(), socket.display());
        Ok(())
    });
    if let Err(err) = listened {
        // 多半是另一个宿主已经在跑（两个 app 同时拉起时由锁决出一个）。
        tracing::info!("the host does not start: {err:#}");
        return 1;
    }
    let stopped = host.run_until_idle(IDLE_EXIT);
    tracing::info!("host {} exits: {stopped:?}", std::process::id());
    0
}

/// `runode --host --take-over`：接手 socket 上旧宿主的会话（见 `Host::take_over`），把结果写成一行
/// `HandoffStatus` 交给拉起它的 app（状态管道，`STATUS_FD`），成了就照 `run` 跑下去；没成时会话
/// 还在旧宿主手里，退出。返回进程的退出码。
pub fn take_over() -> i32 {
    // 先于打开任何文件：没有状态管道时 `STATUS_FD` 这个号会被日志文件这类占去。
    let mut status = StatusPipe::take();
    let dirs = runode_paths::Dirs::from_env();
    init_logging(&dirs);
    if status.0.is_none() {
        tracing::warn!("no status pipe on fd {STATUS_FD}; nobody will hear how the takeover went");
    }
    let host = prepare(&dirs);
    let Some(socket) = dirs.host_socket_file() else {
        tracing::warn!("the socket path is too long, nothing to take over");
        status.report(&HandoffStatus::failed(HandoffFailure::Failed { message: "the socket path is too long".into() }));
        return 1;
    };
    let started = Instant::now();
    tracing::info!("host {} taking over the sessions on {}", std::process::id(), socket.display());
    match host.take_over(&socket, TakeOverOptions::default()) {
        Ok(report) => {
            tracing::info!(
                "host {} took over {} sessions in {:?}; {} of them without scrollback: {:?}",
                std::process::id(),
                report.sessions,
                started.elapsed(),
                report.replayed.len(),
                report.replayed,
            );
            status.report(&HandoffStatus::took_over(report.sessions, report.replayed));
            // 关掉管道，app 不必等这个进程退出。
            drop(status);
            let stopped = host.run_until_idle(IDLE_EXIT);
            tracing::info!("host {} exits: {stopped:?}", std::process::id());
            0
        }
        Err(err) => {
            tracing::warn!("host {} did not take over after {:?}: {err}", std::process::id(), started.elapsed());
            let failure = match err {
                TakeOverError::Refused(reason) => HandoffFailure::Refused { reason },
                TakeOverError::PreHandoff => HandoffFailure::PreHandoff,
                TakeOverError::Failed(message) => HandoffFailure::Failed { message },
            };
            status.report(&HandoffStatus::failed(failure));
            1
        }
    }
}

/// 两种跑法共用的准备：放宽描述符上限、换到家目录，建好标成单独跑的宿主。
fn prepare(dirs: &runode_paths::Dirs) -> Host {
    if let Err(err) = runode_terminal::pty::raise_fd_limit() {
        tracing::warn!("failed to raise the open file limit: {err}");
    }
    // 不占着拉起它的地方（比如某个仓库里的目录）；shell 各自从自己的目录开始。
    if let Some(home) = &dirs.home
        && let Err(err) = std::env::set_current_dir(home)
    {
        tracing::warn!("failed to change to {}: {err}", home.display());
    }
    let host = Host::new(BuildId(env!("RUNODE_BUILD").into()));
    // 开 socket（或接手 socket）之前就标上：第一个连上来的前端也认得出这是单独跑的宿主。
    host.mark_standalone();
    // 之后启动的 shell 里有 `runode_cli::ENV_BIN`，指向这个可执行文件，没把 runode 放进 PATH 也能用命令行。
    if let Ok(exe) = std::env::current_exe() {
        host.set_env(runode_cli::ENV_BIN, exe);
    }
    host
}

/// 拉起这个进程的 app 读结果的管道（`STATUS_FD`）；没有（比如手动跑的）时为空，报告什么都不做。
struct StatusPipe(Option<File>);

impl StatusPipe {
    /// 认领 `STATUS_FD`：它开着而且是管道时才用，设上 close-on-exec，之后启动的 shell 不会带走它
    /// （带走了 app 就等不到结尾）。
    fn take() -> Self {
        // SAFETY: 输出参数指向本地变量；只查这个号上是什么。
        let pipe = unsafe {
            let mut stat: libc::stat = std::mem::zeroed();
            libc::fstat(STATUS_FD, &mut stat) == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFIFO
        };
        if !pipe {
            return Self(None);
        }
        // SAFETY: 只改这个描述符的标志。
        unsafe { libc::fcntl(STATUS_FD, libc::F_SETFD, libc::FD_CLOEXEC) };
        // SAFETY: 这个号开着，是拉起时接上的管道写端，进程里别处不用它；交给 `File` 后由它关。
        Self(Some(unsafe { File::from_raw_fd(STATUS_FD) }))
    }

    /// 写一行 JSON 后关掉；写过一次后再调什么都不做。写不了时记日志。
    fn report(&mut self, status: &HandoffStatus) {
        let Some(mut file) = self.0.take() else { return };
        let written = serde_json::to_vec(status).map_err(std::io::Error::other).and_then(|mut line| {
            line.push(b'\n');
            file.write_all(&line)
        });
        if let Err(err) = written {
            tracing::warn!("failed to report the takeover to the app: {err}");
        }
    }
}

/// 日志追加写到 `host_log_file`，超过 `LOG_LIMIT` 时清空重写；写不了时不记日志。
fn init_logging(dirs: &runode_paths::Dirs) {
    let Some(path) = dirs.host_log_file() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT);
    let file = OpenOptions::new().create(true).append(!too_big).write(true).truncate(too_big).open(&path);
    let Ok(file) = file else { return };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
}
