//! Markdown 里链接和图片地址指向哪儿：网页、本地文件还是同一篇里的锚点。

use std::path::{Component, Path, PathBuf};

/// 点链接去哪儿。
#[derive(Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// 交给系统浏览器（http、https、mailto）。
    Web(String),
    /// 相对 Markdown 文件所在目录的路径，在预览栏里打开。
    File(PathBuf),
    /// 同一篇文档里的锚点（`#` 后面的部分，已经百分号解码）。
    Anchor(String),
}

/// 链接或图片地址 `url` 指向哪儿；`dir` 是 Markdown 文件所在的目录。别的协议为空；省了协议的
/// `//主机/路径` 按 https 的网页算。本地路径去掉 `#锚点` 和 `?参数` 后做百分号解码，`..`、`.` 按字面
/// 消掉（不跟符号链接）；解出来以两个斜杠或反斜杠开头的为空：Windows 上那是别的机器上的共享，
/// 读它会连过去、交出登录凭据。
pub fn link_target(dir: &Path, url: &str) -> Option<LinkTarget> {
    let url = url.trim();
    if url.starts_with("//") {
        return Some(LinkTarget::Web(format!("https:{url}")));
    }
    if let Some((scheme, _)) = url.split_once(':')
        && is_scheme(scheme)
    {
        let web = ["http", "https", "mailto"].iter().any(|known| scheme.eq_ignore_ascii_case(known));
        return web.then(|| LinkTarget::Web(url.to_owned()));
    }
    if let Some(anchor) = url.strip_prefix('#') {
        return (!anchor.is_empty()).then(|| LinkTarget::Anchor(percent_decode(anchor)));
    }
    let path = percent_decode(url.split(['#', '?']).next().unwrap_or_default());
    let network = path.len() >= 2 && path.bytes().take(2).all(|b| matches!(b, b'/' | b'\\'));
    (!path.is_empty() && !network).then(|| LinkTarget::File(normalize(&dir.join(path))))
}

/// 按字面消掉路径里的 `.` 和 `..`：`..` 去掉前一段，到了根就停在根上。不读磁盘。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(component);
                }
            }
            _ => out.push(component),
        }
    }
    out
}

/// 百分号解码（`my%20file.md` 是 `my file.md`）；解出来不是 UTF-8 时原样不动。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|&byte| char::from(byte).to_digit(16));
        match (bytes[ix], hex(ix + 1), hex(ix + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                ix += 3;
            }
            (byte, ..) => {
                out.push(byte);
                ix += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// 像 URL 的协议名：字母开头，两个字符以上（排除 `C:` 这类盘符）。
fn is_scheme(text: &str) -> bool {
    text.len() > 1
        && text.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && text.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}
