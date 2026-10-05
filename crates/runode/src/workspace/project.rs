//! 右侧两栏共用的项目状态：每个 workspace 目录的 git 改动和文件树，显示时定时在后台重读。
//! 两栏的开关、宽度、分隔线和标题栏右上角的开关按钮也在这里。

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui::{
    Context, CursorStyle, Div, MouseButton, MouseDownEvent, SharedString, Stateful, UniformListScrollHandle,
    Window, div, prelude::*, px, svg,
};

use super::{DIVIDER_GRAB_WIDTH, Divider, TITLEBAR_HEIGHT, ToggleChanges, ToggleFiles, WindowView, Workspace};
use crate::{
    assets::{CHANGES_ICON, FILES_ICON},
    git::{self, FileStatus},
    session::Rgb,
    terminal_view::hsla,
};

/// 显示右侧面板时至少隔这么久重读一次 git 状态和展开的目录。
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 上次读得慢时，下次至少隔上次耗时的这么多倍，大仓库里不至于一直有 git 在跑。
const POLL_BACKOFF: u32 = 10;
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

/// 后台读到的一份结果：git 状态，以及项目目录和展开的目录的内容，读不了的目录为 `None`。
struct Scan {
    git: Option<git::Snapshot>,
    listings: Vec<(PathBuf, Option<Vec<DirEntry>>)>,
    /// 读这一份花的时间。
    cost: Duration,
}

fn scan(dir: &Path, expanded: Vec<PathBuf>) -> Scan {
    let started = Instant::now();
    let listings = std::iter::once(dir.to_path_buf())
        .chain(expanded)
        .map(|dir| {
            let listing = read_dir(&dir);
            (dir, listing)
        })
        .collect();
    let git = git::snapshot(dir);
    Scan { git, listings, cost: started.elapsed() }
}

/// 改动栏里的一行；下标指向 `git::Snapshot` 里的文件、块和行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffRow {
    File(usize),
    Hunk(usize, usize),
    Line(usize, usize, usize),
    Note(usize, DiffNote),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffNote {
    Binary,
    Truncated,
    /// 只改了权限或者只改了名。
    NoContent,
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
    pub path: PathBuf,
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

    fn of(&self, path: &Path, is_dir: bool) -> Decoration {
        let Some(git) = self.git else {
            return Decoration::None;
        };
        let Ok(rel) = path.strip_prefix(&git.root) else {
            return Decoration::None;
        };
        if rel.ancestors().any(|dir| self.ignored.contains(dir)) {
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
    /// 最近一次读到的 git 状态；不在 git 仓库里时为空。
    pub git: Option<git::Snapshot>,
    /// 读到过至少一次。
    pub loaded: bool,
    /// 用户点过、和默认展开状态相反的文件，相对仓库根。
    toggled_diffs: HashSet<PathBuf>,
    pub expanded_dirs: HashSet<PathBuf>,
    listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// 文件树里选中的路径。
    pub selected: Option<PathBuf>,
    pub diff_rows: Vec<DiffRow>,
    pub file_rows: Vec<FileRow>,
    pub changes_scroll: UniformListScrollHandle,
    pub files_scroll: UniformListScrollHandle,
    /// 后台正在读，读完之前不再发起。
    refreshing: bool,
    /// 上次开始读的时刻，以及读了多久，定时重读时据此决定隔多久。
    refreshed_at: Option<Instant>,
    scan_cost: Duration,
}

impl Project {
    pub fn diff_expanded(&self, file: &git::FileDiff) -> bool {
        let default = file.added + file.removed <= COLLAPSED_LINES;
        default != self.toggled_diffs.contains(&file.path)
    }

    fn rebuild_diff_rows(&mut self) {
        let mut rows = Vec::new();
        for (fi, file) in self.git.iter().flat_map(|git| git.files.iter().enumerate()) {
            rows.push(DiffRow::File(fi));
            if !self.diff_expanded(file) {
                continue;
            }
            if file.binary {
                rows.push(DiffRow::Note(fi, DiffNote::Binary));
                continue;
            }
            if file.hunks.is_empty() && !file.truncated {
                rows.push(DiffRow::Note(fi, DiffNote::NoContent));
            }
            for (hi, hunk) in file.hunks.iter().enumerate() {
                rows.push(DiffRow::Hunk(fi, hi));
                rows.extend((0..hunk.lines.len()).map(|li| DiffRow::Line(fi, hi, li)));
            }
            if file.truncated {
                rows.push(DiffRow::Note(fi, DiffNote::Truncated));
            }
        }
        self.diff_rows = rows;
    }

    fn rebuild_file_rows(&mut self, root: &Path) {
        let decorator = Decorator::new(self.git.as_ref());
        let mut rows = Vec::new();
        self.push_dir(root, 0, &decorator, &mut rows);
        self.file_rows = rows;
    }

    fn push_dir(&self, dir: &Path, depth: usize, decorator: &Decorator, rows: &mut Vec<FileRow>) {
        let Some(entries) = self.listings.get(dir) else {
            return;
        };
        for entry in entries {
            let path = dir.join(&entry.name);
            let expanded = entry.is_dir && self.expanded_dirs.contains(&path);
            rows.push(FileRow {
                name: entry.name.clone().into(),
                depth,
                is_dir: entry.is_dir,
                expanded,
                decoration: decorator.of(&path, entry.is_dir),
                path: path.clone(),
            });
            if expanded {
                self.push_dir(&path, depth + 1, decorator, rows);
            }
        }
    }

    /// 展开或收起一个文件的改动。
    pub fn toggle_diff(&mut self, path: &Path) {
        if !self.toggled_diffs.remove(path) {
            self.toggled_diffs.insert(path.to_path_buf());
        }
        self.rebuild_diff_rows();
    }

    /// 展开或收起文件树里的目录；展开时当场读它的内容，免得显示收起期间已经变了的旧列表，
    /// 之后跟着定时重读。
    pub fn toggle_dir(&mut self, path: &Path, root: &Path) {
        if !self.expanded_dirs.remove(path) {
            self.expanded_dirs.insert(path.to_path_buf());
            self.listings.insert(path.to_path_buf(), read_dir(path).unwrap_or_default());
        }
        self.rebuild_file_rows(root);
    }

    /// 文件在改动栏里那一行的位置；`path` 是绝对路径。
    pub fn diff_row_of(&self, path: &Path) -> Option<(usize, PathBuf)> {
        let git = self.git.as_ref()?;
        let rel = path.strip_prefix(&git.root).ok()?;
        let fi = git.files.iter().position(|file| file.path == rel)?;
        let row = self.diff_rows.iter().position(|row| *row == DiffRow::File(fi))?;
        Some((row, rel.to_path_buf()))
    }
}

impl Workspace {
    /// 换上后台读到的结果，有变化时返回真。
    fn apply_scan(&mut self, scan: Scan) -> bool {
        let project = &mut self.project;
        let mut changed = !project.loaded;
        project.loaded = true;
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
            project.rebuild_file_rows(&self.dir);
        }
        changed
    }
}

impl WindowView {
    pub(super) fn project_visible(&self) -> bool {
        self.changes_shown || self.files_shown
    }

    /// 在后台重读当前 workspace 的 git 状态和展开的目录；右侧面板都收着或者上次还没读完时
    /// 不读。
    pub(super) fn refresh_project(&mut self, cx: &mut Context<Self>) {
        if !self.project_visible() {
            return;
        }
        let workspace = &mut self.workspaces[self.active];
        if workspace.project.refreshing {
            return;
        }
        workspace.project.refreshing = true;
        workspace.project.refreshed_at = Some(Instant::now());
        let id = workspace.id;
        let dir = workspace.dir.clone();
        let expanded = workspace.project.expanded_dirs.iter().cloned().collect();
        let job = cx.background_spawn(async move { scan(&dir, expanded) });
        cx.spawn(async move |this, cx| {
            let scan = job.await;
            this.update(cx, |this, cx| {
                // 读的时候 workspace 可能已经关掉了。
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                workspace.project.refreshing = false;
                workspace.project.scan_cost = scan.cost;
                if workspace.apply_scan(scan) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 定时重读：离上次开始读至少隔 `POLL_INTERVAL`，上次读得慢时按耗时拉长。
    pub(super) fn poll_project(&mut self, cx: &mut Context<Self>) {
        let project = &self.workspace().project;
        let wait = POLL_INTERVAL.max(project.scan_cost * POLL_BACKOFF);
        if project.refreshed_at.is_some_and(|at| at.elapsed() < wait) {
            return;
        }
        self.refresh_project(cx);
    }

    pub(super) fn toggle_changes(&mut self, _: &ToggleChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.changes_shown = !self.changes_shown;
        self.refresh_project(cx);
        self.save(cx);
        cx.notify();
    }

    pub(super) fn toggle_files(&mut self, _: &ToggleFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.files_shown = !self.files_shown;
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
            files: Vec::new(),
            statuses: HashMap::from([
                ("src/a/b.rs".into(), FileStatus::Modified),
                ("new.txt".into(), FileStatus::Untracked),
            ]),
            ignored: vec!["target".into()],
        }
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
        let dir = |name: &str| DirEntry { name: name.into(), is_dir: true };
        let file = |name: &str| DirEntry { name: name.into(), is_dir: false };
        project.listings.insert(root.into(), vec![dir("src"), file("README.md")]);
        project.listings.insert("/p/src".into(), vec![file("main.rs")]);
        project.rebuild_file_rows(root);
        assert_eq!(project.file_rows.len(), 2);
        project.expanded_dirs.insert("/p/src".into());
        project.rebuild_file_rows(root);
        let rows: Vec<_> = project.file_rows.iter().map(|row| (row.name.to_string(), row.depth)).collect();
        assert_eq!(rows, [("src".into(), 0), ("main.rs".into(), 1), ("README.md".into(), 0)]);
    }
}
