//! 前端浏览宿主这台电脑上的目录（`ClientMsg::ListDirs`）：手机新建工作区时一级级往下点，选一个
//! 目录。只列子目录的名字，回 `HostMsg::Dirs`；列一个目录是读一遍目录项、每项看一眼是不是目录。
//! 断掉的网络挂载上这些会一直卡住，所以连接另起线程来调，见 `Connection::read_files`。

use std::{fs, path::PathBuf};

use runode_protocol::message::MAX_DIRS;

/// 列出来的一个目录。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Listing {
    /// 实际列的目录，规范化后的绝对路径。
    pub(crate) path: PathBuf,
    /// 子目录的名字，按名字不分大小写排好，最多 `MAX_DIRS` 个。
    pub(crate) dirs: Vec<String>,
    /// 子目录多于 `MAX_DIRS` 个，多出来的没给。
    pub(crate) truncated: bool,
}

/// 列出 `path` 里的子目录，为空时列家目录。办不了时返回给前端看的原因。
pub(crate) fn list_dirs(path: Option<PathBuf>) -> Result<Listing, String> {
    let path = match path {
        Some(path) => path,
        None => runode_paths::Dirs::from_env().home.ok_or("the home directory is not known")?,
    };
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    let path = fs::canonicalize(&path).map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let entries = fs::read_dir(&path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    // 读不了的目录项（读到一半被删了之类）跳过，不让整个目录列不出来。
    let mut dirs: Vec<String> = entries
        .filter_map(Result::ok)
        // 符号链接看它指向的东西，指向的东西不在了的不算。
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    dirs.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    let truncated = dirs.len() > MAX_DIRS;
    dirs.truncate(MAX_DIRS);
    Ok(Listing { path, dirs, truncated })
}
