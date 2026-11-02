//! 子 shell 及其伪终端。
//!
//! 读线程把 PTY 输出交给创建时给的 `PtySink`，由宿主那边的会话线程接着处理。写入（按键、
//! 粘贴和 VT 查询的回复）经 `PtyWriter` 排进这个伪终端自己的写队列，由写线程按顺序写出，
//! 所以写入方从不阻塞：给一个不读 stdin 的程序粘贴一大段时，等着的只有写线程。
//!
//! Unix 上的伪终端（`unix`）能在宿主升级时交给新宿主，shell 不中断；Windows 上的是 ConPTY
//! （`conpty`）：没有能交出去的描述符，不能交接，读写都是阻塞的。

#[cfg(windows)]
mod conpty;
mod fish_marks;
#[cfg(unix)]
mod unix;

use std::{
    ffi::OsString,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
};

use portable_pty::PtySize;
use runode_agent::{ForegroundJob, ForegroundProcess};
use runode_types::grid::GridSize;

#[cfg(windows)]
pub(crate) use conpty::default_shell_in;
#[cfg(windows)]
use conpty::group_members;
#[cfg(windows)]
pub use conpty::process_args;
#[cfg(windows)]
use conpty::process_args as args_of;
#[cfg(windows)]
pub(crate) use conpty::process_name;
#[cfg(windows)]
pub use conpty::{Pty, default_shell};
#[cfg(unix)]
pub(crate) use unix::process_name;
#[cfg(unix)]
pub use unix::{AdoptError, Pty, PtyHandoff, default_shell, login_shell, process_args, raise_fd_limit};
#[cfg(unix)]
use unix::{args_of, group_members};

/// 进程号：Unix 上是 `pid_t`，Windows 上是 `u32`。
#[cfg(unix)]
pub(crate) type Pid = libc::pid_t;
#[cfg(windows)]
pub(crate) type Pid = u32;

/// `pid` 写成对外用的 `u32`；不是正数的（Unix 上）为 `None`。
pub(crate) fn pid_u32(pid: Pid) -> Option<u32> {
    #[cfg(unix)]
    return u32::try_from(pid).ok();
    #[cfg(windows)]
    Some(pid)
}

/// 写队列里还没写出的字节超过这么多时丢掉终端查询的回复：程序不读输入时，不断发查询的输出
/// 会让回复无限地堆在队列里。
const MAX_QUEUED_FOR_REPLY: usize = 1 << 20;

/// 读线程交给 `PtySink` 的事件。
pub enum PtyEvent {
    Output(Arc<[u8]>),
    /// PTY 读到 EOF 或出错：子进程已经退出。接手来的会话里 shell 退出也算，见 `Pty::adopt`。
    Exited,
}

/// 收 PTY 输出的一方，在读线程里调用。返回 false 表示不再要了，读线程随之结束。可以阻塞，
/// 用来给读线程限流：收的一方积压太多时让它先别读。
pub type PtySink = Box<dyn FnMut(PtyEvent) -> bool + Send>;

/// 写队列里的一项。
enum WriterMsg {
    Data(Vec<u8>),
    /// 交接：能写的写完，写不进去的连同后面排着的交回来，然后写线程结束，见 `Pty::release`。
    Finish(mpsc::Sender<Vec<u8>>),
}

/// 往 PTY 写的一端：只把数据排进写队列，不等它写出去。第二项是队列里还没写出的字节数。
#[derive(Clone)]
pub struct PtyWriter(mpsc::Sender<WriterMsg>, Arc<AtomicUsize>);

impl PtyWriter {
    pub fn write(&self, data: &[u8]) {
        if !data.is_empty() {
            self.send(data.to_vec());
        }
    }

    /// 同 `write`，数据已经在自己的缓冲里时不必再复制一份。
    pub fn send(&self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        let len = data.len();
        self.1.fetch_add(len, Ordering::Relaxed);
        // 写线程已经结束（PTY 出错、关了或者交出去了）时没有可写的地方，丢掉。
        if self.0.send(WriterMsg::Data(data)).is_err() {
            self.1.fetch_sub(len, Ordering::Relaxed);
            tracing::debug!("pty writer is gone, input dropped");
        }
    }

    /// 同 `write_reply`，用来写终端查询的回复：队列里积压超过 `MAX_QUEUED_FOR_REPLY` 时丢掉。用户的
    /// 按键和粘贴不受这个限制。
    pub fn reply(&self, data: &[u8]) {
        if self.1.load(Ordering::Relaxed) > MAX_QUEUED_FOR_REPLY {
            tracing::debug!("pty write queue is full, terminal query reply dropped");
            return;
        }
        self.write_reply(data);
    }

    /// 同 `write`，写的是终端给程序的回话（查询应答、剪贴板读请求的回话），不受积压上限。
    pub fn write_reply(&self, data: &[u8]) {
        #[cfg(windows)]
        let encoded = conpty::encode_reply(data);
        #[cfg(windows)]
        let data = encoded.as_ref();
        self.write(data);
    }
}

/// 把当前线程设成交互用的服务质量（macOS 的 `QOS_CLASS_USER_INTERACTIVE`），按键到回显路上的
/// 线程不被调度到能效核上排队。别的系统上什么都不做。
pub fn set_current_thread_interactive() {
    #[cfg(target_os = "macos")]
    {
        let result = unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
        if result != 0 {
            tracing::debug!("failed to raise the thread's QoS: {result}");
        }
    }
}

/// 拉起 runode 的那个终端和 agent 会话给子进程设的标识，说的不是 runode 的终端，shell 不继承：
/// Claude Code 带着 `CLAUDE_CODE_CHILD_SESSION` 启动时不在 `~/.claude/sessions` 登记自己的对话，
/// 重启后接不回；tmux 见到 `TMUX` 当成嵌套。只列这些程序自己设的名字，`CLAUDE_CODE_` 开头的
/// 还有用户自己的设置。
const OUTER_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_CI",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_WINDOW_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "STY",
    "TERMINAL_EMULATOR",
    "TERM_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "WT_PROFILE_ID",
    "WT_SESSION",
];

/// 新起的 shell 启动时拿到的 PATH，它的启动脚本再改的不算。Windows 上每次从注册表重读系统和用户的环境变量，
/// 装了程序、改了 PATH 以后新开的终端不用重启宿主就能用；别的系统是本进程的。读注册表要时间，不在界面线程上调。
pub fn spawn_path() -> Option<OsString> {
    portable_pty::CommandBuilder::new("").get_env("PATH").map(ToOwned::to_owned)
}

/// 整组都由终端自己设的变量前缀，同样不继承。
const OUTER_SESSION_PREFIXES: &[&str] = &["ALACRITTY_", "GHOSTTY_", "HERDR_", "ITERM_", "VSCODE_", "WEZTERM_"];

fn drop_outer_session(cmd: &mut portable_pty::CommandBuilder) {
    // Codex 的工具命令带着 CODEX_CI=1、NO_COLOR=1；从工具里启动 app 时，禁色只属于那次
    // 非交互命令，不能传给新终端。普通启动继承的 NO_COLOR 仍是用户的选择。
    if cmd.get_env("CODEX_CI") == Some(std::ffi::OsStr::new("1"))
        && cmd.get_env("NO_COLOR") == Some(std::ffi::OsStr::new("1"))
    {
        cmd.env_remove("NO_COLOR");
    }
    let outer: Vec<String> = cmd
        .iter_full_env_as_str()
        .map(|(key, _)| key)
        .filter(|key| {
            OUTER_SESSION_VARS.contains(key) || OUTER_SESSION_PREFIXES.iter().any(|prefix| key.starts_with(prefix))
        })
        .map(str::to_owned)
        .collect();
    for key in outer {
        cmd.env_remove(key);
    }
}

fn pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.cols.saturating_mul(size.cell_width_px),
        pixel_height: size.rows.saturating_mul(size.cell_height_px),
    }
}

/// 以 `leader` 为组长的进程组里的全部进程，带着进程名和参数，认 agent 用。一个都读不到时
/// 为 `None`。
pub(crate) fn process_group(leader: Pid) -> Option<ForegroundJob> {
    let processes: Vec<ForegroundProcess> = group_members(leader)
        .into_iter()
        .filter_map(|pid| {
            let (exe, argv) = args_of(pid).unzip();
            Some(ForegroundProcess {
                pid: pid_u32(pid)?,
                name: process_name(pid)?,
                exe: exe.filter(|exe| !exe.is_empty()),
                argv0: argv.as_ref().and_then(|argv| argv.first()).and_then(|first| {
                    let name = first.rsplit(std::path::is_separator).next().unwrap_or(first);
                    let name = name.strip_prefix('-').unwrap_or(name);
                    (!name.is_empty()).then(|| name.to_owned())
                }),
                argv,
            })
        })
        .collect();
    let leader = pid_u32(leader)?;
    (!processes.is_empty()).then_some(ForegroundJob { leader, processes })
}

pub fn dir_label(path: &Path) -> String {
    if runode_paths::Dirs::from_env().is_home(path) {
        return "~".into();
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        // 根目录没有文件名。
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod environment_tests {
    use super::drop_outer_session;
    use portable_pty::CommandBuilder;

    /// 外层终端和 Claude Code 的会话标识不传给 shell，用户自己的设置和 runode 的变量照传。
    #[cfg(unix)]
    #[test]
    fn the_shell_does_not_inherit_the_outer_session() {
        let mut cmd = CommandBuilder::new("sh");
        let dropped = ["CLAUDE_CODE_CHILD_SESSION", "CLAUDECODE", "ITERM_SESSION_ID", "TMUX", "VSCODE_IPC_HOOK_CLI"];
        let kept = ["CLAUDE_CODE_USE_BEDROCK", "KITTY_CONFIG_DIRECTORY", "RUNODE_SESSION", "TMUX_TMPDIR"];
        for key in dropped.iter().chain(&kept) {
            cmd.env(key, "1");
        }
        drop_outer_session(&mut cmd);
        assert!(dropped.iter().all(|key| cmd.get_env(key).is_none()));
        assert!(kept.iter().all(|key| cmd.get_env(key).is_some()));
    }

    #[test]
    fn automation_color_settings_do_not_disable_new_terminal_colors() {
        let mut cmd = CommandBuilder::new("sh");
        cmd.env_clear();
        cmd.env("CODEX_CI", "1");
        cmd.env("NO_COLOR", "1");
        drop_outer_session(&mut cmd);
        assert!(cmd.get_env("CODEX_CI").is_none());
        assert!(cmd.get_env("NO_COLOR").is_none());
    }

    #[test]
    fn user_color_preferences_survive_normal_startup() {
        for (marker, color) in [(None, "1"), (Some("0"), "1"), (Some("1"), "custom")] {
            let mut cmd = CommandBuilder::new("sh");
            cmd.env_clear();
            if let Some(marker) = marker {
                cmd.env("CODEX_CI", marker);
            }
            cmd.env("NO_COLOR", color);
            drop_outer_session(&mut cmd);
            assert_eq!(cmd.get_env("NO_COLOR"), Some(std::ffi::OsStr::new(color)));
            assert!(cmd.get_env("CODEX_CI").is_none());
        }
    }
}
