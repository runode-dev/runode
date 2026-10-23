//! 项目目录的 git：读工作区相对 HEAD 的逐行改动、每个文件的状态、分支和 stash；主仓库里的子模块和嵌套仓库各读一份，见 `snapshot_repos`。
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
    borrow::Cow,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// 在 `dir` 里跑 git 的命令，各处共用的开头：标准输入是空的，非 ASCII 路径原样输出。
/// 关掉 `core.fsmonitor` 和 `log.showSignature`：仓库自己的配置能把前者、以及校验签名用的
/// `gpg.*.program` 设成任意命令，进一个别人给的目录不能因此跑了里面的脚本。
/// 命令行上的 `-c` 压过仓库的配置，也传给 git 自己拉起的 git。其余会执行命令的配置：
/// `diff.external` 和 textconv 由 `diff_args` 关掉，过滤器见 `filter_overrides`。
pub(crate) fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    hide_window(&mut command);
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false", "-c", "core.fsmonitor=false", "-c", "log.showSignature=false"])
        .stdin(Stdio::null());
    command
}

/// git 输出里的一段路径。Unix 上按原样的字节，不是 UTF-8 的文件名也找得到；Windows 上的路径不是
/// 字节串，git 按 UTF-8 输出，解不开的字换成替换符。
#[cfg(unix)]
pub(crate) fn os_str(bytes: &[u8]) -> Cow<'_, OsStr> {
    Cow::Borrowed(std::os::unix::ffi::OsStrExt::from_bytes(bytes))
}

#[cfg(not(unix))]
pub(crate) fn os_str(bytes: &[u8]) -> Cow<'_, OsStr> {
    match String::from_utf8_lossy(bytes) {
        Cow::Borrowed(text) => Cow::Borrowed(OsStr::new(text)),
        Cow::Owned(text) => Cow::Owned(text.into()),
    }
}

/// Windows 上拉起控制台程序时不开窗口（`CREATE_NO_WINDOW`）：app 和宿主都没有控制台，不然每跑一次
/// git 就闪一个黑窗。别的系统上什么都不做。
pub(crate) fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, 0x0800_0000);
    #[cfg(not(windows))]
    let _ = command;
}

pub(crate) fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Option<Vec<u8>> {
    git_with(dir, &[], args)
}

/// 同 `git`，另外设上 `env` 里的环境变量（`filter_overrides` 给的）。
pub(crate) fn git_with<S: AsRef<OsStr>>(dir: &Path, env: &[(String, String)], args: &[S]) -> Option<Vec<u8>> {
    let mut command = git_command(dir);
    command.args(args).envs(env.iter().cloned()).stderr(Stdio::null()).env("GIT_OPTIONAL_LOCKS", "0");
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

/// 覆盖掉 `dir` 所在仓库自己配置的过滤器的环境变量，设给只读的 `status`、`diff`（经 `git_with`）：
/// `.gitattributes` 指定的过滤器在 git 比对工作区文件时执行，仓库的配置能把它设成任意命令，
/// 后台自动读状态不能因此跑了别人目录里的脚本；暂存、提交这些写操作要靠过滤器，不覆盖。
/// 只管仓库级的配置，用户全局装的和 `LFS_COMMANDS` 照旧；仓库级配了 `lfs.extension.*` 时，
/// 所有过滤器一样覆盖掉（git-lfs clean 会执行它配的命令）。`required` 也要关掉，免得 git 报错。
/// 经 `GIT_CONFIG_KEY_n`、`GIT_CONFIG_VALUE_n` 给而不用 `-c`：`-c` 在第一个 `=` 处切开键和值，
/// 名字里带 `=` 的过滤器（`.gitattributes` 能写 `filter=a=b`）就覆盖不到。
pub(crate) fn filter_overrides(dir: &Path) -> Vec<(String, String)> {
    let Some(out) = git(
        dir,
        &["config", "-z", "--show-scope", "--get-regexp", r"^(filter\..*\.(clean|process)|lfs\.extension\..*)$"],
    ) else {
        return Vec::new();
    };
    let out = String::from_utf8_lossy(&out);
    let names = local_filter_names(&out);
    if names.is_empty() {
        return Vec::new();
    }
    let items = names.iter().flat_map(|name| {
        [("clean", ""), ("process", ""), ("required", "false")]
            .map(|(key, value)| (format!("filter.{name}.{key}"), value))
    });
    // 接在继承来的那组后面（比如容器里设的 `safe.directory`），从 0 编会把它们冲掉。
    let base: usize = std::env::var("GIT_CONFIG_COUNT").ok().and_then(|count| count.parse().ok()).unwrap_or(0);
    let mut env = vec![("GIT_CONFIG_COUNT".to_owned(), (base + names.len() * 3).to_string())];
    for (n, (key, value)) in (base..).zip(items) {
        env.push((format!("GIT_CONFIG_KEY_{n}"), key));
        env.push((format!("GIT_CONFIG_VALUE_{n}"), value.to_owned()));
    }
    env
}

/// `git lfs install` 写的过滤器命令，跑的是用户自己装的 git-lfs，不是仓库给的脚本。
const LFS_COMMANDS: [&str; 2] = ["git-lfs clean -- %f", "git-lfs filter-process"];

/// `git config -z --show-scope --get-regexp` 的输出（每条是 `范围\0键\n值\0`）里仓库级（`local`、
/// `worktree`）的过滤器名，去重，大小写照原样；命令是 `LFS_COMMANDS` 之一的那条不算，
/// 除非仓库级配了 `lfs.extension.*`，那时不论哪一级、什么命令的过滤器都算上。
fn local_filter_names(out: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut fields = out.split('\0');
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        entries.push((matches!(scope, "local" | "worktree"), key, value));
    }
    let local_extension = entries.iter().any(|&(local, key, _)| local && key.starts_with("lfs.extension."));
    let mut names = Vec::new();
    for (local, key, value) in entries {
        let counts = local_extension || (local && !LFS_COMMANDS.contains(&value));
        if !counts {
            continue;
        }
        let name = key.strip_prefix("filter.").and_then(|rest| rest.rsplit_once('.')).map(|(name, _)| name);
        if let Some(name) = name.filter(|name| !names.contains(name)) {
            names.push(name);
        }
    }
    names
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
    fn picks_filters_from_repository_config_only() {
        let out = "local\0filter.evil.clean\nsh -c x\0global\0filter.lfs.clean\ngit-lfs clean\0\
                   worktree\0filter.My Odd.x.process\ncat\0system\0filter.sys.process\ncat\0\
                   local\0filter.evil.process\ncat\0local\0filter.Up.clean\n\0\
                   local\0filter.lfs.clean\ngit-lfs clean -- %f\0local\0filter.lfs.process\ngit-lfs filter-process\0\
                   local\0filter.fake.process\ngit-lfs filter-process; sh x\0";
        assert_eq!(local_filter_names(out), ["evil", "My Odd.x", "Up", "fake"]);
        assert!(local_filter_names("").is_empty());
    }

    #[test]
    fn repository_lfs_extension_disables_lfs_filters_too() {
        let lfs = "global\0filter.lfs.clean\ngit-lfs clean -- %f\0global\0filter.lfs.process\ngit-lfs filter-process\0\
                   local\0filter.mine.clean\ngit-lfs clean -- %f\0";
        assert!(local_filter_names(lfs).is_empty());
        // 全局配的扩展是用户自己的，不算。
        assert!(local_filter_names(&format!("{lfs}global\0lfs.extension.x.clean\nfoo\0")).is_empty());
        let evil = format!("{lfs}local\0lfs.extension.x.clean\nsh -c evil\0");
        assert_eq!(local_filter_names(&evil), ["lfs", "mine"]);
        let evil = format!("{lfs}worktree\0lfs.extension.x.priority\n0\0");
        assert_eq!(local_filter_names(&evil), ["lfs", "mine"]);
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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
