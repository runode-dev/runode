//! `runode --host`：终端宿主单独一个进程跑，由 app 拉起（见 `runode_host::launch`），app 退出后
//! 会话照常跑着。只开宿主的 socket 等前端连上来，没有会话也没有连接、持续 `IDLE_EXIT` 后自己退出，
//! 收到 `Shutdown` 时结束所有会话后退出。不碰 GPUI、提前拉起 shell 和通知：主题、要不要记命令历史
//! 由连上来的桌面告诉它。配置只读远程访问的两项，自己跟着配置文件开关监听（见
//! `remote_access::follow_config`），app 关着时手机也连得上；开着远程访问、又有手机可能连上来
//! （配对过的设备，或者正在配对）时不因空闲退出。
//!
//! `runode --host --take-over`（`take_over`）是升级时新版本的 app 拉起的新宿主：先接手 socket 上
//! 旧宿主的会话和 socket，再照常跑。

use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    os::fd::FromRawFd as _,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use runode_host::{BuildId, Host, STATUS_FD, TakeOverError, TakeOverOptions};

use crate::host_client::{HandoffFailure, HandoffStatus, READY_BY};

/// 没有会话也没有连接，持续这么久就退出。
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// 日志超过这么大时，启动时清空重写。
const LOG_LIMIT: u64 = 8 << 20;

/// 跑宿主，返回进程的退出码。
pub fn run() -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    init_logging(&dirs);
    let host = prepare(&dirs);
    match crate::host_client::listen(&host, &dirs) {
        Ok(socket) => tracing::info!("host {} listening on {}", std::process::id(), socket.display()),
        Err(err) => {
            // 多半是另一个宿主已经在跑（两个 app 同时拉起时由锁决出一个）。
            tracing::info!("the host does not start: {err:#}");
            return 1;
        }
    }
    serve(&host);
    0
}

/// 跟着配置开关远程访问，跑到空闲或收到 `Shutdown` 为止。
fn serve(host: &Host) {
    let remote = crate::remote_access::follow_config(host);
    let stopped = host.run_until_idle(IDLE_EXIT);
    // 先停远程访问再退出：交接时新宿主在等这个进程放开端口。
    drop(remote);
    tracing::info!("host {} exits: {stopped:?}", std::process::id());
}

/// `runode --host --take-over`：接手 socket 上旧宿主的会话（见 `Host::take_over`），把结果写成一行
/// `HandoffStatus` 交给拉起它的 app（状态管道，`STATUS_FD`），成了就照 `run` 跑下去；没成时会话
/// 还在旧宿主手里，退出。要回 `HandoffReady` 之前先写一行 `HandoffStatus::ready`，从启动起过了
/// `READY_BY` 就不再回，见 `host_client::handoff` 的模块文档。返回进程的退出码。
pub fn take_over() -> i32 {
    let ready_by = Instant::now() + READY_BY;
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
    let status = Arc::new(Mutex::new(status));
    let on_ready = {
        let status = status.clone();
        Arc::new(move || status.lock().unwrap_or_else(PoisonError::into_inner).note(&HandoffStatus::ready()))
    };
    let options = TakeOverOptions { ready_by: Some(ready_by), on_ready: Some(on_ready), ..TakeOverOptions::default() };
    let result = host.take_over(&socket, options);
    let mut status = std::mem::replace(&mut *status.lock().unwrap_or_else(PoisonError::into_inner), StatusPipe(None));
    match result {
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
            // 旧宿主退出前还占着远程访问的端口和锁，这边开不了时隔一会儿再试，见 `runode_remote_access::Service`。
            serve(&host);
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
    crate::host_client::expose_cli(&host);
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

    /// 写一行结果后关掉；写过一次后再调什么都不做。写不了时记日志。
    fn report(&mut self, status: &HandoffStatus) {
        self.note(status);
        self.0 = None;
    }

    /// 写一行 JSON，管道接着开着。写不了时记日志。
    fn note(&mut self, status: &HandoffStatus) {
        let Some(file) = &mut self.0 else { return };
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
