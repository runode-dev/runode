//! 起子进程时各处共用的写法：Windows 上不给控制台程序开窗口，找系统的 curl。

use std::{path::PathBuf, process::Command};

/// Windows 上拉起控制台程序时不开窗口（`CREATE_NO_WINDOW`）：app 和宿主都没有控制台，不然每拉起一次就闪
/// 一个黑窗。别的系统上什么都不做。
pub fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, 0x0800_0000);
    #[cfg(not(windows))]
    let _ = command;
}

/// 系统的 curl。Windows 上 `Command` 先找 System32 再找 PATH，所以先按 PATH 找 `curl.exe`，用户自己装的
/// 新版排在系统自带的前面；PATH 里的空段和相对段会按当前目录找，不认。
pub fn curl_program() -> PathBuf {
    #[cfg(windows)]
    let program = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("curl.exe"))
                .find(|exe| exe.is_file())
        })
        .unwrap_or_else(|| "curl.exe".into());
    #[cfg(not(windows))]
    let program = "curl".into();
    program
}
