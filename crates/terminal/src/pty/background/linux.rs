//! Linux：进程的父子关系扫一遍 `/proc/*/stat`，监听取 `/proc/net/tcp`、`tcp6` 里在监听的
//! 套接字 inode，再对各进程的 `/proc/<pid>/fd/*` 比 `socket:[inode]`。

use std::collections::HashSet;

use super::{super::unix::stat_parent, Pid, listening_inodes};

/// 读的那一刻所有进程的（进程号，父进程号）。
pub(super) struct Tree(Vec<(Pid, Pid)>);

impl Tree {
    /// 读不了 `/proc` 时为 `None`。
    pub(super) fn read() -> Option<Self> {
        let pairs = std::fs::read_dir("/proc")
            .ok()?
            .filter_map(|entry| {
                let pid: Pid = entry.ok()?.file_name().to_str()?.parse().ok()?;
                let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
                Some((pid, stat_parent(&stat)?))
            })
            .collect();
        Some(Self(pairs))
    }

    pub(super) fn children(&self, parent: Pid) -> Vec<Pid> {
        self.0.iter().filter(|&&(pid, ppid)| ppid == parent && pid != parent).map(|&(pid, _)| pid).collect()
    }
}

/// 读的那一刻在监听的 TCP 套接字的 inode。
pub(super) struct Listening(HashSet<u64>);

impl Listening {
    /// `tcp` 和 `tcp6` 都读不了时为 `None`；没开 IPv6 的机器上没有 `tcp6`。
    pub(super) fn read() -> Option<Self> {
        let tables: Vec<String> = ["/proc/net/tcp", "/proc/net/tcp6"]
            .into_iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .collect();
        if tables.is_empty() {
            return None;
        }
        Some(Self(tables.iter().flat_map(|table| listening_inodes(table)).collect()))
    }

    /// `pids` 里有没有进程打开着在监听的套接字；读不了描述符的进程（已经退出）算没有。
    pub(super) fn any(&self, pids: &[Pid]) -> bool {
        !self.0.is_empty()
            && pids.iter().any(|pid| {
                std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten().any(|fd| {
                    std::fs::read_link(fd.path()).ok().is_some_and(|target| {
                        target
                            .to_str()
                            .and_then(|target| target.strip_prefix("socket:[")?.strip_suffix(']')?.parse::<u64>().ok())
                            .is_some_and(|inode| self.0.contains(&inode))
                    })
                })
            })
    }
}
