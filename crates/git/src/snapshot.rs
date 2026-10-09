//! 一个仓库的状态：改动分的两段、文件状态和逐行改动这些数据，读一个仓库（`snapshot`、
//! `read_repo`），以及把未跟踪的文件当作整个新增读进来。

use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    Repo, RepoInfo, RepoKind, find_repo, git, info,
    parse::{expand_tabs, parse_diff, parse_status},
    repos,
};

/// 一个文件最多读这么多行改动，再多的只记总数。
pub(crate) const MAX_FILE_LINES: usize = 3000;
/// 未跟踪的文件比这大时不读内容。
const MAX_UNTRACKED_BYTES: u64 = 256 * 1024;
/// 未跟踪的文件最多读这么多个的内容，其余只列出来。
const MAX_UNTRACKED_FILES: usize = 200;
/// 比这大的文件 git 不算逐行改动，当二进制报，免得一个生成的大文件拖慢整次读取。
pub(crate) const MAX_DIFF_BYTES: u64 = 1024 * 1024;

/// 改动分的两段：已经 `git add` 的改动（暂存区相对 HEAD），以及还没暂存的改动
/// （工作区相对暂存区，含未跟踪的文件）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Staged,
    Unstaged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl FileStatus {
    /// 改动列表里状态的单字母标记。
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// 在旧文件和新文件里的行号；新增的行没有旧行号，删掉的行没有新行号。
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// 去掉开头的 `+`、`-` 或空格，制表符换成空格。
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// `@@ -a,b +c,d @@` 以及后面的函数名之类。
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// 相对仓库根的路径；删掉的文件是原来的路径。
    pub path: PathBuf,
    /// 改名前的路径。
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// 改动超过 `MAX_FILE_LINES` 行，或者是没读内容的未跟踪文件，`hunks` 不全。
    pub truncated: bool,
    /// 是子模块那样记着一个提交号的条目（gitlink），改动是提交号变了。它的内容在子仓库里，
    /// 在这个仓库里只能暂存或撤回暂存，丢不掉，也不能按块操作。
    pub gitlink: bool,
}

/// 一个仓库的状态。只管这一个仓库：里面的子模块只是一个记着提交号的条目，嵌套的仓库是一个
/// 不往里看的未跟踪目录，它们各自另有一份，见 `Repos`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// 仓库根目录。
    pub root: PathBuf,
    /// 仓库的 git 目录；worktree 和子模块的不在 `root` 下面，要另外监听才知道提交和暂存。
    pub git_dir: PathBuf,
    /// 相对主仓库根的路径，主仓库自己为空；其他工作树不在主仓库里，是它的根目录（绝对路径）。
    pub prefix: PathBuf,
    pub kind: RepoKind,
    /// 两段各自有改动的文件，按路径排序；部分暂存的文件两段里都有。路径相对这个仓库的根。
    pub staged: Vec<FileDiff>,
    pub unstaged: Vec<FileDiff>,
    /// 有改动的文件的状态，键是相对这个仓库根的路径。
    pub statuses: HashMap<PathBuf, FileStatus>,
    /// 被忽略的文件和目录，相对这个仓库的根；目录被忽略时里面的不再单列。
    pub ignored: HashSet<PathBuf>,
    /// 这个仓库的分支、上游、进行中的操作和 stash。
    pub info: RepoInfo,
}

impl Snapshot {
    pub fn files(&self, section: Section) -> &[FileDiff] {
        match section {
            Section::Staged => &self.staged,
            Section::Unstaged => &self.unstaged,
        }
    }

    fn all(&self) -> impl Iterator<Item = &FileDiff> {
        self.staged.iter().chain(&self.unstaged)
    }

    pub fn added(&self) -> usize {
        self.all().map(|file| file.added).sum()
    }

    pub fn removed(&self) -> usize {
        self.all().map(|file| file.removed).sum()
    }

    /// 有改动的文件数，部分暂存的只算一个。
    pub fn changed(&self) -> usize {
        self.all().map(|file| &file.path).collect::<HashSet<_>>().len()
    }

    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }

    /// `rel` 相对仓库根，它或者它所在的目录被忽略。
    pub fn is_ignored(&self, rel: &Path) -> bool {
        rel.ancestors().any(|dir| self.ignored.contains(dir))
    }

    /// 是 `git worktree add` 出来的链接工作树：git 目录在共用 git 目录的 `worktrees/` 下面。主工作树
    /// 和普通仓库不是。
    pub fn is_linked_worktree(&self) -> bool {
        self.git_dir.parent().and_then(Path::file_name).is_some_and(|name| name == "worktrees")
    }

    /// 这个仓库的句柄，暂存、提交这些写操作挂在它上面。
    pub fn repo(&self) -> Repo {
        Repo::new(self.root.clone())
    }
}

/// 上次读到的未跟踪文件，按绝对路径记着读时的大小和修改时间；没变的下次不再读内容。
#[derive(Default)]
pub struct UntrackedCache(pub(crate) HashMap<PathBuf, (u64, SystemTime, FileDiff)>);

/// 读 `dir` 所在仓库的状态，只读这一个仓库，子模块和嵌套的仓库不往里看；`dir` 不在 git
/// 仓库里或者没装 git 时为空。没变过的未跟踪文件从 `cache` 里取，读完后 `cache` 只留这次还在的。
pub fn snapshot(dir: &Path, cache: &mut UntrackedCache) -> Option<Snapshot> {
    let (root, git_dir) = find_repo(dir)?;
    let mut seen = UntrackedCache::default();
    let found = read_repo(root, git_dir, PathBuf::new(), RepoKind::Main, cache, &mut seen)?;
    *cache = seen;
    Some(found.snapshot)
}

/// 读 `FileDiff` 用的 `git diff` 命令行（不含 `git`），再加上 `extra`。前缀写明，免得用户配置了
/// `diff.noprefix` 之类改掉 `a/`、`b/`。子模块只在记着的提交号变了时算改动，里面改了文件不算：
/// 那些改动在子模块自己的那份里，在这里既暂存不了也丢不掉。
pub(crate) fn diff_args(extra: &[&str]) -> Vec<String> {
    let threshold = format!("core.bigFileThreshold={MAX_DIFF_BYTES}");
    let mut args = vec!["-c", &threshold, "diff", "-M", "--no-color", "--no-ext-diff", "--no-textconv"];
    args.extend(["--src-prefix=a/", "--dst-prefix=b/", "--ignore-submodules=dirty"]);
    args.extend(extra);
    args.into_iter().map(str::to_owned).collect()
}

/// `repo` 里的 `git diff`，参数见 `diff_args`。
pub(crate) fn diff(repo: &Path, args: &[&str]) -> Vec<FileDiff> {
    let full = diff_args(args);
    let full: Vec<&str> = full.iter().map(String::as_str).collect();
    let mut files = parse_diff(&String::from_utf8_lossy(&git(repo, &full).unwrap_or_default()));
    // 超过大小上限的文本文件也被报成二进制：工作区里的文件超过上限、开头又没有 NUL 字节的
    // 认回来，提示改动太多。暂存段也按工作区里的文件认，暂存后又改过的可能认错，只影响提示。
    for file in files.iter_mut().filter(|file| file.binary) {
        let full = repo.join(&file.path);
        if fs::metadata(&full).is_ok_and(|meta| meta.len() > MAX_DIFF_BYTES) && !starts_binary(&full) {
            file.binary = false;
            file.truncated = true;
        }
    }
    files
}

/// 文件开头一段里有 NUL 字节，像 git 一样当作二进制。
fn starts_binary(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0; 8000];
    let Ok(mut file) = fs::File::open(path) else {
        return true;
    };
    let len = file.read(&mut head).unwrap_or(0);
    head[..len].contains(&0)
}

/// `read_repo` 读到的一个仓库，以及在它里面找到的子仓库。
pub(crate) struct Found {
    pub snapshot: Snapshot,
    /// 已经检出的子模块和未跟踪的嵌套仓库，相对这个仓库的根。
    pub children: Vec<(PathBuf, RepoKind)>,
}

/// 读根目录是 `root`、git 目录是 `git_dir` 的这一个仓库。未跟踪的目录是嵌套的仓库（比如放在
/// 仓库里的 worktree），git 不往里看，这里也不读，记进 `Found::children`；`.gitmodules` 里登记
/// 而且检出了的子模块也记进去。
pub(crate) fn read_repo(
    root: PathBuf,
    git_dir: PathBuf,
    prefix: PathBuf,
    kind: RepoKind,
    cache: &UntrackedCache,
    seen: &mut UntrackedCache,
) -> Option<Found> {
    let repo = root.as_path();
    // 状态、暂存段和未暂存段互不依赖，几个 git 进程同时跑；stash、远端和子模块也一起读。
    let (status, staged, mut unstaged, extra, submodules) = std::thread::scope(|scope| {
        let status = scope.spawn(|| {
            // 开头多一项 `## 分支...上游 [ahead 1, behind 2]`。里面有改动的子模块也报出来，连同没
            // 写进 `.gitmodules` 的 gitlink，据此认出子仓库；列进改动的条目以 `diff` 为准。
            git(
                repo,
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--ignored=matching",
                    "--ignore-submodules=none",
                    "--branch",
                    "--ahead-behind",
                ],
            )
        });
        let staged = scope.spawn(|| {
            // 还没有提交时 `--cached` 自己和空树比，暂存了的新文件也算进来。短哈希给 `RepoInfo::head`。
            let head = git(repo, &["rev-parse", "--verify", "--quiet", "--short", "HEAD"])
                .map(|head| String::from_utf8_lossy(&head).trim().to_owned());
            (diff(repo, &["--cached"]), head)
        });
        let extra = scope.spawn(|| info::read_extra(repo));
        let submodules = scope.spawn(|| repos::submodules(repo));
        // 冲突的文件和「我方」比，不然 git 给的是三方合并的格式。
        let unstaged = diff(repo, &["-2"]);
        let status = status.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        let staged = staged.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        let extra = extra.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        let submodules = submodules.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (status, staged, unstaged, extra, submodules)
    });
    let status = status?;
    let (mut statuses, ignored) = parse_status(&status);
    let (mut staged, head) = staged;
    let info = info::read(&status, head, &git_dir, extra);
    let mut untracked: Vec<_> =
        statuses.iter().filter(|(_, status)| **status == FileStatus::Untracked).map(|(path, _)| path.clone()).collect();
    untracked.sort();
    // `--untracked-files=all` 把未跟踪的文件一个个列出来，只有嵌套的仓库整个报成一个目录。
    // 指向目录的符号链接 git 当文件报，这里也不跟进去。
    let (nested, untracked): (Vec<_>, Vec<_>) =
        untracked.into_iter().partition(|path| fs::symlink_metadata(repo.join(path)).is_ok_and(|meta| meta.is_dir()));
    for (ix, path) in untracked.into_iter().enumerate() {
        unstaged.push(untracked_diff(repo, path, ix < MAX_UNTRACKED_FILES, cache, seen));
    }
    // 冲突等状态以 `git status` 为准，diff 只看得出增删改名。冲突的文件不在暂存区里。
    staged.retain(|file| statuses.get(&file.path) != Some(&FileStatus::Conflicted));
    for file in staged.iter_mut().chain(&mut unstaged) {
        if statuses.get(&file.path) == Some(&FileStatus::Conflicted) {
            file.status = FileStatus::Conflicted;
        }
    }
    staged.sort_by(|a, b| a.path.cmp(&b.path));
    unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    // 已跟踪、自带 `.git` 的目录是 gitlink：`.gitmodules` 里登记的子模块，或者没登记、直接
    // `git add` 进来的仓库。后者只能从 `git status` 认出来，所以只有它有改动时才读得到。只是
    // 里面有改动、提交号没变的不算这个仓库的改动，状态里也不留。
    let gitlinks: Vec<PathBuf> = statuses
        .iter()
        .filter(|(path, status)| **status != FileStatus::Untracked && repo.join(path).join(".git").exists())
        .map(|(path, _)| path.clone())
        .collect();
    for path in &gitlinks {
        if !staged.iter().chain(&unstaged).any(|file| &file.path == path) {
            statuses.remove(path);
        }
    }
    let mut submodules = submodules;
    submodules.extend(gitlinks);
    submodules.sort();
    submodules.dedup();
    let children = submodules
        .into_iter()
        .map(|path| (path, RepoKind::Submodule))
        .chain(
            nested
                .into_iter()
                .filter(|path| repo.join(path).join(".git").exists())
                .map(|path| (path, RepoKind::Nested)),
        )
        .collect();
    let snapshot = Snapshot {
        root,
        git_dir,
        prefix,
        kind,
        staged,
        unstaged,
        statuses,
        ignored: ignored.into_iter().collect(),
        info,
    };
    Some(Found { snapshot, children })
}

/// 未跟踪的文件当作整个新增；`read` 为假或者文件太大时不读内容。大小和修改时间都没变时
/// 用 `cache` 里上次读的，读过的记进 `seen`。符号链接看它自己，不看它指向的。
fn untracked_diff(
    root: &Path,
    path: PathBuf,
    read: bool,
    cache: &UntrackedCache,
    seen: &mut UntrackedCache,
) -> FileDiff {
    let full = root.join(&path);
    let meta = fs::symlink_metadata(&full).ok();
    let stamp = meta.as_ref().and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
    if let Some((len, modified)) = stamp
        && read
        && let Some((cached_len, cached_modified, file)) = cache.0.get(&full)
        && (*cached_len, *cached_modified) == (len, modified)
    {
        seen.0.insert(full, (len, modified, file.clone()));
        return file.clone();
    }
    let file = read_untracked(&full, path, read && meta.is_some_and(|meta| meta.len() <= MAX_UNTRACKED_BYTES));
    if let Some((len, modified)) = stamp
        && read
    {
        seen.0.insert(full, (len, modified, file.clone()));
    }
    file
}

pub(crate) fn read_untracked(full: &Path, path: PathBuf, read: bool) -> FileDiff {
    let mut file = FileDiff {
        path,
        old_path: None,
        status: FileStatus::Untracked,
        added: 0,
        removed: 0,
        hunks: Vec::new(),
        binary: false,
        truncated: false,
        gitlink: false,
    };
    let content = read.then(|| worktree_bytes(full)).flatten();
    let Some(content) = content else {
        file.truncated = true;
        return file;
    };
    if content.contains(&0) {
        file.binary = true;
        return file;
    }
    let text = String::from_utf8_lossy(&content);
    let lines: Vec<_> = text.lines().collect();
    file.added = lines.len();
    file.truncated = lines.len() > MAX_FILE_LINES;
    if !lines.is_empty() {
        file.hunks.push(Hunk {
            header: format!("@@ -0,0 +1,{} @@", lines.len()),
            lines: lines
                .iter()
                .take(MAX_FILE_LINES)
                .zip(1..)
                .map(|(text, n)| Line { kind: LineKind::Added, old: None, new: Some(n), text: expand_tabs(text) })
                .collect(),
        });
    }
    file
}

/// 工作区里一个文件的内容，按 git 的规矩：符号链接是它指向的路径，不跟过去读（指向 `/dev/zero` 的
/// 会把内存读光，指向命名管道的会一直等着）；管道、设备这些不是普通文件的读不到，为空。
pub(crate) fn worktree_bytes(full: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(full).ok()?;
    if meta.file_type().is_symlink() {
        return fs::read_link(full).ok().map(|target| target.into_os_string().into_vec());
    }
    if !meta.is_file() {
        return None;
    }
    fs::read(full).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_the_one_repository() {
        let base = std::env::temp_dir().join(format!("runode-git-nested-{}", std::process::id()));
        let nested = base.join("wt/inner");
        std::fs::create_dir_all(&nested).unwrap();
        let run = |dir: &Path, args: &[&str]| assert!(git(dir, args).is_some(), "git {args:?}");
        let commit = |dir: &Path| {
            std::fs::write(dir.join("a.txt"), "one\n").unwrap();
            run(dir, &["init", "-q"]);
            run(dir, &["add", "a.txt"]);
            run(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
            std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        };
        commit(&base);
        commit(&nested);
        // 嵌套的仓库不并进来，只记着它是个未跟踪的目录。
        let snapshot = snapshot(&base, &mut UntrackedCache::default()).unwrap();
        let paths: Vec<_> = snapshot.unstaged.iter().map(|file| file.path.clone()).collect();
        assert_eq!(paths, vec![PathBuf::from("a.txt")]);
        assert_eq!(snapshot.statuses[Path::new("wt/inner")], FileStatus::Untracked);
        assert_eq!((snapshot.kind, snapshot.prefix.as_os_str().is_empty()), (RepoKind::Main, true));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn splits_staged_and_unstaged_changes() {
        let base = std::env::temp_dir().join(format!("runode-git-sections-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let run = |args: &[&str]| assert!(git(&base, args).is_some(), "git {args:?}");
        std::fs::write(base.join("a.txt"), "one\n").unwrap();
        std::fs::write(base.join("big.txt"), "x\n").unwrap();
        run(&["init", "-q"]);
        run(&["add", "."]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        // a.txt 暂存了一处改动，工作区里又改了一处；big.txt 改得超过大小上限。
        std::fs::write(base.join("a.txt"), "two\n").unwrap();
        run(&["add", "a.txt"]);
        std::fs::write(base.join("a.txt"), "three\n").unwrap();
        std::fs::write(base.join("big.txt"), "y\n".repeat(MAX_DIFF_BYTES as usize)).unwrap();
        std::fs::write(base.join("blob.bin"), vec![0u8; MAX_DIFF_BYTES as usize * 2]).unwrap();
        run(&["add", "-N", "blob.bin"]);
        std::fs::write(base.join("new.txt"), "n\n").unwrap();

        let mut cache = UntrackedCache::default();
        let snapshot = snapshot(&base, &mut cache).unwrap();
        let paths = |files: &[FileDiff]| files.iter().map(|file| file.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&snapshot.staged), [PathBuf::from("a.txt")]);
        assert_eq!(
            paths(&snapshot.unstaged),
            [PathBuf::from("a.txt"), "big.txt".into(), "blob.bin".into(), "new.txt".into()]
        );
        assert_eq!(snapshot.staged[0].hunks[0].lines[1].text, "two");
        assert_eq!(snapshot.unstaged[0].hunks[0].lines[1].text, "three");
        assert_eq!(snapshot.changed(), 4);
        let big = &snapshot.unstaged[1];
        assert!(big.truncated && !big.binary);
        // 真的二进制文件超过上限也还是二进制。
        assert!(snapshot.unstaged[2].binary && !snapshot.unstaged[2].truncated);
        assert_eq!(snapshot.unstaged[3].status, FileStatus::Untracked);
        assert!(snapshot.git_dir.ends_with(".git"));

        // 没变的未跟踪文件下次从缓存里取，删掉的不再留着。
        assert_eq!(cache.0.len(), 1);
        std::fs::remove_file(base.join("new.txt")).unwrap();
        super::snapshot(&base, &mut cache).unwrap();
        assert!(cache.0.is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
