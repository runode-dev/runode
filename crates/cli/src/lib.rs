//! 命令行前端：`runode list`、`runode read`、`runode send`、`runode wait`。经宿主的 Unix socket
//! 按 `runode_protocol` 说话，不碰终端仿真和界面。
//!
//! 和桌面 app 是同一个可执行文件：带子命令时走这里（见 `wants_cli`），不带时开窗口。在 runode
//! 的终端里跑时，宿主设的环境变量告诉它连哪个 socket（`runode_protocol::ENV_SOCKET`）、自己在
//! 哪个会话里（`runode_protocol::ENV_SESSION`），所以 agent 能在自己的终端里调度别的终端。
//!
//! 命令的解析在 `args`，连宿主和收发消息在`client`，各个命令在 `commands`。

mod args;
mod client;
mod commands;

use std::{ffi::OsString, io::Write, path::PathBuf};

/// 桌面 app 给 shell 设的环境变量：runode 可执行文件的路径，没把它放进 PATH 时也能调用命令行。
pub const ENV_BIN: &str = "RUNODE_BIN";

/// 命令行的退出码。
pub mod exit {
    pub const OK: i32 = 0;
    /// 连不上宿主、找不到会话这类失败。
    pub const FAILED: i32 = 1;
    /// 参数写错了。
    pub const USAGE: i32 = 2;
    /// `wait` 等的会话结束了。
    pub const EXITED: i32 = 3;
    /// `wait` 超时，和 `timeout(1)` 一样。
    pub const TIMEOUT: i32 = 124;
}

/// 命令行从外面拿到的东西。
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// 宿主的 socket；为 `None` 时连不上。
    pub socket: Option<PathBuf>,
    /// 在 runode 的终端里跑时，所在会话的标识。
    pub session: Option<String>,
    /// 这次构建的标识，握手时报给宿主。
    pub build: String,
}

impl Env {
    /// 按当前进程的环境变量：先用宿主设的 socket，没有时按 runode 的目录约定找。
    pub fn from_process(build: &str) -> Self {
        let var = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty());
        Self {
            socket: var(runode_protocol::ENV_SOCKET)
                .map(PathBuf::from)
                .or_else(|| runode_paths::Dirs::from_env().host_socket_file()),
            session: var(runode_protocol::ENV_SESSION).map(|value| value.to_string_lossy().into_owned()),
            build: build.into(),
        }
    }
}

/// 可执行文件收到这些参数时走命令行：有参数，而且不是 macOS 启动 app 时自己加的那些
/// （`-psn_…` 进程序列号，`-NS…`、`-Apple…` 这类用户默认值）。
pub fn wants_cli(args: &[OsString]) -> bool {
    args.first().is_some_and(|first| {
        let first = first.to_string_lossy();
        !["-psn_", "-NS", "-Apple"].iter().any(|prefix| first.starts_with(prefix))
    })
}

/// 跑一条命令，返回退出码（见 `exit`）。`args` 不含程序名。
pub fn run(args: &[String], env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let command = match args::parse(args) {
        Ok(command) => command,
        Err(message) => {
            let _ = writeln!(err, "runode: {message}\nTry 'runode help'.");
            return exit::USAGE;
        }
    };
    match commands::run(command, env, out) {
        Ok(()) => exit::OK,
        Err(commands::Failure::Exited) => {
            let _ = writeln!(err, "runode: the session exited");
            exit::EXITED
        }
        Err(commands::Failure::Timeout) => {
            let _ = writeln!(err, "runode: timed out");
            exit::TIMEOUT
        }
        Err(commands::Failure::Error(error)) => {
            let _ = writeln!(err, "runode: {error:#}");
            exit::FAILED
        }
    }
}
