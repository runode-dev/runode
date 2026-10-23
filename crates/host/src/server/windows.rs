//! 宿主的 socket 在 Windows 上的写法：叫醒接受连接的线程用一对 AF_UNIX socket、等它们用
//! `WSAPoll`；认对端是不是同一个用户时取它的进程号，比两边的用户 SID。

use std::{ffi::c_void, io, os::windows::io::AsRawSocket as _, ptr};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Networking::WinSock::{
        POLLRDNORM, SIO_AF_UNIX_GETPEERPID, SOCKET, SOCKET_ERROR, SOL_SOCKET, WSAIoctl, WSAPOLLFD, WSAPoll, setsockopt,
    },
    Security::{GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
    System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
};

use super::{UnixListener, UnixStream};

pub(super) type WakeReader = UnixStream;
pub(super) type WakeWriter = UnixStream;

pub(super) fn wake_pair() -> io::Result<(WakeReader, WakeWriter)> {
    UnixStream::pair()
}

/// 等唤醒的 socket 可读，`accepting` 时也等监听的 socket 来连接；返回两者各自就绪没有。
pub(super) fn wait(wake: &WakeReader, listener: &UnixListener, accepting: bool) -> io::Result<(bool, bool)> {
    let entry = |socket: u64| WSAPOLLFD { fd: socket as SOCKET, events: POLLRDNORM, revents: 0 };
    let mut fds = [entry(wake.as_raw_socket()), entry(listener.as_raw_socket())];
    let count = if accepting { 2 } else { 1 };
    // SAFETY: `fds` 是本地数组，`count` 不超过它的长度；两个 socket 在这次调用期间都开着。
    if unsafe { WSAPoll(fds.as_mut_ptr(), count, -1) } == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0].revents != 0, accepting && fds[1].revents != 0))
}

/// 把 socket 的一个缓冲（`SO_SNDBUF` 或 `SO_RCVBUF`）设成 `size`；设不了时记日志，照常用。
pub(super) fn set_buffer(stream: &UnixStream, option: i32, size: i32) {
    // SAFETY: socket 来自 `stream`；值指向本地变量，长度是它的大小。
    let result = unsafe {
        setsockopt(
            stream.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            option,
            (&raw const size).cast(),
            size_of::<i32>() as i32,
        )
    };
    if result != 0 {
        tracing::debug!("failed to set a socket buffer: {}", io::Error::last_os_error());
    }
}

/// 连上来的进程是不是和自己同一个用户。对端的进程号取不到、或者它的令牌读不了（别的用户的
/// 进程多半如此）时算不是。
pub fn same_user(stream: &UnixStream) -> bool {
    let Some(pid) = peer_pid(stream) else {
        return false;
    };
    // SAFETY: 只是按进程号打开句柄，失败时返回空。
    let peer = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if peer.is_null() {
        return false;
    }
    let theirs = user_sid(peer);
    // SAFETY: 句柄是上面 `OpenProcess` 给的，只关这一次。
    unsafe { CloseHandle(peer) };
    // SAFETY: 当前进程的伪句柄，不用关。
    let ours = user_sid(unsafe { GetCurrentProcess() });
    theirs.is_some() && theirs == ours
}

/// 连接对端进程的进程号（`SIO_AF_UNIX_GETPEERPID`）。
pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut pid = 0u32;
    let mut returned = 0u32;
    // SAFETY: 输出指向本地变量，长度如实传入；不用重叠 I/O。
    let result = unsafe {
        WSAIoctl(
            stream.as_raw_socket() as SOCKET,
            SIO_AF_UNIX_GETPEERPID,
            ptr::null(),
            0,
            (&raw mut pid).cast::<c_void>(),
            size_of::<u32>() as u32,
            &mut returned,
            ptr::null_mut(),
            None,
        )
    };
    (result == 0 && pid != 0).then_some(pid)
}

/// 进程的用户 SID，按字节。
fn user_sid(process: HANDLE) -> Option<Vec<u8>> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: 输出指向本地变量。
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let mut len = 0u32;
    // SAFETY: 先问要多大的缓冲，不写输出。
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len) };
    // 按指针对齐，`TOKEN_USER` 里有指针。
    let mut buf = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
    // SAFETY: 缓冲至少 `len` 字节。
    let ok = unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } != 0;
    let sid = ok.then(|| {
        // SAFETY: 成功时缓冲开头是 `TOKEN_USER`，`Sid` 指向缓冲里一个合法的 SID。
        unsafe {
            let sid = (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            std::slice::from_raw_parts(sid.cast::<u8>(), GetLengthSid(sid) as usize).to_vec()
        }
    });
    // SAFETY: 令牌句柄只关这一次。
    unsafe { CloseHandle(token) };
    sid
}
