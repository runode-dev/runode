//! 前端浏览宿主这台电脑上的目录（`ClientMsg::ListDirs`）：手机新建工作区时一级级往下点，选一个
//! 目录。只列子目录的名字，回 `HostMsg::Dirs`；列一个目录是读一遍目录项、每项看一眼是不是目录。
//! 断掉的网络挂载上这些会一直卡住，所以连接另起线程来调，见 `Connection::read_files`。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use runode_protocol::{ErrorReason, message::MAX_DIRS};

/// 列出来的一个目录。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Listing {
    /// 实际列的目录，规范化后的绝对路径。Windows 上分隔符写成 `/`（`C:/Users/me`）：手机按 `/` 拼子目录、
    /// 算上一级。去不掉 `\\?\` 前缀的写成 `//?/C:/…`，前端原样交回来时 `from_client` 再换回 `\\?\`。
    pub(crate) path: PathBuf,
    /// 子目录的名字，按名字不分大小写排好，最多 `MAX_DIRS` 个。
    pub(crate) dirs: Vec<String>,
    /// 子目录多于 `MAX_DIRS` 个，多出来的没给。
    pub(crate) truncated: bool,
}

/// 列不出目录的原因：`message` 是英文的说明，`reason` 是前端能换成界面语言说的那几种。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ListError {
    pub(crate) message: String,
    pub(crate) reason: Option<ErrorReason>,
}

impl ListError {
    fn new(message: String, reason: Option<ErrorReason>) -> Self {
        Self { message, reason }
    }

    /// 读 `path` 时出的 `err`，`doing` 是在做什么（cannot open、cannot read）。
    fn io(doing: &str, path: &Path, err: &io::Error) -> Self {
        let path_buf = forward_slashes(path.to_path_buf());
        let reason = match err.kind() {
            io::ErrorKind::NotFound => Some(ErrorReason::NotFound { path: path_buf }),
            io::ErrorKind::PermissionDenied => Some(ErrorReason::PermissionDenied { path: path_buf }),
            io::ErrorKind::NotADirectory => Some(ErrorReason::NotADirectory { path: path_buf }),
            _ => None,
        };
        Self::new(format!("{doing} {}: {err}", path.display()), reason)
    }
}

/// 列出 `path` 里的子目录，为空时列家目录。
pub(crate) fn list_dirs(path: Option<PathBuf>) -> Result<Listing, ListError> {
    let path = match path {
        Some(path) => path,
        None => runode_paths::Dirs::from_env()
            .home
            .ok_or_else(|| ListError::new("the home directory is not known".into(), None))?,
    };
    let path = drive_root(from_client(path));
    if !path.is_absolute() {
        let reason = ErrorReason::NotAbsolute { path: path.clone() };
        return Err(ListError::new(format!("{} is not an absolute path", path.display()), Some(reason)));
    }
    let path = canonicalize(&path).map_err(|err| ListError::io("cannot open", &path, &err))?;
    if !path.is_dir() {
        let reason = ErrorReason::NotADirectory { path: forward_slashes(path.clone()) };
        return Err(ListError::new(format!("{} is not a directory", path.display()), Some(reason)));
    }
    let entries = fs::read_dir(&path).map_err(|err| ListError::io("cannot read", &path, &err))?;
    // 读不了的目录项（读到一半被删了之类）跳过，不让整个目录列不出来。
    let mut dirs: Vec<String> = entries
        .filter_map(Result::ok)
        // 目录项自带类型，不用再 stat：卡住的网络挂载上每 stat 一次都可能要等很久。只有符号链接才看它指向的
        // 东西，指向的东西不在了的不算。
        .filter(|entry| match entry.file_type() {
            Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
            Ok(kind) => kind.is_dir(),
            Err(_) => entry.path().is_dir(),
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    dirs.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    let truncated = dirs.len() > MAX_DIRS;
    dirs.truncate(MAX_DIRS);
    Ok(Listing { path: forward_slashes(path), dirs, truncated })
}

/// `fs::canonicalize`，Windows 上去掉它加的 `\\?\` 前缀：结果要给 shell 当目录、和界面记的写法比、回给前端，
/// cmd 不认带前缀的目录，PowerShell 的提示符会带上它。去掉后不再是同一个路径的（超长、带 `NUL` 这类保留名）
/// 照原样留着。
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(plain) = path.to_str().and_then(strip_verbatim) {
        return Ok(plain);
    }
    Ok(path)
}

/// `\\?\C:\…` 写成 `C:\…`、`\\?\UNC\server\…` 写成 `\\server\…`；不是这两种或者去掉前缀就变了意思时为空。
#[cfg(any(windows, test))]
fn strip_verbatim(path: &str) -> Option<PathBuf> {
    const MAX_PATH: usize = 260;
    let plain = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        let rest = path.strip_prefix(r"\\?\")?;
        let bytes = rest.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != br":\" {
            return None;
        }
        rest.to_owned()
    };
    let reserved = |part: &str| {
        let stem = part.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
        let numbered = |name: &str| {
            stem.strip_prefix(name)
                .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
        };
        matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
    };
    let changes_meaning = plain.encode_utf16().count() >= MAX_PATH
        || plain.split('\\').skip(1).any(|part| part.ends_with(['.', ' ']) || reserved(part) || part.contains('/'));
    (!changes_meaning).then(|| PathBuf::from(plain))
}

/// 只有盘符（`C:`）时补上根目录：手机从 `C:/` 往上算一级得到的是它。
fn drive_root(path: PathBuf) -> PathBuf {
    let bytes = path.as_os_str().as_encoded_bytes();
    if cfg!(windows) && bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return path.join(std::path::MAIN_SEPARATOR_STR);
    }
    path
}

/// 前端交来的路径：`//?/…`（`Listing::path` 里带 `\\?\` 前缀的写法，后面可能拼了 `/子目录`）换回
/// `\\?\…`，带前缀的路径只认 `\` 当分隔符；iOS 的 `NSString` 拼路径时把开头的 `//` 并成 `/`，`/?/…` 也认。
/// 别的照原样，Windows 自己认 `/`。
pub(crate) fn from_client(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(verbatim) = path.to_str().and_then(verbatim_from_client) {
        return verbatim;
    }
    path
}

#[cfg(any(windows, test))]
fn verbatim_from_client(path: &str) -> Option<PathBuf> {
    let rest = path.strip_prefix("//?/").or_else(|| path.strip_prefix("/?/"))?;
    Some(PathBuf::from(format!(r"\\?\{}", rest.replace('/', "\\"))))
}

fn forward_slashes(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(text) if cfg!(windows) => PathBuf::from(text.replace('\\', "/")),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_prefixes_are_dropped_only_when_the_path_keeps_its_meaning() {
        assert_eq!(strip_verbatim(r"\\?\C:\Users\me"), Some(PathBuf::from(r"C:\Users\me")));
        assert_eq!(strip_verbatim(r"\\?\C:\"), Some(PathBuf::from(r"C:\")));
        assert_eq!(strip_verbatim(r"\\?\UNC\server\share\dir"), Some(PathBuf::from(r"\\server\share\dir")));
        assert_eq!(strip_verbatim(r"C:\Users"), None);
        assert_eq!(strip_verbatim(r"\\?\Volume{0}\dir"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\dir\nul"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\dir\COM1.txt"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\dir\trailing."), None);
        assert_eq!(strip_verbatim(r"\\?\C:\dir\COM0"), Some(PathBuf::from(r"C:\dir\COM0")));
        assert_eq!(strip_verbatim(r"\\?\C:\dir\com²"), None);
        assert_eq!(strip_verbatim(r"\\?\C:\dir\LPT³.log"), None);
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(strip_verbatim(&long), None);
    }

    #[test]
    fn verbatim_listings_come_back_verbatim() {
        assert_eq!(verbatim_from_client("//?/C:/dir/nul/child"), Some(PathBuf::from(r"\\?\C:\dir\nul\child")));
        assert_eq!(verbatim_from_client("/?/C:/dir/nul/child"), Some(PathBuf::from(r"\\?\C:\dir\nul\child")));
        assert_eq!(verbatim_from_client("C:/dir"), None);
        assert_eq!(verbatim_from_client("//server/share"), None);
    }
}
