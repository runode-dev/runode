//! 写仓库的操作：暂存、丢弃、提交、同步远端和 stash。分支见 `branch`，按块暂存见 `patch`。

use std::{
    collections::HashMap,
    ffi::OsStr,
    fmt, fs,
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{FileDiff, FileStatus, Stash, UntrackedCache, find_repo, git, git_command};

/// 一条写仓库的 git 命令最多跑这么久，到时杀掉：推送、拉取要联网，给得长些；等的是 ssh 问口令、
/// 确认主机指纹或者 gpg 等着输密码这种永远等不来的输入时，不让后台线程一直挂着。
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// git 退出以后最多再等它的输出管道关上这么久：钩子留在后台的进程拿着管道时等不到头。
const PIPE_GRACE: Duration = Duration::from_secs(1);

/// 能在后台线程里用的仓库句柄，写操作都挂在它上面；由 `Snapshot::repo` 取得。git 命令
/// 可能要跑好几秒（推送、拉取），界面线程别直接调。子模块和嵌套的仓库各用各的句柄。
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Repo {
    /// 仓库的根目录，和 `Snapshot::root` 一样。
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

/// `Repo::undo_last_commit` 撤销前看到的 HEAD。
struct HeadCommit {
    /// HEAD 指着的分支（`refs/heads/…`），游离时为空。
    branch: Option<String>,
    /// 要撤销的提交的完整对象号。
    id: String,
    /// 它的父提交，第一个提交没有。
    parent: Option<String>,
    /// 它的完整说明。
    message: String,
}

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
/// 要拿的锁照常拿；`GIT_TERMINAL_PROMPT=0` 让要密码的远端直接报错，不卡着等输入，ssh 同样不问
/// （见 `batch_ssh`）；`input` 给了就写进标准输入，否则标准输入是空的。超过 `RUN_TIMEOUT` 杀掉，
/// 报超时。失败时的错误是 git 的标准错误，没有就用标准输出。
pub(crate) fn run<S: AsRef<OsStr>>(
    dir: &Path,
    args: impl IntoIterator<Item = S>,
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    run_for(dir, args, input, RUN_TIMEOUT)
}

/// 同 `run`，最多跑 `timeout`。
fn run_for<S: AsRef<OsStr>>(
    dir: &Path,
    args: impl IntoIterator<Item = S>,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut command = git_command(dir);
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // 新版 git 不再给「hint:」那些建议；本地化后的提示不以「hint:」开头，滤不掉。
        .env("GIT_ADVICE", "0")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 自成一个进程组，超时时连同 git 拉起的 ssh、gpg 一起杀掉，它们拿着输出的管道。
        .process_group(0);
    batch_ssh(&mut command, dir);
    let mut child = command.spawn().map_err(|err| GitError::new(format!("没能运行 git：{err}")))?;
    // 一边写输入一边读输出，输入输出都大时谁也不会等着谁。不用作用域线程：超时杀掉以后，脱离了
    // 进程组的子孙进程可能还拿着管道，读的线程就留在那里，不等它们。
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_vec();
        thread::spawn(move || {
            // git 不读完就退出时写会失败，以它的退出状态为准。
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = Drain::spawn(child.stdout.take());
    let stderr = Drain::spawn(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_group(&mut child);
                return Err(GitError::new(format!("git 超过 {} 秒没跑完，停下了", timeout.as_secs())));
            }
            Err(err) => {
                kill_group(&mut child);
                return Err(GitError::new(format!("没能运行 git：{err}")));
            }
        }
    };
    // git 写的这时都已经在管道里了。钩子拉起的后台进程继承了管道、一直不关时读不到头，最多等
    // `PIPE_GRACE`，拿到现在读到的，读的线程留给它们。
    let drained = Instant::now() + PIPE_GRACE;
    while !(stdout.is_finished() && stderr.is_finished()) && Instant::now() < drained {
        thread::sleep(Duration::from_millis(10));
    }
    output_result(status, stdout.take(), &stderr.take())
}

/// 在另一个线程里读 `pipe` 直到关上，读到的随时可取。
struct Drain {
    read: Arc<Mutex<Vec<u8>>>,
    thread: JoinHandle<()>,
}

impl Drain {
    fn spawn(pipe: Option<impl Read + Send + 'static>) -> Self {
        let read = Arc::new(Mutex::new(Vec::new()));
        let out = read.clone();
        let thread = thread::spawn(move || {
            let Some(mut pipe) = pipe else { return };
            let mut chunk = [0; 8192];
            while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                out.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(&chunk[..n]);
            }
        });
        Self { read, thread }
    }

    /// 管道关上了，读完了。
    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// 到现在读到的。
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.read.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// 没设 `GIT_SSH_COMMAND`、`GIT_SSH` 和 `core.sshCommand` 时让 ssh 用 `BatchMode`：要输口令、
/// 确认主机指纹时直接失败，不在开着 runode 的终端上等输入。设了的是用户自己的选择，不改：
/// `GIT_SSH_COMMAND` 压过 `core.sshCommand` 和 `GIT_SSH`，这里要是设了就把用户的盖掉了。
fn batch_ssh(command: &mut Command, dir: &Path) {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if set("GIT_SSH_COMMAND") || set("GIT_SSH") || git(dir, &["config", "--get", "core.sshCommand"]).is_some() {
        return;
    }
    command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
}

/// 杀掉 `run_for` 拉起的 git 所在的整个进程组，再收掉 git。
fn kill_group(child: &mut Child) {
    // 进程组号就是 git 的进程号（`process_group(0)`）；std 里没有给进程组发信号的接口，借 `kill`。
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// git 退出以后，成功时是标准输出，失败时把标准错误（没有就用标准输出）整理成错误。
pub(crate) fn output_result(status: ExitStatus, stdout: Vec<u8>, stderr: &[u8]) -> Result<Vec<u8>> {
    if status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(stderr);
    let message: Vec<_> = stderr.lines().filter(|line| !line.starts_with("hint:")).collect();
    let message = message.join("\n").trim().to_owned();
    let message = if message.is_empty() { String::from_utf8_lossy(&stdout).trim().to_owned() } else { message };
    if message.is_empty() {
        return Err(GitError::new(format!("git 失败了（{status}）")));
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
    results.into_iter().fold(Ok(()), Result::and)
}

impl Repo {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `dir` 所在仓库的句柄，根目录和 `snapshot` 读到的一样；`dir` 不在 git 仓库里时为空。只问一次
    /// git，不读状态。
    pub fn open(dir: &Path) -> Option<Self> {
        find_repo(dir).map(|(root, _)| Self::new(root))
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

    /// git 跟踪着 `path`（相对 `root`）；是目录时看它下面有没有跟踪着的文件。
    pub fn is_tracked(&self, path: &Path) -> bool {
        let spec = format!(":(literal){}", path.display());
        git(&self.root, &["ls-files", "--", &spec]).is_some_and(|out| !out.is_empty())
    }

    /// 不再跟踪这些文件或目录：从暂存区拿掉，工作区里的留着，下次提交时从仓库里删掉。暂存区里有和
    /// HEAD、工作区都不一样的内容时 git 拒绝（不加 `-f`），免得丢掉只在暂存区里的改动。
    pub fn untrack(&self, paths: &[PathBuf]) -> Result {
        let groups = self.group(paths.iter().map(PathBuf::as_path))?;
        first_error(groups.iter().map(|(dir, rels)| Self::run_paths(dir, &["rm", "--cached", "-r", "-q"], rels)))
    }

    /// 丢掉工作区里还没暂存的改动，`files` 来自未暂存段。未跟踪的文件从磁盘上删掉（只删
    /// 文件不删目录，删完把因此变空的上层目录也删掉）；`git add -N` 记下的新文件同样删掉并
    /// 移出暂存区；其余的按暂存区里的样子恢复。已经暂存的改动不受影响；冲突的文件暂存区
    /// 里没有单一的版本，不进批量的 `git restore`（它会让整批失败），单独报错。一个文件失败不影响其余的，
    /// 返回第一个错误。子模块那样的
    /// gitlink 跳过：工作区里的提交号要到子仓库里切，`git restore` 对它什么也不做。
    /// `files` 是先前读到的，动手前重读一次：有哪个文件不在未暂存段里了或状态、原路径变了（比如问用户
    /// 的时候在终端里暂存了），一个也不丢、报错，免得照旧状态删掉已经暂存的内容。
    pub fn discard(&self, files: &[FileDiff]) -> Result {
        // shortcut: 重读整个仓库的状态和 diff，大仓库里丢一个文件也多读一遍；嫌慢了再改成只对这些路径跑 git status。
        let Some(now) = crate::snapshot(&self.root, &mut UntrackedCache::default()).map(|s| s.unstaged) else {
            return Err(GitError::new("没能读到仓库现在的状态，什么也没丢"));
        };
        // 按路径建表再比：丢整个目录时文件和未暂存的条目都可能有几万个（未跟踪的文件一个个列），逐个扫要几秒。
        let now: HashMap<&Path, &FileDiff> = now.iter().map(|n| (n.path.as_path(), n)).collect();
        let same = |file: &FileDiff| {
            now.get(file.path.as_path()).is_some_and(|n| n.status == file.status && n.old_path == file.old_path)
        };
        if let Some(file) = files.iter().find(|file| !file.gitlink && !same(file)) {
            return Err(GitError::new(format!("{} 的状态变了，什么也没丢，看过最新的改动再来", file.path.display())));
        }
        let mut results = Vec::new();
        let mut restore = Vec::new();
        let mut remove = Vec::new();
        for file in files.iter().filter(|file| !file.gitlink) {
            match file.status {
                FileStatus::Untracked => results.push(self.delete_untracked(&file.path)),
                // `git restore --worktree` 碰到没解决的冲突整条命令失败，同批的别的文件一个都恢复不了，
                // 所以冲突的（连同它的原路径）不进批量命令，单独报错。
                FileStatus::Conflicted => {
                    results.push(Err(GitError::new(format!(
                        "{} 有没解决的冲突，先解决或者用 git 处理",
                        file.path.display()
                    ))));
                    continue;
                }
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

    /// 暂存这个仓库的所有改动，含未跟踪和删掉的文件，以及子模块记着的提交号；嵌套的仓库不碰，
    /// 它们用自己的句柄暂存。
    pub fn stage_all(&self) -> Result {
        add_all(&self.root)
    }

    /// 撤回这个仓库所有暂存的改动，嵌套的仓库不碰。合并做到一半时不会因此放弃合并。
    pub fn unstage_all(&self) -> Result {
        let dir = &self.root;
        if has_head(dir) {
            Self::run_paths(dir, &["restore", "--staged"], &[".".into()])
        } else {
            Self::run_paths(dir, &["rm", "--cached", "-r", "-f", "-q", "--ignore-unmatch"], &[".".into()])
        }
    }

    /// 提交这个仓库暂存的改动；说明经标准输入交给 git，不经命令行。`amend` 且说明为空时
    /// 沿用上一次提交的说明。没有可提交的改动、说明为空时报 git 的错。`stage_all` 时暂存区里
    /// 有没解决的冲突就报错，什么也不做：全部暂存会把带冲突标记的文件当成解决了提交进去。
    pub fn commit(&self, message: &str, options: CommitOptions) -> Result {
        if options.stage_all {
            if !run(&self.root, ["ls-files", "--unmerged"], None)?.is_empty() {
                return Err(GitError::new("还有没解决的冲突，没有暂存全部再提交"));
            }
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

    /// 提交要带上的改动，写提交说明时给 AI 看：`staged` 时是暂存区相对上一个提交的差异；否则是
    /// 工作区的全部改动（提交时会先全部暂存），未跟踪的文件只在末尾列出文件名。
    pub fn pending_diff(&self, staged: bool) -> Result<String> {
        let diff = if staged {
            run(&self.root, ["diff", "--cached"], None)?
        } else if has_head(&self.root) {
            run(&self.root, ["diff", "HEAD"], None)?
        } else {
            // 还没有提交时和空树比，暂存了的新文件也算进来；空树的对象号随仓库的哈希算法，问 git 要。
            let empty = run(&self.root, ["hash-object", "-t", "tree", "--stdin"], Some(b""))?;
            run(&self.root, ["diff", String::from_utf8_lossy(&empty).trim()], None)?
        };
        let mut text = String::from_utf8_lossy(&diff).into_owned();
        if !staged {
            let untracked = run(&self.root, ["ls-files", "--others", "--exclude-standard"], None)?;
            for path in String::from_utf8_lossy(&untracked).lines() {
                text.push_str(&format!("untracked: {path}\n"));
            }
        }
        Ok(text)
    }

    /// 最近 `limit` 个提交的完整说明，新的在前；还没有提交时为空。
    pub fn recent_messages(&self, limit: usize) -> Result<Vec<String>> {
        if !has_head(&self.root) {
            return Ok(Vec::new());
        }
        let log = run(&self.root, ["log".to_owned(), format!("-{limit}"), "--format=%B%x00".to_owned()], None)?;
        let log = String::from_utf8_lossy(&log);
        Ok(log.split('\0').map(str::trim).filter(|message| !message.is_empty()).map(str::to_owned).collect())
    }

    /// 撤销最近一次提交，改动留在暂存区里；返回被撤销的提交的完整说明，好填回
    /// 输入框。只有一个提交时删掉分支，仓库回到还没有提交的样子。父提交不在本地（浅克隆）、合并、
    /// cherry-pick 或 revert 做到一半、游离的 HEAD 停在第一个提交上时报错，什么也不做；看过以后 HEAD
    /// 又变了（终端里又提交了一个）时也报错，不会连着撤掉两个。
    pub fn undo_last_commit(&self) -> Result<String> {
        let head = self.inspect_head()?;
        self.undo_commit(&head)
    }

    /// 撤销前看一眼 HEAD：所在的分支、指向的提交和它的父提交，后面都按这一次看到的来。
    fn inspect_head(&self) -> Result<HeadCommit> {
        // 分支名按原样用，不做有损转换：不是 UTF-8 的名字换了字就成了另一个（或不存在的）分支。
        let branch = git(&self.root, &["symbolic-ref", "-q", "HEAD"])
            .map(|mut name| {
                if name.ends_with(b"\n") {
                    name.pop();
                }
                String::from_utf8(name).map_err(|_| GitError::new("分支名不是 UTF-8，没有撤销"))
            })
            .transpose()?;
        // 按分支名解析，不再读一次 HEAD：两次之间切了分支的话，分支和提交就对不上了。
        let target = format!("{}^{{commit}}", branch.as_deref().unwrap_or("HEAD"));
        let id = git(&self.root, &["rev-parse", "--verify", "--quiet", &target])
            .map(|id| String::from_utf8_lossy(&id).trim().to_owned())
            .ok_or_else(|| GitError::new("还没有提交，没有可撤销的"))?;
        let message = run(&self.root, ["log", "-1", "--format=%B", &id], None)?;
        let message = String::from_utf8_lossy(&message).trim_end().to_owned();
        // 看提交对象里记的父提交，不看 `HEAD~1` 解不解析得出来：浅克隆里父提交不在本地，那不是第一个提交。
        let raw = run(&self.root, ["cat-file", "commit", &id], None)?;
        let raw = String::from_utf8_lossy(&raw);
        let parent = raw
            .lines()
            .take_while(|line| !line.is_empty())
            .find_map(|line| line.strip_prefix("parent "))
            .map(str::to_owned);
        Ok(HeadCommit { branch, id, parent, message })
    }

    /// 撤销 `inspect_head` 看到的那个提交。引用都带着期望的旧值改：看过以后 HEAD 已经不在那个提交上时
    /// git 拒绝，什么也不动。不用 `reset --soft`，它只认目标、不核对旧值；它和这里一样不碰暂存区和
    /// 工作区，合并做到一半时不肯做，`ORIG_HEAD` 记下撤掉的提交，这几样照着做。
    fn undo_commit(&self, head: &HeadCommit) -> Result<String> {
        // 和 `reset --soft` 一样，有没解决的冲突时也不做：冲突不一定来自合并，cherry-pick、`stash pop`
        // 冲突了也没有 `MERGE_HEAD`。
        let unmerged = run(&self.root, ["ls-files", "--unmerged"], None)?;
        if git(&self.root, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]).is_some() || !unmerged.is_empty() {
            return Err(GitError::new("合并做到一半，没有撤销"));
        }
        // cherry-pick、revert 冲突解决了还没提交，或者 `merge --squash` 了还没提交时，`reset` 会顺手清掉
        // 它们的状态，`update-ref` 不会：留着的话下一次提交还当成那次操作的接续，用它备好的说明。不替用户
        // 丢掉，不做。
        if ["CHERRY_PICK_HEAD", "REVERT_HEAD"]
            .iter()
            .any(|name| git(&self.root, &["rev-parse", "--verify", "--quiet", name]).is_some())
        {
            return Err(GitError::new("cherry-pick 或 revert 做到一半，没有撤销"));
        }
        // `--git-path` 给的是相对仓库根目录的路径（git 目录在别处时是绝对路径），接在根目录后面。
        let squash = git(&self.root, &["rev-parse", "--git-path", "SQUASH_MSG"]);
        if squash.is_some_and(|path| self.root.join(OsStr::from_bytes(path.trim_ascii_end())).exists()) {
            return Err(GitError::new("squash 合并了还没提交，没有撤销"));
        }
        let id = head.id.as_str();
        match head.parent.as_deref() {
            Some(parent) => {
                if git(&self.root, &["cat-file", "-e", &format!("{parent}^{{commit}}")]).is_none() {
                    return Err(GitError::new("上一个提交不在本地（浅克隆），没有撤销"));
                }
                // 改 HEAD 跟着它指到的分支走：期间切到了别的分支，那个分支不在这个提交上就不动它；
                // 恰好也在这个提交上时撤的正是现在检出的这一个，暂存区里留下的改动照样对得上。
                let reason = format!("reset: moving to {parent}");
                run(&self.root, ["update-ref", "-m", &reason, "HEAD", parent, id], None)?;
            }
            None => {
                // 游离的 HEAD 没有分支可删。按名字删分支、不经 HEAD 删：期间 HEAD 游离到这个提交上时，
                // 经 HEAD 删的就是 HEAD 本身，仓库就坏了。
                let Some(branch) = head.branch.as_deref() else {
                    return Err(GitError::new("HEAD 没在分支上，第一个提交撤销不了"));
                };
                let on_branch = || {
                    git(&self.root, &["symbolic-ref", "-q", "HEAD"])
                        .is_some_and(|name| name.trim_ascii_end() == branch.as_bytes())
                };
                // 看过以后切走了（切到同一个提交上的新分支，或者游离了）就不删：删的不再是检出的那一个。
                if !on_branch() {
                    return Err(GitError::new("看过以后 HEAD 换了分支，没有撤销"));
                }
                run(&self.root, ["update-ref", "-d", branch, id], None)?;
                // 上面的检查和删除之间也可能切走，删完再看一次，切走了就照原样建回来（只在它还不存在时建）。
                if !on_branch() {
                    let _ = run(&self.root, ["update-ref", branch, id, ""], None);
                    return Err(GitError::new("撤销时 HEAD 换了分支，已经恢复，没有撤销"));
                }
            }
        }
        // 和 `reset` 一样记下撤掉的提交，`git reset ORIG_HEAD` 能找回来；记不下不影响撤销。
        let _ = run(&self.root, ["update-ref", "ORIG_HEAD", id], None);
        Ok(head.message.clone())
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

    /// 把这个仓库的改动收进一个新的 stash，工作区回到 HEAD 的样子。`include_untracked`
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

    /// 把 `stash` 的改动放回工作区，stash 留着。按对象 ID 应用（`git stash apply` 认任何像 stash 的提交），
    /// 读到它以后序号挪了也不会应用错。
    pub fn stash_apply(&self, stash: &Stash) -> Result {
        run(&self.root, ["stash", "apply", &stash.id], None).map(drop)
    }

    /// 把 `stash` 的改动放回工作区并删掉它；有冲突时 git 不删。读到它以后别处 push、pop 过，
    /// `stash@{index}` 已经不是它时不动，报错。
    pub fn stash_pop(&self, stash: &Stash) -> Result {
        self.stash_command("pop", stash)
    }

    /// 删掉 `stash`。读到它以后别处 push、pop 过，`stash@{index}` 已经不是它时不删，报错。
    pub fn stash_drop(&self, stash: &Stash) -> Result {
        self.stash_command("drop", stash)
    }

    /// pop 和 drop 只认 `stash@{index}` 这样的序号，先核对序号上仍是 `stash`。
    fn stash_command(&self, command: &str, stash: &Stash) -> Result {
        let rev = format!("stash@{{{}}}", stash.index);
        let now = git(&self.root, &["rev-parse", "--verify", "--quiet", &rev]).unwrap_or_default();
        if String::from_utf8_lossy(&now).trim() != stash.id {
            return Err(GitError::new(format!("{rev} 已经变了，什么也没做，看过最新的储藏列表再来")));
        }
        // shortcut: 核对和动手之间还有几毫秒的窗口（git 只认序号），真撞上过再改成拿 refs/stash 的锁。
        run(&self.root, ["stash", command, &rev], None).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-git-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(git(&dir, &["init", "-q"]).is_some());
        dir
    }

    #[test]
    fn kills_git_that_runs_past_the_timeout() {
        let dir = temp_repo("timeout");
        // pre-commit 钩子一直不退出，像 ssh、gpg 等着输入那样；它把进程号记下来，好看它也被杀掉了。
        let pid_file = dir.join("hook.pid");
        let hooks = dir.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n", pid_file.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(git(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]).is_some());
        let started = Instant::now();
        let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"];
        let err = run_for(&dir, args, None, Duration::from_secs(1)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(err.message.contains("超过"), "{}", err.message);
        // 钩子在 git 的进程组里，跟着一起杀掉；死了以后要等被收掉，`kill -0` 才找不到它。
        let pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        let alive = || Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "钩子 {pid} 还活着");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// 钩子把进程留在后台、拿着 git 的输出管道：git 退出了就算跑完，不等那个进程。
    #[test]
    fn returns_when_git_exits_though_a_hook_left_the_pipes_open() {
        let dir = temp_repo("background-hook");
        let pid_file = dir.join("hook.pid");
        let hooks = dir.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("post-commit");
        fs::write(&hook, format!("#!/bin/sh\nsleep 30 &\necho $! > '{}'\n", pid_file.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(git(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]).is_some());
        let started = Instant::now();
        let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"];
        let result = run_for(&dir, args, None, Duration::from_secs(20));
        let elapsed = started.elapsed();
        let pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        let _ = Command::new("kill").arg(&pid).status();
        assert!(result.is_ok(), "{result:?}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// 看过 HEAD、动手之前终端里又提交了一个：不能按看到的父提交把两个一起撤掉，什么也不动。
    #[test]
    fn undo_refuses_when_head_moves_after_inspecting() {
        let dir = temp_repo("undo-race");
        let commit = |message: &str| {
            let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", message];
            assert!(git(&dir, &args).is_some());
        };
        let head = || String::from_utf8(git(&dir, &["rev-parse", "HEAD"]).unwrap()).unwrap();
        let repo = Repo::new(dir.clone());

        // 只有一个提交时要删分支：期间多了一个提交，分支不删。
        commit("a");
        let inspected = repo.inspect_head().unwrap();
        commit("b");
        let moved = head();
        assert!(repo.undo_commit(&inspected).is_err());
        assert_eq!(head(), moved);

        // 有父提交时退回父提交：期间多了一个提交，HEAD 不动，不会连着撤掉两个。
        let inspected = repo.inspect_head().unwrap();
        commit("c");
        let moved = head();
        assert!(repo.undo_commit(&inspected).is_err());
        assert_eq!(head(), moved);

        // 没有变化时照常撤掉，ORIG_HEAD 记着撤掉的那个。
        assert_eq!(repo.undo_last_commit().unwrap(), "c");
        let orig = String::from_utf8(git(&dir, &["rev-parse", "ORIG_HEAD"]).unwrap()).unwrap();
        assert_eq!(orig, moved);
        assert_eq!(String::from_utf8(git(&dir, &["log", "--format=%s"]).unwrap()).unwrap(), "b\na\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// 只有一个提交，看过以后切走了（同一个提交上的新分支，或者游离到这个提交上）：什么也不删，
    /// 刚切走的分支还在，游离的 HEAD 也还在。
    #[test]
    fn undo_of_the_first_commit_refuses_after_head_moves_away() {
        let dir = temp_repo("undo-root-switch");
        let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "a"];
        assert!(git(&dir, &args).is_some());
        let repo = Repo::new(dir.clone());
        let exists = |name: &str| git(&dir, &["rev-parse", "--verify", "--quiet", name]).is_some();
        let first = git(&dir, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        let first = format!("refs/heads/{}", String::from_utf8(first).unwrap().trim());

        let inspected = repo.inspect_head().unwrap();
        assert!(git(&dir, &["switch", "-q", "-c", "other"]).is_some());
        assert!(repo.undo_commit(&inspected).is_err());
        assert!(exists(&first) && exists("refs/heads/other"));

        assert!(git(&dir, &["switch", "-q", &first["refs/heads/".len()..]]).is_some());
        let inspected = repo.inspect_head().unwrap();
        assert!(git(&dir, &["switch", "-q", "--detach"]).is_some());
        assert!(repo.undo_commit(&inspected).is_err());
        assert!(exists(&first) && exists("HEAD"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_the_users_ssh_command() {
        let dir = temp_repo("ssh");
        let env = |command: &Command| {
            command
                .get_envs()
                .find(|(name, _)| *name == "GIT_SSH_COMMAND")
                .and_then(|(_, value)| value.map(OsStr::to_owned))
        };
        let user_set = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || git(&dir, &["config", "--get", "core.sshCommand"]).is_some();
        let mut command = Command::new("git");
        batch_ssh(&mut command, &dir);
        if !user_set {
            assert_eq!(env(&command), Some("ssh -o BatchMode=yes".into()));
        }
        // 仓库里配了 `core.sshCommand` 时不设，不然它被环境变量盖掉。
        assert!(git(&dir, &["config", "core.sshCommand", "ssh -i key"]).is_some());
        let mut command = Command::new("git");
        batch_ssh(&mut command, &dir);
        assert_eq!(env(&command), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
