//! `runode --host`：终端宿主单独一个进程跑，由 app 拉起（见 `runode_host::launch`），app 退出后
//! 会话照常跑着。只开宿主的 socket 等前端连上来，没有会话也没有连接、持续 `IDLE_EXIT` 后自己退出，
//! 收到 `Shutdown` 时结束所有会话后退出。不碰 GPUI、配置、提前拉起 shell 和通知：主题、要不要记
//! 命令历史由连上来的桌面告诉它。

use std::{fs::OpenOptions, sync::Mutex, time::Duration};

use runode_host::{BuildId, Host};

/// 没有会话也没有连接，持续这么久就退出。
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// 日志超过这么大时，启动时清空重写。
const LOG_LIMIT: u64 = 8 << 20;

/// 跑宿主，返回进程的退出码。
pub fn run() -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    init_logging(&dirs);
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
    // 开 socket 之前就标上：第一个连上来的前端也认得出这是单独跑的宿主。
    host.mark_standalone();
    // 之后启动的 shell 里有 `runode_cli::ENV_BIN`，指向这个可执行文件，没把 runode 放进 PATH 也能用命令行。
    if let Ok(exe) = std::env::current_exe() {
        host.set_env(runode_cli::ENV_BIN, exe);
    }
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
