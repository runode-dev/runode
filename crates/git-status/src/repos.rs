//! 主仓库和它工作树里的子仓库：`.gitmodules` 里登记而且检出了的子模块（子模块里还可以有
//! 子模块），以及没登记、自带 `.git` 的嵌套仓库。每个仓库各读一份 `Snapshot`。
//!
//! 嵌套仓库不另外遍历目录去找：`git status --untracked-files=all` 本来就要走一遍工作树找
//! 未跟踪的文件，它把未跟踪的文件一个个列出来，只有自带 `.git` 的目录整个报成一个目录，
//! 拿这个就够了。被忽略的目录（`node_modules`、`target` 之类）git 不往里走，里面的仓库也就
//! 不找，大目录不会因此变慢。子仓库里再找子仓库，最多找 `MAX_DEPTH` 层、`MAX_SUB_REPOS` 个。
//!
//! 同一个仓库的其他工作树（`git worktree list`）也各读一份，最多 `MAX_WORKTREES` 个；它们多半
//! 不在主仓库的目录里，只在 Git 面板里各占一块，不参与文件树和预览的标记。

use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{FileStatus, Found, Repo, Result, Snapshot, UntrackedCache, find_repo, git, ops::run, read_repo};

/// 子仓库最多往下找这么多层：主仓库里的子模块和嵌套仓库是第一层，它们里面的是第二层。
const MAX_DEPTH: usize = 3;
/// 最多读这么多个子仓库，再多的不读。
const MAX_SUB_REPOS: usize = 32;
/// 同时读的子仓库数；读一个仓库自己还要同时跑四五个 git 进程。
const PARALLEL: usize = 8;
/// 最多读这么多个别的工作树，再多的不读。
const MAX_WORKTREES: usize = 16;

/// 仓库是怎么来的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RepoKind {
    /// 读的起点所在的仓库。
    Main,
    /// 上一层仓库的 `.gitmodules` 里登记、已经检出的子模块。
    Submodule,
    /// 没登记、自带 `.git` 的嵌套仓库，在上一层仓库里是个未跟踪的目录。
    Nested,
    /// 同一个仓库的另一个工作树（`git worktree add` 出来的，或者主工作树）。
    Worktree,
}

/// 主仓库、它里面的子仓库，以及同一个仓库的其他工作树各自的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repos {
    pub main: Snapshot,
    /// 按 `Snapshot::prefix` 排，外层的总在它里面的子仓库前面。
    pub subs: Vec<Snapshot>,
    /// 同一个仓库的其他工作树，按根目录排；`Snapshot::prefix` 是它的根目录（绝对路径）。
    /// `locate`、`statuses` 这些按主仓库目录算的查询不管它们。
    pub worktrees: Vec<Snapshot>,
}

impl Repos {
    /// 只有主仓库。
    pub fn new(main: Snapshot) -> Self {
        Self { main, subs: Vec::new(), worktrees: Vec::new() }
    }

    /// 主仓库在前，然后是各个子仓库，最后是其他工作树。
    pub fn iter(&self) -> impl Iterator<Item = &Snapshot> {
        self.local().chain(&self.worktrees)
    }

    /// 主仓库目录里的仓库：主仓库和子仓库。
    fn local(&self) -> impl Iterator<Item = &Snapshot> {
        std::iter::once(&self.main).chain(&self.subs)
    }

    /// 第 `ix` 个仓库，0 是主仓库，和 `iter` 的顺序一样。
    pub fn get(&self, ix: usize) -> Option<&Snapshot> {
        self.iter().nth(ix)
    }

    /// 一共几个仓库，含主仓库和其他工作树。
    pub fn count(&self) -> usize {
        1 + self.subs.len() + self.worktrees.len()
    }

    /// 根目录是 `root` 的仓库在 `iter` 里的位置。
    pub fn position(&self, root: &Path) -> Option<usize> {
        self.iter().position(|repo| repo.root == root)
    }

    /// `rel` 相对主仓库根，落在哪个仓库里：最深的那个，以及相对它的根的路径。
    pub fn locate<'a>(&'a self, rel: &'a Path) -> (&'a Snapshot, &'a Path) {
        // 外层的排在前面，倒着找到的第一个就是最深的。
        self.subs
            .iter()
            .rev()
            .find_map(|sub| Some((sub, rel.strip_prefix(&sub.prefix).ok()?)))
            .unwrap_or((&self.main, rel))
    }

    /// `rel` 相对主仓库根，它或者它所在的目录被所在的仓库忽略。
    pub fn is_ignored(&self, rel: &Path) -> bool {
        let (repo, rel) = self.locate(rel);
        repo.is_ignored(rel)
    }

    /// 主仓库和子仓库里有改动的文件的状态，路径相对主仓库根。子模块那一项（记着的提交号变了）
    /// 和嵌套仓库那一项（未跟踪的目录）也在里面；其他工作树不算。
    pub fn statuses(&self) -> impl Iterator<Item = (PathBuf, FileStatus)> + '_ {
        self.local().flat_map(|repo| repo.statuses.iter().map(|(path, status)| (repo.prefix.join(path), *status)))
    }

    /// 主仓库和子仓库加了多少行、删了多少行，其他工作树不算。
    pub fn added(&self) -> usize {
        self.local().map(Snapshot::added).sum()
    }

    pub fn removed(&self) -> usize {
        self.local().map(Snapshot::removed).sum()
    }

    /// 主仓库和子仓库都没有改动，其他工作树不算。
    pub fn is_clean(&self) -> bool {
        self.local().all(Snapshot::is_clean)
    }
}

/// `git worktree list --porcelain -z` 里的一个工作树。
#[derive(Debug, PartialEq, Eq)]
struct WorktreeEntry {
    path: PathBuf,
    bare: bool,
    /// git 认为它的目录已经没了，等着 `git worktree prune`。
    prunable: bool,
}

/// 解析 `git worktree list --porcelain -z`：每项一行、以 NUL 结尾，工作树之间多一个空项。
fn parse_worktrees(output: &[u8]) -> Vec<WorktreeEntry> {
    let mut entries: Vec<WorktreeEntry> = Vec::new();
    for field in output.split(|b| *b == 0) {
        let field = String::from_utf8_lossy(field);
        if let Some(path) = field.strip_prefix("worktree ") {
            entries.push(WorktreeEntry { path: PathBuf::from(path), bare: false, prunable: false });
        } else if let Some(entry) = entries.last_mut() {
            if field == "bare" {
                entry.bare = true;
            } else if field == "prunable" || field.starts_with("prunable ") {
                entry.prunable = true;
            }
        }
    }
    entries
}

/// `main` 所在仓库的其他工作树的根目录：不含它自己、bare 的、目录已经没了的，以及 `skip` 里
/// 已经当子仓库读过的（工作树放在主仓库目录里又没被忽略时，它也是个嵌套仓库）。
fn other_worktrees(main: &Path, skip: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let Some(output) = git(main, &["worktree", "list", "--porcelain", "-z"]) else {
        return Vec::new();
    };
    let real = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let skip: HashSet<_> = skip.iter().map(|path| real(path)).collect();
    let main = real(main);
    let mut paths: Vec<_> = parse_worktrees(&output)
        .into_iter()
        .filter(|entry| !entry.bare && !entry.prunable && entry.path.is_dir())
        .map(|entry| entry.path)
        .filter(|path| {
            let real = real(path);
            real != main && !skip.contains(&real)
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_WORKTREES);
    paths
}

impl Repo {
    /// 删掉根目录是 `worktree` 的工作树（`git worktree remove`）。工作树里有改动或未跟踪的文件时
    /// git 拒绝，报它的错；`force` 时连改动一起删掉。主工作树删不了。
    pub fn remove_worktree(&self, worktree: &Path, force: bool) -> Result {
        let mut args = vec![std::ffi::OsStr::new("worktree"), std::ffi::OsStr::new("remove")];
        if force {
            args.push(std::ffi::OsStr::new("--force"));
        }
        args.extend([std::ffi::OsStr::new("--"), worktree.as_os_str()]);
        run(&self.root, args, None).map(drop)
    }
}

/// `snapshot_repos` 读哪些。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadOptions {
    /// 也读同一个仓库的其他工作树。它们只在 Git 面板里显示，面板没开时不必读。子模块和嵌套仓库
    /// 总是读：文件树和预览的标记要用。
    pub worktrees: bool,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self { worktrees: true }
    }
}

/// 读 `dir` 所在的仓库、它里面的子模块和嵌套仓库，以及（`options` 要的话）同一个仓库的其他
/// 工作树；`dir` 不在 git 仓库里或者没装 git 时为空。`dir` 在某个链接工作树里时主仓库就是那个
/// 工作树，主工作树算作其他工作树之一。读不了的子仓库跳过。没变过的未跟踪文件从 `cache` 里取，
/// 读完后 `cache` 只留这次还在的。
pub fn snapshot_repos(dir: &Path, cache: &mut UntrackedCache, options: ReadOptions) -> Option<Repos> {
    let (root, git_dir) = find_repo(dir)?;
    let mut seen = UntrackedCache::default();
    let found = read_repo(root, git_dir, PathBuf::new(), RepoKind::Main, cache, &mut seen)?;
    let mut visited = HashSet::from([found.snapshot.root.clone()]);
    let mut level = children(&found, &mut visited);
    let main = found.snapshot;
    let mut subs = Vec::new();
    for _ in 0..MAX_DEPTH {
        level.truncate(MAX_SUB_REPOS - subs.len());
        if level.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for found in read_all(&level, cache, &mut seen) {
            next.extend(children(&found, &mut visited));
            subs.push(found.snapshot);
        }
        level = next;
    }
    // 别的工作树里的子模块不再往下找。
    let worktrees: Vec<_> = if options.worktrees { other_worktrees(&main.root, &visited) } else { Vec::new() }
        .into_iter()
        .map(|root| (root.clone(), root, RepoKind::Worktree))
        .collect();
    let mut worktrees: Vec<_> =
        read_all(&worktrees, cache, &mut seen).into_iter().map(|found| found.snapshot).collect();
    *cache = seen;
    subs.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    worktrees.sort_by(|a, b| a.root.cmp(&b.root));
    Some(Repos { main, subs, worktrees })
}

/// 读 `repos`（根目录、`Snapshot::prefix` 和来历）这些仓库，每批同时读 `PARALLEL` 个；读不了的
/// 跳过。读过的未跟踪文件记进 `seen`。
fn read_all(repos: &[(PathBuf, PathBuf, RepoKind)], cache: &UntrackedCache, seen: &mut UntrackedCache) -> Vec<Found> {
    let mut all = Vec::new();
    for batch in repos.chunks(PARALLEL) {
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(root, prefix, kind)| {
                    scope.spawn(move || {
                        let mut seen = UntrackedCache::default();
                        let found = git_dir_of(root).and_then(|git_dir| {
                            read_repo(root.clone(), git_dir, prefix.clone(), *kind, cache, &mut seen)
                        });
                        (found, seen)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
                .collect()
        });
        for (found, batch_seen) in results {
            seen.0.extend(batch_seen.0);
            all.extend(found);
        }
    }
    all
}

/// `found` 里找到的子仓库：根目录、相对主仓库根的路径和来历。已经读过的不再要。
fn children(found: &Found, visited: &mut HashSet<PathBuf>) -> Vec<(PathBuf, PathBuf, RepoKind)> {
    let snapshot = &found.snapshot;
    found
        .children
        .iter()
        .map(|(rel, kind)| (snapshot.root.join(rel), snapshot.prefix.join(rel), *kind))
        .filter(|(root, _, _)| visited.insert(root.clone()))
        .collect()
}

/// 根目录是 `root` 的仓库的 git 目录：`.git` 是目录时就是它；子模块和 worktree 的 `.git` 是个
/// 文件，写着 `gitdir: 路径`。
fn git_dir_of(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    let dir = if dot.is_dir() {
        dot
    } else {
        let text = fs::read_to_string(&dot).ok()?;
        let dir = root.join(text.lines().next()?.strip_prefix("gitdir:")?.trim());
        fs::canonicalize(&dir).unwrap_or(dir)
    };
    // 空的或者坏了的 `.git` 不算：git 认不出来时会往上找，把外层的仓库当成它读。
    dir.join("HEAD").is_file().then_some(dir)
}

/// `root` 的 `.gitmodules` 里登记、已经检出的子模块，相对 `root`。没检出的只是个空目录，不算。
pub(crate) fn submodules(root: &Path) -> Vec<PathBuf> {
    if !root.join(".gitmodules").is_file() {
        return Vec::new();
    }
    let args = ["config", "-z", "--file", ".gitmodules", "--get-regexp", r"^submodule\..*\.path$"];
    let output = git(root, &args).unwrap_or_default();
    // 每项是「键\n值」，项与项之间是 NUL。
    let mut paths: Vec<PathBuf> = output
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            let path = PathBuf::from(entry.split_once('\n')?.1.trim_end_matches('/'));
            let normal = path.components().next().is_some()
                && path.components().all(|part| matches!(part, Component::Normal(_)));
            (normal && git_dir_of(&root.join(&path)).is_some()).then_some(path)
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_list() {
        let output = b"worktree /repo\0HEAD abc\0branch refs/heads/main\0\0worktree /bare.git\0bare\0\0worktree /gone\0HEAD def\0detached\0prunable gitdir file points to non-existent location\0\0worktree /wt\0HEAD def\0detached\0locked\0\0";
        let entries = parse_worktrees(output);
        let summary: Vec<_> = entries.iter().map(|e| (e.path.to_str().unwrap(), e.bare, e.prunable)).collect();
        assert_eq!(
            summary,
            [("/repo", false, false), ("/bare.git", true, false), ("/gone", false, true), ("/wt", false, false)]
        );
    }
}
