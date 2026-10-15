//! 项目目录的 git：读工作区相对 HEAD 的逐行改动、每个文件的状态、分支和 stash，供右侧的
//! 文件树、预览栏和 Git 面板使用；主仓库里的子模块和嵌套仓库各读一份，见 `snapshot_repos`。
//! 暂存、按块暂存、提交、切分支、stash 和同步远端这些写操作，以及读提交历史和图表（`graph`），
//! 挂在 `Repo` 上；预览栏看一个文件的整篇 diff 见 `view`，一对改过的行里改了哪些字见 `changed_words`，
//! 文件树按名字和内容找文件见 `search`。
//! 一律调 `git` 命令行，不直接读写 git 目录里的对象。

mod branch;
mod graph;
mod info;
mod ops;
mod parse;
mod patch;
mod repos;
mod search;
mod snapshot;
mod view;
mod words;

pub use branch::{Branch, valid_branch_name};
pub use graph::{
    Commit, CommitRef, GraphLine, GraphRow, Half, History, LaneColor, RefKind, graph_layout, refs_changed,
};
pub use info::{Operation, RepoInfo, Stash};
pub use ops::{CommitOptions, GitError, Repo, Result};
pub use patch::{HunkAction, hunk_actionable};
pub use repos::{RepoKind, Repos, snapshot_repos};
pub use search::{GrepMatch, GrepQuery, grep, list_files};
pub use snapshot::{FileDiff, FileStatus, Hunk, Line, LineKind, Section, Snapshot, UntrackedCache, snapshot};
pub use view::{DiffRow, DiffSide, DiffView, ReadOptions, TooLarge, first_lines, merge_rows};
pub use words::changed_words;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// 在 `dir` 里跑 git 的命令，各处共用的开头都在这里：标准输入是空的，路径里的中文等非 ASCII
/// 字符原样输出、不转成八进制转义，关掉 `core.fsmonitor`：仓库自己的配置能把它设成任意命令，
/// 读状态时 git 就会执行它，进一个别人给的目录不能因此跑了里面的脚本。命令行上的 `-c` 压过
/// 仓库的配置。
pub(crate) fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(["-c", "core.quotePath=false", "-c", "core.fsmonitor=false"]).stdin(Stdio::null());
    command
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = git_command(dir).args(args).stderr(Stdio::null()).env("GIT_OPTIONAL_LOCKS", "0").output().ok()?;
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

/// `dir` 所在仓库当前的分支，还没有提交的新分支也算；HEAD 分离时是提交的短哈希，不在仓库里时为空。
pub fn current_branch(dir: &Path) -> Option<String> {
    let out =
        git(dir, &["symbolic-ref", "--short", "-q", "HEAD"]).or_else(|| git(dir, &["rev-parse", "--short", "HEAD"]))?;
    Some(String::from_utf8_lossy(&out).trim().to_owned()).filter(|name| !name.is_empty())
}

/// `dir` 所在仓库 `origin` 远端的地址；没有这个远端时为空。
pub fn origin_url(dir: &Path) -> Option<String> {
    let out = git(dir, &["remote", "get-url", "origin"])?;
    Some(String::from_utf8_lossy(&out).trim().to_owned()).filter(|url| !url.is_empty())
}

/// git 给的仓库根解析过符号链接；按 `dir` 的写法换回来，界面拿 `dir` 下的路径和它比前缀
/// 才对得上。`dir` 本身不在仓库根下面（仓库内部的符号链接）时保持原样。按层数往上数到的那一级
/// 不一定就是仓库根：`dir` 经过仓库里指向别的深度的符号链接、或者是 `sub`、`.` 这样的相对写法
/// 数到了头（空路径），都核对它解析出来是不是仓库根，不是就用 git 给的。
fn local_root(dir: &Path, root: PathBuf) -> PathBuf {
    let (Ok(real_dir), Ok(real_root)) = (fs::canonicalize(dir), fs::canonicalize(&root)) else {
        return root;
    };
    let Ok(rel) = real_dir.strip_prefix(&real_root) else {
        return root;
    };
    dir.ancestors()
        .nth(rel.components().count())
        .filter(|local| !local.as_os_str().is_empty() && fs::canonicalize(local).is_ok_and(|real| real == real_root))
        .map_or(root, Path::to_path_buf)
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

    #[test]
    fn falls_back_to_the_git_root_when_the_spelling_does_not_lead_there() {
        let base = std::env::temp_dir().join(format!("runode-git-root-inner-{}", std::process::id()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("deep/sub")).unwrap();
        // 仓库里的符号链接指向更深的一层：往上数两级数到了仓库外面，不能当仓库根。
        let link = repo.join("link");
        std::os::unix::fs::symlink(repo.join("deep/sub"), &link).unwrap();
        assert_eq!(local_root(&link, repo.clone()), repo);
        assert_eq!(local_root(&repo.join("deep/sub"), repo.clone()), repo);
        // 相对的写法数到头是空路径（测试的当前目录是这个 crate 的目录，仓库根当它的上一级），也用 git 给的。
        let parent = std::env::current_dir().unwrap().parent().unwrap().to_path_buf();
        assert_eq!(local_root(Path::new("."), parent.clone()), parent);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
