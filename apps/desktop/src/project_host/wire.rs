//! 桌面和远端的 `runode --ssh-project` 之间的线上格式。两边一定是同一个构建（装在按构建分开的目录里），
//! 所以不管新旧兼容。一条消息是一行 JSON；带着原样的字节（读到的文件内容）时，那一行写明多少字节，紧接着
//! 是这些字节。

use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
};

use runode_files::Listings;
use serde::{Deserialize, Serialize};

use super::repo::GitOp;

/// 桌面要远端办的事。
#[derive(Debug, Serialize, Deserialize)]
pub(super) enum Call {
    /// 读一遍项目（`scan`）。
    Scan { dir: PathBuf, expanded: Vec<PathBuf>, worktrees: bool },
    /// 有上限地读文件（`runode_files::read_limited`），内容放在回话后面的字节里。
    Read { path: PathBuf, max: u64, whole: bool },
    /// 读文件树里 `root` 下的这些目录（`file_tree::list_dirs`），被忽略的按最近一次读 `root` 时的 git 状态算。
    ListDirs { root: PathBuf, dirs: Vec<PathBuf> },
    /// 对根目录是 `root` 的仓库跑 git。
    Git { root: PathBuf, op: GitOp },
}

/// 远端办完的结果。
#[derive(Debug, Serialize, Deserialize)]
pub(super) enum Reply {
    Scan(Box<RemoteScan>),
    /// 读到了，内容在消息后面的字节里；`cut` 时文件比上限长，后面没读。
    Read {
        cut: bool,
    },
    Listed(Listings),
    /// git 操作的结果，形状是 `runode_git::Repo` 同名方法的返回值。
    Git(serde_json::Value),
    /// 没办成，原因写给人看。
    Failed(String),
}

/// 远端读到的一份项目，`Scan` 里只在本机有意义的（未跟踪文件的缓存、要另外监听的 git 目录）不传。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RemoteScan {
    pub dir: PathBuf,
    pub root: PathBuf,
    pub git: Option<runode_git::Repos>,
    pub listings: Listings,
    pub cost_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Request {
    pub id: u64,
    pub call: Call,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Response {
    pub id: u64,
    pub reply: Reply,
    /// 紧跟在这一行后面的字节数。
    #[serde(default)]
    pub blob: usize,
}

/// 写一条消息，`blob` 跟在后面。消息先整个编好再写，编不了（路径不是 UTF-8）时什么都没写出去。
pub(super) fn write(out: &mut impl Write, message: &impl Serialize, blob: &[u8]) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.write_all(blob)?;
    out.flush()
}

/// 读一行消息；连接关了时为 `None`。
pub(super) fn read_line<T: for<'de> Deserialize<'de>>(input: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line).map(Some).map_err(io::Error::other)
}

/// 读消息后面跟着的 `len` 个字节。
pub(super) fn read_blob(input: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    let mut blob = vec![0; len];
    input.read_exact(&mut blob)?;
    Ok(blob)
}
