//! 在 Unix socket 上连同几个文件描述符一起收发一段字节（`sendmsg`/`recvmsg` 加 `SCM_RIGHTS`）。
//! 宿主升级时，旧宿主用它把各个会话的 PTY master（见 `pty::PtyHandoff`）和监听的 socket 交给新宿主。
//!
//! 一条消息在流上是：8 字节的头（字节数和描述符个数，各是小端的 u32），接着是那段字节。描述符
//! 跟着头的第一个字节走，收的一方只按头里的长度读，不会读进下一条消息，所以描述符不会串到
//! 别的消息上；收到的个数和头里写的对不上时报错。

use std::{
    io,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
};

/// 一条消息最多带这么多描述符。
pub const MAX_FDS: usize = 32;
/// 一条消息最多这么多字节，收的一方据此拒绝不像样的长度，不按它分配内存。
pub const MAX_MESSAGE_BYTES: usize = 256 << 20;

/// 收的时候控制消息的缓冲放得下这么多描述符：一次 `sendmsg` 能带的最多个数（macOS 上实测
/// 254，再多 `sendmsg` 报 `EINVAL`；Linux 是 `SCM_MAX_FD`，253）。缓冲比 `MAX_FDS` 大得多，
/// 是因为 macOS 截断时不会替我们关掉放不下的那些：它们照样装进本进程，只是不出现在控制消息里，
/// 收的一方无从关起，就漏了。缓冲开到一次能发的上限，就不会截断；对面不按 `MAX_FDS` 来时，
/// 收下的照样都接住，再按头里的个数报错、关掉。
const RECV_FDS: usize = 254;

const HEADER_LEN: usize = 8;
const FD_SIZE: usize = size_of::<RawFd>();

/// 发一条消息：`data` 连同 `fds`。描述符在对面收到的是新的一份，这边的照旧开着。`fds` 超过
/// `MAX_FDS` 个或者 `data` 超过 `MAX_MESSAGE_BYTES` 时返回 `InvalidInput`。被信号打断时重试，
/// 内核一次只收下一部分时接着发。
pub fn send_with_fds(stream: &UnixStream, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    if fds.len() > MAX_FDS {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("at most {MAX_FDS} descriptors per message")));
    }
    let len = u32::try_from(data.len())
        .ok()
        .filter(|_| data.len() <= MAX_MESSAGE_BYTES)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "message too long"))?;
    let count = u32::try_from(fds.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&len.to_le_bytes());
    header[4..].copy_from_slice(&count.to_le_bytes());
    let raw: Vec<RawFd> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    let total = HEADER_LEN + data.len();
    // 描述符只随第一次发出去；内核只收下一部分时，余下的字节接着发，不再带描述符。
    let mut sent = send_some(stream, [&header, data], 0, &raw)?;
    while sent < total {
        sent += send_some(stream, [&header, data], sent, &[])?;
    }
    Ok(())
}

/// 收一条消息：那段字节和随它来的描述符，描述符都设了 `FD_CLOEXEC`。对面关了连接时返回
/// `UnexpectedEof`；头不像样（超过 `MAX_FDS` 个描述符也算）、描述符被截断（`MSG_CTRUNC`，见
/// `RECV_FDS`）或者个数对不上时返回 `InvalidData`，已经收到的描述符都关掉。被信号打断时重试。
pub fn recv_with_fds(stream: &UnixStream) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut fds = Vec::new();
    let mut header = [0u8; HEADER_LEN];
    recv_exact(stream, &mut header, &mut fds)?;
    let [l0, l1, l2, l3, c0, c1, c2, c3] = header;
    let len = usize::try_from(u32::from_le_bytes([l0, l1, l2, l3])).unwrap_or(usize::MAX);
    let count = usize::try_from(u32::from_le_bytes([c0, c1, c2, c3])).unwrap_or(usize::MAX);
    if len > MAX_MESSAGE_BYTES || count > MAX_FDS {
        return Err(invalid(format!("bad message header: {len} bytes, {count} descriptors")));
    }
    let mut data = vec![0u8; len];
    recv_exact(stream, &mut data, &mut fds)?;
    if fds.len() != count {
        return Err(invalid(format!("expected {count} descriptors, got {}", fds.len())));
    }
    Ok((data, fds))
}

/// 把长度换成 C 结构里对应字段的类型：`cmsg_len`、`msg_controllen`、`msg_iovlen` 在各个系统上
/// 类型不同，有的就是 `usize`。放不下时为 0，`sendmsg`/`recvmsg` 会报错。
fn c_len<T: TryFrom<usize> + Default>(len: usize) -> T {
    T::try_from(len).unwrap_or_default()
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// 放 `fds` 个描述符的控制消息的缓冲，按 `cmsghdr` 对齐，以及控制消息实际占的字节数
/// （`CMSG_SPACE`）。缓冲按 8 字节取整，可能比控制消息长；发送时 `msg_controllen` 要填后者，
/// 多出来的零字节会被 macOS 当成又一个控制消息头而报 `EINVAL`。
fn control_buffer(fds: usize) -> (Vec<u64>, usize) {
    // SAFETY: `CMSG_SPACE` 只做算术。
    let space = unsafe { libc::CMSG_SPACE(u32::try_from(fds * FD_SIZE).unwrap_or(u32::MAX)) };
    let space = usize::try_from(space).unwrap_or(usize::MAX);
    (vec![0u64; space.div_ceil(size_of::<u64>())], space)
}

/// 发送时不要 SIGPIPE，对面关了就返回 `EPIPE`。macOS 没有这个标志；Rust 程序启动时已经
/// 忽略了 SIGPIPE。
#[cfg(target_os = "linux")]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
#[cfg(not(target_os = "linux"))]
const SEND_FLAGS: libc::c_int = 0;

/// 把 `parts` 跳过前 `skip` 个字节后的内容发一次，`fds` 不空时一起带上；返回这次发出的字节数。
fn send_some(stream: &UnixStream, parts: [&[u8]; 2], skip: usize, fds: &[RawFd]) -> io::Result<usize> {
    let mut iov = Vec::with_capacity(2);
    let mut skip = skip;
    for part in parts {
        let rest = part.get(skip..).unwrap_or_default();
        skip = skip.saturating_sub(part.len());
        if !rest.is_empty() {
            iov.push(libc::iovec { iov_base: rest.as_ptr().cast_mut().cast(), iov_len: rest.len() });
        }
    }
    let (mut control, control_len) = if fds.is_empty() { (Vec::new(), 0) } else { control_buffer(fds.len()) };
    // SAFETY: msghdr 是纯数据的 C 结构，全零是合法的初值。
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = c_len(iov.len());
    if !fds.is_empty() {
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = c_len(control_len);
        // SAFETY: `control` 按 `CMSG_SPACE` 分配、按 `cmsghdr` 对齐，放得下一个带 `fds.len()` 个描述符
        // 的控制消息；`CMSG_FIRSTHDR` 因此不为空，`CMSG_DATA` 指向的地方放得下这些描述符。
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const msg);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            let cmsg_len = libc::CMSG_LEN(u32::try_from(size_of_val(fds)).unwrap_or(u32::MAX));
            (*header).cmsg_len = c_len(usize::try_from(cmsg_len).unwrap_or_default());
            std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(header).cast::<RawFd>(), fds.len());
        }
    }
    loop {
        // SAFETY: `msg` 里的指针指向 `iov`、`control` 和调用方的切片，在这次调用期间都有效。
        let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &raw const msg, SEND_FLAGS) };
        match usize::try_from(sent) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(sent) => return Ok(sent),
            Err(_) => {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
        }
    }
}

/// 收描述符时就设上 `FD_CLOEXEC`；macOS 没有这个标志，收到后再设，见 `recv_some`。
#[cfg(target_os = "linux")]
const RECV_FLAGS: libc::c_int = libc::MSG_CMSG_CLOEXEC;
#[cfg(not(target_os = "linux"))]
const RECV_FLAGS: libc::c_int = 0;

/// 读满 `buf`，随之来的描述符放进 `fds`。
fn recv_exact(stream: &UnixStream, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = recv_some(stream, &mut buf[filled..], fds)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed in the middle of a message"));
        }
        filled += n;
    }
    Ok(())
}

/// 收一次，返回收到的字节数（0 是对面关了），随之来的描述符放进 `fds`。
// `msg_controllen`、`cmsg_len` 和 `CMSG_LEN` 的类型各平台不一样（macOS 上是 u32，Linux 上是 usize），
// 转换在 Linux 上是多余的。
#[cfg_attr(target_os = "linux", allow(clippy::useless_conversion))]
fn recv_some(stream: &UnixStream, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
    let (mut control, control_len) = control_buffer(RECV_FDS);
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // SAFETY: msghdr 是纯数据的 C 结构，全零是合法的初值。
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = c_len(control_len);
    let received = loop {
        // SAFETY: `msg` 里的指针指向 `iov`、`control` 和 `buf`，在这次调用期间都有效。
        let received = unsafe { libc::recvmsg(stream.as_raw_fd(), &raw mut msg, RECV_FLAGS) };
        match usize::try_from(received) {
            Ok(received) => break received,
            Err(_) => {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
        }
    };
    // 先把收到的描述符都接住，后面出错时它们随 `fds` 一起关掉。被截断时 `cmsg_len` 可能还是
    // 截断前的长度，只认落在 `msg_controllen` 之内的那些，免得把缓冲之外的字节当成描述符。
    // SAFETY: `msg` 是 recvmsg 刚填好的，控制消息在 `control` 里，`msg_controllen` 不超过它的
    // 长度；`CMSG_NXTHDR` 只在这个范围内走。读出来的描述符都是内核刚为本进程新开的，没有
    // 别人持有。
    unsafe {
        let control_end = msg
            .msg_control
            .cast::<u8>()
            .cast_const()
            .add(usize::try_from(msg.msg_controllen).unwrap_or_default().min(control_len));
        let mut header = libc::CMSG_FIRSTHDR(&raw const msg);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(header);
                let in_buffer = usize::try_from(control_end.offset_from(data)).unwrap_or_default();
                let data_len = usize::try_from((*header).cmsg_len)
                    .unwrap_or_default()
                    .saturating_sub(usize::try_from(libc::CMSG_LEN(0)).unwrap_or_default())
                    .min(in_buffer);
                for i in 0..data_len / FD_SIZE {
                    let fd = data.add(i * FD_SIZE).cast::<RawFd>().read_unaligned();
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            header = libc::CMSG_NXTHDR(&raw const msg, header);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(invalid("descriptors were truncated (MSG_CTRUNC)".into()));
    }
    for fd in fds.iter() {
        set_cloexec(fd)?;
    }
    Ok(received)
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: `fd` 开着；F_GETFD/F_SETFD 只读写描述符标志。
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd as _;

    use super::*;

    /// 对面不按 `MAX_FDS` 来、一次塞满能发的上限时，收的一方报错，收下的描述符一个不漏地关掉：
    /// 管道读端的副本全关了，写端才会 `EPIPE`。
    #[test]
    fn too_many_descriptors_from_the_peer_are_all_closed() {
        let (a, b) = UnixStream::pair().unwrap();
        let (pipe_rx, mut pipe_tx) = io::pipe().unwrap();
        let many: Vec<RawFd> = (0..RECV_FDS).map(|_| pipe_rx.as_fd().as_raw_fd()).collect();
        let mut header = [0u8; HEADER_LEN];
        header[4..].copy_from_slice(&u32::try_from(many.len()).unwrap().to_le_bytes());
        send_some(&a, [&header, &[]], 0, &many).unwrap();
        drop(pipe_rx);
        let err = recv_with_fds(&b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        drop((a, b));
        use std::io::Write as _;
        assert_eq!(pipe_tx.write(b"x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn a_bad_header_is_rejected() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        send_some(&a, [&header, &[]], 0, &[]).unwrap();
        assert_eq!(recv_with_fds(&b).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
