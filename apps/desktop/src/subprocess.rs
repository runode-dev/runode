//! 拉起子进程时的两件共用的事。一是结束时连同它再拉起的子孙一起结束：包装脚本拉起的真程序、凭据
//! helper 都可能拿着输出的管道。Unix 上让子进程自成一个进程组、到时给整组发 SIGKILL；Windows 上没有
//! 进程组，按父子关系结束整棵进程树（`taskkill /T`）。二是 Windows 上不给控制台程序开窗口：app 和
//! 宿主都没有控制台，不然每拉起一次就闪一个黑窗。

use std::process::Command;

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
