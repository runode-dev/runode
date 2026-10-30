//! 项目的文件和 git 在哪台机器上读写：本机的直接调；SSH 主机上的交给那台机器上同一个构建的
//! `runode --ssh-project`（`serve`），经 ssh 收发（`client`），线上格式在 `wire`。各面板只经 `ProjectHost`
//! 读写，不管项目在哪台机器上。

mod client;
mod edit;
mod repo;
mod scan;
pub(crate) mod serve;
mod wire;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use runode_files::{Limited, Listings, Stamp};
use runode_git::UntrackedCache;

pub(crate) use repo::ProjectRepo;
pub(crate) use scan::{Scan, scan};
use wire::{Call, Reply};

use crate::host_client::remote::HostId;

/// 读一遍远端的项目最多等这么久：大仓库第一次读 git 状态要一阵。
const SCAN_TIMEOUT: Duration = Duration::from_secs(120);
/// 读远端的文件最多等这么久。
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub(crate) enum ProjectHost {
    #[default]
    Local,
    Remote(Arc<client::Client>),
}

impl std::fmt::Debug for ProjectHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => f.write_str("Local"),
            Self::Remote(client) => f.debug_tuple("Remote").field(&client.name()).finish(),
        }
    }
}

impl ProjectHost {
    pub(crate) fn of(host: &HostId) -> Self {
        match host {
            HostId::Local => Self::Local,
            HostId::Ssh(name) => Self::Remote(client::Client::of(name)),
        }
    }

    /// 在项目所在的机器上 `scan`；远端的未跟踪文件缓存留在那边，`untracked` 用不上。会阻塞。
    pub(crate) fn scan(
        &self,
        dir: PathBuf,
        expanded: Vec<PathBuf>,
        untracked: UntrackedCache,
        worktrees: bool,
    ) -> Result<Scan, String> {
        let client = match self {
            Self::Local => return Ok(scan(dir, expanded, untracked, worktrees)),
            Self::Remote(client) => client,
        };
        match client.call(Call::Scan { dir, expanded, worktrees }, SCAN_TIMEOUT)? {
            (Reply::Scan(remote), _) => Ok(Scan {
                dir: remote.dir,
                root: remote.root,
                git: remote.git,
                listings: remote.listings,
                watch_git_dirs: Vec::new(),
                untracked: UntrackedCache::default(),
                cost: Duration::from_millis(remote.cost_ms),
            }),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// 在项目所在的机器上 `runode_files::read_limited`，读不了时交回原因。会阻塞。
    pub(crate) fn read_limited(&self, path: &Path, max: u64, whole: bool) -> Result<Limited, String> {
        let client = match self {
            Self::Local => return runode_files::read_limited(path, max, whole).map_err(|err| err.to_string()),
            Self::Remote(client) => client,
        };
        match client.call(Call::Read { path: path.to_path_buf(), max, whole }, READ_TIMEOUT)? {
            (Reply::Read { cut }, bytes) => Ok(Limited { bytes, cut }),
            (other, _) => Err(unexpected(&other)),
        }
    }

    pub(crate) fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    /// 文件现在的版本戳（`runode_files::stamp`）；远端问不到时为空。会阻塞。
    pub(crate) fn stamp(&self, path: &Path) -> Option<Stamp> {
        let client = match self {
            Self::Local => return runode_files::stamp(path),
            Self::Remote(client) => client,
        };
        match client.call(Call::Stamp { path: path.to_path_buf() }, READ_TIMEOUT) {
            Ok((Reply::Stamp(stamp), _)) => stamp,
            _ => None,
        }
    }

    /// 把 `bytes` 写成 `path`（`runode_paths::replace_file`），交回写完后的版本戳。会阻塞。
    pub(crate) fn replace_file(&self, path: &Path, bytes: &[u8]) -> std::io::Result<Option<Stamp>> {
        let client = match self {
            Self::Local => return runode_paths::replace_file(path, bytes).map(|()| runode_files::stamp(path)),
            Self::Remote(client) => client,
        };
        match client.call_with(Call::Write { path: path.to_path_buf() }, bytes, READ_TIMEOUT) {
            Ok((Reply::Stamp(stamp), _)) => Ok(stamp),
            Ok((other, _)) => Err(std::io::Error::other(unexpected(&other))),
            Err(err) => Err(std::io::Error::other(err)),
        }
    }

    /// SSH 主机的名字，本机为空。
    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Remote(client) => Some(client.name()),
        }
    }

    /// 这台机器上的仓库 `repo`。
    pub(crate) fn repo(&self, repo: runode_git::Repo) -> ProjectRepo {
        ProjectRepo::new(self.clone(), repo)
    }

    /// 在项目所在的机器上读文件树里 `root` 下的这些目录（`file_tree::list_dirs`）；远端按那边最近一次读 `root` 时的 git 状态
    /// 判断忽略，`git` 用不上。读不了时什么都不交回，那些目录等下次再读。会阻塞。
    pub(crate) fn list_dirs(&self, root: &Path, dirs: Vec<PathBuf>, git: Option<&runode_git::Repos>) -> Listings {
        let client = match self {
            Self::Local => return crate::file_tree::list_dirs(dirs, git),
            Self::Remote(client) => client,
        };
        match client.call(Call::ListDirs { root: root.to_path_buf(), dirs }, READ_TIMEOUT) {
            Ok((Reply::Listed(listings), _)) => listings,
            Ok((other, _)) => {
                tracing::warn!("{}", unexpected(&other));
                Listings::new()
            }
            Err(err) => {
                tracing::warn!("cannot list remote directories: {err}");
                Listings::new()
            }
        }
    }
}

fn unexpected(reply: &Reply) -> String {
    format!("unexpected answer: {reply:?}")
}
