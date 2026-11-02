//! 日志记到哪：开窗口的进程记到 stderr，宿主、采集、桥接这些后台进程各记各的日志文件。

use std::{fs::OpenOptions, path::PathBuf, sync::Mutex};

/// 日志文件超过这么大时，启动时清空重写。
const LOG_LIMIT: u64 = 8 << 20;

/// 开窗口的进程：记到 stderr。启动计时点默认不打，要看时设 `RUST_LOG=runode::startup=info`。
pub fn init_stderr() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("info,{}=off", crate::startup::TARGET))),
        )
        .init();
}

/// 后台进程：追加写到 `path`，超过 `LOG_LIMIT` 时清空重写；写不了时不记日志。
pub fn init_file(path: Option<PathBuf>) {
    let Some(path) = path else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT);
    let file = OpenOptions::new().create(true).append(!too_big).write(true).truncate(too_big).open(&path);
    let Ok(file) = file else { return };
    tracing_subscriber::fmt()
        .with_env_filter(
            // portal 的请求和会话对象在 zbus 建好代理之后才出现，它每次都为读不到属性报一条警告，不当回事。
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,zbus::proxy=error")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
}
