//! 看着一个不是自己子进程的 shell 什么时候退出。
//!
//! 接手来的会话（见 `Pty::adopt`）里 shell 不是本进程的子进程，不能 `waitpid`，也拿不到退出码。
//! macOS 上用 kqueue 的 `EVFILT_PROC`/`NOTE_EXIT` 等它退出：kqueue 的描述符本身可以 `poll`，
//! 读线程把它和 PTY 放在一起等。别的系统上退回到定时 `kill(pid, 0)`，进程没了（`ESRCH`）才算
//! 退出；这样认不出还没被回收的僵尸进程，不过接手的一方不是父进程，僵尸由 init 很快回收。

use std::{
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// 没有可等的描述符时，读线程隔这么久查一次 shell 还在不在。
pub(super) const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 结束接手来的 shell 时，发 SIGHUP 后等它自己退出的时间，过了就 SIGKILL。
const HANGUP_GRACE: Duration = Duration::from_millis(200);

pub(super) struct ExitWatch {
    pid: libc::pid_t,
    /// 注册了 `NOTE_EXIT` 的 kqueue；为 `None` 时只能 `kill(pid, 0)` 轮询。
    queue: Option<OwnedFd>,
    exited: AtomicBool,
}

impl ExitWatch {
    pub(super) fn new(pid: libc::pid_t) -> Self {
        let (queue, exited) = register(pid);
        Self { pid, queue, exited: AtomicBool::new(exited) }
    }

    pub(super) fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// shell 退出时变得可读的描述符；为 `None` 时调用方要按 `EXIT_POLL_INTERVAL` 自己来问
    /// `has_exited`。
    pub(super) fn fd(&self) -> Option<BorrowedFd<'_>> {
        self.queue.as_ref().map(AsFd::as_fd)
    }

    /// shell 已经退出了。一旦为 true 就一直是 true。
    pub(super) fn has_exited(&self) -> bool {
        if self.exited.load(Ordering::Acquire) {
            return true;
        }
        let exited = match &self.queue {
            Some(queue) => readable(queue.as_fd()),
            None => gone(self.pid),
        };
        if exited {
            self.exited.store(true, Ordering::Release);
        }
        exited
    }

    /// 结束 shell：给它的进程组和终端当前的前台进程组 `foreground` 发 SIGHUP，等一会儿还没退出
    /// 就 SIGKILL。等的那段在单独的线程里，不阻塞调用方。已经退出的不再发信号，免得进程号被
    /// 别的进程重用后误杀。
    pub(super) fn terminate(self: Arc<Self>, foreground: Option<libc::pid_t>) {
        if self.has_exited() {
            return;
        }
        // shell 启动时 `setsid` 成了会话首进程，进程组号就是它的 pid。
        hangup_group(self.pid);
        if let Some(group) = foreground.filter(|&group| group != self.pid) {
            hangup_group(group);
        }
        let spawned = thread::Builder::new().name("pty-terminator".into()).spawn(move || {
            let until = Instant::now() + HANGUP_GRACE;
            while Instant::now() < until {
                if self.has_exited() {
                    return;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if !self.has_exited() {
                // SAFETY: 只是给进程发信号；上面刚确认它还没退出。
                unsafe { libc::kill(self.pid, libc::SIGKILL) };
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("failed to start the pty terminator thread: {err}");
        }
    }
}

fn hangup_group(group: libc::pid_t) {
    // SAFETY: 只是给进程组发信号。
    if unsafe { libc::killpg(group, libc::SIGHUP) } != 0 {
        tracing::debug!("SIGHUP to process group {group} failed: {}", std::io::Error::last_os_error());
    }
}

/// 进程已经不在了。
fn gone(pid: libc::pid_t) -> bool {
    // SAFETY: 信号 0 不发信号，只检查进程在不在。
    unsafe { libc::kill(pid, 0) != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) }
}

/// 描述符现在就可读。
fn readable(fd: BorrowedFd<'_>) -> bool {
    let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: 只传了一个指向本地变量的 pollfd，超时为 0，不阻塞。
    let ready = unsafe { libc::poll(&raw mut poll, 1, 0) };
    ready > 0 && poll.revents != 0
}

/// 给 `pid` 注册退出通知，返回 kqueue 和进程是不是已经不在了。
#[cfg(target_os = "macos")]
fn register(pid: libc::pid_t) -> (Option<OwnedFd>, bool) {
    // SAFETY: 没有参数，失败时返回 -1。
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        tracing::warn!("kqueue failed, polling for the shell's exit: {}", std::io::Error::last_os_error());
        return (None, gone(pid));
    }
    // SAFETY: `raw` 是刚建的 kqueue，没有别人持有。kqueue 不会被 fork 出的子进程继承。
    let queue = unsafe { OwnedFd::from_raw_fd(raw) };
    let Ok(ident) = usize::try_from(pid) else {
        return (None, true);
    };
    let change = libc::kevent {
        ident,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: 只注册一个事件，不收事件，所以不需要输出缓冲；超时参数在不收事件时不用。
    let result =
        unsafe { libc::kevent(queue.as_raw_fd(), &raw const change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if result < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            // 进程已经不在了（或者只剩僵尸）。
            return (None, true);
        }
        tracing::warn!("kevent(NOTE_EXIT) failed, polling for the shell's exit: {err}");
        return (None, gone(pid));
    }
    (Some(queue), false)
}

#[cfg(not(target_os = "macos"))]
fn register(pid: libc::pid_t) -> (Option<OwnedFd>, bool) {
    (None, gone(pid))
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    fn sleeper() -> std::process::Child {
        Command::new("/bin/sleep").arg("30").stdin(Stdio::null()).spawn().unwrap()
    }

    fn pid_of(child: &std::process::Child) -> libc::pid_t {
        libc::pid_t::try_from(child.id()).unwrap()
    }

    fn eventually(what: &str, check: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// 进程退出后就算还没被回收（僵尸），也认得出来。
    #[test]
    fn notices_the_exit_before_the_process_is_reaped() {
        let mut child = sleeper();
        let watch = ExitWatch::new(pid_of(&child));
        assert!(!watch.has_exited());
        child.kill().unwrap();
        if watch.fd().is_some() {
            eventually("the exit", || watch.has_exited());
        }
        child.wait().unwrap();
        eventually("the exit", || watch.has_exited());
    }

    #[test]
    fn a_process_that_is_already_gone_has_exited() {
        let mut child = sleeper();
        let pid = pid_of(&child);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(ExitWatch::new(pid).has_exited());
    }

    /// 结束一个进程组：SIGHUP 就够的话不再 SIGKILL。
    #[test]
    fn terminate_hangs_up_the_process_group() {
        let mut child = {
            use std::os::unix::process::CommandExt as _;
            let mut command = Command::new("/bin/sleep");
            command.arg("30").stdin(Stdio::null()).process_group(0);
            command.spawn().unwrap()
        };
        let watch = Arc::new(ExitWatch::new(pid_of(&child)));
        watch.terminate(None);
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(status.signal(), Some(libc::SIGHUP));
    }
}
