//! 门禁通过后，在手机的 TLS 连接和到宿主的 Unix 连接之间两个方向搬字节，直到两边都说完。
//!
//! rustls 的连接不能两个线程同时用（读的那边一直等着就把写的那边卡住了），所以一条连接一个线程：
//! 两个 socket 都设成非阻塞，`poll` 等着哪边能读能写，手机发来的明文写给宿主，宿主发来的交给
//! rustls 加密后写给手机。两个方向各自积压到上限就先不读那一头，慢的一方会把快的一方压住，不会
//! 无限攒在内存里。
//!
//! 手机发来的控制帧要过一道筛：管宿主本身的请求（让宿主退出、升级交接这些，见 `frames::refusal`）
//! 不转给宿主，改在发往手机的方向、两帧之间插一条 `HostMsg::Error`，连接照旧。登记推送的请求也不转给
//! 宿主，由 `push` 按这台设备办了，同样在两帧之间插回话。
//!
//! 收尾：宿主那边说完了（读到结尾，或者写给它时它已经关了），先把它最后发的、还在内核里没读的都读
//! 出来送到手机，再发 close_notify；手机说了 close_notify，把它最后发的写给宿主后关掉写宿主的一边，
//! 等宿主自己关。手机的 TCP 断了时没什么可送的，直接结束。

mod frames;
pub(crate) mod push;

use std::{
    io::{self, Read as _, Write as _},
    net::{Shutdown, TcpStream},
    os::{fd::AsRawFd as _, unix::net::UnixStream},
    sync::atomic::{AtomicBool, Ordering},
};

use rustls::ServerConnection;

use self::frames::{FromPhone, LocalRequests, ToPhone};

/// 手机发来、还没写给宿主的明文最多攒这么多。
const TO_HOST_LIMIT: usize = 1 << 20;
/// rustls 里还没写给手机的密文最多攒这么多（`ServerConnection::set_buffer_limit`）。
const TO_PHONE_LIMIT: usize = 1 << 20;
/// 一次从 socket 读这么多。
const CHUNK: usize = 64 << 10;
/// `poll` 最多等这么久就回来看一眼 `cut`，见 `bridge`。
const POLL_TICK_MS: libc::c_int = 1000;

/// 搬到两边都说完，或者 `cut` 被置上（撤销了设备、关了远程访问；置上的一方同时会 `shutdown` 手机的
/// 连接，这边马上醒）。手机发来的请求里由这边办的交给 `local`。返回时两个连接都还开着，由调用方关。
pub(crate) fn bridge(
    conn: &mut ServerConnection,
    phone: &TcpStream,
    host: &UnixStream,
    cut: &AtomicBool,
    local: &mut dyn LocalRequests,
) {
    if let Err(err) = phone.set_nonblocking(true).and_then(|()| host.set_nonblocking(true)) {
        tracing::warn!("cannot bridge a remote connection: {err}");
        return;
    }
    conn.set_buffer_limit(Some(TO_PHONE_LIMIT));
    let mut bridge = Bridge {
        buf: vec![0u8; CHUNK],
        from_phone: FromPhone::default(),
        to_host: Vec::new(),
        host_writable: true,
        phone_done: false,
        from_host: Vec::new(),
        from_host_at: 0,
        to_phone: ToPhone::default(),
        replies: Vec::new(),
        replies_at: 0,
        host_done: false,
        said_bye: false,
    };
    if let Err(err) = bridge.run(conn, phone, host, cut, local) {
        tracing::debug!("remote connection bridge ended: {err}");
    }
}

/// 一条连接上搬字节的状态，见模块文档。
struct Bridge {
    buf: Vec<u8>,
    /// 手机发来的明文按帧过筛。
    from_phone: FromPhone,
    /// 过了筛、还没写给宿主的明文。
    to_host: Vec<u8>,
    /// 宿主还收：写给它出过错（它已经关了）以后，手机再发来的都扔掉。
    host_writable: bool,
    /// 手机说了 close_notify。
    phone_done: bool,
    /// 宿主发来、rustls 还没收下的明文，从 `from_host_at` 起。
    from_host: Vec<u8>,
    from_host_at: usize,
    /// 交给 rustls 的宿主的字节走到哪一帧了，`replies` 只在两帧之间插。
    to_phone: ToPhone,
    /// 挡下或者自己办了手机的请求后要回给它的帧，从 `replies_at` 起；`replies_at` 大于 0 时正插到一半。
    replies: Vec<u8>,
    replies_at: usize,
    /// 宿主说完了：读到了结尾或者读出错。
    host_done: bool,
    /// 发过 close_notify 了。
    said_bye: bool,
}

impl Bridge {
    fn run(
        &mut self,
        conn: &mut ServerConnection,
        phone: &TcpStream,
        host: &UnixStream,
        cut: &AtomicBool,
        local: &mut dyn LocalRequests,
    ) -> io::Result<()> {
        while !cut.load(Ordering::Relaxed) {
            self.phone_to_host(conn, host, local)?;
            self.host_to_phone(conn)?;
            while conn.wants_write() {
                match conn.write_tls(&mut &*phone) {
                    Ok(_) => {}
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(err) => return Err(err),
                }
            }
            if self.said_bye && !conn.wants_write() {
                return Ok(());
            }
            if !self.wait_and_read(conn, phone, host)? {
                return Ok(());
            }
        }
        Ok(())
    }

    /// 手机 → 宿主：rustls 里解好的明文拿出来过筛（门禁之后手机可能已经接着发了几帧），写给宿主。
    fn phone_to_host(
        &mut self,
        conn: &mut ServerConnection,
        host: &UnixStream,
        local: &mut dyn LocalRequests,
    ) -> io::Result<()> {
        while !self.phone_done && self.to_host.len() < TO_HOST_LIMIT {
            match conn.reader().read(&mut self.buf) {
                Ok(0) => self.phone_done = true,
                Ok(n) => {
                    self.from_phone
                        .feed(&self.buf[..n], &mut self.to_host, &mut self.replies, local)
                        .map_err(io::Error::other)?;
                    if !self.host_writable {
                        self.to_host.clear();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            }
        }
        while self.host_writable && !self.to_host.is_empty() {
            match (&*host).write(&self.to_host) {
                Ok(n) => drop(self.to_host.drain(..n)),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    // 宿主已经关了（EPIPE、ECONNRESET）：它最后发的可能还在内核里没读，照常读完送给
                    // 手机再收尾，手机再发来的扔掉。
                    tracing::debug!("the host stopped reading: {err}");
                    self.host_writable = false;
                    self.to_host.clear();
                }
            }
        }
        if self.phone_done && self.to_host.is_empty() && self.host_writable {
            // 手机说完了：让宿主读到结尾，它自己会关，关之前发的照常送到手机。
            self.host_writable = false;
            let _ = host.shutdown(Shutdown::Write);
        }
        Ok(())
    }

    /// 宿主 → 手机：宿主的字节和这边给手机的回话交给 rustls，回话只插在两帧之间；宿主说完、都交出去
    /// 了就发 close_notify。
    fn host_to_phone(&mut self, conn: &mut ServerConnection) -> io::Result<()> {
        loop {
            let injecting = self.replies_at > 0;
            if self.replies_at < self.replies.len() && (injecting || self.to_phone.at_boundary()) {
                let n = conn.writer().write(&self.replies[self.replies_at..])?;
                self.replies_at += n;
                if self.replies_at == self.replies.len() {
                    self.replies.clear();
                    self.replies_at = 0;
                }
                if n == 0 {
                    break;
                }
                continue;
            }
            if injecting || self.from_host_at == self.from_host.len() {
                break;
            }
            let pending = &self.from_host[self.from_host_at..];
            // 有回话等着插时只交到这一帧（或者帧头）的结尾，下一轮在两帧之间插。
            let len =
                if self.replies.is_empty() { pending.len() } else { self.to_phone.to_next_stop().min(pending.len()) };
            let n = conn.writer().write(&pending[..len])?;
            self.to_phone.advance(&pending[..n]);
            self.from_host_at += n;
            if n == 0 {
                break;
            }
        }
        if self.from_host_at == self.from_host.len() {
            self.from_host.clear();
            self.from_host_at = 0;
            if self.host_done && self.replies.is_empty() && !self.said_bye {
                conn.send_close_notify();
                self.said_bye = true;
            }
        }
        Ok(())
    }

    /// 等两边能读能写，读到的交给下一轮。手机的 TCP 断了时返回 false。
    fn wait_and_read(&mut self, conn: &mut ServerConnection, phone: &TcpStream, host: &UnixStream) -> io::Result<bool> {
        let read_phone = !self.phone_done && self.to_host.len() < TO_HOST_LIMIT;
        let read_host = !self.host_done && self.from_host.is_empty();
        let write_host = self.host_writable && !self.to_host.is_empty();
        let mut fds = [
            libc::pollfd {
                fd: phone.as_raw_fd(),
                events: (if read_phone { libc::POLLIN } else { 0 })
                    | (if conn.wants_write() { libc::POLLOUT } else { 0 }),
                revents: 0,
            },
            // 宿主那头既不读也不写时不等它：它挂断了的话 `POLLHUP` 会让 `poll` 一直返回、空转。
            libc::pollfd {
                fd: if read_host || write_host { host.as_raw_fd() } else { -1 },
                events: (if read_host { libc::POLLIN } else { 0 }) | (if write_host { libc::POLLOUT } else { 0 }),
                revents: 0,
            },
        ];
        // SAFETY: `fds` 是本地数组，长度如实给出；两个描述符在这次调用期间都开着。
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_TICK_MS) } < 0 {
            let err = io::Error::last_os_error();
            return if err.kind() == io::ErrorKind::Interrupted { Ok(true) } else { Err(err) };
        }
        let gone = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        let [phone_fd, host_fd] = fds;
        if read_phone && phone_fd.revents & (libc::POLLIN | gone) != 0 {
            match conn.read_tls(&mut &*phone) {
                Ok(0) => return Ok(false),
                Ok(_) => {
                    if let Err(err) = conn.process_new_packets() {
                        // 把 rustls 要发的告警送出去再断。
                        let _ = conn.write_tls(&mut &*phone);
                        return Err(io::Error::other(err));
                    }
                }
                Err(err) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(err) => return Err(err),
            }
        } else if phone_fd.revents & gone != 0 {
            return Ok(false);
        }
        if read_host && host_fd.revents & (libc::POLLIN | gone) != 0 {
            match (&*host).read(&mut self.buf) {
                Ok(0) => self.host_done = true,
                Ok(n) => self.from_host.extend_from_slice(&self.buf[..n]),
                Err(err) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(err) => {
                    // 连接被重置，内核里也没有可读的了：把已经读到的送完就收尾。
                    tracing::debug!("the host connection broke: {err}");
                    self.host_done = true;
                }
            }
        }
        Ok(true)
    }
}
