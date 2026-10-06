//! runode 的终端：接到子 shell 的 PTY 上的 libghostty-vt 状态机，以及 shell 集成、提示符上
//! 正在编辑的输入和命令历史。前台 agent 的识别由 `runode_agent_detect` 判断，这里把前台进程、
//! 屏幕文字、标题和进度报告交给它。
//!
//! 一个终端有两份 VT：宿主那份（`host_session::HostSession`）接着 PTY，是权威的，应答查询、
//! 认标题和 agent、记命令；界面那份（`session::Session`）只消费同样的字节流，用来画屏幕。
//!
//! 对外只用 `runode_shared_types` 里的数据类型；libghostty 和 PTY 的类型不出这个 crate。

pub mod history;
pub mod host_session;
mod prompt_input;
pub mod pty;
pub mod session;
mod shell_integration;
#[cfg(test)]
mod testing;
mod vt;

pub use prompt_input::{InputCell, PromptInput};
pub use vt::SnapshotError;

/// runode 的版本号，即整个工作区的版本；终端回答 XTVERSION、设置 `TERM_PROGRAM_VERSION` 时用。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 终端对 XTVERSION 查询的回答：程序名加 `VERSION`。回调要的是 `'static` 的字符串，所以在
/// 编译期拼好。
const XTVERSION: &str = concat!("runode ", env!("CARGO_PKG_VERSION"));

/// 一个字在终端里占几格：零宽的组合字符为 0，宽字符为 2，其余为 1。和终端排版打印出来的
/// 文字用的是同一张表。
pub fn cell_width(c: char) -> u8 {
    libghostty_vt::unicode::codepoint_width(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xtversion_reports_the_version() {
        assert_eq!(XTVERSION, format!("runode {VERSION}"));
    }
}
