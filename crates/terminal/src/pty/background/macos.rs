//! macOS：子进程用 `proc_listchildpids` 逐个现问，监听经 `proc_pidfdinfo` 逐个套接字读状态。

use std::{mem::size_of, ptr};

use super::Pid;

/// `PROC_PIDFDSOCKETINFO` 返回的 `struct socket_fdinfo`（`sys/proc_info.h`）的大小，以及里面
/// `psi.soi_kind` 和 `psi.soi_proto.pri_tcp.tcpsi_state` 两个 `int` 的偏移。libc 没有这个结构体，
/// 只按偏移读用得上的两项；这三个数要和 SDK 头文件里的 `sizeof`、`offsetof` 一致。
const SOCKET_FDINFO_SIZE: usize = 792;
const SOI_KIND_OFFSET: usize = 256;
const TCPSI_STATE_OFFSET: usize = 344;
/// `proc_pidfdinfo` 读套接字的类型，libc 里没有。
const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
/// `soi_kind` 为 TCP 套接字。
const SOCKINFO_TCP: i32 = 2;
/// `tcpsi_state` 为在监听。
const TSI_S_LISTEN: i32 = 1;

/// macOS 上不预先读进程表，`children` 每次现问内核。
pub(super) struct Tree;

impl Tree {
    pub(super) fn read() -> Option<Self> {
        Some(Self)
    }

    /// `parent` 的直接子进程；读不到时为空。
    pub(super) fn children(&self, parent: Pid) -> Vec<Pid> {
        let mut pids: Vec<Pid> = vec![0; 16];
        // 子进程多得放不下时加倍再读，最多试几次。
        for _ in 0..6 {
            let Ok(capacity) = libc::c_int::try_from(size_of::<Pid>() * pids.len()) else {
                break;
            };
            // SAFETY: 缓冲区长 `capacity` 字节，内核最多写这么多；返回写了几个进程号。
            let count = unsafe { libc::proc_listchildpids(parent, pids.as_mut_ptr().cast(), capacity) };
            let Ok(count) = usize::try_from(count) else {
                break;
            };
            if count < pids.len() {
                pids.truncate(count);
                pids.retain(|&pid| pid > 0);
                return pids;
            }
            pids.resize(pids.len() * 2, 0);
        }
        Vec::new()
    }
}

/// macOS 上不预先读监听表，`any` 逐个进程现问内核。
pub(super) struct Listening;

impl Listening {
    pub(super) fn read() -> Option<Self> {
        Some(Self)
    }

    /// `pids` 里有没有进程在监听 TCP 端口。
    pub(super) fn any(&self, pids: &[Pid]) -> bool {
        pids.iter().any(|&pid| sockets(pid).into_iter().any(|fd| tcp_listening(pid, fd)))
    }
}

/// 进程 `pid` 打开的套接字的描述符；进程已经退出时为空。
fn sockets(pid: Pid) -> Vec<libc::c_int> {
    // SAFETY: 缓冲区为空，只问要多大。
    let needed = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, ptr::null_mut(), 0) };
    let Ok(needed) = usize::try_from(needed) else {
        return Vec::new();
    };
    // 两次调用之间可能又打开了描述符，多留些位置。
    let mut fds = vec![libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; needed / size_of::<libc::proc_fdinfo>() + 32];
    let Ok(capacity) = libc::c_int::try_from(size_of_val(fds.as_slice())) else {
        return Vec::new();
    };
    // SAFETY: 缓冲区长 `capacity` 字节，内核最多写这么多；返回写了几字节。
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), capacity) };
    let Ok(written) = usize::try_from(written) else {
        return Vec::new();
    };
    fds.truncate(written / size_of::<libc::proc_fdinfo>());
    fds.into_iter()
        .filter(|fd| i64::from(fd.proc_fdtype) == i64::from(libc::PROX_FDTYPE_SOCKET))
        .map(|fd| fd.proc_fd)
        .collect()
}

/// 进程 `pid` 的描述符 `fd` 是不是在监听的 TCP 套接字；读不到（描述符刚关掉）时算不是。
fn tcp_listening(pid: Pid, fd: libc::c_int) -> bool {
    let mut info = [0u8; SOCKET_FDINFO_SIZE];
    // SAFETY: 缓冲区长 `SOCKET_FDINFO_SIZE` 字节，正是内核要写的 `socket_fdinfo` 的大小。
    let written = unsafe {
        libc::proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO, info.as_mut_ptr().cast(), SOCKET_FDINFO_SIZE as libc::c_int)
    };
    if usize::try_from(written).ok() != Some(SOCKET_FDINFO_SIZE) {
        return false;
    }
    let int_at =
        |offset: usize| i32::from_ne_bytes([info[offset], info[offset + 1], info[offset + 2], info[offset + 3]]);
    int_at(SOI_KIND_OFFSET) == SOCKINFO_TCP && int_at(TCPSI_STATE_OFFSET) == TSI_S_LISTEN
}
