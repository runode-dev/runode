//! 命令行前端：`runode list`、`read`、`send`、`wait`、`open`、`kill`、`focus`、`setup`、`statusline` 和 `remote`。
//! 经宿主的 Unix socket 按 `runode_protocol` 说话，不碰终端仿真和界面；`remote`（给手机配对远程
//! 访问、列出和撤销配对过的设备）不经宿主，读写 `runode_remote_access` 管的文件。
//!
//! 和桌面 app 是同一个可执行文件：带子命令时走这里（见 `wants_cli`），不带时开窗口。在 runode
//! 的终端里跑时，宿主设的环境变量告诉它连哪个 socket（`runode_protocol::ENV_SOCKET`）、自己在
//! 哪个会话里（`runode_protocol::ENV_SESSION`），所以 agent 能在自己的终端里调度别的终端。
//!
//! 命令的解析在 `args`，连宿主和收发消息在 `client`，按写法找会话在 `select`，各个命令在
//! `commands`，给 agent 装使用说明在 `setup`，Claude Code 的状态栏在 `statusline`，远程访问在 `remote`。

mod args;
mod client;
mod commands;
mod remote;
mod select;
mod setup;
mod statusline;

use std::{ffi::OsString, io::Write, path::PathBuf};

pub use setup::{SetupTarget, setup, setup_path};
pub use statusline::{setup_statusline, statusline_settings_path};

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
    /// `wait --for command` 等的命令运行失败了（退出码不是 0），真正的退出码打印在标准输出上。
    pub const COMMAND_FAILED: i32 = 4;
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
    /// runode 的各个目录，`remote` 在这里找远程访问的文件。其中的家目录：显示目录时缩成 `~`，
    /// `setup` 往这里装使用说明。
    pub dirs: runode_paths::Dirs,
}

impl Env {
    /// 按当前进程的环境变量：先用宿主设的 socket，没有时按 runode 的目录约定找。
    pub fn from_process(build: &str) -> Self {
        let var = |key: &str| std::env::var_os(key).filter(|value| !value.is_empty());
        let dirs = runode_paths::Dirs::from_env();
        Self {
            socket: var(runode_protocol::ENV_SOCKET).map(PathBuf::from).or_else(|| dirs.host_socket_file()),
            session: var(runode_protocol::ENV_SESSION).map(|value| value.to_string_lossy().into_owned()),
            build: build.into(),
            dirs,
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
    match commands::run(command, env, out, err) {
        Ok(()) => exit::OK,
        Err(commands::Failure::CommandFailed) => exit::COMMAND_FAILED,
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
