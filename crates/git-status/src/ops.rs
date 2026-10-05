//! 写仓库的操作：暂存、丢弃、提交、同步远端和 stash。分支见 `branch`，按块暂存见 `patch`。

use std::{
    ffi::OsStr,
    fmt, fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{FileDiff, FileStatus, git};

/// 能在后台线程里用的仓库句柄，写操作都挂在它上面；由 `Snapshot::repo` 取得。git 命令
/// 可能要跑好几秒（推送、拉取），界面线程别直接调。
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Repo {
    /// 顶层仓库的根目录，和 `Snapshot::root` 一样。
    pub root: PathBuf,
}

/// git 命令失败：`message` 是 git 自己的报错，去掉了 `hint:` 开头的提示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitError {
    pub message: String,
}

impl GitError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GitError {}

/// 写操作的结果，默认什么也不带回来。
pub type Result<T = (), E = GitError> = std::result::Result<T, E>;

/// `Repo::commit` 的选项。
#[derive(Clone, Copy, Debug, Default)]
pub struct CommitOptions {
    /// 改写上一次提交；说明为空时沿用原来的说明。
    pub amend: bool,
    /// 提交前先暂存所有改动，含未跟踪和删掉的文件。
    pub stage_all: bool,
}

/// 跑一条会写仓库的 git 命令，返回标准输出。和只读的 `git` 不同：不设 `GIT_OPTIONAL_LOCKS`，
/// 要拿的锁照常拿；`GIT_TERMINAL_PROMPT=0` 让要密码的远端直接报错，不卡着等输入；`input`
/// 给了就写进标准输入，否则标准输入是空的。失败时的错误是 git 的标准错误，没有就用标准输出。
pub(crate) fn run<S: AsRef<OsStr>>(dir: &Path, args: impl IntoIterator<Item = S>, input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        // 路径里的中文等非 ASCII 字符原样输出，不转成八进制转义。
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // 新版 git 不再给「hint:」那些建议；本地化后的提示不以「hint:」开头，滤不掉。
        .env("GIT_ADVICE", "0")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| GitError::new(format!("没能运行 git：{err}")))?;
    let stdin = child.stdin.take();
    // 一边写输入一边读输出，输入输出都大时谁也不会等着谁。
    let output = std::thread::scope(|scope| {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            scope.spawn(move || {
                // git 不读完就退出时写会失败，以它的退出状态为准。
                let _ = stdin.write_all(input);
            });
        }
        child.wait_with_output()
    })
    .map_err(|err| GitError::new(format!("没能运行 git：{err}")))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message: Vec<_> = stderr.lines().filter(|line| !line.starts_with("hint:")).collect();
    let message = message.join("\n").trim().to_owned();
    let message = if message.is_empty() { String::from_utf8_lossy(&output.stdout).trim().to_owned() } else { message };
    if message.is_empty() {
        return Err(GitError::new(format!("git 失败了（{}）", output.status)));
    }
    Err(GitError::new(message))
}

/// 未跟踪的嵌套仓库，相对 `dir`，不带结尾的 `/`。git 把它们当成一个未跟踪的目录，不往里看。
fn nested_repos(dir: &Path) -> Vec<PathBuf> {
    let output = git(dir, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    output
        .split(|b| *b == 0)
        .filter(|entry| entry.ends_with(b"/"))
        .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).trim_end_matches('/')))
        .filter(|path| dir.join(path).join(".git").exists())
        .collect()
}

/// `git add -A` 整个仓库，但跳过里面嵌套的仓库：不然 git 会把它当子模块那样记一个提交号。
fn add_all(dir: &Path) -> Result {
    let mut args = vec!["add".to_owned(), "-A".to_owned(), "--".to_owned(), ".".to_owned()];
    args.extend(nested_repos(dir).iter().map(|path| format!(":(exclude,literal){}", path.to_string_lossy())));
    run(dir, &args, None).map(drop)
}

fn has_head(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_some()
}

/// 依次做每一件事，都做一遍，返回第一个错误。
fn first_error(results: impl IntoIterator<Item = Result>) -> Result {
    let mut first = Ok(());
    for result in results {
        if first.is_ok() {
            first = result;
        }
    }
    first
}

impl Repo {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `path` 相对 `root`，落在哪个仓库里：那个仓库的根目录，以及相对它的路径。路径里
    /// 某一层目录有自己的 `.git` 就是嵌套的仓库，取最深的那个。
    pub(crate) fn locate(&self, path: &Path) -> Result<(PathBuf, PathBuf)> {
        if path.as_os_str().is_empty() || !path.components().all(|part| matches!(part, Component::Normal(_))) {
            return Err(GitError::new(format!("不是仓库里的路径：{}", path.display())));
        }
        for dir in path.ancestors().skip(1).take_while(|dir| !dir.as_os_str().is_empty()) {
            let repo = self.root.join(dir);
            if repo.join(".git").exists() {
                let rel = path.strip_prefix(dir).unwrap_or(path).to_path_buf();
                return Ok((repo, rel));
            }
        }
        Ok((self.root.clone(), path.to_path_buf()))
    }

    /// 按所在的仓库把路径分组，组的顺序按第一次出现。
    fn group<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> Result<Vec<(PathBuf, Vec<PathBuf>)>> {
        let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
        for path in paths {
            let (repo, rel) = self.locate(path)?;
            match groups.iter_mut().find(|(dir, _)| *dir == repo) {
                Some((_, rels)) => rels.push(rel),
                None => groups.push((repo, vec![rel])),
            }
        }
        Ok(groups)
    }

    /// 顶层仓库以及所有嵌套在里面的仓库，顶层的在前。
    fn all_repos(&self) -> Vec<PathBuf> {
        let mut repos = vec![self.root.clone()];
        let mut ix = 0;
        while ix < repos.len() {
            let nested: Vec<_> = nested_repos(&repos[ix]).into_iter().map(|path| repos[ix].join(path)).collect();
            repos.extend(nested);
            ix += 1;
        }
        repos
    }

    /// 在 `dir` 里对 `paths` 跑 `args`；路径按字面认，不当通配符。
    fn run_paths(dir: &Path, args: &[&str], paths: &[PathBuf]) -> Result {
        let mut full: Vec<&OsStr> = vec![OsStr::new("--literal-pathspecs")];
        full.extend(args.iter().map(OsStr::new));
        full.push(OsStr::new("--"));
        full.extend(paths.iter().map(|path| path.as_os_str()));
        run(dir, full, None).map(drop)
    }

    /// 暂存这些文件的改动，含删掉的文件。路径相对 `root`，和 `FileDiff::path` 一样；落在
    /// 嵌套仓库里的转到那个仓库里暂存。
    pub fn stage(&self, paths: &[PathBuf]) -> Result {
        let groups = self.group(paths.iter().map(PathBuf::as_path))?;
        first_error(groups.iter().map(|(dir, rels)| Self::run_paths(dir, &["add", "-A"], rels)))
    }

    /// 把这些文件从暂存区撤回到工作区，工作区里的内容不动。还没有提交的仓库里就是不再
    /// 跟踪它们。暂存了的改名要连同 `FileDiff::old_path` 一起传，不然只撤回新路径那一半。
    pub fn unstage(&self, paths: &[PathBuf]) -> Result {
        let groups = self.group(paths.iter().map(PathBuf::as_path))?;
        first_error(groups.iter().map(|(dir, rels)| {
            if has_head(dir) {
                Self::run_paths(dir, &["restore", "--staged"], rels)
            } else {
                Self::run_paths(dir, &["rm", "--cached", "-r", "-f", "-q"], rels)
            }
        }))
    }

    /// 丢掉工作区里还没暂存的改动，`files` 来自未暂存段。未跟踪的文件从磁盘上删掉（只删
    /// 文件不删目录，删完把因此变空的上层目录也删掉）；`git add -N` 记下的新文件同样删掉并
    /// 移出暂存区；其余的按暂存区里的样子恢复。已经暂存的改动不受影响；冲突的文件暂存区
    /// 里没有单一的版本，报 git 的错。一个文件失败不影响其余的，返回第一个错误。
    pub fn discard(&self, files: &[FileDiff]) -> Result {
        let mut results = Vec::new();
        let mut restore = Vec::new();
        let mut remove = Vec::new();
        for file in files {
            match file.status {
                FileStatus::Untracked => results.push(self.delete_untracked(&file.path)),
                // 未暂存段里的新增和改名都是 `git add -N` 的文件，暂存区里只有个占位。
                FileStatus::Added | FileStatus::Renamed => remove.push(file.path.as_path()),
                _ => restore.push(file.path.as_path()),
            }
            if let Some(old) = &file.old_path {
                restore.push(old);
            }
        }
        for (dir, rels) in self.group(remove)? {
            results.push(Self::run_paths(&dir, &["rm", "-f", "-q"], &rels));
        }
        for (dir, rels) in self.group(restore)? {
            results.push(Self::run_paths(&dir, &["restore", "--worktree"], &rels));
        }
        first_error(results)
    }

    fn delete_untracked(&self, path: &Path) -> Result {
        // 只是为了拒绝 `..` 和绝对路径；删文件不用 git。
        self.locate(path)?;
        let full = self.root.join(path);
        let meta = fs::symlink_metadata(&full).map_err(|err| GitError::new(format!("{}：{err}", path.display())))?;
        if meta.is_dir() {
            return Ok(());
        }
        fs::remove_file(&full).map_err(|err| GitError::new(format!("没能删掉 {}：{err}", path.display())))?;
        for dir in path.ancestors().skip(1).take_while(|dir| !dir.as_os_str().is_empty()) {
            if fs::remove_dir(self.root.join(dir)).is_err() {
                break;
            }
        }
        Ok(())
    }

    /// 暂存所有改动，含未跟踪和删掉的文件，嵌套的仓库各自暂存自己的。
    pub fn stage_all(&self) -> Result {
        first_error(self.all_repos().iter().map(|dir| add_all(dir)))
    }

    /// 撤回所有暂存的改动，嵌套的仓库也一样。合并做到一半时不会因此放弃合并。
    pub fn unstage_all(&self) -> Result {
        first_error(self.all_repos().iter().map(|dir| {
            if has_head(dir) {
                Self::run_paths(dir, &["restore", "--staged"], &[".".into()])
            } else {
                Self::run_paths(dir, &["rm", "--cached", "-r", "-f", "-q", "--ignore-unmatch"], &[".".into()])
            }
        }))
    }

    /// 提交顶层仓库暂存的改动；说明经标准输入交给 git，不经命令行。`amend` 且说明为空时
    /// 沿用上一次提交的说明。没有可提交的改动、说明为空时报 git 的错。
    pub fn commit(&self, message: &str, options: CommitOptions) -> Result {
        if options.stage_all {
            add_all(&self.root)?;
        }
        let mut args = vec!["commit"];
        if options.amend {
            args.push("--amend");
        }
        if options.amend && message.trim().is_empty() {
            args.push("--no-edit");
            return run(&self.root, args, None).map(drop);
        }
        args.extend(["-F", "-"]);
        run(&self.root, args, Some(message.as_bytes())).map(drop)
    }

    /// 顶层仓库最近一次提交的完整说明，给 amend 时填进输入框；还没有提交时为空。
    pub fn last_commit_message(&self) -> Option<String> {
        let message = git(&self.root, &["log", "-1", "--format=%B", "HEAD"])?;
        let message = String::from_utf8_lossy(&message).trim_end().to_owned();
        (!message.is_empty()).then_some(message)
    }

    /// 撤销顶层仓库最近一次提交，改动留在暂存区里；返回被撤销的提交的完整说明，好填回
    /// 输入框。只有一个提交时删掉分支，仓库回到还没有提交的样子。
    pub fn undo_last_commit(&self) -> Result<String> {
        let message = run(&self.root, ["log", "-1", "--format=%B", "HEAD"], None)?;
        let message = String::from_utf8_lossy(&message).trim_end().to_owned();
        if git(&self.root, &["rev-parse", "--verify", "--quiet", "HEAD~1^{commit}"]).is_some() {
            run(&self.root, ["reset", "--soft", "HEAD~1"], None)?;
        } else {
            run(&self.root, ["update-ref", "-d", "HEAD"], None)?;
        }
        Ok(message)
    }

    /// 从默认的远端取最新的提交，不动工作区。
    pub fn fetch(&self) -> Result {
        run(&self.root, ["fetch"], None).map(drop)
    }

    /// 从上游拉取并合进当前分支，合并方式按用户的 git 配置。
    pub fn pull(&self) -> Result {
        run(&self.root, ["pull"], None).map(drop)
    }

    /// 推送当前分支。还没设上游时推到同名的远端分支并设成上游：只有一个远端就用它，
    /// 否则用 `origin`，再没有就用第一个。
    pub fn push(&self) -> Result {
        if self.has_upstream() {
            return run(&self.root, ["push"], None).map(drop);
        }
        let remotes = String::from_utf8_lossy(&run(&self.root, ["remote"], None)?).into_owned();
        let remotes: Vec<_> = remotes.lines().map(str::trim).filter(|name| !name.is_empty()).collect();
        let remote = match remotes.as_slice() {
            [] => return Err(GitError::new("还没有配置远端")),
            [only] => only,
            _ => remotes.iter().find(|name| **name == "origin").unwrap_or(&remotes[0]),
        };
        run(&self.root, ["push", "-u", remote, "HEAD"], None).map(drop)
    }

    /// 先拉取再推送；还没设上游时只推送。
    pub fn sync(&self) -> Result {
        if self.has_upstream() {
            self.pull()?;
        }
        self.push()
    }

    /// 当前分支设了上游，而且上游的远端分支还在。
    fn has_upstream(&self) -> bool {
        git(&self.root, &["rev-parse", "--verify", "--quiet", "@{upstream}"]).is_some()
    }

    /// 把顶层仓库的改动收进一个新的 stash，工作区回到 HEAD 的样子。`include_untracked`
    /// 时未跟踪的文件也收进去（被忽略的不收）。没有改动时 git 什么也不做，也不算失败。
    pub fn stash(&self, message: Option<&str>, include_untracked: bool) -> Result {
        let mut args = vec!["stash", "push"];
        if include_untracked {
            args.push("--include-untracked");
        }
        if let Some(message) = message.filter(|message| !message.trim().is_empty()) {
            args.extend(["-m", message]);
        }
        run(&self.root, args, None).map(drop)
    }

    /// 把 `stash@{index}` 的改动放回工作区，stash 留着。
    pub fn stash_apply(&self, index: usize) -> Result {
        self.stash_command("apply", index)
    }

    /// 把 `stash@{index}` 的改动放回工作区并删掉它；有冲突时 git 不删。
    pub fn stash_pop(&self, index: usize) -> Result {
        self.stash_command("pop", index)
    }

    /// 删掉 `stash@{index}`。
    pub fn stash_drop(&self, index: usize) -> Result {
        self.stash_command("drop", index)
    }

    fn stash_command(&self, command: &str, index: usize) -> Result {
        run(&self.root, ["stash", command, &format!("stash@{{{index}}}")], None).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_repo::TestRepo;
    use crate::{Section, Snapshot, UntrackedCache, snapshot};

    fn read(repo: &TestRepo) -> Snapshot {
        snapshot(repo.path(), &mut UntrackedCache::default()).unwrap()
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// 在 `repo` 里的 `wt/inner` 建一个有一次提交的嵌套仓库。
    fn nested(repo: &TestRepo) -> PathBuf {
        let inner = repo.path().join("wt/inner");
        fs::create_dir_all(&inner).unwrap();
        repo.git(&["-C", "wt/inner", "init", "-q", "-b", "main"]);
        for (key, value) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false")] {
            repo.git(&["-C", "wt/inner", "config", key, value]);
        }
        repo.write("wt/inner/a.txt", "one\n");
        repo.git(&["-C", "wt/inner", "add", "a.txt"]);
        repo.git(&["-C", "wt/inner", "commit", "-q", "-m", "init"]);
        inner
    }

    #[test]
    fn stages_and_unstages_files() {
        let repo = TestRepo::new("ops-stage");
        repo.commit_file("a.txt", "one\n", "init");
        repo.commit_file("b.txt", "b\n", "b");
        repo.write("a.txt", "two\n");
        fs::remove_file(repo.path().join("b.txt")).unwrap();
        repo.write("dir/c [1].txt", "c\n");
        let handle = read(&repo).repo();
        handle.stage(&paths(&["a.txt", "b.txt", "dir/c [1].txt"])).unwrap();
        assert_eq!(repo.status(), ["M  a.txt", "D  b.txt", "A  \"dir/c [1].txt\""]);
        handle.unstage(&paths(&["a.txt", "b.txt", "dir/c [1].txt"])).unwrap();
        assert_eq!(repo.status(), [" M a.txt", " D b.txt", "?? \"dir/c [1].txt\""]);

        handle.stage_all().unwrap();
        assert_eq!(repo.status(), ["M  a.txt", "D  b.txt", "A  \"dir/c [1].txt\""]);
        handle.unstage_all().unwrap();
        assert_eq!(repo.status(), [" M a.txt", " D b.txt", "?? \"dir/c [1].txt\""]);

        assert!(handle.stage(&paths(&["../outside.txt"])).is_err());
        assert!(handle.stage(&paths(&["missing.txt"])).is_err());
    }

    #[test]
    fn unstages_before_the_first_commit() {
        let repo = TestRepo::new("ops-unborn");
        repo.write("a.txt", "one\n");
        repo.write("b.txt", "b\n");
        let handle = read(&repo).repo();
        handle.stage(&paths(&["a.txt", "b.txt"])).unwrap();
        assert_eq!(repo.status(), ["A  a.txt", "A  b.txt"]);
        // 暂存后又改过，暂存区和工作区、HEAD 都不一样也能撤回。
        repo.write("a.txt", "two\n");
        handle.unstage(&paths(&["a.txt"])).unwrap();
        assert_eq!(repo.status(), ["A  b.txt", "?? a.txt"]);
        assert_eq!(repo.read("a.txt"), "two\n");
        handle.unstage_all().unwrap();
        assert_eq!(repo.status(), ["?? a.txt", "?? b.txt"]);
        handle.unstage_all().unwrap();
    }

    #[test]
    fn stages_inside_nested_repositories() {
        let repo = TestRepo::new("ops-nested");
        repo.commit_file("top.txt", "top\n", "init");
        let inner = nested(&repo);
        repo.write("wt/inner/a.txt", "two\n");
        repo.write("wt/inner/new.txt", "n\n");
        repo.write("top.txt", "changed\n");
        let handle = read(&repo).repo();
        handle.stage(&paths(&["wt/inner/a.txt"])).unwrap();
        let inner_status = || repo.git(&["-C", "wt/inner", "status", "--porcelain=v1", "--untracked-files=all"]);
        assert_eq!(inner_status(), "M  a.txt\n?? new.txt");
        assert_eq!(repo.status(), [" M top.txt", "?? wt/inner/"]);

        // 全部暂存不把嵌套的仓库当子模块记进顶层仓库。
        handle.stage_all().unwrap();
        assert_eq!(inner_status(), "M  a.txt\nA  new.txt");
        assert_eq!(repo.status(), ["M  top.txt", "?? wt/inner/"]);
        handle.unstage_all().unwrap();
        assert_eq!(inner_status(), " M a.txt\n?? new.txt");
        assert_eq!(repo.status(), [" M top.txt", "?? wt/inner/"]);

        handle.commit("top", CommitOptions { stage_all: true, ..Default::default() }).unwrap();
        assert_eq!(repo.git(&["ls-files"]), "top.txt");
        assert!(inner.join(".git").exists());
    }

    #[test]
    fn discards_unstaged_changes() {
        let repo = TestRepo::new("ops-discard");
        repo.commit_file("a.txt", "one\n", "init");
        repo.commit_file("b.txt", "b\n", "b");
        repo.commit_file("keep/k.txt", "k\n", "k");
        // a.txt 暂存了 two，工作区里又改成 three；b.txt 删了；还有未跟踪和 `add -N` 的文件。
        repo.write("a.txt", "two\n");
        repo.git(&["add", "a.txt"]);
        repo.write("a.txt", "three\n");
        fs::remove_file(repo.path().join("b.txt")).unwrap();
        repo.write("d/e/new.txt", "n\n");
        repo.write("keep/new.txt", "n\n");
        repo.write("ita.txt", "i\n");
        repo.git(&["add", "-N", "ita.txt"]);
        let snapshot = read(&repo);
        assert_eq!(snapshot.unstaged.len(), 5, "{:?}", snapshot.unstaged);
        snapshot.repo().discard(&snapshot.unstaged).unwrap();
        assert_eq!(repo.status(), ["M  a.txt"]);
        assert_eq!(repo.read("a.txt"), "two\n");
        assert_eq!(repo.read("b.txt"), "b\n");
        // 删掉文件后变空的目录一起删掉，原本就有别的文件的目录留着。
        assert!(!repo.path().join("d").exists());
        assert!(repo.path().join("keep/k.txt").exists());
        assert!(!repo.path().join("ita.txt").exists());
    }

    #[test]
    fn discards_inside_nested_repositories() {
        let repo = TestRepo::new("ops-discard-nested");
        repo.commit_file("top.txt", "top\n", "init");
        nested(&repo);
        repo.write("wt/inner/a.txt", "changed\n");
        repo.write("wt/inner/extra.txt", "x\n");
        let snapshot = read(&repo);
        snapshot.repo().discard(snapshot.files(Section::Unstaged)).unwrap();
        assert_eq!(repo.read("wt/inner/a.txt"), "one\n");
        assert!(!repo.path().join("wt/inner/extra.txt").exists());
        assert!(repo.path().join("wt/inner/.git").exists());
    }

    #[test]
    fn commits_and_amends() {
        let repo = TestRepo::new("ops-commit");
        let handle = read(&repo).repo();
        assert_eq!(handle.last_commit_message(), None);
        repo.write("a.txt", "one\n");
        handle.stage(&paths(&["a.txt"])).unwrap();
        let message = "first\n\nwith a body, \"quotes\" and\n-dashes";
        handle.commit(message, CommitOptions::default()).unwrap();
        assert_eq!(handle.last_commit_message().as_deref(), Some(message));

        // 没有暂存的改动时报 git 的错。
        let error = handle.commit("nothing", CommitOptions::default()).unwrap_err();
        assert!(!error.message.is_empty());
        assert!(!error.message.lines().any(|line| line.starts_with("hint:")), "{error}");

        // 说明为空的 amend 沿用原来的说明，只把新暂存的改动并进去。
        repo.write("b.txt", "b\n");
        handle.stage(&paths(&["b.txt"])).unwrap();
        handle.commit("", CommitOptions { amend: true, ..Default::default() }).unwrap();
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(repo.git(&["ls-files"]), "a.txt\nb.txt");
        assert_eq!(handle.last_commit_message().as_deref(), Some(message));

        handle.commit("renamed", CommitOptions { amend: true, ..Default::default() }).unwrap();
        assert_eq!(handle.last_commit_message().as_deref(), Some("renamed"));
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "1");

        // 先全部暂存再提交，含未跟踪的文件。
        repo.write("a.txt", "two\n");
        repo.write("c.txt", "c\n");
        handle.commit("second", CommitOptions { stage_all: true, ..Default::default() }).unwrap();
        assert!(repo.status().is_empty());
        assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "2");
    }

    #[test]
    fn undoes_the_last_commit() {
        let repo = TestRepo::new("ops-undo");
        let handle = read(&repo).repo();
        assert!(handle.undo_last_commit().is_err());
        repo.commit_file("a.txt", "one\n", "first");
        repo.commit_file("b.txt", "b\n", "second\n\nbody");
        assert_eq!(handle.undo_last_commit().unwrap(), "second\n\nbody");
        assert_eq!(repo.git(&["log", "--format=%s"]), "first");
        assert_eq!(repo.status(), ["A  b.txt"]);

        // 只剩一个提交：撤销后回到还没有提交的样子，文件都在暂存区里。
        assert_eq!(handle.undo_last_commit().unwrap(), "first");
        assert!(repo.try_git(&["rev-parse", "--verify", "--quiet", "HEAD"]).is_none());
        assert_eq!(repo.git(&["branch", "--show-current"]), "main");
        assert_eq!(repo.status(), ["A  a.txt", "A  b.txt"]);
        assert_eq!(read(&repo).staged.len(), 2);
    }

    #[test]
    fn pushes_pulls_and_syncs() {
        let remote = TestRepo::bare("ops-remote");
        let repo = TestRepo::new("ops-local");
        repo.commit_file("a.txt", "one\n", "init");
        let handle = read(&repo).repo();
        assert!(handle.push().is_err(), "还没有远端");

        // 有两个远端时选 origin；第一次推送设上游。
        let spare = TestRepo::bare("ops-spare");
        repo.git(&["remote", "add", "aaa", &spare.path().to_string_lossy()]);
        repo.git(&["remote", "add", "origin", &remote.path().to_string_lossy()]);
        handle.push().unwrap();
        assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "main@{upstream}"]), "origin/main");
        assert_eq!(remote.git(&["log", "--format=%s", "main"]), "init");
        assert!(spare.try_git(&["rev-parse", "--verify", "--quiet", "main"]).is_none());

        // 别人推了一个提交：fetch 后落后一个，pull 后拿到。
        let other = TestRepo::clone_of(&remote, "ops-other");
        other.commit_file("b.txt", "b\n", "from other");
        other.git(&["push", "-q"]);
        handle.fetch().unwrap();
        assert_eq!(read(&repo).info.behind, 1);
        handle.pull().unwrap();
        assert_eq!(repo.read("b.txt"), "b\n");

        // 两边各有新提交：sync 先合并再推上去。
        other.commit_file("c.txt", "c\n", "other again");
        other.git(&["push", "-q"]);
        repo.commit_file("d.txt", "d\n", "local");
        handle.sync().unwrap();
        let info = read(&repo).info;
        assert_eq!((info.ahead, info.behind), (0, 0));
        assert_eq!(repo.read("c.txt"), "c\n");
        other.git(&["pull", "-q"]);
        assert_eq!(other.read("d.txt"), "d\n");

        // 新分支还没有上游：sync 只推送并设上游。
        repo.git(&["switch", "-q", "-c", "feat"]);
        repo.commit_file("e.txt", "e\n", "feat");
        handle.sync().unwrap();
        assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "feat@{upstream}"]), "origin/feat");
    }

    #[test]
    fn stashes_and_restores_changes() {
        let repo = TestRepo::new("ops-stash");
        repo.commit_file("a.txt", "one\n", "init");
        let handle = read(&repo).repo();
        repo.write("a.txt", "two\n");
        handle.stash(Some("tracked only"), false).unwrap();
        assert!(repo.status().is_empty());

        // 不含未跟踪的文件时新文件留着，也就没有可收的改动。
        repo.write("new.txt", "n\n");
        handle.stash(None, false).unwrap();
        assert_eq!(repo.status(), ["?? new.txt"]);
        handle.stash(Some(""), true).unwrap();
        assert!(repo.status().is_empty());
        let stashes = read(&repo).info.stashes;
        assert_eq!(stashes.len(), 2);
        assert_eq!(stashes[1].message, "On main: tracked only");

        handle.stash_apply(1).unwrap();
        assert_eq!(repo.read("a.txt"), "two\n");
        assert_eq!(read(&repo).info.stashes.len(), 2);
        repo.git(&["checkout", "--", "a.txt"]);

        handle.stash_pop(0).unwrap();
        assert_eq!(repo.read("new.txt"), "n\n");
        assert_eq!(read(&repo).info.stashes.len(), 1);
        handle.stash_drop(0).unwrap();
        assert!(read(&repo).info.stashes.is_empty());
        assert!(handle.stash_drop(0).is_err());
    }

    #[test]
    fn stash_keeps_nested_repositories() {
        let repo = TestRepo::new("ops-stash-nested");
        repo.commit_file("top.txt", "top\n", "init");
        let inner = nested(&repo);
        repo.write("new.txt", "n\n");
        let handle = read(&repo).repo();
        handle.stash(None, true).unwrap();
        assert!(inner.join(".git").exists() && inner.join("a.txt").exists());
        assert!(!repo.path().join("new.txt").exists());
        handle.stash_pop(0).unwrap();
        assert_eq!(repo.read("new.txt"), "n\n");
        assert!(inner.join(".git").exists() && inner.join("a.txt").exists());
    }
}
