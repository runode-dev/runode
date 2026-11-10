//! 桌面和远端的 `runode --ssh-project` 之间的线上格式。两边一定是同一个构建（装在按构建分开的目录里），
//! 所以不管新旧兼容。一条消息是一行 JSON；带着原样的字节（读到的文件内容）时，那一行写明多少字节，紧接着
//! 是这些字节。

use std::{
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
};

use runode_files::{Listings, Stamp};
use serde::{Deserialize, Serialize};

use super::{
    edit::{FileOp, OpFailure},
    repo::GitOp,
    upload::UploadOp,
};

/// 一条消息后面最多跟这么多字节，比读文件（`Call::Read` 的上限）、写文件和传文件的一块都大得多。对面说要
/// 更多时不读、断开：长度是对面写的，不能照着先分配。
const MAX_BLOB: usize = 256 << 20;
/// 一行消息最多这么多字节，大仓库列出的全部文件也放得下；更长的不再往下读，报错断开。
const MAX_LINE: u64 = 256 << 20;
/// 读消息后面的字节时一开始最多先分配这么多，收到多少再长多少。
const BLOB_RESERVE: usize = 1 << 20;

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
    /// 改文件。
    Edit(FileOp),
    /// 列 `root` 下按名称找的文件，相对 `root`，只列还在的。
    ListFiles { root: PathBuf },
    /// 在 `root` 下按内容找（`runode_git::grep`），字段是 `GrepQuery` 的。
    Grep {
        root: PathBuf,
        pattern: String,
        ignore_case: bool,
        whole_word: bool,
        regex: bool,
        pathspecs: Vec<String>,
        limit: usize,
    },
    /// 文件现在的版本戳（`runode_files::stamp`）。
    Stamp { path: PathBuf },
    /// 这些路径在不在、是不是目录，回 `Reply::Kinds`。
    Kinds { paths: Vec<PathBuf> },
    /// 这台机器上在监听的 TCP 端口（`status_bar::listening_ports`），回 `Reply::Ports`。
    Ports,
    /// `dir` 所在仓库的 `RepoSummary`，回 `Reply::RepoBadge`。
    RepoBadge { dir: PathBuf },
    /// 把请求后面跟着的字节写成 `path`（`runode_paths::replace_file`），交回写完后的版本戳。
    Write { path: PathBuf },
    /// 传文件（`upload`），回 `Reply::Upload`。
    Upload(UploadOp),
    /// 监听 `dirs`（项目目录和不在它下面的 git 目录）：桌面每个监听一个 `token`。还没有这个监听时开始听，
    /// 马上回 `Reply::Changed` 的 `started`；已经在听时等到攒下改动，或者等够 `serve::WATCH_HOLD` 交回空的。
    Watch { token: u64, dirs: Vec<PathBuf> },
    /// 让编号为 `id` 的请求停下，停得下的只有 `Grep` 和 `Watch`；这条本身不回话，停下的那条照样回话。
    Cancel { id: u64 },
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
    /// 改完了；挪进、复制进目录的是落到了哪里。
    Edited(Result<Option<PathBuf>, OpFailure>),
    Stamp(Option<Stamp>),
    Files(Vec<PathBuf>),
    Found(Vec<runode_git::GrepMatch>),
    /// 按请求的顺序，不在时为空，在时是不是目录。
    Kinds(Vec<Option<bool>>),
    Ports(Vec<crate::system_probe::Port>),
    RepoBadge(super::RepoSummary),
    /// 监听到的改动；`started` 时是刚开始听，之前的改动听不到。
    Changed {
        started: bool,
        changes: runode_files::Changes,
    },
    /// 传文件的这一步办完了，`UploadOp::Begin` 带着新建的子目录。
    Upload(Option<PathBuf>),
    /// 没办成，原因写给人看。
    Failed(String),
}

impl Call {
    /// 对请求里的每个路径调一次 `f`。
    pub(super) fn for_each_path(&mut self, f: &mut dyn FnMut(&mut PathBuf)) {
        match self {
            Self::Scan { dir, expanded, .. } => {
                f(dir);
                expanded.iter_mut().for_each(f);
            }
            Self::ListDirs { root, dirs } => {
                f(root);
                dirs.iter_mut().for_each(f);
            }
            Self::Watch { dirs, .. } | Self::Kinds { paths: dirs } => dirs.iter_mut().for_each(f),
            Self::Git { root, op } => {
                f(root);
                op.for_each_path(f);
            }
            Self::Edit(op) => op.for_each_path(f),
            Self::Upload(op) => op.for_each_path(f),
            Self::Read { path, .. } | Self::Stamp { path } | Self::Write { path } | Self::RepoBadge { dir: path } => {
                f(path)
            }
            Self::ListFiles { root } | Self::Grep { root, .. } => f(root),
            Self::Ports | Self::Cancel { .. } => {}
        }
    }
}

impl Reply {
    /// 去掉 JSON 写不出的路径（不是 UTF-8 的），免得一个这样的文件名让整份回话都编不了：被监听到改动的
    /// 改成整个重扫，仓库自己的目录写不出时整个仓库不要。文件树里各项的名字按字节传，不受影响。
    pub(super) fn drop_unsendable(&mut self) {
        match self {
            Self::Scan(scan) => {
                scan.listings.retain(|(dir, _)| sendable(dir));
                scan.watch_git_dirs.retain(|dir| sendable(dir));
                if scan.git.as_mut().is_some_and(|git| !drop_unsendable_repos(git)) {
                    scan.git = None;
                }
            }
            Self::Listed(listings) => listings.retain(|(dir, _)| sendable(dir)),
            Self::Files(files) => files.retain(|path| sendable(path)),
            Self::Found(found) => found.retain(|found| sendable(&found.path)),
            Self::Changed { changes, .. } => {
                let heard = changes.paths.len();
                changes.paths.retain(|path| sendable(path));
                changes.everything |= changes.paths.len() < heard;
            }
            _ => {}
        }
    }
}

fn sendable(path: &Path) -> bool {
    path.to_str().is_some()
}

/// 主仓库自己的目录写不出时返回 `false`。
fn drop_unsendable_repos(repos: &mut runode_git::Repos) -> bool {
    repos.subs.retain_mut(drop_unsendable_snapshot);
    repos.worktrees.retain_mut(drop_unsendable_snapshot);
    drop_unsendable_snapshot(&mut repos.main)
}

/// 仓库自己的目录写不出时返回 `false`。
fn drop_unsendable_snapshot(snapshot: &mut runode_git::Snapshot) -> bool {
    if ![&snapshot.root, &snapshot.git_dir, &snapshot.prefix].into_iter().all(|dir| sendable(dir)) {
        return false;
    }
    let file = |file: &runode_git::FileDiff| sendable(&file.path) && file.old_path.as_deref().is_none_or(sendable);
    snapshot.staged.retain(file);
    snapshot.unstaged.retain(file);
    snapshot.statuses.retain(|path, _| sendable(path));
    snapshot.ignored.retain(|path| sendable(path));
    snapshot.head_stats.retain(|path, _| sendable(path));
    true
}

/// 远端读到的一份项目，`Scan` 里只在本机有意义的未跟踪文件缓存不传。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct RemoteScan {
    pub dir: PathBuf,
    pub root: PathBuf,
    pub git: Option<runode_git::Repos>,
    pub listings: Listings,
    /// 监听项目时还要另外听的 git 目录（`Scan::watch_git_dirs`），在那台机器上算。
    #[serde(default)]
    pub watch_git_dirs: Vec<PathBuf>,
    pub cost_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Request {
    pub id: u64,
    pub call: Call,
    /// 紧跟在这一行后面的字节数。
    #[serde(default)]
    pub blob: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Response {
    pub id: u64,
    pub reply: Reply,
    /// 紧跟在这一行后面的字节数。
    #[serde(default)]
    pub blob: usize,
}

/// 写一条消息，`blob` 跟在后面。消息先整个编好再写，编不了（路径不是 UTF-8）时什么都没写出去，报
/// `InvalidData`，连接照样能用。
pub(super) fn write(out: &mut impl Write, message: &impl Serialize, blob: &[u8]) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.write_all(blob)?;
    out.flush()
}

/// 读一行消息；连接关了时为 `None`，比 `MAX_LINE` 长时报 `InvalidData`。
pub(super) fn read_line<T: for<'de> Deserialize<'de>>(input: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    let read = input.take(MAX_LINE).read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if read as u64 == MAX_LINE && !line.ends_with('\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a message line is too long"));
    }
    serde_json::from_str(&line).map(Some).map_err(io::Error::other)
}

/// 读消息后面跟着的 `len` 个字节；超过 `MAX_BLOB` 时报 `InvalidData`，什么都不读。
pub(super) fn read_blob(input: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    if len > MAX_BLOB {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a message is followed by {len} bytes")));
    }
    let mut blob = Vec::with_capacity(len.min(BLOB_RESERVE));
    input.take(len as u64).read_to_end(&mut blob)?;
    if blob.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(blob)
}

#[cfg(all(test, unix))]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    use super::*;

    #[test]
    fn blobs_are_read_whole_and_capped() {
        let mut input: &[u8] = b"hello";
        assert_eq!(read_blob(&mut input, 4).unwrap(), b"hell");
        assert_eq!(read_blob(&mut input, 4).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let mut input: &[u8] = b"";
        assert_eq!(read_blob(&mut input, usize::MAX).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn unsendable_paths_are_dropped_instead_of_failing_the_reply() {
        let bad = PathBuf::from(OsStr::from_bytes(b"caf\xe9.txt"));
        let mut files = Reply::Files(vec![PathBuf::from("a.txt"), bad.clone()]);
        files.drop_unsendable();
        assert!(matches!(&files, Reply::Files(files) if files == &[PathBuf::from("a.txt")]));
        let changes = runode_files::Changes { paths: vec![bad], everything: false };
        let mut changed = Reply::Changed { started: false, changes };
        assert!(write(&mut Vec::new(), &changed, &[]).is_err_and(|err| err.kind() == io::ErrorKind::InvalidData));
        changed.drop_unsendable();
        assert!(matches!(&changed, Reply::Changed { changes, .. } if changes.paths.is_empty() && changes.everything));
        write(&mut Vec::new(), &changed, &[]).unwrap();
    }
}
