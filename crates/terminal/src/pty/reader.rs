//! PTY 的读线程：把输出交给 `PtySink`，读到 EOF 或出错时报告 `PtyEvent::Exited`。
//!
//! 读之前先 `poll`，同时等着 `Notifier`：交接时用它叫醒读线程、让它停下而不再从 PTY 里多读一个
//! 字节，接手来的会话里还用它知道 shell 退出了。叫停后读线程把 `PtySink` 交回来，可以接着读，
//! 见 `Reader::resume`。PTY 的描述符是非阻塞的，见 `Pty::open`。`Pty::adopt_paused` 接手来的，
//! 读线程开始读之前先等闸门（`Gate`）打开。

use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    thread::{self, JoinHandle},
};

use anyhow::{Context as _, Result};

use super::{
    Gate, PtyEvent, PtySink,
    notify::{EXIT_POLL_INTERVAL, Notifier},
    set_current_thread_interactive,
};

/// shell 已经退出、PTY 还没读到 EOF 时（比如后台程序还开着终端），最多再读这么多次，把 shell
/// 退出前写的输出读完，然后报告退出。
const DRAIN_READS: usize = 64;

/// 读线程的状态，和「看一眼状态、读一次」一起在锁里，见 `Reader::stop`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Running,
    /// 叫停了，读线程还没看到。
    StopRequested,
    /// 读线程看到了叫停，交回 `PtySink` 后结束了（或者正在结束）。
    Stopped,
}

pub(super) struct Reader {
    master: Arc<OwnedFd>,
    notifier: Arc<Notifier>,
    state: Arc<Mutex<State>>,
    /// 读线程；叫停后结束时交回 `PtySink`，读到 EOF 或者 `PtySink` 不要了时交回 `None`。
    thread: Option<JoinHandle<Option<PtySink>>>,
}

impl Reader {
    /// 起读线程，从 `master` 读，输出交给 `sink`。`notifier` 看着 shell 时，shell 退出也算会话
    /// 结束，不必等到 PTY 读到 EOF。给了 `gate` 时读线程先等它打开再读；放弃了就不读，像叫停了
    /// 一样交回 `PtySink`。
    pub(super) fn start(
        master: Arc<OwnedFd>,
        notifier: Arc<Notifier>,
        sink: PtySink,
        gate: Option<Arc<Gate>>,
    ) -> Result<Self> {
        let mut reader = Self { master, notifier, state: Arc::new(Mutex::new(State::Running)), thread: None };
        reader.spawn(sink, gate)?;
        Ok(reader)
    }

    fn spawn(&mut self, sink: PtySink, gate: Option<Arc<Gate>>) -> Result<()> {
        let (master, notifier, state) = (self.master.clone(), self.notifier.clone(), self.state.clone());
        let thread = thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                if let Some(gate) = gate
                    && !gate.wait()
                {
                    *state.lock().unwrap_or_else(PoisonError::into_inner) = State::Stopped;
                    return Some(sink);
                }
                read_loop(&master, &notifier, &state, sink)
            })
            .context("failed to start pty reader thread")?;
        self.thread = Some(thread);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 让读线程停下。返回之后它不会再从 PTY 读；已经读出来、正在交给 `PtySink` 的那块照样交出去，
    /// 交完线程就结束，不报告 `PtyEvent::Exited`。不等线程结束，见 `finished`、`join`。
    pub(super) fn stop(&mut self) {
        let mut state = self.lock();
        if *state == State::Running {
            *state = State::StopRequested;
        }
        drop(state);
        // 线程已经结束时没有要叫醒的。
        if !self.finished() {
            self.notifier.wake();
        }
    }

    /// 接着读：叫停后线程还没看到时撤回叫停，已经停下时用交回的 `PtySink` 重新起读线程。读到
    /// EOF 已经结束的不再起。
    pub(super) fn resume(&mut self) -> Result<()> {
        let mut state = self.lock();
        match *state {
            State::Running => Ok(()),
            State::StopRequested => {
                *state = State::Running;
                Ok(())
            }
            State::Stopped => {
                drop(state);
                if let Some(sink) = self.join() {
                    *self.lock() = State::Running;
                    self.spawn(sink, None)?;
                }
                Ok(())
            }
        }
    }

    /// 读线程已经结束：读到了 EOF，`PtySink` 不要了，或者 `stop` 之后交完了手里的输出。
    pub(super) fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// 等读线程结束，返回它交回的 `PtySink`。它正卡在 `PtySink` 里时会一直等下去，见 `Pty::release`。
    pub(super) fn join(&mut self) -> Option<PtySink> {
        let thread = self.thread.take()?;
        thread.join().unwrap_or_else(|_| {
            tracing::warn!("the pty reader thread panicked");
            None
        })
    }
}

fn poll_entry(fd: libc::c_int) -> libc::pollfd {
    libc::pollfd { fd, events: libc::POLLIN, revents: 0 }
}

/// 读一次。`Ok(None)` 是 EOF，`Err` 是读不下去了；被打断或者暂时没有数据时为 `Ok(Some(0))`。
fn read_once(master: &OwnedFd, buf: &mut [u8]) -> io::Result<Option<usize>> {
    // SAFETY: `master` 开着，`buf` 是可写的缓冲，长度如实传入。
    let n = unsafe { libc::read(master.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    match usize::try_from(n) {
        Ok(0) => Ok(None),
        Ok(n) => Ok(Some(n)),
        Err(_) => {
            let err = io::Error::last_os_error();
            if matches!(err.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) {
                Ok(Some(0))
            } else {
                Err(err)
            }
        }
    }
}

fn read_loop(master: &OwnedFd, notifier: &Notifier, state: &Mutex<State>, mut sink: PtySink) -> Option<PtySink> {
    set_current_thread_interactive();
    let mut buf = vec![0u8; 64 * 1024];
    let mut fds = [poll_entry(master.as_raw_fd()), poll_entry(notifier.fd().as_raw_fd())];
    // 拿不到退出通知时定时问一次。
    let exit_poll = notifier.polls_exit();
    // shell 退出后还能再读几次；为 `None` 时 shell 还在，或者不看 shell 退不退出。
    let mut draining: Option<usize> = None;
    loop {
        let timeout = match draining {
            Some(_) => 0,
            None if exit_poll => libc::c_int::try_from(EXIT_POLL_INTERVAL.as_millis()).unwrap_or(libc::c_int::MAX),
            None => -1,
        };
        for entry in &mut fds {
            entry.revents = 0;
        }
        // SAFETY: `fds` 是本地数组，长度如实传入；里面的描述符在这个函数里一直开着。
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!("pty poll failed: {err}");
            break;
        }
        if fds[1].revents != 0 {
            // 收走唤醒和退出通知；叫停看下面的状态。
            notifier.collect();
        }
        let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
        if *guard == State::StopRequested {
            *guard = State::Stopped;
            return Some(sink);
        }
        if draining.is_none() && (fds[1].revents != 0 || exit_poll) && notifier.has_exited() {
            draining = Some(0);
        }
        if fds[0].revents == 0 {
            if draining.is_some() {
                // shell 退出了，PTY 里也没有剩下的输出。
                break;
            }
            continue;
        }
        if let Some(reads) = &mut draining {
            if *reads >= DRAIN_READS {
                break;
            }
            *reads += 1;
        }
        // 描述符是非阻塞的，拿着锁读不耽误 `stop`。
        match read_once(master, &mut buf) {
            Ok(None) => break,
            Ok(Some(0)) => {}
            Ok(Some(n)) => {
                drop(guard);
                if !sink(PtyEvent::Output(buf[..n].into())) {
                    return None;
                }
            }
            Err(err) => {
                // shell 退出、从设备都关了以后读 master 得到 EIO，和 EOF 一样。
                tracing::debug!("pty read ended: {err}");
                break;
            }
        }
    }
    sink(PtyEvent::Exited);
    None
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        sync::mpsc,
        time::Duration,
    };

    use super::*;

    fn output(rx: &mpsc::Receiver<PtyEvent>) -> Option<Vec<u8>> {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(PtyEvent::Output(data)) => Some(data.to_vec()),
            Ok(PtyEvent::Exited) | Err(_) => None,
        }
    }

    fn pipe() -> (io::PipeReader, io::PipeWriter) {
        let (rx, tx) = io::pipe().unwrap();
        super::super::set_nonblocking(std::os::fd::AsFd::as_fd(&rx)).unwrap();
        (rx, tx)
    }

    fn start(rx: OwnedFd, notifier: Option<libc::pid_t>) -> (Reader, mpsc::Receiver<PtyEvent>) {
        let (tx, events) = mpsc::channel();
        let notifier = Arc::new(Notifier::new(notifier).unwrap());
        let reader =
            Reader::start(Arc::new(rx), notifier, Box::new(move |event| tx.send(event).is_ok()), None).unwrap();
        (reader, events)
    }

    /// 叫停之后读线程不再从描述符里读：后来写进去的字节还原样留着，可以交给别人读。
    #[test]
    fn stop_leaves_unread_bytes_in_place() {
        let (pipe_rx, mut pipe_tx) = pipe();
        let (mut reader, rx) = start(pipe_rx.try_clone().unwrap().into(), None);
        pipe_tx.write_all(b"before").unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"before"[..]));
        reader.stop();
        assert!(reader.join().is_some(), "a stopped reader hands its sink back");
        assert!(reader.finished());
        pipe_tx.write_all(b"after").unwrap();
        drop(pipe_tx);
        let mut rest = Vec::new();
        let mut pipe_rx = pipe_rx;
        // 非阻塞的读端：写端关了，读到 EOF 为止。
        pipe_rx.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"after");
        // 叫停的读线程不报告退出。
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    /// 叫停后接着读：同一个 `PtySink` 收到后来的输出。线程停下前后撤回都行。
    #[test]
    fn a_stopped_reader_resumes_with_the_same_sink() {
        let (pipe_rx, mut pipe_tx) = pipe();
        let (mut reader, rx) = start(pipe_rx.into(), None);
        reader.stop();
        reader.resume().unwrap();
        pipe_tx.write_all(b"one").unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"one"[..]));
        reader.stop();
        while !reader.finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        pipe_tx.write_all(b"two").unwrap();
        reader.resume().unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"two"[..]));
        drop(pipe_tx);
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(PtyEvent::Exited)));
    }

    /// 丢掉 `Reader` 不叫停读线程，它照常读到 EOF 并报告退出。
    #[test]
    fn dropping_the_reader_keeps_reading_until_eof() {
        let (pipe_rx, mut pipe_tx) = pipe();
        let (reader, rx) = start(pipe_rx.into(), None);
        drop(reader);
        pipe_tx.write_all(b"still").unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"still"[..]));
        drop(pipe_tx);
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(PtyEvent::Exited)));
    }

    /// 看着的进程退出时，描述符那头还开着、读不到 EOF，也读完已有的输出后报告退出。
    #[test]
    fn the_watched_process_exiting_ends_reading() {
        let mut child = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let (pipe_rx, mut pipe_tx) = pipe();
        let (_reader, rx) = start(pipe_rx.into(), Some(libc::pid_t::try_from(child.id()).unwrap()));
        pipe_tx.write_all(b"last words").unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"last words"[..]));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(PtyEvent::Exited)));
        // 写端一直开着。
        drop(pipe_tx);
    }
}
