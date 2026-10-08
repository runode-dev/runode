//! 一个 workspace 的文件树状态：展开收起、排成行，以及换上后台读到的结果。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{ScrollStrategy, SharedString, UniformListScrollHandle};
use runode_git as git;
use runode_protocol::TaskSource;

use super::{
    MAX_COMPACT,
    scan::{Decoration, Decorator, DirEntry, Scan, list_dirs, single_dir},
};
use crate::window::model::Workspace;

/// 文件树里的一行。
#[derive(Clone, Debug)]
pub(in crate::window) struct FileRow {
    /// 并成一行的目录是链条最深的那个。
    pub path: PathBuf,
    /// 并成一行的目录是「a / b / c」。
    pub name: SharedString,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub decoration: Decoration,
}

/// 一个 workspace 的改动和文件树。
#[derive(Default)]
pub(in crate::window) struct Project {
    /// 文件树的根目录，跟着当前终端的目录变；还没读过时为空，读到过一次之后就一直有。
    pub root: Option<PathBuf>,
    /// 上次读的是哪个目录，终端换了目录时据此在文件树里定位过去。
    pub(super) dir: Option<PathBuf>,
    /// 最近一次读到的 git 状态：主仓库以及它里面的子模块和嵌套仓库；不在 git 仓库里时为空。
    pub git: Option<git::Repos>,
    /// 当前目录在不在 git 仓库里，没问过时为空；面板都收着时只靠它决定显不显示 Git 按钮。
    pub(super) in_repo: Option<bool>,
    /// 上次问 `in_repo` 的目录和时刻，以及后台是不是正在问。
    pub(super) probed: Option<(PathBuf, Instant)>,
    pub(super) probing: bool,
    pub expanded_dirs: HashSet<PathBuf>,
    pub(super) listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// 文件树里选中的路径。
    pub selected: Option<PathBuf>,
    /// 预览栏的标签。
    pub previews: super::super::preview::PreviewTabs,
    /// Git 面板排成的行、展开收起和提交说明。
    pub git_panel: super::super::git_panel::GitPanel,
    pub file_rows: Vec<FileRow>,
    pub files_scroll: UniformListScrollHandle,
    /// 后台正在读，读完之前不再发起。
    pub(super) refreshing: bool,
    /// 监听到了改动还没重读，或者读的时候又有了改动。
    pub(super) stale: bool,
    /// 上次开始读的时刻，以及读了多久，据此决定下次隔多久。
    pub(super) refreshed_at: Option<Instant>,
    pub(super) scan_cost: Duration,
    /// 标题栏命令菜单里的项目命令（Makefile 的目标、package.json 的 scripts）和给哪个目录列的；还没列过时
    /// 为空。后台正在列时不再发起；Makefile、package.json 这类文件变了时标成要重列。
    pub tasks: Option<(PathBuf, Vec<TaskSource>)>,
    pub tasks_listing: bool,
    pub tasks_stale: bool,
    /// 交给后台读的时候拿走，读完放回来。
    pub(super) untracked: git::UntrackedCache,
}

impl Project {
    pub(super) fn rebuild_file_rows(&mut self, root: &Path, show_ignored: bool) {
        let decorator = Decorator::new(self.git.as_ref());
        let mut rows = Vec::new();
        self.push_dir(root, 0, &decorator, show_ignored, &mut rows);
        self.file_rows = rows;
    }

    fn push_dir(&self, dir: &Path, depth: usize, decorator: &Decorator, show_ignored: bool, rows: &mut Vec<FileRow>) {
        let Some(entries) = self.listings.get(dir) else {
            return;
        };
        for entry in entries {
            let mut path = dir.join(&entry.name);
            if !show_ignored && decorator.of(&path, entry.is_dir) == Decoration::Ignored {
                continue;
            }
            let mut name = entry.name.clone();
            if entry.is_dir {
                for _ in 0..MAX_COMPACT {
                    let Some(child) = self.listings.get(&path).and_then(|entries| single_dir(entries)) else {
                        break;
                    };
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
            let expanded = entry.is_dir && self.expanded_dirs.contains(&path);
            rows.push(FileRow {
                name: name.into(),
                depth,
                is_dir: entry.is_dir,
                expanded,
                decoration: decorator.of(&path, entry.is_dir),
                path: path.clone(),
            });
            if expanded {
                self.push_dir(&path, depth + 1, decorator, show_ignored, rows);
            }
        }
    }

    /// 当场读 `dirs` 以及往下多看的一层，读不了的不记。
    fn list_now(&mut self, dirs: Vec<PathBuf>) {
        if dirs.is_empty() {
            return;
        }
        let listings = list_dirs(dirs, &Decorator::new(self.git.as_ref()));
        for (dir, listing) in listings {
            if let Some(listing) = listing {
                self.listings.insert(dir, listing);
            }
        }
    }

    /// 展开或收起文件树里的目录；展开时当场读它的内容，免得显示收起期间已经变了的旧列表，
    /// 之后跟着重读。
    pub fn toggle_dir(&mut self, path: &Path, root: &Path, show_ignored: bool) {
        if self.expanded_dirs.remove(path) {
            // 选中的在收起的目录里面时，选中改成这个目录，免得选中项看不见还被删除、新建用上。
            if self.selected.as_deref().is_some_and(|selected| selected != path && selected.starts_with(path)) {
                self.selected = Some(path.to_path_buf());
            }
        } else {
            self.expanded_dirs.insert(path.to_path_buf());
            self.list_now(vec![path.to_path_buf()]);
        }
        self.rebuild_file_rows(root, show_ignored);
    }

    /// 终端换了目录：在文件树里展开到 `dir`，连它本身也展开，选中它、滚到中间。
    fn reveal_dir(&mut self, dir: &Path, root: &Path, show_ignored: bool) {
        self.reveal(dir, true, root, show_ignored, ScrollStrategy::Center);
    }

    /// 在文件树里展开到 `path` 所在的目录，选中它、滚到能看见。已经显示着时只选中，不重排。
    pub fn reveal_file(&mut self, path: &Path, root: &Path, show_ignored: bool) {
        if let Some(ix) = self.file_rows.iter().position(|row| row.path == path) {
            self.select_row(ix);
            self.files_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            return;
        }
        self.reveal(path, false, root, show_ignored, ScrollStrategy::Nearest);
    }

    /// 在文件树里展开到 `path` 并选中它，按 `strategy` 滚到能看见的地方；`expand` 时连 `path`
    /// 这个目录本身也展开。`path` 就是根目录或者不在根下面时不动。
    fn reveal(&mut self, path: &Path, expand: bool, root: &Path, show_ignored: bool, strategy: ScrollStrategy) {
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
        self.expanded_dirs.extend(chain);
        self.rebuild_file_rows(root, show_ignored);
        // 并成一行的目录，行的路径是链条最深的那个，`path` 在链条中间时找不到原样的路径。
        let rows = &self.file_rows;
        let Some(ix) = rows
            .iter()
            .position(|row| row.path == path)
            .or_else(|| rows.iter().position(|row| row.path.starts_with(path)))
        else {
            return;
        };
        self.selected = Some(rows[ix].path.clone());
        self.files_scroll.scroll_to_item(ix, strategy);
    }

    /// 收起根目录下所有展开的目录。
    pub fn collapse_all(&mut self, root: &Path, show_ignored: bool) {
        self.expanded_dirs.retain(|dir| !dir.starts_with(root));
        self.rebuild_file_rows(root, show_ignored);
        // 选中的行跟着收起来看不见了，不留着。
        if self.selected_row().is_none() {
            self.selected = None;
        }
    }

    /// 改了文件之后当场重读 `dirs`，不等监听到改动；后台的重读随后照常进行。
    pub fn relist(&mut self, dirs: Vec<PathBuf>, root: &Path, show_ignored: bool) {
        self.list_now(dirs);
        self.rebuild_file_rows(root, show_ignored);
    }

    /// `from` 改名或挪到了 `to`：展开的目录和选中的路径跟过去，旧路径下读过的目录不再要。
    pub fn moved(&mut self, from: &Path, to: &Path) {
        let follow = |path: &Path| follow_move(path, from, to);
        self.expanded_dirs = self.expanded_dirs.drain().map(|dir| follow(&dir).unwrap_or(dir)).collect();
        self.listings.retain(|dir, _| !dir.starts_with(from));
        if let Some(selected) = self.selected.as_deref().and_then(follow) {
            self.selected = Some(selected);
        }
    }

    /// `path` 被删掉了：它和下面的目录不再展开。
    pub fn removed(&mut self, path: &Path) {
        self.expanded_dirs.retain(|dir| !dir.starts_with(path));
        self.listings.retain(|dir, _| !dir.starts_with(path));
    }

    /// 选中的行在 `file_rows` 里的位置。
    pub fn selected_row(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.file_rows.iter().position(|row| row.path == *selected)
    }

    /// 键盘上下移动选中的行，`step` 为负往上；没选中时往下从第一行开始，往上从最后一行开始。
    /// 返回新选中的位置。
    pub fn select_step(&mut self, step: isize) -> Option<usize> {
        let last = self.file_rows.len().checked_sub(1)?;
        let ix = match self.selected_row() {
            Some(ix) => ix.saturating_add_signed(step).min(last),
            None if step > 0 => 0,
            None => last,
        };
        self.selected = Some(self.file_rows[ix].path.clone());
        Some(ix)
    }

    /// 选中第 `ix` 行。
    pub fn select_row(&mut self, ix: usize) -> Option<usize> {
        let row = self.file_rows.get(ix)?;
        self.selected = Some(row.path.clone());
        Some(ix)
    }

    /// 键盘往左：展开着的目录收起，否则跳到上一级目录。返回新选中的位置。
    pub fn select_out(&mut self, root: &Path, show_ignored: bool) -> Option<usize> {
        let ix = self.selected_row()?;
        let row = &self.file_rows[ix];
        if row.is_dir && row.expanded {
            let path = row.path.clone();
            self.toggle_dir(&path, root, show_ignored);
            return Some(ix);
        }
        let depth = row.depth;
        let parent = self.file_rows[..ix].iter().rposition(|row| row.depth < depth)?;
        self.select_row(parent)
    }

    /// 键盘往右：收着的目录展开，展开着的跳到它的第一项。返回新选中的位置。
    pub fn select_in(&mut self, root: &Path, show_ignored: bool) -> Option<usize> {
        let ix = self.selected_row()?;
        let row = &self.file_rows[ix];
        if !row.is_dir {
            return None;
        }
        if !row.expanded {
            let path = row.path.clone();
            self.toggle_dir(&path, root, show_ignored);
            return Some(ix);
        }
        let depth = row.depth;
        self.file_rows.get(ix + 1).filter(|child| child.depth > depth)?;
        self.select_row(ix + 1)
    }
}

impl Workspace {
    /// 换上后台读到的结果，有变化时返回真。
    pub(super) fn apply_scan(&mut self, scan: Scan, show_ignored: bool) -> bool {
        let project = &mut self.project;
        // 第一次读时 `root` 还是空的，下面换上根目录时一定算作有变化。
        let mut changed = false;
        project.untracked = scan.untracked;
        project.in_repo = Some(scan.git.is_some());
        project.probed = Some((scan.dir.clone(), Instant::now()));
        // 终端换到了别的仓库或目录：上一处的目录列表不再相干。展开的目录
        // 是绝对路径，留着，回到原处时还是展开的。
        if project.root.as_ref() != Some(&scan.root) {
            project.root = Some(scan.root.clone());
            project.listings.clear();
            project.files_scroll.scroll_to_item(0, ScrollStrategy::Top);
            changed = true;
        }
        if project.git != scan.git {
            let old = std::mem::replace(&mut project.git, scan.git);
            for preview in &mut project.previews.tabs {
                preview.git_changed(old.as_ref(), project.git.as_ref());
            }
            changed = true;
        }
        for (dir, listing) in scan.listings {
            match listing {
                Some(entries) => {
                    if project.listings.get(&dir) != Some(&entries) {
                        project.listings.insert(dir, entries);
                        changed = true;
                    }
                }
                // 目录被删掉或者改了名，收起来。
                None => {
                    changed |= project.listings.remove(&dir).is_some();
                    changed |= project.expanded_dirs.remove(&dir);
                }
            }
        }
        if changed {
            project.git_panel.rebuild(project.git.as_ref());
            project.rebuild_file_rows(&scan.root, show_ignored);
        }
        // 终端换了目录，文件树跟过去。
        if project.dir.as_ref() != Some(&scan.dir) {
            project.reveal_dir(&scan.dir, &scan.root, show_ignored);
            project.dir = Some(scan.dir);
            changed = true;
        }
        changed
    }
}

/// `from` 改名或挪到了 `to` 之后 `path` 的新路径；`path` 不是 `from` 也不在它下面时为 `None`。
/// `path` 就是 `from` 时直接是 `to`：`to.join("")` 会多出结尾的 `/`，把文件当成目录去读。
pub fn follow_move(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use runode_git::FileStatus;

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

    fn names(project: &Project) -> Vec<(String, usize)> {
        project.file_rows.iter().map(|row| (row.name.to_string(), row.depth)).collect()
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
        let mut project = Project::default();
        let root = Path::new("/p");
        project.listings.insert(root.into(), vec![dir("src"), file("README.md")]);
        project.listings.insert("/p/src".into(), vec![file("main.rs")]);
        project.rebuild_file_rows(root, true);
        assert_eq!(project.file_rows.len(), 2);
        project.expanded_dirs.insert("/p/src".into());
        project.rebuild_file_rows(root, true);
        assert_eq!(names(&project), [("src".into(), 0), ("main.rs".into(), 1), ("README.md".into(), 0)]);
    }

    #[test]
    fn compacts_single_child_dirs() {
        let mut project = Project::default();
        let root = Path::new("/p");
        project.listings.insert(root.into(), vec![dir("crates"), file("Cargo.toml")]);
        project.listings.insert("/p/crates".into(), vec![dir("app")]);
        project.listings.insert("/p/crates/app".into(), vec![dir("src"), file("Cargo.toml")]);
        project.rebuild_file_rows(root, true);
        assert_eq!(names(&project), [("crates / app".into(), 0), ("Cargo.toml".into(), 0)]);
        assert_eq!(project.file_rows[0].path, PathBuf::from("/p/crates/app"));
        project.expanded_dirs.insert("/p/crates/app".into());
        project.rebuild_file_rows(root, true);
        assert_eq!(names(&project)[1..3], [("src".into(), 1), ("Cargo.toml".into(), 1)]);
    }

    #[test]
    fn hides_ignored_entries() {
        let mut project = Project { git: Some(snapshot()), ..Default::default() };
        let root = Path::new("/repo");
        project.listings.insert(root.into(), vec![dir("src"), dir("target"), file("new.txt")]);
        project.rebuild_file_rows(root, false);
        assert_eq!(names(&project), [("src".into(), 0), ("new.txt".into(), 0)]);
        project.rebuild_file_rows(root, true);
        assert_eq!(project.file_rows.len(), 3);

        // 只装着被忽略目录的目录不和它并成一行。
        project.git.as_mut().unwrap().main.ignored.insert("out/target".into());
        project.listings.insert(root.into(), vec![dir("out")]);
        project.listings.insert("/repo/out".into(), vec![dir("target")]);
        project.rebuild_file_rows(root, false);
        assert_eq!(names(&project), [("out".into(), 0)]);
        project.rebuild_file_rows(root, true);
        assert_eq!(names(&project), [("out".into(), 0)]);
        assert_eq!(project.file_rows[0].decoration, Decoration::None);
    }

    #[test]
    fn reveals_the_terminal_dir() {
        let mut project = Project::default();
        let root = Path::new("/p");
        project.listings.insert(root.into(), vec![dir("a"), dir("b")]);
        project.listings.insert("/p/b".into(), vec![dir("c"), file("x")]);
        project.listings.insert("/p/b/c".into(), vec![file("y")]);
        project.reveal_dir(Path::new("/p/b/c"), root, true);
        assert_eq!(
            names(&project),
            [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("y".into(), 2), ("x".into(), 1)]
        );
        assert_eq!(project.selected, Some(PathBuf::from("/p/b/c")));

        // 终端目录在并成一行的链条中间时，展开到链条尽头。
        project.listings.insert("/p/a".into(), vec![dir("m")]);
        project.listings.insert("/p/a/m".into(), vec![file("z")]);
        project.reveal_dir(Path::new("/p/a"), root, true);
        assert_eq!(names(&project)[..2], [("a / m".into(), 0), ("z".into(), 1)]);
        assert_eq!(project.selected, Some(PathBuf::from("/p/a/m")));
    }

    #[test]
    fn reveals_files_without_expanding_them() {
        let mut project = Project::default();
        let root = Path::new("/p");
        project.listings.insert(root.into(), vec![dir("a"), dir("b")]);
        project.listings.insert("/p/b".into(), vec![dir("c"), file("x")]);
        project.listings.insert("/p/b/c".into(), vec![file("y")]);
        project.reveal_file(Path::new("/p/b/x"), root, true);
        assert_eq!(names(&project), [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("x".into(), 1)]);
        assert_eq!(project.selected, Some(PathBuf::from("/p/b/x")));
        project.collapse_all(root, true);
        assert_eq!(names(&project), [("a".into(), 0), ("b".into(), 0)]);
    }

    #[test]
    fn moves_the_selection_with_the_keyboard() {
        let mut project = Project::default();
        let root = Path::new("/p");
        project.listings.insert(root.into(), vec![dir("a"), file("z")]);
        project.listings.insert("/p/a".into(), vec![file("b"), file("c")]);
        project.rebuild_file_rows(root, true);
        assert_eq!(project.select_step(-1), Some(1));
        assert_eq!(project.select_step(1), Some(1));
        assert_eq!(project.select_step(-5), Some(0));
        // 往右先展开，再进到第一项；往左先回到上一级，再收起。
        assert_eq!(project.select_in(root, true), Some(0));
        assert_eq!(project.file_rows.len(), 4);
        assert_eq!(project.select_in(root, true), Some(1));
        assert_eq!(project.selected, Some(PathBuf::from("/p/a/b")));
        assert_eq!(project.select_in(root, true), None);
        assert_eq!(project.select_out(root, true), Some(0));
        assert_eq!(project.select_out(root, true), Some(0));
        assert_eq!(project.file_rows.len(), 2);
        assert_eq!(project.select_out(root, true), None);

        // 改名或挪走的目录还是展开的，选中的跟过去。
        project.select_in(root, true);
        project.select_step(1);
        project.moved(Path::new("/p/a"), Path::new("/p/d"));
        assert!(project.expanded_dirs.contains(Path::new("/p/d")));
        assert!(!project.listings.contains_key(Path::new("/p/a")));
        assert_eq!(project.selected, Some(PathBuf::from("/p/d/b")));
        project.removed(Path::new("/p/d"));
        assert!(project.expanded_dirs.is_empty());
    }
}
