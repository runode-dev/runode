//! 文件路径和 `file://` URI 的换算。路径可以是别的电脑上的，所以按路径的写法分 Unix 和 Windows，不按这个程序
//! 编给哪个系统：`/` 开头的是 Unix 的，带盘符的是 Windows 的。

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use lsp_types::Uri;

/// `path`（绝对路径）的 `file://` URI；路径不是 UTF-8 时没有。
pub fn from_path(path: &Path) -> Option<Uri> {
    let path = path.to_str()?;
    let windows = !path.starts_with('/');
    let mut uri = String::from(if windows { "file:///" } else { "file://" });
    for byte in path.bytes() {
        match byte {
            b'\\' if windows => uri.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                uri.push(char::from(byte))
            }
            _ => write!(uri, "%{byte:02X}").ok()?,
        }
    }
    uri.parse().ok()
}

/// `file://` URI 指的路径；别的 scheme、带主机名的没有。
pub fn to_path(uri: &Uri) -> Option<PathBuf> {
    let rest = uri.as_str().strip_prefix("file://")?;
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let mut rest = rest.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte == b'%' {
            let (hex, tail) = rest.split_at_checked(2)?;
            bytes.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            rest = tail;
        } else {
            bytes.push(byte);
        }
    }
    let path = String::from_utf8(bytes).ok()?;
    let windows =
        matches!(path.as_bytes(), [b'/', drive, b':'] | [b'/', drive, b':', b'/', ..] if drive.is_ascii_alphabetic());
    Some(PathBuf::from(if windows { path[1..].replace('/', "\\") } else { path }))
}
