//! 拉起 agent 时的进程处理：结束时连同它再拉起的子孙一起结束（包装脚本拉起的真 agent、后台的 helper
//! 都可能拿着输出的管道），Unix 上让它自成一个进程组、给整组发 SIGKILL，Windows 上按父子关系结束整棵
//! 进程树；Windows 上不给控制台程序开窗口。

#[cfg(windows)]
use std::process::Stdio;
use std::{
    io,
    process::{Child, Command},
};

/// `CREATE_NO_WINDOW` 的值：拉起控制台程序时不给它开窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 拉起之前调：Unix 上让子进程自成一个进程组，组号就是它的进程号；Windows 上不给它开控制台窗口。
pub(crate) fn isolate(command: &mut Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    hide_window(command);
}

/// Windows 上拉起控制台程序时不开窗口（`CREATE_NO_WINDOW`）：宿主和 app 都没有控制台，不然每拉起一次就闪
/// 一个黑窗。别的系统上什么都不做。
pub(crate) fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

/// `child` 退出了没有，不收尸：Unix 上用 `waitid` 的 `WNOWAIT`，组号在之后 `kill_tree` 时还是它的。
#[cfg(unix)]
pub(crate) fn has_exited(child: &mut Child) -> io::Result<bool> {
    // SAFETY: `info` 是纯数据，全零合法；`waitid` 只写它。
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: 等的是自己拉起的子进程，`info` 指向局部变量。
    let result =
        unsafe { libc::waitid(libc::P_PID, child.id(), &raw mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // 没有退出时 `waitid` 不填 `info`，`si_pid` 保持调用前的零。
    // SAFETY: `waitid` 成功返回，`info` 要么由它填好，要么还是全零。
    Ok(unsafe { info.si_pid() } != 0)
}

/// `Child` 拿着进程句柄，收了尸进程号也不会被别的进程占去，直接 `try_wait`。
#[cfg(windows)]
pub(crate) fn has_exited(child: &mut Child) -> io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

/// 结束 `isolate` 过的子进程 `pid` 和它的整组进程。要在收尸之前调，不然组号可能已经给了别的进程。
#[cfg(unix)]
pub(crate) fn kill_tree(pid: u32) {
    if let Ok(group) = libc::pid_t::try_from(pid) {
        // SAFETY: 只是给自己拉起的那个进程组发信号；组长还没收尸，组号不会被别的进程占去。
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
}

/// 连同子孙进程一起强杀（`taskkill /T /F`）。
/// shortcut: 进程已经退出时 taskkill 找不到这棵树，留下的子孙进程不杀；真碰上再改用 Job 对象。
#[cfg(windows)]
pub(crate) fn kill_tree(pid: u32) {
    let mut command = Command::new("taskkill");
    command.args(["/T", "/F", "/PID", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null());
    hide_window(&mut command);
    let _ = command.stderr(Stdio::null()).status();
}
