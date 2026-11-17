//! Windows：进程的父子关系取 Toolhelp 快照，监听取 `GetExtendedTcpTable` 的
//! `TCP_TABLE_OWNER_PID_LISTENER` 表里的进程号。

use std::{collections::HashSet, mem::size_of};

use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
};

use super::{
    super::conpty::{children, snapshot},
    Pid,
};

/// 读的那一刻所有进程的（进程号，父进程号）。
pub(super) struct Tree(Vec<(u32, u32)>);

impl Tree {
    /// 取不到快照时为 `None`。
    pub(super) fn read() -> Option<Self> {
        let pairs = snapshot();
        (!pairs.is_empty()).then_some(Self(pairs))
    }

    /// `parent` 的直接子进程；比 `parent` 起得早的不算（父进程号被重用了）。
    pub(super) fn children(&self, parent: Pid) -> Vec<Pid> {
        children(&self.0, parent).into_iter().map(|(pid, _)| pid).collect()
    }
}

/// 读的那一刻在监听 TCP 端口的进程号。
pub(super) struct Listening(HashSet<u32>);

impl Listening {
    /// IPv4 和 IPv6 的监听表都读不到时为 `None`。
    pub(super) fn read() -> Option<Self> {
        let v4 = owners::<MIB_TCPROW_OWNER_PID>(AF_INET, |row| row.dwOwningPid);
        let v6 = owners::<MIB_TCP6ROW_OWNER_PID>(AF_INET6, |row| row.dwOwningPid);
        if v4.is_none() && v6.is_none() {
            return None;
        }
        Some(Self(v4.into_iter().chain(v6).flatten().collect()))
    }

    pub(super) fn any(&self, pids: &[Pid]) -> bool {
        pids.iter().any(|pid| self.0.contains(pid))
    }
}

/// 一个地址族里在监听 TCP 端口的进程号；问不到时为 `None`。`Row` 是这个地址族的
/// `TCP_TABLE_OWNER_PID_LISTENER` 表里一行的类型，`owner` 取出它的进程号。
fn owners<Row>(family: u16, owner: impl Fn(&Row) -> u32) -> Option<Vec<u32>> {
    let mut buffer: Vec<u64> = Vec::new();
    let mut size = 0_u32;
    // 两次调用之间表可能变大，缓冲区不够时按新的大小再来。
    for _ in 0..4 {
        buffer.resize((size as usize).div_ceil(size_of::<u64>()), 0);
        // SAFETY: 缓冲区至少 `size` 字节，可写；`size` 为 0 时只问要多大。
        let status = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut size,
                0,
                u32::from(family),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != NO_ERROR || buffer.is_empty() {
            return None;
        }
        // SAFETY: 调用成功时缓冲区开头是行数，后面紧跟着这么多行 `Row`，都在 `size` 字节以内；`Row` 的对齐
        // 不超过 `u32`，从开头偏 `u32` 的位置就是第一行（表结构里 `table` 紧跟在 `dwNumEntries` 后面）。
        let rows = unsafe {
            let count = *buffer.as_ptr().cast::<u32>() as usize;
            std::slice::from_raw_parts(buffer.as_ptr().cast::<u32>().add(1).cast::<Row>(), count)
        };
        return Some(rows.iter().map(&owner).collect());
    }
    None
}
