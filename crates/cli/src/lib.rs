//! 命令行前端：`runode list`、`read`、`send`、`wait`、`open`、`kill`、`focus`、`todo`、`schedule`、`remote`
//! 和 `service`。
//! 经宿主的 Unix socket 按 `runode_protocol` 说话，不碰终端仿真和界面；`remote`（给手机配对远程
//! 访问、列出和撤销配对过的设备）不经宿主，读写 `runode_remote` 管的文件。
//!
//! 和桌面 app 是同一个可执行文件：带子命令时走这里（见 `wants_cli`），不带时开窗口。在 runode
//! 的终端里跑时，宿主设的环境变量告诉它连哪个 socket（`runode_protocol::ENV_SOCKET`）、自己在
//! 哪个会话里（`runode_protocol::ENV_SESSION`），所以 agent 能在自己的终端里调度别的终端。
//!
//! 命令的解析在 `args`，连宿主和收发消息在 `client`，按写法找会话在 `select`，各个命令在
//! `commands`，用法说明在 `help`，远程访问在 `remote`，看板（`todo`、
//! `schedule`）在 `todo`，定时的本地钟点在 `local_time`，shell 补全在 `complete`。

mod args;
mod client;
mod commands;
mod complete;
mod help;
mod local_time;
mod remote;
mod select;
mod service;
mod todo;

use std::{ffi::OsString, io::Write, path::PathBuf};

/// 桌面 app 给 shell 设的环境变量：runode 可执行文件的路径，没把它放进 PATH 时也能调用命令行。
pub const ENV_BIN: &str = "RUNODE_BIN";

/// shell 的补全脚本设了这个环境变量来问候选，值是正在打的词在整条命令行上的序号（程序名是 0），
/// 见 `completion_index` 和 `complete`。
pub const ENV_GET_COMPLETIONS: &str = "RUNODE_GET_COMPLETIONS";

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
    /// runode 的各个目录，`remote` 在这里找远程访问的文件。其中的家目录：显示目录时缩成 `~`。
    pub dirs: runode_paths::Dirs,
    /// 连不上宿主时拿它拉起一个（`<它> --host`）再连；为 `None` 时不拉起，报错说宿主没在跑。没有桌面
    /// app 替命令行拉起宿主的地方（无头的 Linux）才给。
    pub launch_host: Option<PathBuf>,
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
            launch_host: None,
        }
    }
}

/// 进程是补全脚本为了问候选起的（设了 `ENV_GET_COMPLETIONS`）时，正在打的是第几个参数（从 0 数，
/// 不含程序名）。值不是数字或者指着程序名时当没设。
pub fn completion_index() -> Option<usize> {
    let index: usize = std::env::var(ENV_GET_COMPLETIONS).ok()?.trim().parse().ok()?;
    index.checked_sub(1)
}

/// 打印第 `index` 个参数（见 `completion_index`）的候选，给 `runode completions` 的脚本用：第一行
/// 是 shell 另外还能补什么（`normal`、`files`、`dirs`），后面每行一个候选，制表符后面是说明。
/// 宿主没开时没有会话和任务的候选，不报错。
pub fn complete(args: &[String], index: usize, env: &Env, out: &mut dyn Write) -> i32 {
    match complete::complete(args, index, env, out) {
        Ok(()) => exit::OK,
        Err(_) => exit::FAILED,
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

/// Windows 上让标准输出和标准错误所在的控制台认 VT 转义（`ENABLE_VIRTUAL_TERMINAL_PROCESSING`）：conhost 和
/// 没打开它的 ConPTY 会话默认把转义码原样印出来。不是控制台（重定向到文件、管道）时什么都不做。命令行程序在
/// 写输出之前调一次。
pub fn prepare_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
            SetConsoleMode,
        };
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: 句柄由系统给，只读写控制台的模式字。
            unsafe {
                let handle = GetStdHandle(which);
                let mut mode = 0;
                if GetConsoleMode(handle, &mut mode) != 0 {
                    SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }
    }
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
        Err(commands::Failure::Signal(signal)) => 128 + signal,
        Err(commands::Failure::Timeout) => {
            let _ = writeln!(err, "runode: timed out");
            exit::TIMEOUT
        }
        // 读输出的一头先关了（`| head`）：照别的命令行工具安静地停下。Rust 忽略 SIGPIPE，写不进去只是个
        // 错误；这个进程也是桌面 app 和宿主，不能全局改回 SIGPIPE。和宿主的连接断了的错误不带 `io::Error`。
        // `--json` 写的是 `serde_json::Error`，它的 `source` 跳过了里面的 `io::Error`，要单独认。
        Err(commands::Failure::Error(error))
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind)
                    .or_else(|| cause.downcast_ref::<serde_json::Error>().and_then(serde_json::Error::io_error_kind))
                    == Some(std::io::ErrorKind::BrokenPipe)
            }) =>
        {
            exit::OK
        }
        Err(commands::Failure::Error(error)) => {
            let _ = writeln!(err, "runode: {error:#}");
            exit::FAILED
        }
    }
}
