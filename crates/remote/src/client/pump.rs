//! 过了门禁的 TLS 连接交给调用方：开一对本机 socket，一头交出去，另一头由一个线程和 TLS 互相搬。
//!
//! rustls 的连接拆不开读写两半，两个线程分着用就得加锁，读的那边等着时会卡住写的那边。所以一条连接一个
//! 线程：TCP 和本机 socket 都设成非阻塞，`poll` 等着哪边能读能写，没有动静时睡在 `poll` 里，不轮询；读到
//! 就送、不攒批，远程桌面的画面不因此多等。两个方向各自积压到上限就先不读那一头，慢的一方会把快的一方
//! 压住。
//!
//! 收尾：调用方关了写的一边（或者整个关了），这边发 close_notify，接着把对面最后发的送给调用方，等对面关；
//! 对面关了（close_notify 或者 TCP 断了），把收到的送完就关掉本机 socket，调用方读到结尾；调用方已经不收了
//! （写给它出错），直接收尾。

use std::{
    io::{self, Read as _, Write as _},
    net::TcpStream,
    thread,
    time::Duration,
};

use rustls::ClientConnection;

use crate::{UnixStream, poll};

/// 对面发来、还没写给调用方的明文最多攒这么多。
const TO_LOCAL_LIMIT: usize = 1 << 20;
/// rustls 里还没写给对面的密文最多攒这么多（`ClientConnection::set_buffer_limit`）。
const TO_REMOTE_LIMIT: usize = 1 << 20;
/// 一次从 socket 读这么多。
const CHUNK: usize = 64 << 10;
/// `poll` 一次最多等这么久。醒来不为查什么，只是不把等待交给一个无限长的超时。
const POLL_TICK: Duration = Duration::from_secs(30);

/// 开一对本机 socket，交回调用方那一头；另一头和 `conn`、`tcp` 之间由后台线程搬字节。`tcp` 要关了 Nagle，
/// 不然小的帧（输入、控制消息）要攒一会儿才发。
pub(super) fn spawn(conn: ClientConnection, tcp: TcpStream) -> io::Result<UnixStream> {
    let (ours, theirs) = UnixStream::pair()?;
    tcp.set_nonblocking(true)?;
    ours.set_nonblocking(true)?;
    thread::Builder::new().name("remote-client".into()).spawn(move || {
        let mut pump = Pump {
            conn,
            buf: vec![0u8; CHUNK],
            to_local: Vec::new(),
            to_local_at: 0,
            to_remote: Vec::new(),
            to_remote_at: 0,
            tcp_eof: false,
            remote_done: false,
            local_done: false,
        };
        pump.conn.set_buffer_limit(Some(TO_REMOTE_LIMIT));
        if let Err(err) = pump.run(&tcp, &ours) {
            tracing::debug!("a remote client connection ended: {err}");
        }
        let _ = tcp.shutdown(std::net::Shutdown::Both);
    })?;
    Ok(theirs)
}

struct Pump {
    conn: ClientConnection,
    buf: Vec<u8>,
    /// 对面发来、还没写给调用方的明文，从 `to_local_at` 起。
    to_local: Vec<u8>,
    to_local_at: usize,
    /// 调用方发来、rustls 还没收下的明文，从 `to_remote_at` 起。
    to_remote: Vec<u8>,
    to_remote_at: usize,
    /// TCP 读到了结尾。
    tcp_eof: bool,
    /// 对面说完了：close_notify，或者 TCP 断了。
    remote_done: bool,
    /// 调用方说完了：读到了结尾，close_notify 已经交给 rustls。
    local_done: bool,
}

impl Pump {
    fn run(&mut self, tcp: &TcpStream, local: &UnixStream) -> io::Result<()> {
        loop {
            self.remote_to_local(local)?;
            if self.remote_done && self.to_local.is_empty() {
                self.say_bye(tcp);
                return Ok(());
            }
            loop {
                let left = self.to_remote.len() - self.to_remote_at;
                self.local_to_remote()?;
                let flushing = self.conn.wants_write();
                if !self.flush_tls(tcp)? {
                    return Ok(());
                }
                // rustls 的密文都交给 TCP 了、调用方的明文还没交完：接着交。`poll` 只等 TCP 能写和对面发来，
                // 这时两样都不会来，要睡到 `POLL_TICK` 才动。
                let moved = self.to_remote.len() - self.to_remote_at != left;
                if self.to_remote.is_empty() || self.conn.wants_write() || !(moved || flushing) {
                    break;
                }
            }
            if !self.wait_and_read(tcp, local)? {
                self.say_bye(tcp);
                return Ok(());
            }
        }
    }

    /// rustls 里解好的明文拿出来写给调用方。调用方已经不收时报错。
    fn remote_to_local(&mut self, local: &UnixStream) -> io::Result<()> {
        while !self.remote_done && self.to_local.len() < TO_LOCAL_LIMIT {
            match self.conn.reader().read(&mut self.buf) {
                Ok(0) => self.remote_done = true,
                Ok(n) => self.to_local.extend_from_slice(&self.buf[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                // TCP 断了、没有 close_notify：收到的照样送完。
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => self.remote_done = true,
                Err(err) => return Err(err),
            }
        }
        while self.to_local_at < self.to_local.len() {
            match (&*local).write(&self.to_local[self.to_local_at..]) {
                Ok(n) => self.to_local_at += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        if self.to_local_at == self.to_local.len() {
            self.to_local.clear();
            self.to_local_at = 0;
        }
        Ok(())
    }

    /// 调用方发来的明文交给 rustls 加密，交不下的留到下一轮。
    fn local_to_remote(&mut self) -> io::Result<()> {
        while self.to_remote_at < self.to_remote.len() {
            let n = self.conn.writer().write(&self.to_remote[self.to_remote_at..])?;
            if n == 0 {
                break;
            }
            self.to_remote_at += n;
        }
        if self.to_remote_at == self.to_remote.len() {
            self.to_remote.clear();
            self.to_remote_at = 0;
        }
        Ok(())
    }

    /// rustls 要发的密文写给 TCP，写不下了就停。对面已经断了时返回 false。
    fn flush_tls(&mut self, tcp: &TcpStream) -> io::Result<bool> {
        while self.conn.wants_write() {
            match self.conn.write_tls(&mut &*tcp) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) if matches!(err.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) => {
                    return Ok(false);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(true)
    }

    /// 等两边能读能写，读到的交给下一轮。调用方那头断了时返回 false。
    fn wait_and_read(&mut self, tcp: &TcpStream, local: &UnixStream) -> io::Result<bool> {
        // rustls 里还有没拿出来的明文时不读 TCP：它收的明文满了会报错。
        let read_tcp = !self.tcp_eof && self.to_local.len() < TO_LOCAL_LIMIT;
        let read_local = !self.local_done && self.to_remote.is_empty();
        let write_local = !self.to_local.is_empty();
        let write_tcp = self.conn.wants_write();
        // 哪头既不读也不写时不等它：它挂断了的话 `POLLHUP` 会让 `poll` 一直返回、空转。TCP 那头不读时
        // 是调用方还没收完，断了的事等收完了再读 TCP 时知道。
        let mut fds = [
            if read_tcp || write_tcp {
                poll::entry(tcp, (if read_tcp { poll::IN } else { 0 }) | (if write_tcp { poll::OUT } else { 0 }))
            } else {
                poll::skipped()
            },
            if read_local || write_local {
                poll::entry(local, (if read_local { poll::IN } else { 0 }) | (if write_local { poll::OUT } else { 0 }))
            } else {
                poll::skipped()
            },
        ];
        if let Err(err) = poll::wait(&mut fds, POLL_TICK) {
            return if err.kind() == io::ErrorKind::Interrupted { Ok(true) } else { Err(err) };
        }
        let gone = poll::HUP | poll::ERR | poll::NVAL;
        let [tcp_fd, local_fd] = fds;
        if read_tcp && tcp_fd.revents & (poll::IN | gone) != 0 {
            match self.conn.read_tls(&mut &*tcp) {
                // 读到结尾：rustls 记下了，里面没读完的明文照样送完，之后 `reader` 报结尾。
                Ok(0) => self.tcp_eof = true,
                Ok(_) => {
                    if let Err(err) = self.conn.process_new_packets() {
                        // 把 rustls 要发的告警送出去再断。
                        let _ = self.conn.write_tls(&mut &*tcp);
                        return Err(io::Error::other(err));
                    }
                }
                Err(err) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                // 连接被重置：rustls 不知道，这边记下对面说完了。
                Err(_) => (self.tcp_eof, self.remote_done) = (true, true),
            }
        }
        if read_local && local_fd.revents & (poll::IN | gone) != 0 {
            match (&*local).read(&mut self.buf) {
                Ok(0) => {
                    self.local_done = true;
                    self.conn.send_close_notify();
                }
                Ok(n) => self.to_remote.extend_from_slice(&self.buf[..n]),
                Err(err) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(_) => return Ok(false),
            }
        }
        Ok(true)
    }

    /// 告诉对面这边说完了，尽量送出去，不等。
    fn say_bye(&mut self, tcp: &TcpStream) {
        if !self.local_done {
            self.local_done = true;
            self.conn.send_close_notify();
        }
        let _ = self.flush_tls(tcp);
    }
}
