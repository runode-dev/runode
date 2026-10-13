//! 一个 workspace 的文件树状态：展开收起、排成行，以及换上 app 在后台读到的结果。只管数据，不碰界面。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{ScrollStrategy, SharedString, UniformListScrollHandle};
use runode_git as git;

use crate::file_tree::{
    TreeScan,
    listing::{Decoration, Decorator, DirEntry, MAX_COMPACT, list_dirs, single_dir},
};

/// 文件树里显示哪些：被 git 忽略的、名字以 `.` 开头的。整个窗口一个设置，见 `FileTree::filter`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TreeFilter {
    pub ignored: bool,
    pub dotfiles: bool,
}

/// 文件树里的一行。
#[derive(Clone, Debug)]
pub(crate) struct FileRow {
    /// 并成一行的目录是链条最深的那个。
    pub path: PathBuf,
    /// 并成一行的目录是「a / b / c」。
    pub name: SharedString,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub decoration: Decoration,
}

/// 一个 workspace 的文件树。
#[derive(Default)]
pub(crate) struct TreeState {
    /// 文件树的根目录，跟着当前终端的目录变；还没读过时为空，读到过一次之后就一直有。
    pub root: Option<PathBuf>,
    /// 最近一次读到的 git 状态，由 app 交进来，行按它着色；不在 git 仓库里时为空。
    pub git: Option<Arc<git::Repos>>,
    pub expanded: HashSet<PathBuf>,
    pub listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// 文件树里选中的路径。
    pub selected: Option<PathBuf>,
    pub rows: Vec<FileRow>,
    /// 搜索框的搜索词：搜索框整个窗口一个，切走时把框里的词存在这里，切回来时放回去。
    pub query: String,
    pub scroll: UniformListScrollHandle,
}

impl TreeState {
    pub(crate) fn rebuild_rows(&mut self, root: &Path, filter: TreeFilter) {
        let decorator = Decorator::new(self.git.as_deref());
        let mut rows = Vec::new();
        self.push_dir(root, 0, &decorator, filter, &mut rows);
        self.rows = rows;
    }

    fn push_dir(&self, dir: &Path, depth: usize, decorator: &Decorator, filter: TreeFilter, rows: &mut Vec<FileRow>) {
        let Some(entries) = self.listings.get(dir) else {
            return;
        };
        for entry in entries {
            let mut path = dir.join(&entry.name);
            let hidden = (!filter.ignored && decorator.of(&path, entry.is_dir) == Decoration::Ignored)
                || (!filter.dotfiles && entry.name.starts_with('.'));
            if hidden {
                continue;
            }
            let mut name = entry.name.clone();
            if entry.is_dir {
                for _ in 0..MAX_COMPACT {
                    let Some(child) = self.listings.get(&path).and_then(|entries| single_dir(entries)) else {
                        break;
                    };
                    // 藏着点文件时点开头的子目录也不并进来，不然隐藏的目录跟着上级露出来。
                    if !filter.dotfiles && child.starts_with('.') {
                        break;
                    }
                    // 被忽略的子目录不并进没被忽略的目录：隐藏时要单独藏起来，显示时也不该连累上级变暗。
                    if decorator.of(&path.join(child), true) == Decoration::Ignored
                        && decorator.of(&path, true) != Decoration::Ignored
                    {
                        break;
                    }
                    name = format!("{name} / {child}");
                    path = path.join(child);
                }
            }
            let expanded = entry.is_dir && self.expanded.contains(&path);
            rows.push(FileRow {
                name: name.into(),
                depth,
                is_dir: entry.is_dir,
                expanded,
                decoration: decorator.of(&path, entry.is_dir),
                path: path.clone(),
            });
            if expanded {
                self.push_dir(&path, depth + 1, decorator, filter, rows);
            }
        }
    }

    /// 当场读 `dirs` 以及往下多看的一层，读不了的不记。
    fn list_now(&mut self, dirs: Vec<PathBuf>) {
        if dirs.is_empty() {
            return;
        }
        let listings = list_dirs(dirs, self.git.as_deref());
        for (dir, listing) in listings {
            if let Some(listing) = listing {
                self.listings.insert(dir, listing);
            }
        }
    }

    /// 展开或收起文件树里的目录；展开时当场读它的内容，免得显示收起期间已经变了的旧列表，
    /// 之后跟着重读。
    pub(crate) fn toggle_dir(&mut self, path: &Path, root: &Path, filter: TreeFilter) {
        if self.expanded.remove(path) {
            // 选中的在收起的目录里面时，选中改成这个目录，免得选中项看不见还被删除、新建用上。
            if self.selected.as_deref().is_some_and(|selected| selected != path && selected.starts_with(path)) {
                self.selected = Some(path.to_path_buf());
            }
        } else {
            self.expanded.insert(path.to_path_buf());
            self.list_now(vec![path.to_path_buf()]);
        }
        self.rebuild_rows(root, filter);
    }

    /// 终端换了目录：在文件树里展开到 `dir`，连它本身也展开，选中它、滚到中间。
    fn reveal_dir(&mut self, dir: &Path, root: &Path, filter: TreeFilter) {
        self.reveal(dir, true, root, filter, ScrollStrategy::Center);
    }

    /// 在文件树里展开到 `path` 所在的目录，选中它、滚到能看见。已经显示着时只选中，不重排。
    pub(crate) fn reveal_file(&mut self, path: &Path, root: &Path, filter: TreeFilter) {
        if let Some(ix) = self.rows.iter().position(|row| row.path == path) {
            self.select_row(ix);
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            return;
        }
        self.reveal(path, false, root, filter, ScrollStrategy::Nearest);
    }

    /// 在文件树里展开到 `path` 并选中它，按 `strategy` 滚到能看见的地方；`expand` 时连 `path`
    /// 这个目录本身也展开。`path` 就是根目录或者不在根下面时不动。
    fn reveal(&mut self, path: &Path, expand: bool, root: &Path, filter: TreeFilter, strategy: ScrollStrategy) {
        let Ok(rel) = path.strip_prefix(root) else {
            return;
        };
        if rel.as_os_str().is_empty() {
            return;
        }
        let mut chain: Vec<_> = rel
            .ancestors()
            .skip(usize::from(!expand))
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| root.join(rel))
            .collect();
        chain.reverse();
        let missing = chain.iter().filter(|dir| !self.listings.contains_key(*dir)).cloned().collect();
        self.list_now(missing);
        // `path` 和它唯一的子目录并成一行时，这一行按链条最深的那个展开，一路展开下去。
        if expand {
            for _ in 0..MAX_COMPACT {
                let last = &chain[chain.len() - 1];
                let Some(child) =
                    self.listings.get(last).and_then(|entries| single_dir(entries)).map(|name| last.join(name))
                else {
                    break;
                };
                if !self.listings.contains_key(&child) {
                    self.list_now(vec![child.clone()]);
                }
                chain.push(child);
            }
        }
        self.expanded.extend(chain);
        self.rebuild_rows(root, filter);
        // 并成一行的目录，行的路径是链条最深的那个，`path` 在链条中间时找不到原样的路径。
        let rows = &self.rows;
        let Some(ix) = rows
            .iter()
            .position(|row| row.path == path)
            .or_else(|| rows.iter().position(|row| row.path.starts_with(path)))
        else {
            return;
        };
        self.selected = Some(rows[ix].path.clone());
        self.scroll.scroll_to_item(ix, strategy);
    }

    /// 收起根目录下所有展开的目录。
    pub(crate) fn collapse_all(&mut self, root: &Path, filter: TreeFilter) {
        self.expanded.retain(|dir| !dir.starts_with(root));
        self.rebuild_rows(root, filter);
        // 选中的行跟着收起来看不见了，不留着。
        if self.selected_row().is_none() {
            self.selected = None;
        }
    }

    /// 改了文件之后当场重读 `dirs`，不等监听到改动；后台的重读随后照常进行。
    pub(crate) fn relist(&mut self, dirs: Vec<PathBuf>, root: &Path, filter: TreeFilter) {
        self.list_now(dirs);
        self.rebuild_rows(root, filter);
    }

    /// `from` 改名或挪到了 `to`：展开的目录和选中的路径跟过去，旧路径下读过的目录不再要。
    pub(crate) fn moved(&mut self, from: &Path, to: &Path) {
        let follow = |path: &Path| crate::ui::follow_move(path, from, to);
        self.expanded = self.expanded.drain().map(|dir| follow(&dir).unwrap_or(dir)).collect();
        self.listings.retain(|dir, _| !dir.starts_with(from));
        if let Some(selected) = self.selected.as_deref().and_then(follow) {
            self.selected = Some(selected);
        }
    }

    /// `path` 被删掉了：它和下面的目录不再展开。
    pub(crate) fn removed(&mut self, path: &Path) {
        self.expanded.retain(|dir| !dir.starts_with(path));
        self.listings.retain(|dir, _| !dir.starts_with(path));
    }

    /// 选中的行在 `rows` 里的位置。
    pub(crate) fn selected_row(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|row| row.path == *selected)
    }

    /// 键盘上下移动选中的行，`step` 为负往上；没选中时往下从第一行开始，往上从最后一行开始。
    /// 返回新选中的位置。
    pub(crate) fn select_step(&mut self, step: isize) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        let ix = match self.selected_row() {
            Some(ix) => ix.saturating_add_signed(step).min(last),
            None if step > 0 => 0,
            None => last,
        };
        self.selected = Some(self.rows[ix].path.clone());
        Some(ix)
    }

    /// 选中第 `ix` 行。
    pub(crate) fn select_row(&mut self, ix: usize) -> Option<usize> {
        let row = self.rows.get(ix)?;
        self.selected = Some(row.path.clone());
        Some(ix)
    }

    /// 键盘往左：展开着的目录收起，否则跳到上一级目录。返回新选中的位置。
    pub(crate) fn select_out(&mut self, root: &Path, filter: TreeFilter) -> Option<usize> {
        let ix = self.selected_row()?;
        let row = &self.rows[ix];
        if row.is_dir && row.expanded {
            let path = row.path.clone();
            self.toggle_dir(&path, root, filter);
            return Some(ix);
        }
        let depth = row.depth;
        let parent = self.rows[..ix].iter().rposition(|row| row.depth < depth)?;
        self.select_row(parent)
    }

    /// 键盘往右：收着的目录展开，展开着的跳到它的第一项。返回新选中的位置。
    pub(crate) fn select_in(&mut self, root: &Path, filter: TreeFilter) -> Option<usize> {
        let ix = self.selected_row()?;
        let row = &self.rows[ix];
        if !row.is_dir {
            return None;
        }
        if !row.expanded {
            let path = row.path.clone();
            self.toggle_dir(&path, root, filter);
            return Some(ix);
        }
        let depth = row.depth;
        self.rows.get(ix + 1).filter(|child| child.depth > depth)?;
        self.select_row(ix + 1)
    }

    /// 换上 app 在后台读到的结果，有变化时返回真。
    pub(crate) fn apply_scan(&mut self, scan: TreeScan, filter: TreeFilter) -> bool {
        let TreeScan { root, git, listings, reveal } = scan;
        // 第一次读时 `root` 还是空的，下面换上根目录时一定算作有变化。
        let mut changed = false;
        // 终端换到了别的仓库或目录：上一处的目录列表和选中的行不再相干，选中项留着的话删除、改名
        // 还会作用在看不见的旧路径上。展开的目录是绝对路径，留着，回到原处时还是展开的。
        if self.root.as_ref() != Some(&root) {
            self.root = Some(root.clone());
            self.listings.clear();
            self.selected = None;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            changed = true;
        }
        // app 只在 git 状态变了时才换一份新的，按是不是同一份比。
        let same_git = match (&self.git, &git) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (old, new) => old.is_none() && new.is_none(),
        };
        if !same_git {
            self.git = git;
            changed = true;
        }
        for (dir, listing) in listings {
            match listing {
                Some(entries) => {
                    if self.listings.get(&dir) != Some(&entries) {
                        self.listings.insert(dir, entries);
                        changed = true;
                    }
                }
                // 目录被删掉或者改了名，收起来。
                None => {
                    changed |= self.listings.remove(&dir).is_some();
                    changed |= self.expanded.remove(&dir);
                }
            }
        }
        if changed {
            self.rebuild_rows(&root, filter);
        }
        // 终端换了目录，文件树跟过去。
        if let Some(dir) = reveal {
            self.reveal_dir(&dir, &root, filter);
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runode_git::FileStatus;

    const ALL: TreeFilter = TreeFilter { ignored: true, dotfiles: true };
    const NO_IGNORED: TreeFilter = TreeFilter { ignored: false, dotfiles: true };

    fn snapshot() -> git::Repos {
        git::Repos::new(git::Snapshot {
            root: "/repo".into(),
            git_dir: "/repo/.git".into(),
            prefix: PathBuf::new(),
            kind: git::RepoKind::Main,
            staged: Vec::new(),
            unstaged: Vec::new(),
            statuses: HashMap::from([
                ("src/a/b.rs".into(), FileStatus::Modified),
                ("new.txt".into(), FileStatus::Untracked),
                ("docs/new/a.md".into(), FileStatus::Untracked),
                ("docs/old.md".into(), FileStatus::Deleted),
                ("src/c.rs".into(), FileStatus::Added),
            ]),
            ignored: HashSet::from(["target".into()]),
            info: Default::default(),
        })
    }

    fn dir(name: &str) -> DirEntry {
        DirEntry { name: name.into(), is_dir: true }
    }

    fn file(name: &str) -> DirEntry {
        DirEntry { name: name.into(), is_dir: false }
    }

    fn names(state: &TreeState) -> Vec<(String, usize)> {
        state.rows.iter().map(|row| (row.name.to_string(), row.depth)).collect()
    }

    #[test]
    fn decorates_paths_by_status() {
        let git = snapshot();
        let decorator = Decorator::new(Some(&git));
        let of = |path: &str, is_dir| decorator.of(Path::new(path), is_dir);
        assert_eq!(of("/repo/src/a/b.rs", false), Decoration::Status(FileStatus::Modified));
        // 目录按里面最要紧的改动着色：有修改算修改，只有新文件的才算新增或未跟踪。
        assert_eq!(of("/repo/src", true), Decoration::ContainsChanges(FileStatus::Modified));
        assert_eq!(of("/repo/src/a", true), Decoration::ContainsChanges(FileStatus::Modified));
        assert_eq!(of("/repo/docs/new", true), Decoration::ContainsChanges(FileStatus::Untracked));
        assert_eq!(of("/repo/docs", true), Decoration::ContainsChanges(FileStatus::Modified));
        assert_eq!(of("/repo/lib", true), Decoration::None);
        assert_eq!(of("/repo/new.txt", false), Decoration::Status(FileStatus::Untracked));
        assert_eq!(of("/repo/target", true), Decoration::Ignored);
        assert_eq!(of("/repo/target/debug/x", false), Decoration::Ignored);
        assert_eq!(of("/elsewhere/x", false), Decoration::None);
    }

    #[test]
    fn decorates_paths_inside_sub_repositories() {
        let mut git = snapshot();
        git.main.statuses.insert("libs/lib".into(), FileStatus::Modified);
        git.subs.push(git::Snapshot {
            root: "/repo/libs/lib".into(),
            git_dir: "/repo/.git/modules/libs/lib".into(),
            prefix: "libs/lib".into(),
            kind: git::RepoKind::Submodule,
            staged: Vec::new(),
            unstaged: Vec::new(),
            statuses: HashMap::from([("src/x.rs".into(), FileStatus::Added)]),
            ignored: HashSet::from(["build".into()]),
            info: Default::default(),
        });
        let decorator = Decorator::new(Some(&git));
        let of = |path: &str, is_dir| decorator.of(Path::new(path), is_dir);
        // 子模块里的文件按子模块自己的状态，忽略也按它自己的规则。
        assert_eq!(of("/repo/libs/lib/src/x.rs", false), Decoration::Status(FileStatus::Added));
        assert_eq!(of("/repo/libs/lib/build/out", false), Decoration::Ignored);
        // 子模块那个目录：提交号变了算修改，和里面的新增并在一起还是修改。
        assert_eq!(of("/repo/libs/lib", true), Decoration::ContainsChanges(FileStatus::Modified));
        assert_eq!(of("/repo/libs/lib/src", true), Decoration::ContainsChanges(FileStatus::Added));
        assert_eq!(of("/repo/libs", true), Decoration::ContainsChanges(FileStatus::Modified));
        assert_eq!(of("/repo/src/a/b.rs", false), Decoration::Status(FileStatus::Modified));
    }

    #[test]
    fn flattens_expanded_dirs() {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir("src"), file("README.md")]);
        state.listings.insert("/p/src".into(), vec![file("main.rs")]);
        state.rebuild_rows(root, ALL);
        assert_eq!(state.rows.len(), 2);
        state.expanded.insert("/p/src".into());
        state.rebuild_rows(root, ALL);
        assert_eq!(names(&state), [("src".into(), 0), ("main.rs".into(), 1), ("README.md".into(), 0)]);
    }

    #[test]
    fn compacts_single_child_dirs() {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir("crates"), file("Cargo.toml")]);
        state.listings.insert("/p/crates".into(), vec![dir("app")]);
        state.listings.insert("/p/crates/app".into(), vec![dir("src"), file("Cargo.toml")]);
        state.rebuild_rows(root, ALL);
        assert_eq!(names(&state), [("crates / app".into(), 0), ("Cargo.toml".into(), 0)]);
        assert_eq!(state.rows[0].path, PathBuf::from("/p/crates/app"));
        state.expanded.insert("/p/crates/app".into());
        state.rebuild_rows(root, ALL);
        assert_eq!(names(&state)[1..3], [("src".into(), 1), ("Cargo.toml".into(), 1)]);
    }

    #[test]
    fn hides_ignored_entries() {
        let mut state = TreeState { git: Some(Arc::new(snapshot())), ..Default::default() };
        let root = Path::new("/repo");
        state.listings.insert(root.into(), vec![dir("src"), dir("target"), file("new.txt")]);
        state.rebuild_rows(root, NO_IGNORED);
        assert_eq!(names(&state), [("src".into(), 0), ("new.txt".into(), 0)]);
        state.rebuild_rows(root, ALL);
        assert_eq!(state.rows.len(), 3);

        // 只装着被忽略目录的目录不和它并成一行。
        Arc::get_mut(state.git.as_mut().unwrap()).unwrap().main.ignored.insert("out/target".into());
        state.listings.insert(root.into(), vec![dir("out")]);
        state.listings.insert("/repo/out".into(), vec![dir("target")]);
        state.rebuild_rows(root, NO_IGNORED);
        assert_eq!(names(&state), [("out".into(), 0)]);
        state.rebuild_rows(root, ALL);
        assert_eq!(names(&state), [("out".into(), 0)]);
        assert_eq!(state.rows[0].decoration, Decoration::None);
    }

    #[test]
    fn hides_dotfiles() {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir(".github"), file(".env"), file("a")]);
        state.rebuild_rows(root, TreeFilter { ignored: true, dotfiles: false });
        assert_eq!(names(&state), [("a".into(), 0)]);
        state.rebuild_rows(root, ALL);
        assert_eq!(state.rows.len(), 3);

        // 只装着一个点开头子目录的目录不和它并成一行。
        state.listings.insert(root.into(), vec![dir("visible")]);
        state.listings.insert("/p/visible".into(), vec![dir(".hidden")]);
        state.rebuild_rows(root, TreeFilter { ignored: true, dotfiles: false });
        assert_eq!(names(&state), [("visible".into(), 0)]);
        state.rebuild_rows(root, ALL);
        assert_eq!(names(&state), [("visible / .hidden".into(), 0)]);
    }

    #[test]
    fn reveals_the_terminal_dir() {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir("a"), dir("b")]);
        state.listings.insert("/p/b".into(), vec![dir("c"), file("x")]);
        state.listings.insert("/p/b/c".into(), vec![file("y")]);
        state.reveal_dir(Path::new("/p/b/c"), root, ALL);
        assert_eq!(
            names(&state),
            [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("y".into(), 2), ("x".into(), 1)]
        );
        assert_eq!(state.selected, Some(PathBuf::from("/p/b/c")));

        // 终端目录在并成一行的链条中间时，展开到链条尽头。
        state.listings.insert("/p/a".into(), vec![dir("m")]);
        state.listings.insert("/p/a/m".into(), vec![file("z")]);
        state.reveal_dir(Path::new("/p/a"), root, ALL);
        assert_eq!(names(&state)[..2], [("a / m".into(), 0), ("z".into(), 1)]);
        assert_eq!(state.selected, Some(PathBuf::from("/p/a/m")));
    }

    #[test]
    fn reveals_files_without_expanding_them() {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir("a"), dir("b")]);
        state.listings.insert("/p/b".into(), vec![dir("c"), file("x")]);
        state.listings.insert("/p/b/c".into(), vec![file("y")]);
        state.reveal_file(Path::new("/p/b/x"), root, ALL);
        assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("x".into(), 1)]);
        assert_eq!(state.selected, Some(PathBuf::from("/p/b/x")));
        state.collapse_all(root, ALL);
        assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0)]);
    }

    #[test]
    fn moves_the_selection_with_the_keyboard() {
        let mut state = TreeState::default();
        let root = Path::new("/p");
        state.listings.insert(root.into(), vec![dir("a"), file("z")]);
        state.listings.insert("/p/a".into(), vec![file("b"), file("c")]);
        state.rebuild_rows(root, ALL);
        assert_eq!(state.select_step(-1), Some(1));
        assert_eq!(state.select_step(1), Some(1));
        assert_eq!(state.select_step(-5), Some(0));
        // 往右先展开，再进到第一项；往左先回到上一级，再收起。
        assert_eq!(state.select_in(root, ALL), Some(0));
        assert_eq!(state.rows.len(), 4);
        assert_eq!(state.select_in(root, ALL), Some(1));
        assert_eq!(state.selected, Some(PathBuf::from("/p/a/b")));
        assert_eq!(state.select_in(root, ALL), None);
        assert_eq!(state.select_out(root, ALL), Some(0));
        assert_eq!(state.select_out(root, ALL), Some(0));
        assert_eq!(state.rows.len(), 2);
        assert_eq!(state.select_out(root, ALL), None);

        // 改名或挪走的目录还是展开的，选中的跟过去。
        state.select_in(root, ALL);
        state.select_step(1);
        state.moved(Path::new("/p/a"), Path::new("/p/d"));
        assert!(state.expanded.contains(Path::new("/p/d")));
        assert!(!state.listings.contains_key(Path::new("/p/a")));
        assert_eq!(state.selected, Some(PathBuf::from("/p/d/b")));
        state.removed(Path::new("/p/d"));
        assert!(state.expanded.is_empty());
    }

    /// 换上扫描结果：换了根目录时丢掉上一处的目录列表，同一份 git 状态不算变化，读不了的目录收起来，
    /// 终端换了目录时定位过去。
    #[test]
    fn applies_scans() {
        let mut state = TreeState::default();
        let listing = |dir: &str, entries| (PathBuf::from(dir), Some(entries));
        let scan = |listings, reveal: Option<&str>| TreeScan {
            root: "/p".into(),
            git: None,
            listings,
            reveal: reveal.map(PathBuf::from),
        };
        assert!(state.apply_scan(scan(vec![listing("/p", vec![dir("a"), file("b")])], None), ALL));
        assert_eq!(names(&state), [("a".into(), 0), ("b".into(), 0)]);
        assert!(!state.apply_scan(scan(vec![listing("/p", vec![dir("a"), file("b")])], None), ALL));
        assert!(state.apply_scan(scan(vec![listing("/p/a", vec![file("c")])], Some("/p/a")), ALL));
        assert_eq!(state.selected, Some(PathBuf::from("/p/a")));
        assert_eq!(names(&state), [("a".into(), 0), ("c".into(), 1), ("b".into(), 0)]);
        assert!(state.apply_scan(scan(vec![(PathBuf::from("/p/a"), None)], None), ALL));
        assert!(!state.expanded.contains(Path::new("/p/a")));
        // 换了根目录时上一处选中的路径不留着，免得删除、改名作用在看不见的旧路径上。
        state.selected = Some(PathBuf::from("/p/b"));
        let moved = TreeScan { root: "/q".into(), git: None, listings: Vec::new(), reveal: None };
        assert!(state.apply_scan(moved, ALL));
        assert!(state.listings.is_empty());
        assert_eq!(state.selected, None);
    }
}
