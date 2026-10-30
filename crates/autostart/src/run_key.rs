//! Windows 的登录自启：当前用户的 `Run` 键（`RUN_KEY`）下一个值（`VALUE`），登录时 Explorer 照它拉起
//! 宿主。不要管理员权限；安装包卸载时删掉这个值。

use std::{
    io,
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW},
};

/// 当前用户登录时拉起的程序都登记在这个键下（`HKEY_CURRENT_USER` 之下）。
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 宿主在 `RUN_KEY` 下的值名；安装包卸载时按这个名字删，改名要和安装脚本一起改。
const VALUE: &str = "Runode Host";

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// 登录时跑的命令行：宿主是 `exe` 旁边的 runode.exe（命令行 rn.exe 只管命令），路径加引号，安装目录在
/// 带空格的用户名下也对；`canonicalize` 加的 `\\?\` 前缀换回普通写法，网络共享上的 `\\?\UNC\` 换回 `\\`。
pub(crate) fn command(exe: &Path) -> String {
    let exe = exe.with_file_name("runode.exe");
    let exe = exe.to_string_lossy();
    let exe = match exe.strip_prefix(r"\\?\UNC\") {
        Some(share) => format!(r"\\{share}"),
        None => exe.strip_prefix(r"\\?\").unwrap_or(&exe).to_owned(),
    };
    format!("\"{exe}\" --host --stay")
}

/// 登记着的命令行；没登记时为 `None`。
fn get() -> Option<String> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE));
    let mut size = 0u32;
    // SAFETY: 字符串都以 NUL 结尾；先只问长度。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut data = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: `data` 有 `size` 个字节。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            data.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let end = data.iter().position(|&c| c == 0).unwrap_or(data.len());
    Some(String::from_utf16_lossy(&data[..end]))
}

pub(crate) fn is_installed() -> bool {
    get().is_some()
}

/// 登记宿主，再在后台拉起一个，不用等下次登录；已经有宿主在跑时新拉起的抢不到锁、自己退出。拉不起来时
/// 登记恢复成原样，免得报了失败却留着一个登录时会跑的登记。
pub(crate) fn install(exe: &Path) -> io::Result<()> {
    let previous = get();
    set(&command(exe))?;
    let spawned = Command::new(exe.with_file_name("runode.exe"))
        .args(["--host", "--stay"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn();
    if let Err(err) = spawned {
        let restored = match &previous {
            Some(previous) => set(previous),
            None => uninstall().map(drop),
        };
        if let Err(restore) = restored {
            return Err(io::Error::new(err.kind(), format!("{err}; the login item was left changed: {restore}")));
        }
        return Err(err);
    }
    Ok(())
}

fn set(command: &str) -> io::Result<()> {
    let (key, value, data) = (wide(RUN_KEY), wide(VALUE), wide(command));
    let bytes = u32::try_from(data.len() * 2).map_err(io::Error::other)?;
    // SAFETY: 字符串都以 NUL 结尾，`bytes` 是 `data` 含结尾 NUL 的字节数。
    let status = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), bytes)
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// 去掉登记，原来就没有时返回 `false`。在跑的宿主不停。
pub(crate) fn uninstall() -> io::Result<bool> {
    let (key, value) = (wide(RUN_KEY), wide(VALUE));
    // SAFETY: 字符串都以 NUL 结尾。
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        status => Err(io::Error::from_raw_os_error(status as i32)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_quotes_the_host_next_to_the_cli() {
        let exe = Path::new(r"\\?\C:\Users\A B\AppData\Local\Programs\Runode\rn.exe");
        assert_eq!(command(exe), r#""C:\Users\A B\AppData\Local\Programs\Runode\runode.exe" --host --stay"#);
    }

    #[test]
    fn the_command_keeps_network_shares_absolute() {
        let exe = Path::new(r"\\?\UNC\server\apps\Runode\rn.exe");
        assert_eq!(command(exe), r#""\\server\apps\Runode\runode.exe" --host --stay"#);
    }
}
