//! 一个 workspace 的改动栏和文件树状态：展开收起、排成行，以及换上后台读到的结果。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{ScrollStrategy, SharedString, UniformListScrollHandle};
use runode_git_status::{self as git, Section};

use super::{
    MAX_COMPACT,
    scan::{Decoration, Decorator, DirEntry, Scan, list_dirs, single_dir},
};
use crate::workspace::model::Workspace;

/// 改动超过这么多行的文件默认收起。
const COLLAPSED_LINES: usize = 400;

/// 改动栏里的一行；下标指向 `git::Snapshot` 那一段里的文件、块和行，或者 `Project::diff_groups`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum DiffRow {
    /// 「已暂存」「未暂存」的标题，只在有暂存的改动时分段。
    Section(Section),
    /// 目录的标题，同一个目录下的文件归在它下面。
    Group(usize),
    File(Section, usize),
    Hunk(Section, usize, usize),
    Line(Section, usize, usize, usize),
    Note(Section, usize, DiffNote),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum DiffNote {
    Binary,
    Truncated,
    /// 只改了权限或者只改了名。
    NoContent,
}

/// 改动栏里的一个目录分组。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::workspace) struct DiffGroup {
    pub section: Section,
    /// 相对仓库根；仓库根下的文件为空。
    pub dir: PathBuf,
    pub files: usize,
    pub expanded: bool,
}

/// 改动的文件归在哪个目录分组。
fn group_dir(file: &git::FileDiff) -> &Path {
    file.path.parent().unwrap_or(Path::new(""))
}

/// 文件树里的一行。
#[derive(Clone, Debug)]
pub(in crate::workspace) struct FileRow {
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
pub(in crate::workspace) struct Project {
    /// 文件树的根目录，跟着当前终端的目录变；还没读过时为空。
    pub root: Option<PathBuf>,
    /// 上次读的是哪个目录，终端换了目录时据此在文件树里定位过去。
    pub(super) dir: Option<PathBuf>,
    /// 最近一次读到的 git 状态；不在 git 仓库里时为空。
    pub git: Option<git::Snapshot>,
    /// 读到过至少一次。
    pub loaded: bool,
    /// 用户点过、和默认展开状态相反的文件，相对仓库根。
    toggled_diffs: HashSet<(Section, PathBuf)>,
    /// 改动栏里收起的分段和目录分组。
    collapsed_sections: HashSet<Section>,
    collapsed_groups: HashSet<(Section, PathBuf)>,
    pub expanded_dirs: HashSet<PathBuf>,
    pub(super) listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// 文件树里选中的路径。
    pub selected: Option<PathBuf>,
    pub diff_rows: Vec<DiffRow>,
    pub diff_groups: Vec<DiffGroup>,
    pub file_rows: Vec<FileRow>,
    pub changes_scroll: UniformListScrollHandle,
    pub files_scroll: UniformListScrollHandle,
    /// 后台正在读，读完之前不再发起。
    pub(super) refreshing: bool,
    /// 监听到了改动还没重读，或者读的时候又有了改动。
    pub(super) stale: bool,
    /// 上次开始读的时刻，以及读了多久，据此决定下次隔多久。
    pub(super) refreshed_at: Option<Instant>,
    pub(super) scan_cost: Duration,
    /// 交给后台读的时候拿走，读完放回来。
    pub(super) untracked: git::UntrackedCache,
}

impl Project {
    pub fn diff_expanded(&self, section: Section, file: &git::FileDiff) -> bool {
        let default = file.added + file.removed <= COLLAPSED_LINES;
        default != self.toggled_diffs.contains(&(section, file.path.clone()))
    }

    /// 有暂存的改动时分成「已暂存」「未暂存」两段，否则不分段。
    pub fn split_sections(&self) -> bool {
        self.git.as_ref().is_some_and(|git| !git.staged.is_empty())
    }

    pub fn section_expanded(&self, section: Section) -> bool {
        !self.collapsed_sections.contains(&section)
    }

    fn rebuild_diff_rows(&mut self) {
        let mut rows = Vec::new();
        let mut groups = Vec::new();
        let split = self.split_sections();
        for section in [Section::Staged, Section::Unstaged] {
            let Some(files) = self.git.as_ref().map(|git| git.files(section)).filter(|files| !files.is_empty()) else {
                continue;
            };
            if split {
                rows.push(DiffRow::Section(section));
                if !self.section_expanded(section) {
                    continue;
                }
            }
            // 按目录分组：组按目录排，组里按文件名排。
            let mut order: Vec<usize> = (0..files.len()).collect();
            order.sort_by(|&a, &b| group_dir(&files[a]).cmp(group_dir(&files[b])).then(files[a].path.cmp(&files[b].path)));
            for chunk in order.chunk_by(|&a, &b| group_dir(&files[a]) == group_dir(&files[b])) {
                let dir = group_dir(&files[chunk[0]]).to_path_buf();
                let expanded = !self.collapsed_groups.contains(&(section, dir.clone()));
                rows.push(DiffRow::Group(groups.len()));
                groups.push(DiffGroup { section, dir, files: chunk.len(), expanded });
                if !expanded {
                    continue;
                }
                for &fi in chunk {
                    let file = &files[fi];
                    rows.push(DiffRow::File(section, fi));
                    if !self.diff_expanded(section, file) {
                        continue;
                    }
                    if file.binary {
                        rows.push(DiffRow::Note(section, fi, DiffNote::Binary));
                        continue;
                    }
                    if file.hunks.is_empty() && !file.truncated {
                        rows.push(DiffRow::Note(section, fi, DiffNote::NoContent));
                    }
                    for (hi, hunk) in file.hunks.iter().enumerate() {
                        rows.push(DiffRow::Hunk(section, fi, hi));
                        rows.extend((0..hunk.lines.len()).map(|li| DiffRow::Line(section, fi, hi, li)));
                    }
                    if file.truncated {
                        rows.push(DiffRow::Note(section, fi, DiffNote::Truncated));
                    }
                }
            }
        }
        self.diff_rows = rows;
        self.diff_groups = groups;
    }

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
        let listings = list_dirs(dirs, &Decorator::new(self.git.as_ref()));
        for (dir, listing) in listings {
            if let Some(listing) = listing {
                self.listings.insert(dir, listing);
            }
        }
    }

    /// 展开或收起一个文件的改动。
    pub fn toggle_diff(&mut self, section: Section, path: &Path) {
        let key = (section, path.to_path_buf());
        if !self.toggled_diffs.remove(&key) {
            self.toggled_diffs.insert(key);
        }
        self.rebuild_diff_rows();
    }

    /// 展开或收起改动栏的一段。
    pub fn toggle_section(&mut self, section: Section) {
        if !self.collapsed_sections.remove(&section) {
            self.collapsed_sections.insert(section);
        }
        self.rebuild_diff_rows();
    }

    /// 展开或收起改动栏里的一个目录分组。
    pub fn toggle_group(&mut self, section: Section, dir: &Path) {
        let key = (section, dir.to_path_buf());
        if !self.collapsed_groups.remove(&key) {
            self.collapsed_groups.insert(key);
        }
        self.rebuild_diff_rows();
    }

    /// 展开或收起文件树里的目录；展开时当场读它的内容，免得显示收起期间已经变了的旧列表，
    /// 之后跟着重读。
    pub fn toggle_dir(&mut self, path: &Path, root: &Path, show_ignored: bool) {
        if !self.expanded_dirs.remove(path) {
            self.expanded_dirs.insert(path.to_path_buf());
            self.list_now(vec![path.to_path_buf()]);
        }
        self.rebuild_file_rows(root, show_ignored);
    }

    /// 在文件树里展开到 `dir` 并选中它，滚到能看见的地方。`dir` 就是根目录或者不在根下面时
    /// 不动。
    fn reveal_dir(&mut self, dir: &Path, root: &Path, show_ignored: bool) {
        let Ok(rel) = dir.strip_prefix(root) else {
            return;
        };
        let mut chain: Vec<_> = rel.ancestors().filter(|rel| !rel.as_os_str().is_empty()).map(|rel| root.join(rel)).collect();
        if chain.is_empty() {
            return;
        }
        chain.reverse();
        let missing = chain.iter().filter(|dir| !self.listings.contains_key(*dir)).cloned().collect();
        self.list_now(missing);
        // `dir` 和它唯一的子目录并成一行时，这一行按链条最深的那个展开，一路展开下去。
        for _ in 0..MAX_COMPACT {
            let last = &chain[chain.len() - 1];
            let Some(child) = self.listings.get(last).and_then(|entries| single_dir(entries)).map(|name| last.join(name))
            else {
                break;
            };
            if !self.listings.contains_key(&child) {
                self.list_now(vec![child.clone()]);
            }
            chain.push(child);
        }
        self.expanded_dirs.extend(chain);
        self.rebuild_file_rows(root, show_ignored);
        // 并成一行的目录，行的路径是链条最深的那个，`dir` 在链条中间时找不到原样的路径。
        let rows = &self.file_rows;
        let Some(ix) = rows.iter().position(|row| row.path == dir).or_else(|| rows.iter().position(|row| row.path.starts_with(dir)))
        else {
            return;
        };
        self.selected = Some(rows[ix].path.clone());
        self.files_scroll.scroll_to_item(ix, ScrollStrategy::Center);
    }

    /// 在改动栏里展开到文件 `path`（绝对路径）那一行，返回它的位置；有未暂存的改动时找那一段。
    pub fn reveal_diff(&mut self, path: &Path) -> Option<usize> {
        let git = self.git.as_ref()?;
        let rel = path.strip_prefix(&git.root).ok()?.to_path_buf();
        let (section, fi) = [Section::Unstaged, Section::Staged]
            .into_iter()
            .find_map(|section| Some((section, git.files(section).iter().position(|file| file.path == rel)?)))?;
        let file = &git.files(section)[fi];
        let dir = group_dir(file).to_path_buf();
        if !self.diff_expanded(section, file) {
            let key = (section, rel);
            if !self.toggled_diffs.remove(&key) {
                self.toggled_diffs.insert(key);
            }
        }
        self.collapsed_sections.remove(&section);
        self.collapsed_groups.remove(&(section, dir));
        self.rebuild_diff_rows();
        self.diff_rows.iter().position(|row| *row == DiffRow::File(section, fi))
    }
}

impl Workspace {
    /// 换上后台读到的结果，有变化时返回真。
    pub(super) fn apply_scan(&mut self, scan: Scan, show_ignored: bool) -> bool {
        let project = &mut self.project;
        let mut changed = !project.loaded;
        project.loaded = true;
        project.untracked = scan.untracked;
        // 终端换到了别的仓库或目录：上一处的目录列表和展开过的改动不再相干。展开的目录
        // 是绝对路径，留着，回到原处时还是展开的。
        if project.root.as_ref() != Some(&scan.root) {
            project.root = Some(scan.root.clone());
            project.listings.clear();
            project.toggled_diffs.clear();
            project.collapsed_groups.clear();
            project.changes_scroll.scroll_to_item(0, ScrollStrategy::Top);
            project.files_scroll.scroll_to_item(0, ScrollStrategy::Top);
            changed = true;
        }
        if project.git != scan.git {
            project.git = scan.git;
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
            project.rebuild_diff_rows();
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

#[cfg(test)]
mod tests {
    use super::*;
    use runode_git_status::FileStatus;

    fn snapshot() -> git::Snapshot {
        git::Snapshot {
            root: "/repo".into(),
            git_dir: "/repo/.git".into(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            statuses: HashMap::from([
                ("src/a/b.rs".into(), FileStatus::Modified),
                ("new.txt".into(), FileStatus::Untracked),
                ("docs/new/a.md".into(), FileStatus::Untracked),
                ("docs/old.md".into(), FileStatus::Deleted),
                ("src/c.rs".into(), FileStatus::Added),
            ]),
            ignored: vec!["target".into()],
        }
    }

    fn diff(path: &str, added: usize) -> git::FileDiff {
        git::FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            added,
            removed: 0,
            hunks: Vec::new(),
            binary: false,
            truncated: false,
        }
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
        project.git.as_mut().unwrap().ignored.push("out/target".into());
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
        assert_eq!(names(&project), [("a".into(), 0), ("b".into(), 0), ("c".into(), 1), ("y".into(), 2), ("x".into(), 1)]);
        assert_eq!(project.selected, Some(PathBuf::from("/p/b/c")));

        // 终端目录在并成一行的链条中间时，展开到链条尽头。
        project.listings.insert("/p/a".into(), vec![dir("m")]);
        project.listings.insert("/p/a/m".into(), vec![file("z")]);
        project.reveal_dir(Path::new("/p/a"), root, true);
        assert_eq!(names(&project)[..2], [("a / m".into(), 0), ("z".into(), 1)]);
        assert_eq!(project.selected, Some(PathBuf::from("/p/a/m")));
    }

    #[test]
    fn groups_changes_by_section_and_dir() {
        let mut git = snapshot();
        git.unstaged = vec![diff("README.md", 1), diff("src/a.rs", 1), diff("src/a/b.rs", 1), diff("src/c.rs", 1)];
        let mut project = Project { git: Some(git), ..Default::default() };
        project.rebuild_diff_rows();
        // 没有暂存的改动时不分段；`src/a/b.rs` 按路径排在 `src/c.rs` 前面，但分组跟着目录走。
        let groups: Vec<_> = project.diff_groups.iter().map(|group| (group.dir.clone(), group.files)).collect();
        assert_eq!(groups, [("".into(), 1), ("src".into(), 2), ("src/a".into(), 1)]);
        assert_eq!(project.diff_rows[..4], [DiffRow::Group(0), DiffRow::File(Section::Unstaged, 0), DiffRow::Note(Section::Unstaged, 0, DiffNote::NoContent), DiffRow::Group(1)]);

        project.git.as_mut().unwrap().staged = vec![diff("src/a.rs", 2)];
        project.toggle_group(Section::Unstaged, Path::new("src"));
        assert_eq!(project.diff_rows[0], DiffRow::Section(Section::Staged));
        let unstaged = project.diff_rows.iter().position(|row| *row == DiffRow::Section(Section::Unstaged)).unwrap();
        // 收起的分组只剩标题。
        assert_eq!(project.diff_rows[unstaged + 4..unstaged + 6], [DiffRow::Group(2), DiffRow::Group(3)]);
        project.toggle_section(Section::Staged);
        assert_eq!(project.diff_rows[..2], [DiffRow::Section(Section::Staged), DiffRow::Section(Section::Unstaged)]);

        // 从文件树定位过去时展开它所在的分段和分组，有未暂存的改动时找那一段。
        let row = project.reveal_diff(Path::new("/repo/src/a.rs")).unwrap();
        assert_eq!(project.diff_rows[row], DiffRow::File(Section::Unstaged, 1));
        // 手动收起过的小文件也展开。
        project.toggle_diff(Section::Unstaged, Path::new("src/a.rs"));
        let row = project.reveal_diff(Path::new("/repo/src/a.rs")).unwrap();
        assert_eq!(project.diff_rows[row + 1], DiffRow::Note(Section::Unstaged, 1, DiffNote::NoContent));
    }
}
