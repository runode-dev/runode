//! 项目目录的 git：读工作区相对 HEAD 的逐行改动、每个文件的状态、分支和 stash，供右侧的
//! 文件树、预览栏和 Git 面板使用；主仓库里的子模块和嵌套仓库各读一份，见 `snapshot_repos`。
//! 暂存、按块暂存、提交、切分支、stash 和同步远端这些写操作，以及读提交历史和图表（`graph`），
//! 挂在 `Repo` 上；预览栏看一个文件的整篇 diff 见 `view`。一律调 `git` 命令行，不直接读写 git
//! 目录里的对象。

mod branch;
mod graph;
mod info;
mod ops;
mod patch;
mod repos;
mod view;

pub use branch::{Branch, valid_branch_name};
pub use graph::{Commit, CommitRef, GraphLine, GraphRow, Half, History, RefKind, graph_layout, refs_changed};
pub use info::{Operation, RepoInfo, Stash};
pub use ops::{CommitOptions, GitError, Repo, Result};
pub use patch::{HunkAction, hunk_actionable};
pub use repos::{ReadOptions, RepoKind, Repos, snapshot_repos};
pub use view::{DiffRow, DiffSide, DiffView, merge_rows};

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

/// 一个文件最多读这么多行改动，再多的只记总数。
const MAX_FILE_LINES: usize = 3000;
/// 未跟踪的文件比这大时不读内容。
const MAX_UNTRACKED_BYTES: u64 = 256 * 1024;
/// 未跟踪的文件最多读这么多个的内容，其余只列出来。
const MAX_UNTRACKED_FILES: usize = 200;
/// 比这大的文件 git 不算逐行改动，当二进制报，免得一个生成的大文件拖慢整次读取。
const MAX_DIFF_BYTES: u64 = 1024 * 1024;

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
pub struct UntrackedCache(HashMap<PathBuf, (u64, SystemTime, FileDiff)>);

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

/// 读 `dir` 所在仓库的状态，只读这一个仓库，子模块和嵌套的仓库不往里看；`dir` 不在 git
/// 仓库里或者没装 git 时为空。没变过的未跟踪文件从 `cache` 里取，读完后 `cache` 只留这次还在的。
pub fn snapshot(dir: &Path, cache: &mut UntrackedCache) -> Option<Snapshot> {
    let (root, git_dir) = find_repo(dir)?;
    let mut seen = UntrackedCache::default();
    let found = read_repo(root, git_dir, PathBuf::new(), RepoKind::Main, cache, &mut seen)?;
    *cache = seen;
    Some(found.snapshot)
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

/// `repo` 里的 `git diff`，再加上 `args`。前缀写明，免得用户配置了 `diff.noprefix` 之类
/// 改掉 `a/`、`b/`。子模块只在记着的提交号变了时算改动，里面改了文件不算：那些改动在子模块
/// 自己的那份里，在这里既暂存不了也丢不掉。
fn diff(repo: &Path, args: &[&str]) -> Vec<FileDiff> {
    let threshold = format!("core.bigFileThreshold={MAX_DIFF_BYTES}");
    let mut full = vec!["-c", &threshold, "diff", "-M", "--no-color", "--no-ext-diff", "--no-textconv"];
    full.extend(["--src-prefix=a/", "--dst-prefix=b/", "--ignore-submodules=dirty"]);
    full.extend(args);
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
            // 还没有提交时和空树比，暂存了的新文件也算进来。短哈希顺带给 `RepoInfo::head`。
            let head = git(repo, &["rev-parse", "--verify", "--quiet", "--short", "HEAD"])
                .map(|head| String::from_utf8_lossy(&head).trim().to_owned());
            let base = match head {
                Some(_) => "HEAD".to_owned(),
                None => {
                    String::from_utf8_lossy(&git(repo, &["hash-object", "-t", "tree", "/dev/null"])?).trim().to_owned()
                }
            };
            Some((diff(repo, &["--cached", &base]), head))
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
    let (mut staged, head) = staged?;
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

/// 解析 `git status --porcelain=v1 -z` 的输出：各个文件的状态，以及被忽略的路径。带
/// `--branch` 时开头的分支那一项跳过，由 `info::parse_branch` 读。
fn parse_status(output: &[u8]) -> (HashMap<PathBuf, FileStatus>, Vec<PathBuf>) {
    let mut statuses = HashMap::new();
    let mut ignored = Vec::new();
    let mut fields = output.split(|b| *b == 0).filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 || field.starts_with(b"## ") {
            continue;
        }
        let (x, y) = (field[0], field[1]);
        let path = PathBuf::from(String::from_utf8_lossy(&field[3..]).trim_end_matches('/'));
        // 改名和复制后面跟着原来的路径，单独占一项。
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            fields.next();
        }
        let status = match (x, y) {
            (b'!', b'!') => {
                ignored.push(path);
                continue;
            }
            (b'?', b'?') => FileStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileStatus::Conflicted,
            (b'D', _) | (_, b'D') => FileStatus::Deleted,
            (b'R', _) | (_, b'R') => FileStatus::Renamed,
            (b'A', _) => FileStatus::Added,
            _ => FileStatus::Modified,
        };
        statuses.insert(path, status);
    }
    (statuses, ignored)
}

/// 解析 `git diff` 的统一格式输出。
fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    // 当前块里下一行的旧行号和新行号。
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    // 当前块的头有没有记下来；超过行数上限后不再记新块，它的行也不能并进上一个块。
    let mut hunk_kept = false;
    // 不是 `diff --git` 开头的文件（三方合并的格式之类）整个跳过。
    let mut skipping = false;
    for line in text.lines() {
        if line.starts_with("diff ") && !line.starts_with("diff --git ") {
            skipping = true;
            continue;
        }
        if let Some(header) = line.strip_prefix("diff --git ") {
            skipping = false;
            files.push(FileDiff {
                path: header_path(header).unwrap_or_default(),
                old_path: None,
                status: FileStatus::Modified,
                added: 0,
                removed: 0,
                hunks: Vec::new(),
                binary: false,
                truncated: false,
                gitlink: false,
            });
            in_hunk = false;
            continue;
        }
        if skipping {
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if !in_hunk {
            // 文件头里的模式是 160000 的是 gitlink：`index 旧..新 160000`、`new file mode 160000`。
            if (line.starts_with("index ") || line.contains(" mode ")) && line.ends_with(" 160000") {
                file.gitlink = true;
            }
            if line.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = Some(unquote(from).into());
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.path = unquote(to).into();
            } else if line.starts_with("Binary files ") {
                file.binary = true;
            } else if let Some(to) = line.strip_prefix("+++ ") {
                // 路径里有空格时 git 在行尾补一个制表符。
                if let Some(path) = unquote(to.trim_end_matches('\t')).strip_prefix("b/") {
                    file.path = path.into();
                }
            } else if let Some(from) = line.strip_prefix("--- ") {
                // 删掉的文件新路径是 /dev/null，用旧路径。
                if let Some(path) = unquote(from.trim_end_matches('\t')).strip_prefix("a/") {
                    file.path = path.into();
                }
            }
        }
        if let Some(range) = line.strip_prefix("@@ ") {
            let (o, n) = parse_hunk_range(range);
            (old, new) = (o, n);
            in_hunk = true;
            hunk_kept = file.added + file.removed < MAX_FILE_LINES;
            if hunk_kept {
                file.hunks.push(Hunk { header: line.to_owned(), lines: Vec::new() });
            } else {
                file.truncated = true;
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            // 「\ No newline at end of file」之类的说明。
            _ => continue,
        };
        let numbers = match kind {
            LineKind::Added => (None, Some(new)),
            LineKind::Removed => (Some(old), None),
            LineKind::Context => (Some(old), Some(new)),
        };
        if kind != LineKind::Added {
            old += 1;
        }
        if kind != LineKind::Removed {
            new += 1;
        }
        match kind {
            LineKind::Added => file.added += 1,
            LineKind::Removed => file.removed += 1,
            LineKind::Context => {}
        }
        if file.added + file.removed > MAX_FILE_LINES {
            file.truncated = true;
            continue;
        }
        if !hunk_kept {
            continue;
        }
        if let Some(hunk) = file.hunks.last_mut() {
            hunk.lines.push(Line { kind, old: numbers.0, new: numbers.1, text: expand_tabs(text) });
        }
    }
    files
}

/// `-a,b +c,d @@ ...` 里的起始行号 `a` 和 `c`。
fn parse_hunk_range(range: &str) -> (u32, u32) {
    let mut parts = range.split(' ');
    let start = |part: Option<&str>, sign: char| {
        part.and_then(|part| part.strip_prefix(sign))
            .and_then(|part| part.split(',').next())
            .and_then(|start| start.parse().ok())
            .unwrap_or(0)
    };
    let old = start(parts.next(), '-');
    let new = start(parts.next(), '+');
    (old, new)
}

/// `diff --git a/路径 b/路径` 里的路径。只在新旧路径相同时可靠，改名时由后面的
/// `rename to` 或 `+++` 行纠正。
fn header_path(header: &str) -> Option<PathBuf> {
    if let Some(rest) = header.strip_prefix('"') {
        let end = rest.find("\" ")? + 2;
        return unquote(&header[..end]).strip_prefix("a/").map(PathBuf::from);
    }
    // 两个路径一样长：`a/P b/P` 共 2P+5 个字节。
    let len = header.len().checked_sub(5)? / 2;
    let (a, b) = (header.get(2..2 + len)?, header.get(header.len() - len..)?);
    (a == b).then(|| PathBuf::from(a))
}

/// 去掉 git 给特殊路径加的双引号和 C 风格转义。
fn unquote(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|text| text.strip_suffix('"')) else {
        return text.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    if let Some(next) = chars.next_if(|b| (b'0'..=b'7').contains(b)) {
                        value = value * 8 + u32::from(next - b'0');
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn expand_tabs(text: &str) -> String {
    text.replace('\t', "    ")
}

/// 未跟踪的文件当作整个新增；`read` 为假或者文件太大时不读内容。大小和修改时间都没变时
/// 用 `cache` 里上次读的，读过的记进 `seen`。
fn untracked_diff(
    root: &Path,
    path: PathBuf,
    read: bool,
    cache: &UntrackedCache,
    seen: &mut UntrackedCache,
) -> FileDiff {
    let full = root.join(&path);
    let meta = fs::metadata(&full).ok();
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

fn read_untracked(full: &Path, path: PathBuf, read: bool) -> FileDiff {
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
    let content = read.then(|| fs::read(full).ok()).flatten();
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

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -2,3 +2,4 @@ fn main() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
 }
\\ No newline at end of file
diff --git a/old name.txt b/new name.txt
similarity index 100%
rename from old name.txt
rename to new name.txt
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..4444444
Binary files /dev/null and b/logo.png differ
";

    #[test]
    fn parses_unified_diff() {
        let files = parse_diff(DIFF);
        assert_eq!(files.len(), 4);

        let main = &files[0];
        assert_eq!(main.path, PathBuf::from("src/main.rs"));
        assert_eq!((main.status, main.added, main.removed), (FileStatus::Modified, 2, 1));
        let lines = &main.hunks[0].lines;
        assert_eq!(main.hunks[0].header, "@@ -2,3 +2,4 @@ fn main() {");
        assert_eq!(lines.len(), 5);
        assert_eq!((lines[0].old, lines[0].new), (Some(2), Some(2)));
        assert_eq!((lines[1].kind, lines[1].old, lines[1].new), (LineKind::Removed, Some(3), None));
        assert_eq!((lines[2].kind, lines[2].old, lines[2].new), (LineKind::Added, None, Some(3)));
        assert_eq!((lines[4].old, lines[4].new), (Some(4), Some(5)));
        assert_eq!(lines[2].text, "let b = 3;");

        let renamed = &files[1];
        assert_eq!(renamed.status, FileStatus::Renamed);
        assert_eq!(renamed.path, PathBuf::from("new name.txt"));
        assert_eq!(renamed.old_path, Some(PathBuf::from("old name.txt")));

        let deleted = &files[2];
        assert_eq!(
            (deleted.status, deleted.path.clone(), deleted.removed),
            (FileStatus::Deleted, "gone.txt".into(), 1)
        );

        let binary = &files[3];
        assert!(binary.binary);
        assert_eq!((binary.status, binary.path.clone()), (FileStatus::Added, "logo.png".into()));
    }

    #[test]
    fn strips_the_tab_git_adds_after_paths_with_spaces() {
        let files =
            parse_diff("diff --git a/a b.txt b/a b.txt\n--- a/a b.txt\t\n+++ b/a b.txt\t\n@@ -1 +1,2 @@\n a\n+b\n");
        assert_eq!(files[0].path, PathBuf::from("a b.txt"));
    }

    #[test]
    fn parses_porcelain_status() {
        let output = b"## main...origin/main [ahead 1]\0 M src/a.rs\0?? new.txt\0R  b.rs\0a.rs\0!! target/\0UU both.rs\0A  added.rs\0";
        let (statuses, ignored) = parse_status(output);
        assert_eq!(statuses[Path::new("src/a.rs")], FileStatus::Modified);
        assert_eq!(statuses[Path::new("new.txt")], FileStatus::Untracked);
        assert_eq!(statuses[Path::new("b.rs")], FileStatus::Renamed);
        assert_eq!(statuses[Path::new("both.rs")], FileStatus::Conflicted);
        assert_eq!(statuses[Path::new("added.rs")], FileStatus::Added);
        assert!(!statuses.contains_key(Path::new("a.rs")));
        assert_eq!(statuses.len(), 5);
        assert_eq!(ignored, vec![PathBuf::from("target")]);
    }

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

    #[test]
    fn unquotes_paths() {
        assert_eq!(unquote("\"a\\\"b\\tc\""), "a\"b\tc");
        assert_eq!(unquote("\"\\346\\226\\207.txt\""), "文.txt");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(header_path("a/x y.txt b/x y.txt"), Some(PathBuf::from("x y.txt")));
        assert_eq!(header_path("\"a/q\\\"x\" \"b/q\\\"x\""), Some(PathBuf::from("q\"x")));
    }
}
