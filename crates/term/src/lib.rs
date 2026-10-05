//! runode 的终端：接到子 shell 的 PTY 上的 libghostty-vt 状态机，以及 shell 集成、提示符上
//! 正在编辑的输入、agent 状态的识别和命令历史。
//!
//! 对外只用 `runode_model` 里的数据类型；libghostty 和 PTY 的类型不出这个 crate。

pub mod agent;
pub mod history;
mod prompt_input;
pub mod pty;
pub mod session;
mod shell_integration;

pub use prompt_input::PromptInput;

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

    #[test]
    fn cell_widths() {
        assert_eq!(cell_width('a'), 1);
        assert_eq!(cell_width('中'), 2);
        assert_eq!(cell_width('\u{301}'), 0);
    }
}
