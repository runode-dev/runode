//! agent 自己起的后台 shell 在做什么：一直跑着的服务（dev server）不算 agent 在干活。
//!
//! 进程树由调用方读好了交进来，这里只给判断的口径。

use crate::{ForegroundProcess, identify};

/// agent 名下的后台 shell。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundShells {
    /// 找到几个 agent 起的 shell（`is_tool_shell`）。
    pub shells: u32,
    /// 其中几个的进程子树里有进程在监听 TCP 端口。
    pub serving: u32,
}

impl BackgroundShells {
    /// 还有不是服务的 shell，agent 在等它跑完。一个 shell 都没找到时说不清，也算。
    pub fn busy(self) -> bool {
        self.shells == 0 || self.serving < self.shells
    }
}

/// `p` 是不是 agent 用来跑命令的 shell：进程名是 sh、bash、zsh、dash、ksh 或 fish，
/// 参数里有带 `c` 的短选项串（`-c`、`-lc`）。
pub fn is_tool_shell(p: &ForegroundProcess) -> bool {
    let name = identify::lookup_name(identify::basename(&p.name));
    matches!(name.as_str(), "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish")
        && p.argv.as_deref().is_some_and(|argv| {
            argv.iter().skip(1).any(|arg| {
                arg.strip_prefix('-').is_some_and(|flags| {
                    !flags.is_empty() && flags.bytes().all(|b| b.is_ascii_alphabetic()) && flags.contains('c')
                })
            })
        })
}
