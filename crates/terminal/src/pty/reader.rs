//! PTY 的读线程：把输出交给 `PtySink`，读到 EOF 或出错时报告 `PtyEvent::Exited`。
//!
//! 读之前先 `poll`，同时等着一根唤醒用的管道和（接手来的会话里）shell 的退出通知，这样交接时
//! 能让它停下而不再从 PTY 里多读一个字节，见 `Reader::stop`。PTY 的描述符始终是阻塞的：
//! `O_NONBLOCK` 记在打开的文件上，和写线程、和交接对面的进程共用，不能改。

use std::{
    fs::File,
    io::{self, PipeWriter, Read, Write as _},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    sync::{Arc, Mutex, PoisonError},
    thread::{self, JoinHandle},
};

use anyhow::{Context as _, Result};

use super::{
    PtyEvent, PtySink,
    exit_watch::{EXIT_POLL_INTERVAL, ExitWatch},
    set_current_thread_interactive,
};

/// shell 已经退出、PTY 还没读到 EOF 时（比如后台程序还开着终端），最多再读这么多次，把 shell
/// 退出前写的输出读完，然后报告退出。
const DRAIN_READS: usize = 64;

pub(super) struct Reader {
    /// 为 true 时读线程不再读。读线程在「看一眼这个标记、读一次」期间一直拿着锁，所以
    /// `stop` 拿到锁、置上标记之后，读线程不会再读。
    stopped: Arc<Mutex<bool>>,
    /// 往里写一个字节，叫醒等在 `poll` 里的读线程。丢掉它不算叫停，见 `read_loop`。
    wake: PipeWriter,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// 起读线程，从 `master` 读，输出交给 `sink`。`exit` 给出时，shell 退出也算会话结束，
    /// 不必等到 PTY 读到 EOF。
    pub(super) fn start(master: OwnedFd, exit: Option<Arc<ExitWatch>>, sink: PtySink) -> Result<Self> {
        let (wake_rx, wake) = io::pipe().context("failed to create the pty reader's wake pipe")?;
        let stopped = Arc::new(Mutex::new(false));
        let thread = thread::Builder::new()
            .name("pty-reader".into())
            .spawn({
                let stopped = stopped.clone();
                move || read_loop(File::from(master), &OwnedFd::from(wake_rx), &stopped, exit.as_deref(), sink)
            })
            .context("failed to start pty reader thread")?;
        Ok(Self { stopped, wake, thread: Some(thread) })
    }

    /// 让读线程停下。返回之后它不会再从 PTY 读；已经读出来、正在交给 `PtySink` 的那块照样交出去，
    /// 交完线程就结束，不报告 `PtyEvent::Exited`。不等线程结束，见 `finished`、`join`。
    pub(super) fn stop(&mut self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        if let Err(err) = self.wake.write_all(&[0]) {
            tracing::debug!("failed to wake the pty reader: {err}");
        }
    }

    /// 读线程已经结束：读到了 EOF，`PtySink` 不要了，或者 `stop` 之后交完了手里的输出。
    pub(super) fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// 等读线程结束。它正卡在 `PtySink` 里时会一直等下去，见 `Pty::release`。
    pub(super) fn join(&mut self) {
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("the pty reader thread panicked");
        }
    }
}

fn poll_entry(fd: RawFd) -> libc::pollfd {
    libc::pollfd { fd, events: libc::POLLIN, revents: 0 }
}

fn read_loop(mut master: File, wake: &OwnedFd, stopped: &Mutex<bool>, exit: Option<&ExitWatch>, mut sink: PtySink) {
    set_current_thread_interactive();
    let mut buf = vec![0u8; 64 * 1024];
    // `poll` 忽略描述符为负的项。
    let mut fds = [
        poll_entry(master.as_raw_fd()),
        poll_entry(wake.as_raw_fd()),
        poll_entry(exit.and_then(ExitWatch::fd).map_or(-1, |fd| fd.as_raw_fd())),
    ];
    // 没有可等的退出通知时定时问一次。
    let exit_poll = exit.is_some_and(|exit| exit.fd().is_none());
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
        let guard = stopped.lock().unwrap_or_else(PoisonError::into_inner);
        if *guard {
            return;
        }
        if fds[1].revents != 0 {
            // 没叫停却有动静：`Reader` 被丢掉、管道的写端关了。这时还照常读到 EOF，只是不再等这根管道。
            fds[1].fd = -1;
        }
        if draining.is_none()
            && let Some(exit) = exit
            && (fds[2].revents != 0 || exit_poll)
            && exit.has_exited()
        {
            draining = Some(0);
            fds[2].fd = -1;
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
        // `poll` 说可读，又只有这一个线程在读，所以这里不会阻塞，拿着锁读不耽误 `stop`。
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                drop(guard);
                if !sink(PtyEvent::Output(buf[..n].into())) {
                    return;
                }
            }
            Err(err) if matches!(err.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => {}
            Err(err) => {
                // shell 退出、从设备都关了以后读 master 得到 EIO，和 EOF 一样。
                tracing::debug!("pty read ended: {err}");
                break;
            }
        }
    }
    sink(PtyEvent::Exited);
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    fn output(rx: &mpsc::Receiver<PtyEvent>) -> Option<Vec<u8>> {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(PtyEvent::Output(data)) => Some(data.to_vec()),
            Ok(PtyEvent::Exited) | Err(_) => None,
        }
    }

    /// 叫停之后读线程不再从描述符里读：后来写进去的字节还原样留着，可以交给别人读。
    #[test]
    fn stop_leaves_unread_bytes_in_place() {
        let (pipe_rx, mut pipe_tx) = io::pipe().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut reader =
            Reader::start(pipe_rx.try_clone().unwrap().into(), None, Box::new(move |event| tx.send(event).is_ok()))
                .unwrap();
        pipe_tx.write_all(b"before").unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"before"[..]));
        reader.stop();
        reader.join();
        assert!(reader.finished());
        pipe_tx.write_all(b"after").unwrap();
        drop(pipe_tx);
        let mut rest = Vec::new();
        let mut pipe_rx = pipe_rx;
        pipe_rx.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"after");
        // 叫停的读线程不报告退出。
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    /// 丢掉 `Reader` 不叫停读线程，它照常读到 EOF 并报告退出。
    #[test]
    fn dropping_the_reader_keeps_reading_until_eof() {
        let (pipe_rx, mut pipe_tx) = io::pipe().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = Reader::start(pipe_rx.into(), None, Box::new(move |event| tx.send(event).is_ok())).unwrap();
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
        let exit = Arc::new(ExitWatch::new(libc::pid_t::try_from(child.id()).unwrap()));
        let (pipe_rx, mut pipe_tx) = io::pipe().unwrap();
        let (tx, rx) = mpsc::channel();
        let _reader = Reader::start(pipe_rx.into(), Some(exit), Box::new(move |event| tx.send(event).is_ok())).unwrap();
        pipe_tx.write_all(b"last words").unwrap();
        assert_eq!(output(&rx).as_deref(), Some(&b"last words"[..]));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(PtyEvent::Exited)));
        // 写端一直开着。
        drop(pipe_tx);
    }
}
