//! 拉起子进程时的两件共用的事。一是结束时连同它再拉起的子孙一起结束：包装脚本拉起的真程序、凭据
//! helper 都可能拿着输出的管道。Unix 上让子进程自成一个进程组、到时给整组发 SIGKILL；Windows 上没有
//! 进程组，按父子关系结束整棵进程树（`taskkill /T`）。二是 Windows 上不给控制台程序开窗口：app 和
//! 宿主都没有控制台，不然每拉起一次就闪一个黑窗。

use std::{
    io::{self, Read},
    process::{Command, Output, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// `output_within` 没拿到输出的原因。
#[derive(Debug)]
pub(crate) enum RunError {
    /// 拉不起来。
    Spawn(io::Error),
    /// 等它退出时出错。
    Wait(io::Error),
    /// 到点没跑完，已经连同子孙一起结束了。
    Timeout,
}

/// 跑完 `command` 拿输出（标准输入是空的）；`timeout` 内没跑完时连同它拉起的子孙一起结束（`isolate`、`kill`）。
/// 输出另起线程读，多过管道的缓冲时它不会卡在写上；到点时不等这两个线程：脱离了进程组的子孙进程可能还拿着
/// 管道。两个管道都读到头了才收尸：收了尸它的进程号就可能给了别的进程，到点时会杀到别人。
pub(crate) fn output_within(command: &mut Command, timeout: Duration) -> Result<Output, RunError> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate(command);
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let (stdout, stderr) = (read_all(child.stdout.take()), read_all(child.stderr.take()));
    let deadline = Instant::now() + timeout;
    loop {
        let waited = if stdout.is_finished() && stderr.is_finished() { child.try_wait() } else { Ok(None) };
        match waited {
            Ok(Some(status)) => {
                let (stdout, stderr) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
                return Ok(Output { status, stdout, stderr });
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            waited => {
                kill(child.id());
                let _ = child.wait();
                return Err(waited.err().map_or(RunError::Timeout, RunError::Wait));
            }
        }
    }
}

/// 在另一个线程里读完 `pipe`。
fn read_all(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut out);
        }
        out
    })
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
    std::os::windows::process::CommandExt::creation_flags(
        command,
        windows_sys::Win32::System::Threading::CREATE_NO_WINDOW,
    );
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
        let mut command = Command::new("taskkill");
        command.args(["/T", "/F", "/PID", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null());
        hide_window(&mut command);
        let _ = command.stderr(Stdio::null()).status();
    }
}
