//! Git 面板排成的行：每个仓库的冲突、已暂存、未暂存三段改动、储藏和提交历史的图表，各段可以
//! 收起；展开的提交下面跟着它改的文件。文件的改动不在面板里展开，点了在预览栏里看整篇 diff。有
//! 子仓库时每个仓库一块，块头也占一行。只管数据，不碰界面。

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{Entity, ListAlignment, ListState, Subscription, UniformListScrollHandle, px};
use runode_git::{self as git, FileStatus, RepoKind, Section};

use super::tree::{TreeItem, file_tree};
use crate::ui::text_area::TextArea;

/// 多个仓库时的列表往可见区域外多画这么高，滚动时块头不至于一下子冒出来。
const LIST_OVERDRAW: f32 = 200.;

/// Git 面板里的一段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::window) enum GitSection {
    /// 有冲突、等着解决的文件，从未暂存的改动里挑出来放在最前面。
    Merge,
    Staged,
    Unstaged,
    Stashes,
    /// 提交历史的图表，在每个仓库的最后。展开着才读历史。
    Graph,
}

impl GitSection {
    /// 这一段里的文件来自 `git::Snapshot` 的哪一段。
    pub fn source(self) -> Section {
        match self {
            Self::Staged => Section::Staged,
            Self::Merge | Self::Unstaged | Self::Stashes | Self::Graph => Section::Unstaged,
        }
    }
}

/// 文件在 Git 面板里归哪一段：未暂存的冲突文件单独成段。
pub(in crate::window) fn section_of(file: &git::FileDiff, section: Section) -> GitSection {
    match section {
        Section::Staged => GitSection::Staged,
        Section::Unstaged if file.status == FileStatus::Conflicted => GitSection::Merge,
        Section::Unstaged => GitSection::Unstaged,
    }
}

/// Git 面板里的一行。第一个下标是仓库在 `git::Repos::iter` 里的位置（0 是主仓库），后面的指向
/// 那个仓库的 `git::Snapshot` 那一段里的文件、块和行，或者储藏列表。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum GitRow {
    /// 多个仓库时每块开头：仓库的标题，展开时连着分支栏和提交说明框，高度不固定。只有一个
    /// 仓库时没有这一行，分支栏和提交说明框固定在列表上面。
    Repo(usize),
    Section(usize, GitSection),
    /// 以树形式查看时的目录，下标指向 `GitPanel::dirs`；冲突、已暂存、未暂存各段和展开的提交下面
    /// 的文件都可能有。
    Dir(usize, usize),
    File(usize, Section, usize),
    Stash(usize, usize),
    /// 只有一个仓库、又没有改动时的一句说明。
    Clean(usize),
    /// 图表里的提交，下标指向 `Graph::history` 里的提交；展开后跟着它改的文件，下标指向
    /// `Graph::changes` 里这个提交的文件。
    Commit(usize, usize),
    CommitFile(usize, usize, usize),
    CommitNote(usize, usize, CommitNote),
    /// 图表里不是提交的那一行：在读、读不了、没有提交，或者「加载更多」。
    GraphNote(usize, GraphNote),
}

impl GitRow {
    /// 这一行属于第几个仓库。
    pub fn repo(self) -> usize {
        match self {
            Self::Repo(ri)
            | Self::Section(ri, _)
            | Self::Dir(ri, _)
            | Self::File(ri, ..)
            | Self::Stash(ri, _)
            | Self::Clean(ri)
            | Self::Commit(ri, _)
            | Self::CommitFile(ri, ..)
            | Self::CommitNote(ri, ..)
            | Self::GraphNote(ri, _) => ri,
        }
    }
}

/// 图表里不是提交的那一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum GraphNote {
    Loading,
    Failed,
    Empty,
    /// 后面还有更早的提交，点了多读一页。
    More,
}

/// 展开的提交下面不列文件时的说明。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum CommitNote {
    Loading,
    Failed,
    /// 没改文件，比如空提交。
    Empty,
}

/// 图表一次多读这么多个提交。
pub(in crate::window) const GRAPH_PAGE: usize = 50;

/// 读到的一个提交改了什么。
pub(in crate::window) enum CommitChanges {
    Loading,
    Failed,
    Ready(Vec<git::FileDiff>),
}

/// 一个仓库的图表：读到的历史、读到哪儿了，以及展开了哪些提交和文件。历史不跟着每次扫描读，
/// 图表展开着、而且仓库的 HEAD、分支、上游或 stash 变了（`RepoInfo` 变了）或者点了刷新时才重读。
#[derive(Default)]
pub(in crate::window) struct Graph {
    /// 读到的历史，读不了时是错误；还没读过时为空。
    pub history: Option<Result<git::History, String>>,
    /// 读到的图最多用到几条 lane，各行按它定 lane 的宽度，线才连得上。
    pub lanes: usize,
    /// 要读多少个提交，加载更多时加一页；为零时按一页算。
    pub limit: usize,
    pub loading: bool,
    /// 上次读的时候仓库的 `RepoInfo`，和现在的不一样就重读。
    pub read_for: Option<git::RepoInfo>,
    /// 点了刷新，下次显示时重读。
    pub stale: bool,
    /// 展开的提交，按提交号记。
    pub expanded: HashSet<String>,
    /// 以树形式查看时收起的目录（提交号、相对仓库根的路径）。
    pub collapsed_dirs: HashSet<(String, PathBuf)>,
    /// 展开过的提交改了什么，按提交号记；收起时不再留着。
    pub changes: HashMap<String, CommitChanges>,
}

impl Graph {
    pub fn limit(&self) -> usize {
        if self.limit == 0 { GRAPH_PAGE } else { self.limit }
    }

    /// 仓库现在是 `info` 时要不要（重）读。
    pub fn needs_read(&self, info: &git::RepoInfo) -> bool {
        !self.loading && (self.history.is_none() || self.stale || self.read_for.as_ref() != Some(info))
    }

    /// 读到的第 `ci` 个提交。
    pub fn commit(&self, ci: usize) -> Option<&git::Commit> {
        self.history.as_ref()?.as_ref().ok()?.commits.get(ci)
    }

    /// 第 `ci` 个提交改的第 `fi` 个文件。
    pub fn file(&self, ci: usize, fi: usize) -> Option<&git::FileDiff> {
        match self.changes.get(&self.commit(ci)?.id)? {
            CommitChanges::Ready(files) => files.get(fi),
            _ => None,
        }
    }
}

/// 正在跑的 git 操作，写在面板顶上或者那个仓库的标题上；跑完之前这个仓库不接新的操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum Busy {
    Stage,
    Discard,
    Commit,
    Checkout,
    Fetch,
    Pull,
    Push,
    Sync,
    Stash,
    RemoveWorktree,
}

impl Busy {
    pub fn label(self) -> String {
        match self {
            Self::Stage => rust_i18n::t!("git.busy.stage"),
            Self::Discard => rust_i18n::t!("git.busy.discard"),
            Self::Commit => rust_i18n::t!("git.busy.commit"),
            Self::Checkout => rust_i18n::t!("git.busy.checkout"),
            Self::Fetch => rust_i18n::t!("git.busy.fetch"),
            Self::Pull => rust_i18n::t!("git.busy.pull"),
            Self::Push => rust_i18n::t!("git.busy.push"),
            Self::Sync => rust_i18n::t!("git.busy.sync"),
            Self::Stash => rust_i18n::t!("git.busy.stash"),
            Self::RemoveWorktree => rust_i18n::t!("git.busy.remove_worktree"),
        }
        .into_owned()
    }
}

/// 一个仓库在 Git 面板里自己的东西：提交说明框和正在跑的操作。按仓库的根目录记，终端换到
/// 别处再回来时说明框里写了一半的话还在。
#[derive(Default)]
pub(in crate::window) struct RepoPanel {
    /// 提交说明框，第一次显示这个仓库时建出来；以及它的事件订阅。
    pub commit_box: Option<Entity<TextArea>>,
    pub commit_events: Option<Subscription>,
    /// 提交说明框的提示里写的分支，变了才换提示。
    pub placeholder_branch: Option<Option<String>>,
    pub busy: Option<Busy>,
    pub graph: Graph,
}

/// 以树形式查看时的一个目录行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::window) struct DirRow {
    /// 所在仓库的根目录。
    pub root: PathBuf,
    /// 相对仓库根的路径，并成一行的是链条最深的那个；`name` 是显示的名字，如 `crates/desktop/src`。
    pub path: PathBuf,
    pub name: String,
    pub expanded: bool,
    pub owner: DirOwner,
}

/// 目录行是哪儿的：工作区改动的哪一段，还是图表里展开的哪个提交（`Graph::history` 里的下标）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum DirOwner {
    Section(GitSection),
    Commit(usize),
}

/// 排行时一边排一边记的东西。
#[derive(Default)]
struct Out {
    rows: Vec<GitRow>,
    depth: Vec<usize>,
    dirs: Vec<DirRow>,
    /// 接下来的行缩进几层。
    level: usize,
}

impl Out {
    fn push(&mut self, row: GitRow) {
        self.rows.push(row);
        self.depth.push(self.level);
    }

    fn dir(&mut self, ri: usize, root: &Path, item: (PathBuf, String, bool), owner: DirOwner) {
        let (path, name, expanded) = item;
        let di = self.dirs.len();
        self.dirs.push(DirRow { root: root.to_path_buf(), path, name, expanded, owner });
        self.push(GitRow::Dir(ri, di));
    }
}

/// 一个 workspace 的 Git 面板。
pub(in crate::window) struct GitPanel {
    pub rows: Vec<GitRow>,
    /// 和 `rows` 一一对应：以树形式查看时这一行在第几层，列表形式时都是 0。
    pub depth: Vec<usize>,
    /// `GitRow::Dir` 指向的目录行。
    pub dirs: Vec<DirRow>,
    /// 改动的文件以树形式查看，否则是列表；跟着窗口的设置。
    pub tree: bool,
    /// 以树形式查看时收起的目录：仓库根、段和相对仓库根的路径；默认都展开。
    collapsed_dirs: HashSet<(PathBuf, GitSection, PathBuf)>,
    /// 收起的段，按仓库根记。
    collapsed: HashSet<(PathBuf, GitSection)>,
    /// 用户收起或展开过的仓库块，按仓库根记；没动过的按 `repo_open_by_default`。
    repo_open: HashMap<PathBuf, bool>,
    /// 只有一个仓库时的列表。
    pub scroll: UniformListScrollHandle,
    /// 多个仓库时的列表：块头高度不一，用 `gpui::list`；它记着各行量过的高度，`rows` 变了要
    /// 告诉它，`list_rows` 是它现在知道的那些行。
    pub list: ListState,
    list_rows: Vec<GitRow>,
    /// 各个仓库的提交说明框和忙碌状态，按仓库根记。
    pub repos: HashMap<PathBuf, RepoPanel>,
    /// 最近点过的那块的仓库根：菜单和快捷键派发的动作作用到它，见 `WindowView::git_target`。
    pub active: Option<PathBuf>,
    /// 面板上次画多宽，图表的行据此决定放不放得下日期和引用标签。
    pub width: f32,
}

impl Default for GitPanel {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            depth: Vec::new(),
            dirs: Vec::new(),
            tree: false,
            collapsed_dirs: HashSet::new(),
            collapsed: HashSet::new(),
            repo_open: HashMap::new(),
            scroll: UniformListScrollHandle::default(),
            list: ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW)),
            list_rows: Vec::new(),
            repos: HashMap::new(),
            active: None,
            width: 0.,
        }
    }
}

/// 没收起展开过的仓库块默认展开不展开：主仓库总展开；子仓库有改动、合并之类做到一半、和上游
/// 差着提交，或者会显示发布分支的按钮时展开，好让提交完接着同步或发布；其余干净的收着，子模块
/// 多时不至于满屏空的提交说明框。
fn repo_open_by_default(repo: &git::Snapshot) -> bool {
    let info = &repo.info;
    let unsynced = info.upstream.is_some() && (info.ahead > 0 || info.behind > 0);
    let publishable = info.upstream.is_none() && info.has_remote && info.branch.is_some() && info.head.is_some();
    repo.kind == RepoKind::Main || !repo.is_clean() || info.operation.is_some() || unsynced || publishable
}

impl GitPanel {
    /// 根目录是 `root` 的仓库正在跑的操作。
    pub fn busy(&self, root: &Path) -> Option<Busy> {
        self.repos.get(root).and_then(|repo| repo.busy)
    }

    pub fn repo_mut(&mut self, root: &Path) -> &mut RepoPanel {
        self.repos.entry(root.to_path_buf()).or_default()
    }

    pub fn section_expanded(&self, root: &Path, section: GitSection) -> bool {
        !self.collapsed.contains(&(root.to_path_buf(), section))
    }

    /// 这个仓库的块展开着。只有一个仓库时总是展开的。用户正在操作的那块（`active`）不因为
    /// 提交完变干净就自动收起，点到别的块或者手动收起以后才按默认的来。
    pub fn repo_expanded(&self, repo: &git::Snapshot) -> bool {
        self.repo_open
            .get(&repo.root)
            .copied()
            .unwrap_or_else(|| repo_open_by_default(repo) || self.active.as_deref() == Some(repo.root.as_path()))
    }

    /// 点了块头：`was_expanded` 是点之前画出来的样子。不能在这里再用 `repo_expanded` 算，点下去
    /// 的那一刻 `active` 已经换成这块，干净的子仓库会被算成展开着，点一下反而收起。
    pub fn toggle_repo(&mut self, root: &Path, was_expanded: bool, git: Option<&git::Repos>) {
        self.repo_open.insert(root.to_path_buf(), !was_expanded);
        self.rebuild(git);
    }

    pub fn toggle_section(&mut self, root: &Path, section: GitSection, git: Option<&git::Repos>) {
        let key = (root.to_path_buf(), section);
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.rebuild(git);
    }

    /// 展开或收起图表里的提交；收起时它改了什么不再留着。返回展开后还没读它改了什么，要去读。
    pub fn toggle_commit(&mut self, root: &Path, id: &str, git: Option<&git::Repos>) -> bool {
        let graph = &mut self.repo_mut(root).graph;
        let load = if graph.expanded.remove(id) {
            graph.changes.remove(id);
            false
        } else {
            graph.expanded.insert(id.to_owned());
            !graph.changes.contains_key(id)
        };
        if load {
            graph.changes.insert(id.to_owned(), CommitChanges::Loading);
        }
        self.rebuild(git);
        load
    }

    /// 这一段里的文件在 `git::Snapshot` 那一段里的下标，按路径排。
    pub fn files(git: &git::Snapshot, section: GitSection) -> Vec<usize> {
        let source = section.source();
        let files = git.files(source);
        (0..files.len()).filter(|&fi| section_of(&files[fi], source) == section).collect()
    }

    /// 按 `git` 重新排行。
    pub fn rebuild(&mut self, git: Option<&git::Repos>) {
        let mut out = Out::default();
        let Some(git) = git else {
            (self.rows, self.depth, self.dirs) = (out.rows, out.depth, out.dirs);
            return;
        };
        let multi = git.count() > 1;
        for (ri, repo) in git.iter().enumerate() {
            if multi {
                out.push(GitRow::Repo(ri));
                if !self.repo_expanded(repo) {
                    continue;
                }
            }
            self.push_repo(&mut out, ri, repo, multi);
        }
        (self.rows, self.depth, self.dirs) = (out.rows, out.depth, out.dirs);
        if multi {
            self.sync_list();
        }
    }

    /// 一个仓库的各段，最后是图表。
    fn push_repo(&self, out: &mut Out, ri: usize, git: &git::Snapshot, multi: bool) {
        // 只有一个仓库时块头上没有改动数，没改动要说一句。
        if !multi && git.is_clean() {
            out.push(GitRow::Clean(ri));
        }
        for section in [GitSection::Merge, GitSection::Staged, GitSection::Unstaged] {
            let files = Self::files(git, section);
            if files.is_empty() {
                continue;
            }
            out.push(GitRow::Section(ri, section));
            if !self.section_expanded(&git.root, section) {
                continue;
            }
            let source = section.source();
            let push_file = |out: &mut Out, fi: usize| out.push(GitRow::File(ri, source, fi));
            if !self.tree {
                for fi in files {
                    push_file(out, fi);
                }
                continue;
            }
            let paths: Vec<_> = files.iter().map(|&fi| (fi, git.files(source)[fi].path.as_path())).collect();
            let expanded = |dir: &Path| !self.collapsed_dirs.contains(&(git.root.clone(), section, dir.to_path_buf()));
            for item in file_tree(&paths, expanded) {
                match item {
                    TreeItem::Dir { path, name, depth, expanded } => {
                        out.level = depth;
                        out.dir(ri, &git.root, (path, name, expanded), DirOwner::Section(section));
                    }
                    TreeItem::File { index, depth } => {
                        out.level = depth;
                        push_file(out, index);
                    }
                }
            }
            out.level = 0;
        }
        if !git.info.stashes.is_empty() {
            out.push(GitRow::Section(ri, GitSection::Stashes));
            if self.section_expanded(&git.root, GitSection::Stashes) {
                for si in 0..git.info.stashes.len() {
                    out.push(GitRow::Stash(ri, si));
                }
            }
        }
        out.push(GitRow::Section(ri, GitSection::Graph));
        if self.section_expanded(&git.root, GitSection::Graph) {
            let graph = self.repos.get(&git.root).map(|repo| &repo.graph);
            push_graph(out, ri, &git.root, graph, self.tree);
        }
    }

    /// 以树形式查看时展开或收起第 `di` 个目录行。
    pub fn toggle_dir(&mut self, di: usize, git: Option<&git::Repos>) {
        let Some(dir) = self.dirs.get(di).cloned() else {
            return;
        };
        match dir.owner {
            DirOwner::Section(section) => {
                let key = (dir.root.clone(), section, dir.path.clone());
                if !self.collapsed_dirs.remove(&key) {
                    self.collapsed_dirs.insert(key);
                }
            }
            DirOwner::Commit(ci) => {
                let graph = &mut self.repo_mut(&dir.root).graph;
                let Some(id) = graph.commit(ci).map(|commit| commit.id.clone()) else {
                    return;
                };
                let key = (id, dir.path.clone());
                if !graph.collapsed_dirs.remove(&key) {
                    graph.collapsed_dirs.insert(key);
                }
            }
        }
        self.rebuild(git);
    }

    /// 告诉多个仓库时的列表哪些行变了：只换掉前后相同部分中间的那一段，滚动位置留着。块头
    /// 里的分支、提示条随时会变，看不见的块头也重新量。
    fn sync_list(&mut self) {
        let changed = changed_range(&self.list_rows, &self.rows);
        self.list.splice(changed.old, changed.new_len);
        self.list_rows = self.rows.clone();
        for (ix, row) in self.rows.iter().enumerate() {
            if matches!(row, GitRow::Repo(_)) {
                self.list.remeasure_items(ix..ix + 1);
            }
        }
    }
}

/// `old` 换成 `new` 时变了的那一段：在 `old` 里的范围，以及换成了几行。
#[derive(Debug, PartialEq, Eq)]
struct Changed {
    old: Range<usize>,
    new_len: usize,
}

fn changed_range<T: PartialEq>(old: &[T], new: &[T]) -> Changed {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    Changed { old: prefix..old.len() - suffix, new_len: new.len() - prefix - suffix }
}

/// 图表的各行：读到的提交，展开的提交下面跟着它改的文件，再展开文件是块和行。
fn push_graph(out: &mut Out, ri: usize, root: &Path, graph: Option<&Graph>, tree: bool) {
    let history = match graph.and_then(|graph| graph.history.as_ref()) {
        None => {
            out.push(GitRow::GraphNote(ri, GraphNote::Loading));
            return;
        }
        Some(Err(_)) => {
            out.push(GitRow::GraphNote(ri, GraphNote::Failed));
            return;
        }
        Some(Ok(history)) => history,
    };
    let Some(graph) = graph else {
        return;
    };
    if history.commits.is_empty() {
        out.push(GitRow::GraphNote(ri, GraphNote::Empty));
        return;
    }
    for (ci, commit) in history.commits.iter().enumerate() {
        out.push(GitRow::Commit(ri, ci));
        if !graph.expanded.contains(&commit.id) {
            continue;
        }
        let files = match graph.changes.get(&commit.id) {
            None | Some(CommitChanges::Loading) => {
                out.push(GitRow::CommitNote(ri, ci, CommitNote::Loading));
                continue;
            }
            Some(CommitChanges::Failed) => {
                out.push(GitRow::CommitNote(ri, ci, CommitNote::Failed));
                continue;
            }
            Some(CommitChanges::Ready(files)) => files,
        };
        if files.is_empty() {
            out.push(GitRow::CommitNote(ri, ci, CommitNote::Empty));
        }
        let push_file = |out: &mut Out, fi: usize| out.push(GitRow::CommitFile(ri, ci, fi));
        if !tree {
            for fi in 0..files.len() {
                push_file(out, fi);
            }
            continue;
        }
        let paths: Vec<_> = files.iter().enumerate().map(|(fi, file)| (fi, file.path.as_path())).collect();
        let expanded = |dir: &Path| !graph.collapsed_dirs.contains(&(commit.id.clone(), dir.to_path_buf()));
        for item in file_tree(&paths, expanded) {
            match item {
                TreeItem::Dir { path, name, depth, expanded } => {
                    out.level = depth;
                    out.dir(ri, root, (path, name, expanded), DirOwner::Commit(ci));
                }
                TreeItem::File { index, depth } => {
                    out.level = depth;
                    push_file(out, index);
                }
            }
        }
        out.level = 0;
    }
    if history.more {
        out.push(GitRow::GraphNote(ri, if graph.loading { GraphNote::Loading } else { GraphNote::More }));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use runode_git::{FileDiff, RepoInfo};

    fn repo(prefix: &str, kind: RepoKind, unstaged: &[&str]) -> git::Snapshot {
        let file = |path: &&str| FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            added: 1,
            removed: 0,
            hunks: Vec::new(),
            binary: false,
            truncated: false,
            gitlink: false,
        };
        git::Snapshot {
            root: Path::new("/repo").join(prefix),
            git_dir: Path::new("/repo/.git").join(prefix),
            prefix: prefix.into(),
            kind,
            staged: Vec::new(),
            unstaged: unstaged.iter().map(file).collect(),
            statuses: HashMap::new(),
            ignored: HashSet::new(),
            info: RepoInfo::default(),
        }
    }

    #[test]
    fn finds_the_changed_range() {
        assert_eq!(changed_range(&[1, 2, 3, 4], &[1, 2, 3, 4]), Changed { old: 4..4, new_len: 0 });
        assert_eq!(changed_range(&[1, 2, 3, 4], &[1, 9, 9, 4]), Changed { old: 1..3, new_len: 2 });
        assert_eq!(changed_range(&[1, 2, 4], &[1, 2, 3, 4]), Changed { old: 2..2, new_len: 1 });
        assert_eq!(changed_range(&[1, 1], &[1, 1, 1]), Changed { old: 2..2, new_len: 1 });
        assert_eq!(changed_range::<u8>(&[], &[1]), Changed { old: 0..0, new_len: 1 });
    }

    #[test]
    fn keeps_blocks_open_after_a_commit() {
        let mut panel = GitPanel::default();
        let mut sub = repo("libs/lib", RepoKind::Submodule, &[]);
        assert!(!panel.repo_expanded(&sub));
        // 提交完变干净了，但还领先上游，或者还没发布：留着同步、发布的按钮。
        sub.info.upstream = Some("origin/main".into());
        sub.info.ahead = 1;
        assert!(panel.repo_expanded(&sub));
        sub.info =
            RepoInfo { has_remote: true, branch: Some("main".into()), head: Some("abc".into()), ..Default::default() };
        assert!(panel.repo_expanded(&sub));
        // 正在操作的那块干净了也不自动收起，点到别的块以后才收。
        sub.info = RepoInfo::default();
        panel.active = Some(sub.root.clone());
        assert!(panel.repo_expanded(&sub));
        panel.active = Some("/repo".into());
        assert!(!panel.repo_expanded(&sub));
    }

    /// 还没读历史时图表那一段：段标题和「在读」。
    fn graph_rows(ri: usize) -> [GitRow; 2] {
        [GitRow::Section(ri, GitSection::Graph), GitRow::GraphNote(ri, GraphNote::Loading)]
    }

    #[test]
    fn one_block_per_repository() {
        let mut panel = GitPanel::default();
        // 只有一个仓库时没有块头，和以前一样；图表在最后。
        let single = git::Repos::new(repo("", RepoKind::Main, &["a.txt"]));
        panel.rebuild(Some(&single));
        let mut expected = vec![GitRow::Section(0, GitSection::Unstaged), GitRow::File(0, Section::Unstaged, 0)];
        expected.extend(graph_rows(0));
        assert_eq!(panel.rows, expected);

        // 有改动的子仓库展开，干净的收着。
        let mut repos = git::Repos::new(repo("", RepoKind::Main, &["a.txt"]));
        repos.subs.push(repo("libs/clean", RepoKind::Submodule, &[]));
        repos.subs.push(repo("tools/dirty", RepoKind::Nested, &["b.txt"]));
        panel.rebuild(Some(&repos));
        let mut expected =
            vec![GitRow::Repo(0), GitRow::Section(0, GitSection::Unstaged), GitRow::File(0, Section::Unstaged, 0)];
        expected.extend(graph_rows(0));
        expected.extend([
            GitRow::Repo(1),
            GitRow::Repo(2),
            GitRow::Section(2, GitSection::Unstaged),
            GitRow::File(2, Section::Unstaged, 0),
        ]);
        expected.extend(graph_rows(2));
        assert_eq!(panel.rows, expected);
        assert_eq!(panel.list.item_count(), panel.rows.len());

        // 收起主仓库、展开干净的子仓库，按仓库记着。
        panel.toggle_repo(Path::new("/repo"), true, Some(&repos));
        panel.toggle_repo(Path::new("/repo/libs/clean"), false, Some(&repos));
        let mut expected = vec![GitRow::Repo(0), GitRow::Repo(1)];
        expected.extend(graph_rows(1));
        expected.extend([
            GitRow::Repo(2),
            GitRow::Section(2, GitSection::Unstaged),
            GitRow::File(2, Section::Unstaged, 0),
        ]);
        expected.extend(graph_rows(2));
        assert_eq!(panel.rows, expected);
        assert_eq!(panel.list.item_count(), panel.rows.len());
        assert!(panel.rows.iter().all(|row| row.repo() < repos.count()));
    }

    #[test]
    fn one_click_opens_a_clean_block() {
        let mut panel = GitPanel::default();
        let mut repos = git::Repos::new(repo("", RepoKind::Main, &[]));
        repos.subs.push(repo("libs/clean", RepoKind::Submodule, &[]));
        let sub = Path::new("/repo/libs/clean");
        panel.rebuild(Some(&repos));
        assert!(!panel.repo_expanded(&repos.subs[0]));
        // 点块头时捕获阶段先把它记成 active，再按点之前的样子切换：一下就展开。
        panel.active = Some(sub.to_path_buf());
        panel.toggle_repo(sub, false, Some(&repos));
        assert!(panel.repo_expanded(&repos.subs[0]));
        panel.toggle_repo(sub, true, Some(&repos));
        assert!(!panel.repo_expanded(&repos.subs[0]));
    }

    #[test]
    fn worktrees_follow_sub_repositories() {
        let mut panel = GitPanel::default();
        let mut repos = git::Repos::new(repo("", RepoKind::Main, &[]));
        repos.subs.push(repo("libs/lib", RepoKind::Submodule, &["x.txt"]));
        let mut clean = repo("", RepoKind::Worktree, &[]);
        clean.root = "/elsewhere/clean".into();
        let mut dirty = repo("", RepoKind::Worktree, &["y.txt"]);
        dirty.root = "/elsewhere/dirty".into();
        repos.worktrees.extend([clean, dirty]);
        panel.rebuild(Some(&repos));
        let blocks: Vec<_> = panel
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, GitRow::Repo(_)))
            .map(|(ix, row)| (row.repo(), panel.rows.get(ix + 1).copied()))
            .collect();
        // 工作树排在子仓库后面；干净的收着，有改动的展开。
        assert_eq!(
            blocks,
            [
                (0, Some(GitRow::Section(0, GitSection::Graph))),
                (1, Some(GitRow::Section(1, GitSection::Unstaged))),
                (2, Some(GitRow::Repo(3))),
                (3, Some(GitRow::Section(3, GitSection::Unstaged))),
            ]
        );
        assert_eq!(repos.get(3).map(|wt| wt.root.as_path()), Some(Path::new("/elsewhere/dirty")));
    }

    #[test]
    fn lays_out_changes_as_a_tree() {
        let mut panel = GitPanel { tree: true, ..Default::default() };
        let repos = git::Repos::new(repo("", RepoKind::Main, &["README.md", "src/a.rs", "src/b/c.rs"]));
        panel.rebuild(Some(&repos));
        let file = |fi| GitRow::File(0, Section::Unstaged, fi);
        let unstaged = panel.rows.iter().position(|row| *row == GitRow::Section(0, GitSection::Unstaged)).unwrap();
        // 目录在前、文件在后；文件缩进到所在的层级。
        assert_eq!(
            panel.rows[unstaged..unstaged + 6],
            [GitRow::Section(0, GitSection::Unstaged), GitRow::Dir(0, 0), GitRow::Dir(0, 1), file(2), file(1), file(0)]
        );
        assert_eq!(panel.depth[unstaged..unstaged + 6], [0, 0, 1, 2, 1, 0]);
        assert_eq!((panel.dirs[0].name.as_str(), panel.dirs[1].path.as_path()), ("src", Path::new("src/b")));
        assert_eq!(panel.dirs[1].owner, DirOwner::Section(GitSection::Unstaged));

        // 收起 src：下面的都不排，按仓库根、段和目录记着，重排以后还是收着。
        panel.toggle_dir(0, Some(&repos));
        assert_eq!(
            panel.rows[unstaged..unstaged + 3],
            [GitRow::Section(0, GitSection::Unstaged), GitRow::Dir(0, 0), file(0)]
        );
        assert!(!panel.dirs[0].expanded);
        panel.rebuild(Some(&repos));
        assert!(!panel.dirs[0].expanded);

        // 列表形式不分层。
        panel.tree = false;
        panel.rebuild(Some(&repos));
        assert!(panel.dirs.is_empty() && panel.depth.iter().all(|&depth| depth == 0));
    }

    #[test]
    fn lists_the_graph_with_expanded_commits() {
        let mut panel = GitPanel::default();
        let repos = git::Repos::new(repo("", RepoKind::Main, &[]));
        let commit = |id: &str, parents: &[&str]| git::Commit {
            id: id.into(),
            parents: parents.iter().map(|&parent| parent.into()).collect(),
            subject: id.into(),
            author: String::new(),
            date: String::new(),
            refs: Vec::new(),
        };
        let commits = vec![commit("b", &["a"]), commit("a", &[])];
        let rows = git::graph_layout(&commits);
        let graph = &mut panel.repo_mut(Path::new("/repo")).graph;
        graph.history = Some(Ok(git::History { commits, rows, more: true }));
        let root = Path::new("/repo");

        // 干净的单个仓库先说一句没有改动；读到了历史就列提交，后面还有时是「加载更多」。
        panel.rebuild(Some(&repos));
        assert_eq!(
            panel.rows,
            [
                GitRow::Clean(0),
                GitRow::Section(0, GitSection::Graph),
                GitRow::Commit(0, 0),
                GitRow::Commit(0, 1),
                GitRow::GraphNote(0, GraphNote::More),
            ]
        );

        // 展开提交时先是「在读」，读到了列它改的文件；收起后改动不再留着。
        assert!(panel.toggle_commit(root, "b", Some(&repos)));
        assert_eq!(panel.rows[3], GitRow::CommitNote(0, 0, CommitNote::Loading));
        let mut file = repo("", RepoKind::Main, &["x.txt"]).unstaged.remove(0);
        file.hunks.push(git::Hunk {
            header: "@@ -1 +1 @@".into(),
            lines: vec![git::Line { kind: git::LineKind::Added, old: None, new: Some(1), text: "x".into() }],
        });
        panel.repo_mut(root).graph.changes.insert("b".into(), CommitChanges::Ready(vec![file]));
        panel.rebuild(Some(&repos));
        assert_eq!(panel.rows[2..5], [GitRow::Commit(0, 0), GitRow::CommitFile(0, 0, 0), GitRow::Commit(0, 1)]);
        assert!(!panel.toggle_commit(root, "b", Some(&repos)));
        assert!(panel.repo_mut(root).graph.changes.is_empty());
        assert_eq!(panel.rows.len(), 5);

        // 图表收起时不列，也就不读。
        panel.toggle_section(root, GitSection::Graph, Some(&repos));
        assert_eq!(panel.rows, [GitRow::Clean(0), GitRow::Section(0, GitSection::Graph)]);
        assert!(!panel.section_expanded(root, GitSection::Graph));
    }
}
