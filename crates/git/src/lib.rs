//! 项目目录的 git：读工作区相对 HEAD 的逐行改动、每个文件的状态、分支和 stash，供右侧的
//! 文件树、预览栏和 Git 面板使用；主仓库里的子模块和嵌套仓库各读一份，见 `snapshot_repos`。
//! 暂存、按块暂存、提交、切分支、stash 和同步远端这些写操作，以及读提交历史和图表（`graph`），
//! 挂在 `Repo` 上；预览栏看一个文件的整篇 diff 见 `view`。一律调 `git` 命令行，不直接读写 git
//! 目录里的对象。

mod branch;
mod graph;
mod info;
mod ops;
mod parse;
mod patch;
mod repos;
mod snapshot;
mod view;

pub use branch::{Branch, valid_branch_name};
pub use graph::{Commit, CommitRef, GraphLine, GraphRow, Half, History, RefKind, graph_layout, refs_changed};
pub use info::{Operation, RepoInfo, Stash};
pub use ops::{CommitOptions, GitError, Repo, Result};
pub use patch::{HunkAction, hunk_actionable};
pub use repos::{RepoKind, Repos, snapshot_repos};
pub use snapshot::{FileDiff, FileStatus, Hunk, Line, LineKind, Section, Snapshot, UntrackedCache, snapshot};
pub use view::{DiffRow, DiffSide, DiffView, merge_rows};

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub(crate) fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        // 路径里的中文等非 ASCII 字符原样输出，不转成八进制转义。
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// `dir` 所在仓库的根目录（按 `dir` 的写法）和 git 目录。
pub(crate) fn find_repo(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let paths = git(dir, &["rev-parse", "--show-toplevel", "--absolute-git-dir"])?;
    let paths = String::from_utf8_lossy(&paths);
    let mut paths = paths.lines();
    let root = local_root(dir, PathBuf::from(paths.next()?));
    let git_dir = PathBuf::from(paths.next()?);
    Some((root, git_dir))
}

/// `dir` 所在仓库的根目录，按 `dir` 的写法；不在仓库里时为空。
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    find_repo(dir).map(|(root, _)| root)
}

/// `dir` 在不在 git 仓库里；比读整份状态便宜，只问一次 git。
pub fn in_repo(dir: &Path) -> bool {
    find_repo(dir).is_some()
}

/// git 给的仓库根解析过符号链接；按 `dir` 的写法换回来，界面拿 `dir` 下的路径和它比前缀
/// 才对得上。`dir` 本身不在仓库根下面（仓库内部的符号链接）时保持原样。
fn local_root(dir: &Path, root: PathBuf) -> PathBuf {
    let (Ok(real_dir), Ok(real_root)) = (fs::canonicalize(dir), fs::canonicalize(&root)) else {
        return root;
    };
    let Ok(rel) = real_dir.strip_prefix(&real_root) else {
        return root;
    };
    dir.ancestors().nth(rel.components().count()).map_or(root, Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_symlinked_spelling_of_the_root() {
        let base = std::env::temp_dir().join(format!("runode-git-root-{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(local_root(&link.join("sub"), real.clone()), link);
        assert_eq!(local_root(&link, real.clone()), link);
        assert_eq!(local_root(&real.join("sub"), real.clone()), real);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
