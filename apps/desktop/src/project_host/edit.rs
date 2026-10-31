//! 改项目里的文件：本机的直接调 `runode_files::ops`；远端的编成 `FileOp` 交给那边的 `runode --ssh-project`
//! 跑，失败的原因按 `OpFailure` 带回来，再换回 `OpError`。

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use runode_files::ops::{self, OpError};
use serde::{Deserialize, Serialize};

use super::{
    ProjectHost,
    wire::{Call, Reply},
};

/// 远端改文件最多等这么久：复制、删掉大目录要一阵。
const EDIT_TIMEOUT: Duration = Duration::from_secs(600);

/// 远端要做的一个改文件的操作，字段是 `runode_files::ops` 同名函数的参数。
#[derive(Debug, Serialize, Deserialize)]
pub(super) enum FileOp {
    Create { path: PathBuf, is_dir: bool },
    Rename { from: PathBuf, to: PathBuf },
    MoveInto { src: PathBuf, dir: PathBuf },
    CopyInto { src: PathBuf, dir: PathBuf },
    AddToGitignore { root: PathBuf, rel: PathBuf, is_dir: bool },
    Remove { path: PathBuf },
}

impl FileOp {
    /// 对操作里的每个路径调一次 `f`。
    pub(super) fn for_each_path(&mut self, f: &mut dyn FnMut(&mut PathBuf)) {
        match self {
            Self::Create { path, .. } | Self::Remove { path } => f(path),
            Self::Rename { from: a, to: b }
            | Self::MoveInto { src: a, dir: b }
            | Self::CopyInto { src: a, dir: b }
            | Self::AddToGitignore { root: a, rel: b, .. } => {
                f(a);
                f(b);
            }
        }
    }
}

/// `OpError` 在线上的样子：io 的错误只带说明。
#[derive(Debug, Serialize, Deserialize)]
pub(super) enum OpFailure {
    Exists,
    IntoItself,
    NewlineInName,
    Unsupported,
    Other(String),
}

impl From<OpError> for OpFailure {
    fn from(err: OpError) -> Self {
        match err {
            OpError::Exists => Self::Exists,
            OpError::IntoItself => Self::IntoItself,
            OpError::NewlineInName => Self::NewlineInName,
            OpError::Io(err) if err.kind() == io::ErrorKind::Unsupported => Self::Unsupported,
            OpError::Io(err) => Self::Other(err.to_string()),
        }
    }
}

impl From<OpFailure> for OpError {
    fn from(failure: OpFailure) -> Self {
        match failure {
            OpFailure::Exists => Self::Exists,
            OpFailure::IntoItself => Self::IntoItself,
            OpFailure::NewlineInName => Self::NewlineInName,
            OpFailure::Unsupported => Self::Io(io::ErrorKind::Unsupported.into()),
            OpFailure::Other(message) => Self::Io(io::Error::other(message)),
        }
    }
}

impl FileOp {
    /// 在远端做；挪进、复制进目录的交回落到了哪里。
    pub(super) fn run(self) -> Result<Option<PathBuf>, OpFailure> {
        let done = match self {
            Self::Create { path, is_dir } => ops::create(&path, is_dir).map(|()| None),
            Self::Rename { from, to } => ops::rename(&from, &to).map(|()| None),
            Self::MoveInto { src, dir } => ops::move_into(&src, &dir).map(Some),
            Self::CopyInto { src, dir } => ops::copy_into(&src, &dir).map(Some),
            Self::AddToGitignore { root, rel, is_dir } => ops::add_to_gitignore(&root, &rel, is_dir).map(|()| None),
            Self::Remove { path } => ops::remove(&path).map(|()| None),
        };
        done.map_err(OpFailure::from)
    }
}

impl ProjectHost {
    /// 在项目所在的机器上做 `op`，`local` 是本机的做法。会阻塞。
    fn edit<T>(
        &self,
        op: impl FnOnce() -> FileOp,
        local: impl FnOnce() -> Result<T, OpError>,
        done: impl FnOnce(Option<PathBuf>) -> T,
    ) -> Result<T, OpError> {
        let client = match self {
            Self::Local => return local(),
            Self::Remote(client) => client,
        };
        match client.call(Call::Edit(op()), EDIT_TIMEOUT).map_err(|err| OpError::Io(io::Error::other(err)))? {
            (Reply::Edited(result), _) => result.map(done).map_err(OpError::from),
            (other, _) => Err(OpError::Io(io::Error::other(super::unexpected(&other)))),
        }
    }

    pub(crate) fn create(&self, path: &Path, is_dir: bool) -> Result<(), OpError> {
        let op = || FileOp::Create { path: path.to_path_buf(), is_dir };
        self.edit(op, || ops::create(path, is_dir), drop)
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> Result<(), OpError> {
        let op = || FileOp::Rename { from: from.to_path_buf(), to: to.to_path_buf() };
        self.edit(op, || ops::rename(from, to), drop)
    }

    pub(crate) fn move_into(&self, src: &Path, dir: &Path) -> Result<PathBuf, OpError> {
        let op = || FileOp::MoveInto { src: src.to_path_buf(), dir: dir.to_path_buf() };
        self.edit(op, || ops::move_into(src, dir), Option::unwrap_or_default)
    }

    pub(crate) fn copy_into(&self, src: &Path, dir: &Path) -> Result<PathBuf, OpError> {
        let op = || FileOp::CopyInto { src: src.to_path_buf(), dir: dir.to_path_buf() };
        self.edit(op, || ops::copy_into(src, dir), Option::unwrap_or_default)
    }

    pub(crate) fn add_to_gitignore(&self, root: &Path, rel: &Path, is_dir: bool) -> Result<(), OpError> {
        let op = || FileOp::AddToGitignore { root: root.to_path_buf(), rel: rel.to_path_buf(), is_dir };
        self.edit(op, || ops::add_to_gitignore(root, rel, is_dir), drop)
    }

    /// 本机的移到废纸篓；远端的没有废纸篓，直接删掉，调用方先问过要不要永久删除。
    pub(crate) fn delete(&self, path: &Path) -> Result<(), OpError> {
        self.edit(|| FileOp::Remove { path: path.to_path_buf() }, || ops::trash(path), drop)
    }
}
