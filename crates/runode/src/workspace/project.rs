//! 右侧两栏共用的项目状态：当前终端所在仓库的 git 改动和文件树。面板显示时监听仓库目录，
//! 有文件变了才在后台重读，监听不了时定时重读。两栏的开关、宽度、分隔线和标题栏右上角的
//! 开关按钮也在这里。

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    Context, CursorStyle, Div, MouseButton, MouseDownEvent, ScrollStrategy, SharedString, Stateful,
    UniformListScrollHandle,
    Window, div, prelude::*, px, svg,
};
use notify::Watcher as _;
use runode_git::{self as git, FileStatus, Section};

use super::{DIVIDER_GRAB_WIDTH, Divider, TITLEBAR_HEIGHT, ToggleChanges, ToggleFiles, WindowView, Workspace};
use crate::{
    assets::{CHANGES_ICON, FILES_ICON},
    session::Rgb,
    terminal_view::hsla,
};

/// 显示右侧面板时隔这么久看一次终端换没换目录。
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 监听不了目录时退回定时重读，至少隔这么久；上次读得慢时，至少隔上次耗时的
/// `POLL_BACKOFF` 倍，大仓库里不至于一直有 git 在跑。
const FALLBACK_INTERVAL: Duration = Duration::from_secs(2);
const POLL_BACKOFF: u32 = 10;
/// 监听到改动后先攒这么久再读：保存文件、提交这类操作会连着来一串事件。
pub(super) const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// 监听到改动时，离上次开始读至少隔上次耗时的这么多倍。
const WATCH_BACKOFF: u32 = 3;
/// 只有一个子目录的目录最多连着并这么多层，防着指回上层的符号链接绕圈。
const MAX_COMPACT: usize = 16;
/// 改动栏和文件树的默认宽度，以及拖动的下限。
const CHANGES_WIDTH: f32 = 520.;
const CHANGES_MIN_WIDTH: f32 = 280.;
const FILES_WIDTH: f32 = 240.;
const FILES_MIN_WIDTH: f32 = 160.;
/// 右侧面板再宽也给终端区留这么宽。
const MAIN_MIN_WIDTH: f32 = 240.;
/// 改动超过这么多行的文件默认收起。
const COLLAPSED_LINES: usize = 400;
/// 标题栏右上角开关按钮的尺寸和间距。
const TOGGLE_WIDTH: f32 = 24.;
const TOGGLE_HEIGHT: f32 = 20.;
const TOGGLE_GAP: f32 = 2.;
const TOGGLE_MARGIN: f32 = 8.;
/// 右侧面板都收着时标题栏右边给开关按钮让出的宽度。
pub(super) const PANEL_TOGGLES_INSET: f32 = TOGGLE_WIDTH * 2. + TOGGLE_GAP + TOGGLE_MARGIN + 6.;

/// 改动和文件状态的颜色，深浅背景上都看得清。
pub(super) const ADDED: Rgb = Rgb(0x57, 0xAB, 0x5A);
pub(super) const REMOVED: Rgb = Rgb(0xE5, 0x53, 0x4B);
pub(super) const MODIFIED: Rgb = Rgb(0xD2, 0xA8, 0x3E);
pub(super) const RENAMED: Rgb = Rgb(0x53, 0x9B, 0xF5);

pub(super) fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 目录里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// 读一个目录，目录在前，各自按名字排，不分大小写；读不了时为空。
fn read_dir(dir: &Path) -> Option<Vec<DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == ".DS_Store" {
                return None;
            }
            // 指向目录的符号链接也当目录；只有符号链接才要再查一次。
            let is_dir = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()),
                Ok(kind) => kind.is_dir(),
                Err(_) => false,
            };
            Some(DirEntry { name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Some(entries)
}

/// 目录里只有一个子目录时是它的名字，文件树把这样的目录和子目录并成一行。
fn single_dir(entries: &[DirEntry]) -> Option<&str> {
    match entries {
        [entry] if entry.is_dir => Some(&entry.name),
        _ => None,
    }
}

/// 读 `dirs` 这些目录，再往下多看一层：收起的子目录只有一个子目录时要和它并成一行，得先
/// 知道它里面有什么；这样的链条一直读到头。被忽略的子目录不往下看，里面多半很大。
fn list_dirs(dirs: Vec<PathBuf>, decorator: &Decorator) -> Vec<(PathBuf, Option<Vec<DirEntry>>)> {
    let mut listed: HashSet<PathBuf> = dirs.iter().cloned().collect();
    let mut listings: Vec<_> = dirs.into_iter().map(|dir| {
        let listing = read_dir(&dir);
        (dir, listing)
    }).collect();
    let children: Vec<_> = listings
        .iter()
        .flat_map(|(dir, listing)| listing.iter().flatten().filter(|entry| entry.is_dir).map(|entry| dir.join(&entry.name)))
        .filter(|child| decorator.of(child, true) != Decoration::Ignored)
        .collect();
    for child in children {
        let mut next = Some(child);
        for _ in 0..MAX_COMPACT {
            let Some(dir) = next.take().filter(|dir| listed.insert(dir.clone())) else {
                break;
            };
            let listing = read_dir(&dir);
            next = listing.as_deref().and_then(single_dir).map(|name| dir.join(name));
            listings.push((dir, listing));
        }
    }
    listings
}

/// 后台读到的一份结果：git 状态，以及文件树根目录和展开的目录的内容，读不了的目录为 `None`。
struct Scan {
    /// 读的是哪个目录：当前终端的目录，取不到时是 workspace 的目录。
    dir: PathBuf,
    /// 文件树的根目录：`dir` 所在仓库的根，不在仓库里时是 `dir` 本身。
    root: PathBuf,
    git: Option<git::Snapshot>,
    listings: Vec<(PathBuf, Option<Vec<DirEntry>>)>,
    untracked: git::UntrackedCache,
    /// 读这一份花的时间。
    cost: Duration,
}

fn scan(dir: PathBuf, expanded: Vec<PathBuf>, mut untracked: git::UntrackedCache) -> Scan {
    let started = Instant::now();
    let git = git::snapshot(&dir, &mut untracked);
    let root = git.as_ref().map_or_else(|| dir.clone(), |git| git.root.clone());
    let dirs = std::iter::once(root.clone()).chain(expanded.into_iter().filter(|path| path.starts_with(&root)));
    let listings = list_dirs(dirs.collect(), &Decorator::new(git.as_ref()));
    Scan { dir, root, git, listings, untracked, cost: started.elapsed() }
}

/// 改动栏里的一行；下标指向 `git::Snapshot` 那一段里的文件、块和行，或者 `Project::diff_groups`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffRow {
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
pub(super) enum DiffNote {
    Binary,
    Truncated,
    /// 只改了权限或者只改了名。
    NoContent,
}

/// 改动栏里的一个目录分组。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DiffGroup {
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

/// 文件树里一项的 git 标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decoration {
    None,
    Status(FileStatus),
    /// 目录里有改动的文件。
    ContainsChanges,
    Ignored,
}

/// 文件树里的一行。
#[derive(Clone, Debug)]
pub(super) struct FileRow {
    /// 并成一行的目录是链条最深的那个。
    pub path: PathBuf,
    /// 并成一行的目录是「a / b / c」。
    pub name: SharedString,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    pub decoration: Decoration,
}

/// 按 git 状态给文件树里的路径找标记。
struct Decorator<'a> {
    git: Option<&'a git::Snapshot>,
    /// 含有改动文件的目录，相对仓库根。
    changed_dirs: HashSet<&'a Path>,
    ignored: HashSet<&'a Path>,
}

impl<'a> Decorator<'a> {
    fn new(git: Option<&'a git::Snapshot>) -> Self {
        let mut changed_dirs = HashSet::new();
        let mut ignored = HashSet::new();
        if let Some(git) = git {
            for path in git.statuses.keys() {
                changed_dirs.extend(path.ancestors().skip(1).filter(|dir| !dir.as_os_str().is_empty()));
            }
            ignored.extend(git.ignored.iter().map(PathBuf::as_path));
        }
        Self { git, changed_dirs, ignored }
    }

    /// `rel` 相对仓库根，它或者它所在的目录被忽略。
    fn ignores(&self, rel: &Path) -> bool {
        rel.ancestors().any(|dir| self.ignored.contains(dir))
    }

    fn of(&self, path: &Path, is_dir: bool) -> Decoration {
        let Some(git) = self.git else {
            return Decoration::None;
        };
        let Ok(rel) = path.strip_prefix(&git.root) else {
            return Decoration::None;
        };
        if self.ignores(rel) {
            return Decoration::Ignored;
        }
        if is_dir {
            if self.changed_dirs.contains(rel) {
                return Decoration::ContainsChanges;
            }
        } else if let Some(status) = git.statuses.get(rel) {
            return Decoration::Status(*status);
        }
        Decoration::None
    }
}

/// 一个 workspace 的改动和文件树。
#[derive(Default)]
pub(super) struct Project {
    /// 文件树的根目录，跟着当前终端的目录变；还没读过时为空。
    pub root: Option<PathBuf>,
    /// 上次读的是哪个目录，终端换了目录时据此在文件树里定位过去。
    dir: Option<PathBuf>,
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
    listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// 文件树里选中的路径。
    pub selected: Option<PathBuf>,
    pub diff_rows: Vec<DiffRow>,
    pub diff_groups: Vec<DiffGroup>,
    pub file_rows: Vec<FileRow>,
    pub changes_scroll: UniformListScrollHandle,
    pub files_scroll: UniformListScrollHandle,
    /// 后台正在读，读完之前不再发起。
    refreshing: bool,
    /// 监听到了改动还没重读，或者读的时候又有了改动。
    stale: bool,
    /// 上次开始读的时刻，以及读了多久，据此决定下次隔多久。
    refreshed_at: Option<Instant>,
    scan_cost: Duration,
    /// 交给后台读的时候拿走，读完放回来。
    untracked: git::UntrackedCache,
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

    fn rebuild_file_rows(&mut self, root: &Path, show_ignored: bool) {
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
    fn apply_scan(&mut self, scan: Scan, show_ignored: bool) -> bool {
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

/// 右侧面板在看的仓库或目录的监听。
pub(super) struct ProjectWatch {
    root: PathBuf,
    /// 不在 `root` 下面的 git 目录（worktree 的），也要听。
    git_dir: Option<PathBuf>,
    /// 事件里的路径解析过符号链接，按这两个的真实路径比。
    real_root: PathBuf,
    real_git_dir: Option<PathBuf>,
    _watcher: notify::RecommendedWatcher,
}

impl ProjectWatch {
    /// 开始监听，事件里的路径交给 `events`；监听不了时为空，由调用方退回定时重读。
    fn new(root: PathBuf, git_dir: Option<PathBuf>, events: UnboundedSender<Vec<PathBuf>>) -> Option<Self> {
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && !event.kind.is_access()
            {
                events.unbounded_send(event.paths).ok();
            }
        })
        .inspect_err(|err| tracing::warn!("监听 {} 失败：{err}", root.display()))
        .ok()?;
        for dir in std::iter::once(&root).chain(&git_dir) {
            if let Err(err) = watcher.watch(dir, notify::RecursiveMode::Recursive) {
                tracing::warn!("监听 {} 失败：{err}", dir.display());
                return None;
            }
        }
        let real_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let real_git_dir = git_dir.as_ref().map(|dir| fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()));
        Some(Self { root, git_dir, real_root, real_git_dir, _watcher: watcher })
    }

    /// `path` 变了要不要重读。git 目录里只看暂存区、引用这些，写对象和锁文件之后总跟着
    /// 改它们。被忽略的文件只在它所在的目录正显示在文件树里时才算，免得编译时产物目录里
    /// 一直在写，跟着一直重读。
    fn affects(&self, path: &Path, project: &Project) -> bool {
        // 文件系统监视的守护进程每次 `git status` 都会在自己的目录里写一下，也不看。
        let git_internal = |rel: &Path| {
            !rel.components().any(|part| part.as_os_str() == "objects" || part.as_os_str() == "fsmonitor--daemon")
                && rel.extension().is_none_or(|ext| ext != "lock")
        };
        if let Some(rel) = [&self.git_dir, &self.real_git_dir]
            .into_iter()
            .flatten()
            .find_map(|dir| path.strip_prefix(dir).ok())
        {
            return git_internal(rel);
        }
        let Some(rel) = [&self.root, &self.real_root].into_iter().find_map(|root| path.strip_prefix(root).ok()) else {
            return false;
        };
        if let Some(ix) = rel.components().position(|part| part.as_os_str() == ".git") {
            return git_internal(&rel.components().skip(ix + 1).collect::<PathBuf>());
        }
        let ignored = project.git.as_ref().is_some_and(|git| rel.ancestors().any(|dir| git.ignored.iter().any(|ig| ig == dir)));
        !ignored || rel.parent().is_some_and(|parent| project.listings.contains_key(&self.root.join(parent)))
    }
}

impl WindowView {
    pub(super) fn project_visible(&self) -> bool {
        self.changes_shown || self.files_shown
    }

    /// 右侧面板读哪个目录：当前终端的目录，取不到时是 workspace 的目录。
    fn project_dir(&self, cx: &Context<Self>) -> PathBuf {
        let cwd = self.tab().focused_view().read(cx).cwd();
        cwd.filter(|cwd| cwd.is_dir()).unwrap_or_else(|| self.workspace().dir.clone())
    }

    /// 正在监听当前 workspace 的文件树根目录。
    fn watching(&self) -> bool {
        let root = self.workspace().project.root.as_ref();
        self.project_watch.as_ref().is_some_and(|watch| Some(&watch.root) == root)
    }

    /// 让监听跟上当前 workspace 的文件树根目录；右侧面板都收着时不听。
    pub(super) fn sync_project_watch(&mut self) {
        let project = &self.workspace().project;
        let root = project.root.clone().filter(|_| self.project_visible());
        let Some(root) = root else {
            self.project_watch = None;
            return;
        };
        let real_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let git_dir = project.git.as_ref().map(|git| git.git_dir.clone()).filter(|dir| {
            let real = fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
            !real.starts_with(&real_root)
        });
        if self.project_watch.as_ref().is_some_and(|watch| watch.root == root && watch.git_dir == git_dir) {
            return;
        }
        self.project_watch = ProjectWatch::new(root, git_dir, self.project_events.clone());
        // 监听建好之前那次读的期间改了什么听不到，按有改动再读一次。
        if self.project_watch.is_some() {
            self.workspace_mut().project.stale = true;
        }
    }

    /// 监听到 `paths` 变了：有要紧的就重读。窗口在后台时只记下来，切到前台时再读。
    pub(super) fn project_changed(&mut self, paths: Vec<PathBuf>, active: bool, cx: &mut Context<Self>) {
        let Some(watch) = &self.project_watch else {
            return;
        };
        let project = &self.workspace().project;
        if !self.watching() || !paths.iter().any(|path| watch.affects(path, project)) {
            return;
        }
        self.workspace_mut().project.stale = true;
        if active {
            self.refresh_if_due(cx);
        }
    }

    /// 有没读的改动时重读；上次读得慢时等够上次耗时的 `WATCH_BACKOFF` 倍，没等够的由
    /// `poll_project` 接着等。
    fn refresh_if_due(&mut self, cx: &mut Context<Self>) {
        let project = &self.workspace().project;
        let due = project.refreshed_at.is_none_or(|at| at.elapsed() >= project.scan_cost * WATCH_BACKOFF);
        if project.stale && due {
            self.refresh_project(cx);
        }
    }

    /// 在后台重读当前终端目录的 git 状态和展开的目录；右侧面板都收着时不读，上次还没读完时
    /// 记下来，读完再读一次。
    pub(super) fn refresh_project(&mut self, cx: &mut Context<Self>) {
        if !self.project_visible() {
            return;
        }
        let dir = self.project_dir(cx);
        let workspace = &mut self.workspaces[self.active];
        let project = &mut workspace.project;
        if project.refreshing {
            project.stale = true;
            return;
        }
        project.refreshing = true;
        project.stale = false;
        project.refreshed_at = Some(Instant::now());
        let id = workspace.id;
        let expanded = project.expanded_dirs.iter().cloned().collect();
        let untracked = std::mem::take(&mut project.untracked);
        let job = cx.background_spawn(async move { scan(dir, expanded, untracked) });
        cx.spawn(async move |this, cx| {
            let scan = job.await;
            this.update(cx, |this, cx| {
                let show_ignored = this.show_ignored;
                // 读的时候 workspace 可能已经关掉了。
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                workspace.project.refreshing = false;
                workspace.project.scan_cost = scan.cost;
                if workspace.apply_scan(scan, show_ignored) {
                    cx.notify();
                }
                if this.workspace().id == id {
                    this.sync_project_watch();
                    this.refresh_if_due(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 定时检查：终端换了目录时重读；有没读的改动时按 `refresh_if_due` 重读；监听不了时
    /// 退回定时重读，离上次开始读至少隔 `FALLBACK_INTERVAL`，上次读得慢时按耗时拉长。
    pub(super) fn poll_project(&mut self, cx: &mut Context<Self>) {
        if !self.project_visible() {
            return;
        }
        let dir = self.project_dir(cx);
        let project = &self.workspace().project;
        if project.dir.as_ref() != Some(&dir) {
            self.refresh_project(cx);
        } else if project.stale {
            self.refresh_if_due(cx);
        } else if !self.watching() {
            let wait = FALLBACK_INTERVAL.max(project.scan_cost * POLL_BACKOFF);
            if project.refreshed_at.is_none_or(|at| at.elapsed() >= wait) {
                self.refresh_project(cx);
            }
        }
    }

    /// 切换文件树里是否显示被 git 忽略的文件。
    pub(super) fn toggle_show_ignored(&mut self, cx: &mut Context<Self>) {
        self.show_ignored = !self.show_ignored;
        let show_ignored = self.show_ignored;
        for workspace in &mut self.workspaces {
            if let Some(root) = workspace.project.root.clone() {
                workspace.project.rebuild_file_rows(&root, show_ignored);
            }
        }
        self.save(cx);
        cx.notify();
    }

    pub(super) fn toggle_changes(&mut self, _: &ToggleChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.changes_shown = !self.changes_shown;
        self.sync_project_watch();
        self.refresh_project(cx);
        self.save(cx);
        cx.notify();
    }

    pub(super) fn toggle_files(&mut self, _: &ToggleFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.files_shown = !self.files_shown;
        self.sync_project_watch();
        self.refresh_project(cx);
        self.save(cx);
        cx.notify();
    }

    /// 改动栏和文件树实际画多宽，收着的为零。窗口窄时先压改动栏再压文件树，尽量给终端区
    /// 留出 `MAIN_MIN_WIDTH`，但不窄于各自的下限。
    pub(super) fn right_panel_widths(&self, viewport: f32) -> (f32, f32) {
        let sidebar = if self.sidebar_visible() { self.sidebar_width() } else { 0. };
        let room = viewport - sidebar - MAIN_MIN_WIDTH;
        let files = if self.files_shown { self.files_width.unwrap_or(FILES_WIDTH) } else { 0. };
        let changes = if self.changes_shown {
            self.changes_width.unwrap_or(CHANGES_WIDTH).min(room - files).max(CHANGES_MIN_WIDTH)
        } else {
            0.
        };
        let files = if self.files_shown { files.min(room - changes).max(FILES_MIN_WIDTH) } else { 0. };
        (changes, files)
    }

    /// 拖动右侧面板左边的分隔线，左边跟到窗口里的横坐标 `x`。
    pub(super) fn resize_right_panel(&mut self, divider: Divider, x: f32, viewport: f32) {
        let sidebar = if self.sidebar_visible() { self.sidebar_width() } else { 0. };
        let room = viewport - sidebar - MAIN_MIN_WIDTH;
        let (changes, files) = self.right_panel_widths(viewport);
        match divider {
            Divider::Changes => {
                let width = (viewport - files - x).min(room - files).max(CHANGES_MIN_WIDTH);
                self.changes_width = Some(width);
            }
            Divider::Files => {
                let width = (viewport - x).min(room - changes).max(FILES_MIN_WIDTH);
                self.files_width = Some(width);
            }
            Divider::Split(..) | Divider::Sidebar => {}
        }
    }

    /// 右侧面板左边的分隔线把手，盖在窗口的最上层；`right` 是分隔线离窗口右边的距离。
    pub(super) fn render_right_handle(&self, divider: Divider, right: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = if matches!(divider, Divider::Changes) { "changes-divider" } else { "files-divider" };
        div()
            .id(id)
            .absolute()
            .top_0()
            .h_full()
            .right(px(right - DIVIDER_GRAB_WIDTH / 2.))
            .w(px(DIVIDER_GRAB_WIDTH))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        // 双击恢复默认宽度。
                        match divider {
                            Divider::Changes => this.changes_width = None,
                            _ => this.files_width = None,
                        }
                        this.save(cx);
                    } else {
                        this.dragging_divider = Some(divider);
                    }
                    cx.notify();
                }),
            )
    }

    /// 标题栏右上角开关改动栏和文件树的两个按钮，打开着的那个底色亮一些。
    pub(super) fn render_panel_toggles(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let button = |id: &'static str, icon: &'static str, shown: bool, cx: &mut Context<Self>| {
            let hover_bg = hsla(bg.mix(fg, 0.10));
            let active_bg = hsla(bg.mix(fg, 0.14));
            let fg = hsla(fg);
            div()
                .id(id)
                .group(id)
                .w(px(TOGGLE_WIDTH))
                .h(px(TOGGLE_HEIGHT))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .when(shown, |button| button.bg(active_bg))
                .hover(|button| button.bg(hover_bg))
                .child(
                    svg()
                        .path(icon)
                        .size(px(16.))
                        .text_color(fg.opacity(if shown { 0.9 } else { 0.55 }))
                        .group_hover(id, |icon| icon.text_color(fg)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if id == "toggle-changes" {
                            this.toggle_changes(&ToggleChanges, window, cx);
                        } else {
                            this.toggle_files(&ToggleFiles, window, cx);
                        }
                    }),
                )
        };
        div()
            .absolute()
            .top(px((TITLEBAR_HEIGHT - TOGGLE_HEIGHT) / 2.))
            .right(px(TOGGLE_MARGIN))
            .flex()
            .gap(px(TOGGLE_GAP))
            .child(button("toggle-changes", CHANGES_ICON, self.changes_shown, cx))
            .child(button("toggle-files", FILES_ICON, self.files_shown, cx))
    }

    /// 右侧面板顶上和标题栏等高的一条：能拖动窗口、双击缩放，最右边的面板给开关按钮让位。
    pub(super) fn panel_header(&self, rightmost: bool, fg: Rgb) -> Div {
        div()
            .flex_none()
            .h(px(TITLEBAR_HEIGHT))
            .px(px(10.))
            .when(rightmost, |header| header.pr(px(PANEL_TOGGLES_INSET)))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(hsla(fg).opacity(0.12))
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> git::Snapshot {
        git::Snapshot {
            root: "/repo".into(),
            git_dir: "/repo/.git".into(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            statuses: HashMap::from([
                ("src/a/b.rs".into(), FileStatus::Modified),
                ("new.txt".into(), FileStatus::Untracked),
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
        assert_eq!(of("/repo/src", true), Decoration::ContainsChanges);
        assert_eq!(of("/repo/src/a", true), Decoration::ContainsChanges);
        assert_eq!(of("/repo/docs", true), Decoration::None);
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
