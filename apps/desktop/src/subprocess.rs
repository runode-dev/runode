//! 拉起子进程时的两件共用的事。一是结束时连同它再拉起的子孙一起结束：包装脚本拉起的真程序、凭据
//! helper 都可能拿着输出的管道。Unix 上让子进程自成一个进程组、到时给整组发 SIGKILL；Windows 上没有
//! 进程组，按父子关系结束整棵进程树（`taskkill /T`）。二是 Windows 上不给控制台程序开窗口：app 和
//! 宿主都没有控制台，不然每拉起一次就闪一个黑窗。

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// 命令文件名在程序名后面接的扩展名。Windows 上只认程序和批处理（npm 装的 agent 是 `.cmd`），不照搬
/// `PATHEXT`：那里还有 `.js`、`.vbs` 这类交给脚本宿主打开的，同名的脚本不是要找的命令。
const COMMAND_EXTENSIONS: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };

/// `dirs` 里按先后找到的第一个叫 `program` 的命令文件，文件名带着找到的扩展名。
pub(crate) fn find_command(dirs: &[PathBuf], program: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        COMMAND_EXTENSIONS.iter().map(|ext| dir.join(format!("{program}{ext}"))).find(|file| is_command(file))
    })
}

/// `file` 是 shell 能跑的命令：Unix 上是有可执行位的文件（符号链接按它指向的算），Windows 上扩展名已经说明了。
fn is_command(file: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// 拉起之前调：Unix 上让子进程自成一个进程组，组号就是它的进程号；Windows 上不给它开控制台窗口。
pub(crate) fn isolate(command: &mut Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    hide_window(command);
}

/// Windows 上拉起控制台程序时不开窗口（`CREATE_NO_WINDOW`）；别的系统上什么都不做。
pub(crate) fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, 0x0800_0000);
    #[cfg(not(windows))]
    let _ = command;
}

/// 结束 `isolate` 过的子进程 `pid` 和它的子孙。要在收尸之前调，不然进程号可能已经给了别的进程。
pub(crate) fn kill(pid: u32) {
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(pid) {
        // SAFETY: 只是给自己拉起的那个进程组发信号，不碰内存。
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        use std::process::Stdio;
        let mut command = Command::new("taskkill");
        command.args(["/T", "/F", "/PID", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null());
        hide_window(&mut command);
        let _ = command.stderr(Stdio::null()).status();
    }
}
