//! 一个仓库的状态：改动分的两段、文件状态和逐行改动这些数据，读一个仓库（`snapshot`、
//! `read_repo`），以及把未跟踪的文件当作整个新增读进来。

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    Repo, RepoInfo, RepoKind, filter_overrides, find_repo, git, git_with, info,
    parse::{expand_tabs, parse_diff, parse_status, status_sections},
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
    /// 改动超过 `MAX_FILE_LINES` 行，或者是没读内容的未跟踪文件，或者 diff 读不出来、只从 `git status`
    /// 知道它改了，`hunks` 不全。
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
/// 那些改动在子模块自己的那份里，在这里既暂存不了也丢不掉；子模块的改动钉成 `diff --git` 的格式，
/// 用户配了 `diff.submodule=log` 时 git 给的是读不出来的摘要。路径里非 ASCII 的字节按八进制转义：
/// 输出要当文本读，原样给的不是 UTF-8 的文件名会被换成 U+FFFD，转义了才能由 `unquote` 还原成原来的字节。
pub(crate) fn diff_args(extra: &[&str]) -> Vec<String> {
    let threshold = format!("core.bigFileThreshold={MAX_DIFF_BYTES}");
    let mut args = vec![
        "-c",
        &threshold,
        "-c",
        "core.quotePath=true",
        // 用户配了 `diff.suppressBlankEmpty` 时空的上下文行前面没有空格，`parse_diff` 认不出，后面的行号全错。
        "-c",
        "diff.suppressBlankEmpty=false",
        "diff",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
    ];
    // 上下文钉成三行：仓库配了 `diff.context=0` 时块里没有上下文，`git apply` 不收按块暂存拼出的补丁。
    args.extend([
        "--unified=3",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--ignore-submodules=dirty",
        "--submodule=short",
    ]);
    args.extend(extra);
    args.into_iter().map(str::to_owned).collect()
}

/// `repo` 里的 `git diff`，参数见 `diff_args`，`filters` 是 `filter_overrides` 给的环境变量；
/// git 报错时为空，不当成没有改动。
pub(crate) fn diff(repo: &Path, filters: &[(String, String)], args: &[impl AsRef<OsStr>]) -> Option<Vec<FileDiff>> {
    let full: Vec<OsString> =
        diff_args(&[]).into_iter().map(OsString::from).chain(args.iter().map(|arg| arg.as_ref().to_owned())).collect();
    let mut files = parse_diff(&String::from_utf8_lossy(&git_with(repo, filters, &full)?));
    // 超过大小上限的文本文件也被报成二进制：工作区里的文件超过上限、开头又没有 NUL 字节的
    // 认回来，提示改动太多。暂存段也按工作区里的文件认，暂存后又改过的可能认错，只影响提示。
    for file in files.iter_mut().filter(|file| file.binary) {
        let full = repo.join(&file.path);
        if fs::metadata(&full).is_ok_and(|meta| meta.len() > MAX_DIFF_BYTES) && !starts_binary(&full) {
            file.binary = false;
            file.truncated = true;
        }
    }
    Some(files)
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
/// 而且检出了的子模块也记进去。`git status` 读不出来时为空。
pub(crate) fn read_repo(
    root: PathBuf,
    git_dir: PathBuf,
    prefix: PathBuf,
    kind: RepoKind,
    cache: &UntrackedCache,
    seen: &mut UntrackedCache,
) -> Option<Found> {
    let repo = root.as_path();
    let filters = filter_overrides(repo);
    // 状态、暂存段和未暂存段互不依赖，几个 git 进程同时跑；stash、远端和子模块也一起读。
    let (status, staged, unstaged, extra, submodules) = std::thread::scope(|scope| {
        let status = scope.spawn(|| {
            // 开头多一项 `## 分支...上游 [ahead 1, behind 2]`。里面有改动的子模块也报出来，连同没
            // 写进 `.gitmodules` 的 gitlink，据此认出子仓库；列进改动的条目以 `diff` 为准。
            // shortcut: status 递归进子模块时，子模块自己配置的过滤器不在覆盖之列；要挡就得连子模块的配置
            // 一起列出来，或者把 --ignore-submodules 改成 dirty。
            let args = [
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
                "--ignore-submodules=none",
                "--branch",
                "--ahead-behind",
            ];
            git_with(repo, &filters, &args)
        });
        let staged = scope.spawn(|| {
            // 还没有提交时 `--cached` 自己和空树比，暂存了的新文件也算进来。短哈希给 `RepoInfo::head`。
            let head = git(repo, &["rev-parse", "--verify", "--quiet", "--short", "HEAD"])
                .map(|head| String::from_utf8_lossy(&head).trim().to_owned());
            (diff(repo, &filters, &["--cached"]), head)
        });
        let extra = scope.spawn(|| info::read_extra(repo));
        let submodules = scope.spawn(|| repos::submodules(repo));
        // 冲突的文件和「我方」比，不然 git 给的是三方合并的格式。
        let unstaged = diff(repo, &filters, &["-2"]);
        let status = status.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        let staged = staged.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        let extra = extra.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        let submodules = submodules.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (status, staged, unstaged, extra, submodules)
    });
    let status = status?;
    let (mut statuses, ignored) = parse_status(&status);
    let (staged, head) = staged;
    // 哪一段 diff 读不出来（比如对象库里丢了 blob）就改用 `git status` 列出的文件，只有路径和状态：
    // 当成没有改动会把有改动的仓库报成干净的，整个仓库当成读不出来又会被当成不在仓库里。
    // 哪些是 gitlink 以 git 记着的模式为准，不看工作区长什么样：没检出的子模块没有 `.git`，被嵌套仓库
    // 换掉的普通文件、指向别的仓库的符号链接却像子模块。
    let listed = |section| {
        let (staged, unstaged) = status_sections(&status);
        let index = recorded_gitlinks(repo, None);
        match section {
            Section::Staged => {
                // 暂存了删除的不在暂存区里，看 HEAD 里记的。
                let removed = staged.iter().any(|file| file.status == FileStatus::Deleted);
                let head = if removed { recorded_gitlinks(repo, Some("HEAD")) } else { HashMap::new() };
                staged
                    .into_iter()
                    .map(|mut file| {
                        file.gitlink = index.contains_key(&file.path) || head.contains_key(&file.path);
                        file
                    })
                    .collect::<Vec<_>>()
            }
            Section::Unstaged => unstaged
                .into_iter()
                .filter_map(|mut file| {
                    let Some(recorded) = index.get(&file.path) else {
                        return Some(file);
                    };
                    file.gitlink = true;
                    if file.status == FileStatus::Deleted {
                        return Some(file);
                    }
                    // porcelain v1 分不清子模块是换了提交号还是只是里面有改动：拿子模块检出的提交和
                    // 暂存区里记的比，不一样才是这个仓库的改动。没检出的子模块换不了提交号；不是目录时
                    // 不进去问，免得问到外面这个仓库的 HEAD。
                    let path = repo.join(&file.path);
                    let checked_out =
                        fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) && path.join(".git").exists();
                    let current = checked_out.then(|| git(&path, &["rev-parse", "--verify", "--quiet", "HEAD"]));
                    current
                        .flatten()
                        .is_some_and(|current| String::from_utf8_lossy(&current).trim() != recorded)
                        .then_some(file)
                })
                .collect::<Vec<_>>(),
        }
    };
    let mut staged = staged.unwrap_or_else(|| listed(Section::Staged));
    let mut unstaged = unstaged.unwrap_or_else(|| listed(Section::Unstaged));
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
    // 缺「我方」版本的冲突（我方删了、对方改了这类）`git diff -2` 只报一句 Unmerged path，读不出 diff：
    // 照样列进未暂存段，面板里才看得到、能暂存成已解决，不然只剩这种冲突时面板说没有改动。
    for (path, status) in &statuses {
        if *status == FileStatus::Conflicted && !unstaged.iter().any(|file| &file.path == path) {
            unstaged.push(FileDiff {
                path: path.clone(),
                old_path: None,
                status: FileStatus::Conflicted,
                added: 0,
                removed: 0,
                hunks: Vec::new(),
                binary: false,
                truncated: false,
                gitlink: false,
            });
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

/// git 记着的 gitlink（模式 160000）和它记的提交号：`tree` 为空时读暂存区，否则读那棵树。读不出来时为空。
fn recorded_gitlinks(repo: &Path, tree: Option<&str>) -> HashMap<PathBuf, String> {
    let output = match tree {
        None => git(repo, &["ls-files", "--stage", "-z"]),
        Some(tree) => git(repo, &["ls-tree", "-r", "-z", tree]),
    };
    // 暂存区一行是「模式 提交号 阶段\t路径」，树是「模式 类型 提交号\t路径」。
    let oid_field = if tree.is_some() { 2 } else { 1 };
    output
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let tab = entry.iter().position(|b| *b == b'\t')?;
            let mut fields = entry[..tab].split(|b| *b == b' ');
            if fields.next()? != b"160000" {
                return None;
            }
            let oid = fields.nth(oid_field - 1)?;
            let path = PathBuf::from(std::ffi::OsString::from_vec(entry[tab + 1..].to_vec()));
            Some((path, String::from_utf8_lossy(oid).into_owned()))
        })
        .collect()
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
    // 调用方按大小决定读不读，读的时候文件又变大了的，读到上限为止、当作没读。
    let content = read
        .then(|| worktree_bytes(full, MAX_DIFF_BYTES))
        .flatten()
        .filter(|content| content.len() as u64 <= MAX_DIFF_BYTES);
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
/// 会把内存读光，指向命名管道的会一直等着）；管道、设备这些不是普通文件的读不到，为空。最多读
/// `max_bytes` 加一个字节：先看过大小、读的时候文件又变大了也不整个读进内存，多出的那一个字节让
/// 调用方认出它超了。
pub(crate) fn worktree_bytes(full: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let meta = fs::symlink_metadata(full).ok()?;
    if meta.file_type().is_symlink() {
        return fs::read_link(full).ok().map(|target| target.into_os_string().into_vec());
    }
    if !meta.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    open_regular(full).ok()?.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// `O_NONBLOCK` 的值，和 libc 里的一样（标准库没有这个常量）；没列出的平台为空，照旧阻塞地打开。
const O_NONBLOCK: Option<i32> = if cfg!(target_vendor = "apple") {
    Some(0x0004)
} else if cfg!(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
)) {
    Some(0o4000)
} else {
    None
};

/// `O_NOFOLLOW` 的值，和 libc 里的一样；Linux 上各架构不同。没列出的平台为空，照旧跟着符号链接打开。
const O_NOFOLLOW: Option<i32> = if cfg!(target_vendor = "apple") {
    Some(0x0100)
} else if cfg!(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
)) {
    Some(0o400000)
} else if cfg!(all(target_os = "linux", any(target_arch = "arm", target_arch = "aarch64", target_arch = "powerpc64"))) {
    Some(0o100000)
} else {
    None
};

/// 打开 `path`，打开的不是普通文件时报错。调用方先看过路径是普通文件，可看过以后路径还可能被换成
/// 别的（命名管道、指向设备的符号链接），所以打开以后对着打开的这个再看一次，读的才一定是普通文件。
/// 不阻塞地打开：换成了没有写端的命名管道时阻塞式的打开一直等，走不到这次检查；对普通文件这个标志
/// 不起作用。也不跟着符号链接打开，换成了符号链接时报错，和 `worktree_bytes` 不跟过去读的规矩一致。
fn open_regular(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    let flags = O_NONBLOCK.unwrap_or(0) | O_NOFOLLOW.unwrap_or(0);
    if flags != 0 {
        options.custom_flags(flags);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_at_most_one_byte_past_the_limit() {
        let path = std::env::temp_dir().join(format!("runode-git-bounded-{}", std::process::id()));
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(worktree_bytes(&path, 3).unwrap(), b"0123");
        assert_eq!(worktree_bytes(&path, 100).unwrap(), b"0123456789");
        fs::remove_file(&path).unwrap();
    }

    /// 看过是普通文件以后路径被换成没有写端的命名管道或符号链接：打开不阻塞、不跟过去，报错。
    #[test]
    fn open_regular_rejects_a_fifo_or_symlink_swapped_in() {
        use std::{process::Command, sync::mpsc, thread, time::Duration};

        let dir = std::env::temp_dir().join(format!("runode-git-fifo-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("a.txt");
        fs::remove_file(&fifo).ok();
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let (tx, rx) = mpsc::channel();
        let path = fifo.clone();
        thread::spawn(move || tx.send(open_regular(&path).map(drop)).ok());
        let opened = rx.recv_timeout(Duration::from_secs(5)).expect("打开命名管道阻塞了");
        assert!(opened.is_err());
        // 指向命名管道的符号链接同样不阻塞；认得 `O_NOFOLLOW` 的平台上指向普通文件的也不跟过去。
        let link = dir.join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        let (tx, rx) = mpsc::channel();
        let path = link.clone();
        thread::spawn(move || tx.send(open_regular(&path).map(drop)).ok());
        let opened = rx.recv_timeout(Duration::from_secs(5)).expect("打开指向命名管道的符号链接阻塞了");
        assert!(opened.is_err());
        let file = dir.join("b.txt");
        fs::write(&file, b"b").unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(open_regular(&file).is_ok());
        assert_eq!(open_regular(&link).is_err(), O_NOFOLLOW.is_some());
        fs::remove_dir_all(&dir).ok();
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
}
