//! 等几个 socket 能读能写：Unix 上是 `poll`，Windows 上是 `WSAPoll`。`WSAPoll` 只认 socket，所以
//! 叫醒等着的线程用的（`waker`）在 Windows 上是回环上的一对 UDP socket，Unix 上是管道。

use std::{io, time::Duration};

#[cfg(unix)]
pub(crate) use libc::{POLLERR as ERR, POLLHUP as HUP, POLLIN as IN, POLLNVAL as NVAL, POLLOUT as OUT};
#[cfg(windows)]
pub(crate) use windows_sys::Win32::Networking::WinSock::{
    POLLERR as ERR, POLLHUP as HUP, POLLNVAL as NVAL, POLLRDNORM as IN, POLLWRNORM as OUT,
};

#[cfg(unix)]
pub(crate) type Entry = libc::pollfd;
#[cfg(windows)]
pub(crate) type Entry = windows_sys::Win32::Networking::WinSock::WSAPOLLFD;

#[cfg(unix)]
pub(crate) type WakeRx = io::PipeReader;
#[cfg(unix)]
pub(crate) type WakeTx = io::PipeWriter;
#[cfg(windows)]
pub(crate) type WakeRx = std::net::UdpSocket;
#[cfg(windows)]
pub(crate) type WakeTx = std::net::UdpSocket;

/// 等 `source` 的 `events`（`IN`、`OUT` 的组合）。
#[cfg(unix)]
pub(crate) fn entry(source: &impl std::os::fd::AsRawFd, events: i16) -> Entry {
    libc::pollfd { fd: source.as_raw_fd(), events, revents: 0 }
}

/// 等 `source` 的 `events`（`IN`、`OUT` 的组合）。
#[cfg(windows)]
pub(crate) fn entry(source: &impl std::os::windows::io::AsRawSocket, events: i16) -> Entry {
    Entry { fd: source.as_raw_socket() as usize, events, revents: 0 }
}

/// 不等的一项：`wait` 跳过它，`revents` 为 0。
#[cfg(unix)]
pub(crate) fn skipped() -> Entry {
    libc::pollfd { fd: -1, events: 0, revents: 0 }
}

/// 不等的一项：`wait` 跳过它，`revents` 为 0。
#[cfg(windows)]
pub(crate) fn skipped() -> Entry {
    Entry { fd: windows_sys::Win32::Networking::WinSock::INVALID_SOCKET, events: 0, revents: 0 }
}

/// 等到 `entries` 里有一项有动静，最多等 `timeout`。被信号打断时是 `Interrupted`。
#[cfg(unix)]
pub(crate) fn wait(entries: &mut [Entry], timeout: Duration) -> io::Result<()> {
    let timeout = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `entries` 是调用方的数组，长度如实给出；里面的描述符在这次调用期间都开着。
    if unsafe { libc::poll(entries.as_mut_ptr(), entries.len() as libc::nfds_t, timeout) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 等到 `entries` 里有一项有动静，最多等 `timeout`。`WSAPoll` 不跳过 `skipped` 的项（报
/// `POLLNVAL`，马上返回），所以先拿掉它们再等。
#[cfg(windows)]
pub(crate) fn wait(entries: &mut [Entry], timeout: Duration) -> io::Result<()> {
    use windows_sys::Win32::Networking::WinSock::{INVALID_SOCKET, WSAGetLastError, WSAPoll};
    let timeout = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut live: Vec<Entry> = entries.iter().copied().filter(|entry| entry.fd != INVALID_SOCKET).collect();
    // SAFETY: `live` 是本地数组，长度如实给出；里面的 socket 在这次调用期间都开着。
    if unsafe { WSAPoll(live.as_mut_ptr(), live.len() as u32, timeout) } < 0 {
        // SAFETY: 只读本线程上一次 socket 调用的错误码。
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    let mut live = live.into_iter();
    for entry in entries.iter_mut().filter(|entry| entry.fd != INVALID_SOCKET) {
        entry.revents = live.next().map_or(0, |polled| polled.revents);
    }
    Ok(())
}

/// 一对唤醒用的端口：往 `WakeTx` 写（`wake`），在 `WakeRx` 上等着的 `wait` 就醒了。
#[cfg(unix)]
pub(crate) fn waker() -> io::Result<(WakeRx, WakeTx)> {
    io::pipe()
}

/// 一对唤醒用的端口：往 `WakeTx` 写（`wake`），在 `WakeRx` 上等着的 `wait` 就醒了。
#[cfg(windows)]
pub(crate) fn waker() -> io::Result<(WakeRx, WakeTx)> {
    use std::net::{Ipv4Addr, UdpSocket};
    let rx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let tx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    tx.connect(rx.local_addr()?)?;
    // 只收 `tx` 发来的：回环上别的进程往这个端口发一个包，不能叫停监听。
    rx.connect(tx.local_addr()?)?;
    Ok((rx, tx))
}

pub(crate) fn wake(tx: &WakeTx) {
    #[cfg(unix)]
    let _ = io::Write::write_all(&mut &*tx, &[1]);
    #[cfg(windows)]
    let _ = tx.send(&[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakes_a_waiting_thread() {
        let (rx, tx) = waker().unwrap();
        let mut entries = [entry(&rx, IN), skipped()];
        wait(&mut entries, Duration::ZERO).unwrap();
        assert_eq!(entries[0].revents, 0);
        wake(&tx);
        wait(&mut entries, Duration::from_secs(5)).unwrap();
        assert_ne!(entries[0].revents & IN, 0);
        assert_eq!(entries[1].revents, 0);
    }
}
