//! runode 的终端：接到子 shell 的 PTY 上的 libghostty-vt 状态机，以及 shell 集成、提示符上
//! 正在编辑的输入和运行完的命令。前台 agent 的识别由 `runode_agent` 判断，这里把前台进程、
//! 屏幕文字、标题和进度报告交给它。
//!
//! 一个终端有两份 VT：宿主那份（`host_session::HostSession`）接着 PTY，是权威的，应答查询、
//! 认标题和 agent、记命令；界面那份（`session::Session`）只消费同样的字节流，用来画屏幕。
//!
//! 对外只用 `runode_types` 里的数据类型；libghostty 和 PTY 的类型不出这个 crate。

#[cfg(unix)]
pub mod fd_passing;
pub mod host_session;
mod prompt_input;
pub mod pty;
pub mod session;
mod shell_integration;
#[cfg(test)]
mod testing;
mod vt;

pub use prompt_input::PromptInput;
pub use vt::SnapshotError;

/// runode 的版本号，即整个工作区的版本；终端回答 XTVERSION、设置 `TERM_PROGRAM_VERSION` 时用。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 终端对 XTVERSION 查询的回答：程序名加 `VERSION`。回调要的是 `'static` 的字符串，所以在
/// 编译期拼好。
const XTVERSION: &str = concat!("runode ", env!("CARGO_PKG_VERSION"));

/// 装上 Kitty 图片的 PNG 解码器，进程里的 VT 才收 PNG 图片；没装时 PNG 图片被拒，原始像素的照收。
/// 解码器是整个进程共用的，宿主和界面的 VT 在同一个进程里时也只装一次。
///
/// # Safety
///
/// 调的时候不能有别的线程正在用 libghostty，所以在进程刚启动、还没建任何 VT 时调。
pub unsafe fn install_png_decoder() {
    let decoder = libghostty_vt::kitty::graphics::RustPngDecoder::new();
    // SAFETY: 由调用方保证没有别的线程在用 libghostty。
    if let Err(err) = unsafe { libghostty_vt::kitty::graphics::set_png_decoder(Some(Box::new(decoder))) } {
        tracing::warn!("failed to install the PNG decoder for Kitty images: {err}");
    }
}

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
