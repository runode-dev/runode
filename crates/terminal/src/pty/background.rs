//! agent 放到后台的 shell 在做什么：读 agent 的子进程里有几个是它跑命令的 shell
//! （`runode_agent::background::is_tool_shell`），其中几个的进程子树在监听 TCP 端口。
//!
//! 各平台提供同样的两样东西：`Tree` 是一次读下来的进程父子关系，`Listening` 是一次读下来的
//! TCP 监听，每次探测各读一次，不按 shell 重读。

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows;

use runode_agent::{BackgroundShells, background::is_tool_shell};

#[cfg(target_os = "linux")]
use linux::{Listening, Tree};
#[cfg(target_os = "macos")]
use macos::{Listening, Tree};
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use other::{Listening, Tree};
#[cfg(windows)]
use windows::{Listening, Tree};

use super::{Pid, process_info};

/// `agent` 的直接子进程里它跑命令的 shell，以及其中几个的子树（含 shell 自己）在监听 TCP 端口。
///
/// Unix 上排除 `foreground` 里的进程：agent 放到后台的 shell 自成进程组，和 agent 同组的是它的
/// MCP、LSP 这类常驻子进程。Windows 上不用 `foreground`，那里的前台进程组是组长和它全部的子孙，
/// 后台 shell 也在里面。
///
/// 进程表或监听表整个读不到时为 `None`；单个进程在两次读之间退出了，按没有监听算。
pub(crate) fn background_shells(agent: Pid, foreground: &[Pid]) -> Option<BackgroundShells> {
    let tree = Tree::read()?;
    let shells: Vec<Pid> = tree
        .children(agent)
        .into_iter()
        .filter(|pid| cfg!(windows) || !foreground.contains(pid))
        .filter(|&pid| process_info(pid).is_some_and(|process| is_tool_shell(&process)))
        .collect();
    if shells.is_empty() {
        return Some(BackgroundShells { shells: 0, serving: 0 });
    }
    let listening = Listening::read()?;
    let serving = shells.iter().filter(|&&shell| listening.any(&subtree(&tree, shell))).count();
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    Some(BackgroundShells { shells: count(shells.len()), serving: count(serving) })
}

/// `root` 和它全部的子孙进程；`root` 在最前面。
fn subtree(tree: &Tree, root: Pid) -> Vec<Pid> {
    let mut pids = vec![root];
    let mut next = 0;
    while let Some(&parent) = pids.get(next) {
        for child in tree.children(parent) {
            if !pids.contains(&child) {
                pids.push(child);
            }
        }
        next += 1;
    }
    pids
}

/// `/proc/net/tcp` 或 `tcp6` 里在监听（状态 `0A`）的套接字的 inode。
#[cfg(any(test, target_os = "linux"))]
fn listening_inodes(table: &str) -> impl Iterator<Item = u64> + '_ {
    table.lines().skip(1).filter_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(3) != Some(&"0A") {
            return None;
        }
        fields.get(9)?.parse().ok().filter(|&inode| inode != 0)
    })
}

/// 别的 Unix 上读不了进程树，探测总是 `None`。
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod other {
    use super::Pid;

    pub(super) struct Tree;

    impl Tree {
        pub(super) fn read() -> Option<Self> {
            None
        }

        pub(super) fn children(&self, _pid: Pid) -> Vec<Pid> {
            Vec::new()
        }
    }

    pub(super) struct Listening;

    impl Listening {
        pub(super) fn read() -> Option<Self> {
            None
        }

        pub(super) fn any(&self, _pids: &[Pid]) -> bool {
            false
        }
    }
}
