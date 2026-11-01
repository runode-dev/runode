//! 项目的文件和 git 在哪台机器上读写：本机的直接调；SSH 主机上的交给那台机器上同一个构建的
//! `runode --ssh-project`（`serve`），经 ssh 收发（`client`），线上格式在 `wire`。各面板只经 `ProjectHost`
//! 读写，不管项目在哪台机器上。

mod client;
mod edit;
mod repo;
mod scan;
pub(crate) mod serve;
#[cfg(test)]
mod tests;
mod wire;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use runode_files::{Changes, Limited, Listings, Stamp};
use runode_git::UntrackedCache;

pub(crate) use repo::ProjectRepo;
pub(crate) use scan::{Scan, scan};
use wire::{Call, Reply};

use crate::host_client::remote::HostId;

/// 读一遍远端的项目最多等这么久：大仓库第一次读 git 状态要一阵。
const SCAN_TIMEOUT: Duration = Duration::from_secs(120);
/// 读远端的文件最多等这么久。
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// 问远端的路径在不在、在监听哪些端口最多等这么久：终端里按着 ⌘ 悬停、状态栏轮询时在等它。
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

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

/// 发给远端（Linux、macOS）的路径：Windows 上 `Path::join` 拼进去的 `\` 换成 `/`，那边只认 `/`。带盘符或
/// `\\?\` 前缀的是 Windows 自己的路径，和别的系统上的一样原样交回。
pub(crate) fn remote_path(path: &Path) -> PathBuf {
    let windows_own = matches!(path.components().next(), Some(std::path::Component::Prefix(_)));
    if cfg!(windows) && !windows_own {
        PathBuf::from(path.to_string_lossy().replace('\\', "/"))
    } else {
        path.to_path_buf()
    }
}

/// 忘掉 SSH 主机 `name` 上读写项目的那条连接，再用这台主机时从头起。
pub(crate) fn forget(name: &str) {
    client::Client::forget(name);
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
                watch_git_dirs: remote.watch_git_dirs,
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

    /// 这些路径在不在、是不是目录，按 `paths` 的顺序：不在（远端问不到）时为空，在时是不是目录。会阻塞。
    pub(crate) fn kinds(&self, paths: Vec<PathBuf>) -> Vec<Option<bool>> {
        let client = match self {
            Self::Local => return paths.iter().map(|path| serve::kind(path)).collect(),
            Self::Remote(client) => client,
        };
        let count = paths.len();
        match client.call(Call::Kinds { paths }, QUERY_TIMEOUT) {
            Ok((Reply::Kinds(kinds), _)) if kinds.len() == count => kinds,
            _ => vec![None; count],
        }
    }

    /// `dir` 所在仓库的当前分支（HEAD 分离时是短哈希）和 `origin` 的地址，不在仓库里时都为空；远端问不到
    /// 时为 `Err`。会阻塞。
    pub(crate) fn repo_badge(&self, dir: &Path) -> Result<(Option<String>, Option<String>), String> {
        let client = match self {
            Self::Local => return Ok(serve::repo_badge(dir)),
            Self::Remote(client) => client,
        };
        match client.call(Call::RepoBadge { dir: dir.to_path_buf() }, QUERY_TIMEOUT)? {
            (Reply::RepoBadge { branch, origin }, _) => Ok((branch, origin)),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// 这台机器上在监听的 TCP 端口。会阻塞。
    pub(crate) fn listening_ports(&self) -> Result<Vec<crate::system_probe::Port>, String> {
        let client = match self {
            Self::Local => return Ok(crate::system_probe::listening_ports()),
            Self::Remote(client) => client,
        };
        match client.call(Call::Ports, QUERY_TIMEOUT)? {
            (Reply::Ports(ports), _) => Ok(ports),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// 远端项目的监听（`Call::Watch`）：等编号 `token` 的那个监听攒下改动交回。第一次问时开始听、马上交回
    /// `None`，连接断了又连上后再问也是这样，期间的改动听不到。`cancel` 置位时不再等。本机的项目直接用
    /// `runode_files::watch`，这里只办远端的。会阻塞。
    pub(crate) fn wait_for_changes(
        &self,
        token: u64,
        dirs: Vec<PathBuf>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<Changes>, String> {
        let Self::Remote(client) = self else {
            return Err("local projects are watched directly".into());
        };
        match client.call_cancellable(Call::Watch { token, dirs }, serve::WATCH_HOLD + QUERY_TIMEOUT, cancel)? {
            (Reply::Changed { started: true, .. }, _) => Ok(None),
            (Reply::Changed { started: false, changes }, _) => Ok(Some(changes)),
            (other, _) => Err(unexpected(&other)),
        }
    }

    /// 把 `bytes` 写成 `path`（`runode_paths::replace_file`），交回写完后的版本戳。会阻塞。
    pub(crate) fn replace_file(&self, path: &Path, bytes: &[u8]) -> std::io::Result<Option<Stamp>> {
        let client = match self {
            Self::Local => return runode_paths::replace_file(path, bytes).map(|()| runode_files::stamp(path)),
            Self::Remote(client) => client,
        };
        match client.call_with(Call::Write { path: path.to_path_buf() }, bytes, READ_TIMEOUT, None) {
            Ok((Reply::Stamp(stamp), _)) => Ok(stamp),
            Ok((other, _)) => Err(std::io::Error::other(unexpected(&other))),
            Err(err) => Err(std::io::Error::other(err)),
        }
    }

    /// 列 `root` 下按名称找的文件，相对 `root`：在仓库里时是 git 列的，不在时走一遍目录。远端读不到时为空。
    /// 会阻塞。
    pub(crate) fn list_files(&self, root: &Path) -> Vec<PathBuf> {
        let client = match self {
            Self::Local => return runode_git::list_files(root).unwrap_or_else(|| runode_files::walk_files(root)),
            Self::Remote(client) => client,
        };
        match client.call(Call::ListFiles { root: root.to_path_buf() }, SCAN_TIMEOUT) {
            Ok((Reply::Files(files), _)) => files,
            _ => Vec::new(),
        }
    }

    /// 在 `root` 下按内容找（`runode_git::grep`），出错时是 git 的报错；`cancel` 置位时停下，远端的也一起停。会阻塞。
    pub(crate) fn grep(
        &self,
        root: &Path,
        query: &runode_git::GrepQuery,
        limit: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<runode_git::GrepMatch>, String> {
        let client = match self {
            Self::Local => return runode_git::grep(root, query, limit, cancel).map_err(|err| err.message),
            Self::Remote(client) => client,
        };
        let call = Call::Grep {
            root: root.to_path_buf(),
            pattern: query.pattern.to_owned(),
            ignore_case: query.ignore_case,
            whole_word: query.whole_word,
            regex: query.regex,
            pathspecs: query.pathspecs.to_vec(),
            limit,
        };
        match client.call_cancellable(call, SCAN_TIMEOUT, cancel)? {
            (Reply::Found(found), _) => Ok(found),
            (other, _) => Err(unexpected(&other)),
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
