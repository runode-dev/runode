//! 叫醒读线程、看着接手来的 shell 什么时候退出，两件事共用一个描述符。
//!
//! macOS 上是一个 kqueue：`EVFILT_USER` 用来叫醒，`EVFILT_PROC`/`NOTE_EXIT` 等 shell 退出。
//! kqueue 的描述符本身可以 `poll`，读线程把它和 PTY 放在一起等，每个会话只多占这一个描述符。
//! 接手来的会话里 shell 不是本进程的子进程，不能 `waitpid`，也拿不到退出码；kqueue 在它退出
//! 那一刻就通知，还没被回收（僵尸）也认得出，所以结束 shell 时不会把信号发给重用了这个进程号
//! 的别的进程。
//!
//! 别的系统上叫醒用一根非阻塞的管道（两个描述符），shell 退没退出定时 `kill(pid, 0)`：进程没了
//! （`ESRCH`）才算退出，认不出还没被回收的僵尸；不过接手的一方不是父进程，僵尸由 init 很快回收。

#[cfg(target_os = "macos")]
use std::os::fd::FromRawFd;
use std::{
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// 拿不到退出通知时，读线程隔这么久查一次 shell 还在不在。
pub(super) const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 结束接手来的 shell 时，发 SIGHUP 后等它自己退出的时间，过了就 SIGKILL。
const HANGUP_GRACE: Duration = Duration::from_millis(200);
/// `EVFILT_USER` 事件的标识，一个 kqueue 里只有这一个。
#[cfg(target_os = "macos")]
const WAKE_IDENT: usize = 0;

pub(super) struct Notifier {
    /// macOS 上是 kqueue，别的系统上是唤醒管道的读端。
    fd: OwnedFd,
    /// 唤醒管道的写端；macOS 上用 kqueue 叫醒，没有它。
    #[cfg(not(target_os = "macos"))]
    wake_tx: OwnedFd,
    /// 看着的 shell；为 `None` 时只用来叫醒。
    pid: Option<libc::pid_t>,
    /// 拿不到退出通知，只能 `kill(pid, 0)` 轮询。
    polls_exit: bool,
    exited: AtomicBool,
}

impl Notifier {
    /// `pid` 给出时还看着这个进程什么时候退出。
    pub(super) fn new(pid: Option<libc::pid_t>) -> std::io::Result<Self> {
        let mut notifier = Self::open()?;
        if let Some(pid) = pid {
            notifier.pid = Some(pid);
            notifier.watch(pid);
        }
        Ok(notifier)
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// 看着的进程退出时读线程要按 `EXIT_POLL_INTERVAL` 自己来问 `has_exited`。
    pub(super) fn polls_exit(&self) -> bool {
        self.polls_exit
    }

    /// 看着的 shell 已经退出了；没在看任何进程时为 false。一旦为 true 就一直是 true。
    pub(super) fn has_exited(&self) -> bool {
        let Some(pid) = self.pid else {
            return false;
        };
        if self.exited.load(Ordering::Acquire) {
            return true;
        }
        if self.polls_exit {
            if gone(pid) {
                self.exited.store(true, Ordering::Release);
            }
        } else if self.collect() {
            // 收走的唤醒不是给这里的，补回去，免得等着它的读线程睡过头。
            self.wake();
        }
        self.exited.load(Ordering::Acquire)
    }

    /// 把等着的通知都收走：记下 shell 退出，返回其中有没有唤醒。读线程在 `fd` 可读后调用。
    pub(super) fn collect(&self) -> bool {
        self.collect_events()
    }

    /// 结束 shell：先给终端的前台进程组 `foreground` 发 SIGHUP（shell 已经退出、前台程序还在时
    /// 也发），再给 shell 的进程组发 SIGHUP，等一会儿还没退出就 SIGKILL。等的那段在单独的线程
    /// 里，不阻塞调用方。shell 已经退出的不再给它发信号，免得进程号被别的进程重用后误杀；
    /// `foreground` 由调用方当场从终端读出、确认过属于 shell 的会话，见 `Pty` 的 `Drop`。
    pub(super) fn terminate(self: Arc<Self>, foreground: Option<libc::pid_t>) {
        let Some(pid) = self.pid else {
            return;
        };
        if let Some(group) = foreground.filter(|&group| group != pid) {
            hangup_group(group);
        }
        if self.has_exited() {
            return;
        }
        // shell 启动时 `setsid` 成了会话首进程，进程组号就是它的 pid。
        hangup_group(pid);
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
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("failed to start the pty terminator thread: {err}");
        }
    }
}

#[cfg(target_os = "macos")]
impl Notifier {
    fn open() -> std::io::Result<Self> {
        // SAFETY: 没有参数，失败时返回 -1。
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` 是刚建的 kqueue，没有别人持有。kqueue 不会被 fork 出的子进程继承。
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let notifier = Self { fd, pid: None, polls_exit: false, exited: AtomicBool::new(false) };
        // 用 EV_CLEAR：取走一次就复位。
        notifier.change(WAKE_IDENT, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0)?;
        Ok(notifier)
    }

    fn watch(&mut self, pid: libc::pid_t) {
        let Ok(ident) = usize::try_from(pid) else {
            self.exited.store(true, Ordering::Release);
            return;
        };
        match self.change(ident, libc::EVFILT_PROC, libc::EV_ADD, libc::NOTE_EXIT) {
            Ok(()) => {}
            // 进程已经不在了（或者只剩僵尸）。
            Err(err) if err.raw_os_error() == Some(libc::ESRCH) => self.exited.store(true, Ordering::Release),
            Err(err) => {
                tracing::warn!("kevent(NOTE_EXIT) failed, polling for the shell's exit: {err}");
                self.polls_exit = true;
                if gone(pid) {
                    self.exited.store(true, Ordering::Release);
                }
            }
        }
    }

    /// 叫醒等在 `fd` 上的读线程。
    pub(super) fn wake(&self) {
        if let Err(err) = self.change(WAKE_IDENT, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER) {
            tracing::debug!("failed to wake the pty reader: {err}");
        }
    }

    fn change(&self, ident: usize, filter: i16, flags: u16, fflags: u32) -> std::io::Result<()> {
        let change = libc::kevent { ident, filter, flags, fflags, data: 0, udata: std::ptr::null_mut() };
        // SAFETY: 只提交一个变更，不收事件，所以不需要输出缓冲；超时参数在不收事件时不用。
        let result = unsafe {
            libc::kevent(self.fd.as_raw_fd(), &raw const change, 1, std::ptr::null_mut(), 0, std::ptr::null())
        };
        if result < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    }

    fn collect_events(&self) -> bool {
        let mut events =
            [libc::kevent { ident: 0, filter: 0, flags: 0, fflags: 0, data: 0, udata: std::ptr::null_mut() }; 4];
        let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let mut woken = false;
        loop {
            // SAFETY: 输出缓冲是本地数组，长度如实传入；超时为 0，不阻塞。
            let count = unsafe {
                libc::kevent(
                    self.fd.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    libc::c_int::try_from(events.len()).unwrap_or(1),
                    &raw const zero,
                )
            };
            let Ok(count) = usize::try_from(count) else {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return woken;
            };
            for event in &events[..count] {
                match event.filter {
                    libc::EVFILT_USER => woken = true,
                    libc::EVFILT_PROC => self.exited.store(true, Ordering::Release),
                    _ => {}
                }
            }
            if count < events.len() {
                return woken;
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Notifier {
    fn open() -> std::io::Result<Self> {
        let (rx, tx) = std::io::pipe()?;
        let (fd, wake_tx) = (OwnedFd::from(rx), OwnedFd::from(tx));
        super::set_nonblocking(fd.as_fd())?;
        super::set_nonblocking(wake_tx.as_fd())?;
        Ok(Self { fd, wake_tx, pid: None, polls_exit: false, exited: AtomicBool::new(false) })
    }

    fn watch(&mut self, pid: libc::pid_t) {
        self.polls_exit = true;
        if gone(pid) {
            self.exited.store(true, Ordering::Release);
        }
    }

    pub(super) fn wake(&self) {
        // 管道满了也没关系：里面已经有叫醒用的字节了。
        // SAFETY: 写端开着，只写一个字节的本地缓冲。
        unsafe { libc::write(self.wake_tx.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
    }

    fn collect_events(&self) -> bool {
        let mut woken = false;
        let mut buf = [0u8; 64];
        // SAFETY: 读端开着且非阻塞，缓冲是本地数组。
        while unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {
            woken = true;
        }
        woken
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

    fn readable(fd: BorrowedFd<'_>, timeout_ms: libc::c_int) -> bool {
        let mut poll = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: 只传了一个指向本地变量的 pollfd。
        unsafe { libc::poll(&raw mut poll, 1, timeout_ms) > 0 }
    }

    /// 进程退出后就算还没被回收（僵尸），也认得出来。
    #[test]
    fn notices_the_exit_before_the_process_is_reaped() {
        let mut child = sleeper();
        let notifier = Notifier::new(Some(pid_of(&child))).unwrap();
        assert!(!notifier.has_exited());
        child.kill().unwrap();
        if !notifier.polls_exit() {
            eventually("the exit", || notifier.has_exited());
        }
        child.wait().unwrap();
        eventually("the exit", || notifier.has_exited());
    }

    #[test]
    fn a_process_that_is_already_gone_has_exited() {
        let mut child = sleeper();
        let pid = pid_of(&child);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(Notifier::new(Some(pid)).unwrap().has_exited());
    }

    /// 叫醒让描述符可读，收走后复位；别处问 shell 退没退出时收走的唤醒会补回去。
    #[test]
    fn a_wake_survives_an_exit_check() {
        let mut child = sleeper();
        let notifier = Notifier::new(Some(pid_of(&child))).unwrap();
        assert!(!readable(notifier.fd(), 0));
        notifier.wake();
        assert!(readable(notifier.fd(), 1000));
        assert!(!notifier.has_exited());
        assert!(readable(notifier.fd(), 1000));
        assert!(notifier.collect());
        assert!(!readable(notifier.fd(), 0));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// 结束一个进程组：SIGHUP 就够的话不再 SIGKILL。
    #[test]
    fn terminate_hangs_up_the_process_group() {
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};

        let mut child = Command::new("/bin/sleep").arg("30").stdin(Stdio::null()).process_group(0).spawn().unwrap();
        let notifier = Arc::new(Notifier::new(Some(pid_of(&child))).unwrap());
        notifier.terminate(None);
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGHUP));
    }

    /// shell 已经退出、前台程序还在时，前台进程组照样收到 SIGHUP。
    #[test]
    fn terminate_hangs_up_the_foreground_after_the_shell_exited() {
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};

        let mut shell = sleeper();
        let mut foreground =
            Command::new("/bin/sleep").arg("30").stdin(Stdio::null()).process_group(0).spawn().unwrap();
        let notifier = Arc::new(Notifier::new(Some(pid_of(&shell))).unwrap());
        shell.kill().unwrap();
        shell.wait().unwrap();
        eventually("the shell's exit", || notifier.has_exited());
        notifier.terminate(Some(pid_of(&foreground)));
        assert_eq!(foreground.wait().unwrap().signal(), Some(libc::SIGHUP));
    }
}
