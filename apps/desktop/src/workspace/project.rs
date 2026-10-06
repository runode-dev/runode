//! 右侧各栏共用的项目状态：当前终端所在仓库的 git 改动和文件树。面板显示时监听仓库目录，
//! 有文件变了才在后台重读，监听不了时定时重读。Git 面板和文件树的开关，右侧各栏的宽度、
//! 分隔线，以及标题栏右上角的开关按钮也在这里。
//!
//! 读目录和 git 状态、给路径找标记在 `scan`，文件树排成行的状态在 `state`，监听目录
//! 在 `watch`；这三处不碰界面。

mod scan;
mod state;
mod watch;

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{
    Action, Context, CursorStyle, Div, Focusable, MouseButton, MouseDownEvent, Stateful, Window, div, prelude::*, px,
};
use runode_git::FileStatus;
use runode_shared_types::color::Rgb;

use super::{
    DIVIDER_GRAB_WIDTH, Divider, TITLEBAR_HEIGHT, ToggleFiles, ToggleGit, WindowView, divider_color, drag_window,
    titlebar::icon_toggle,
};
use crate::{
    assets::{FILES_ICON, GIT_ICON},
    ui::{hsla, tooltip::tooltip},
};
use scan::scan;

pub(super) use scan::Decoration;
pub(super) use state::Project;
pub(super) use watch::ProjectWatch;

/// 显示右侧面板时隔这么久看一次终端换没换目录。
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 监听不了目录时退回定时重读，至少隔这么久；上次读得慢时，至少隔上次耗时的
/// `POLL_BACKOFF` 倍，大仓库里不至于一直有 git 在跑。
const FALLBACK_INTERVAL: Duration = Duration::from_secs(2);
const POLL_BACKOFF: u32 = 10;
/// 有其他工作树时至少隔这么久重读一次：它们的工作目录不在监听的范围里，只有 git 目录（暂存、
/// 提交、切分支）在。上次读得慢时按 `POLL_BACKOFF` 拉长。
const WORKTREE_INTERVAL: Duration = Duration::from_secs(10);
/// 监听到改动后先攒这么久再读：保存文件、提交这类操作会连着来一串事件。
pub(super) const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// 监听到改动时，离上次开始读至少隔上次耗时的这么多倍。
const WATCH_BACKOFF: u32 = 3;
/// 只有一个子目录的目录最多连着并这么多层，防着指回上层的符号链接绕圈。
const MAX_COMPACT: usize = 16;
/// 右侧各栏的默认宽度，以及拖动的下限。
const GIT_WIDTH: f32 = 300.;
const GIT_MIN_WIDTH: f32 = 220.;
const FILES_WIDTH: f32 = 240.;
const FILES_MIN_WIDTH: f32 = 160.;
const PREVIEW_WIDTH: f32 = 480.;
const PREVIEW_MIN_WIDTH: f32 = 240.;
/// 右侧面板再宽也给终端区留这么宽。
const MAIN_MIN_WIDTH: f32 = 240.;
/// 标题栏右上角开关按钮的尺寸和间距。
const TOGGLE_WIDTH: f32 = 28.;
const TOGGLE_HEIGHT: f32 = 24.;
const TOGGLE_GAP: f32 = 4.;
const TOGGLE_MARGIN: f32 = 10.;
/// 右侧面板都收着时标题栏右边给开关按钮让出的宽度。
pub(super) const PANEL_TOGGLES_INSET: f32 = TOGGLE_WIDTH * 2. + TOGGLE_GAP + TOGGLE_MARGIN + 6.;

/// 改动和文件状态的颜色，深浅背景上都看得清。
pub(super) const ADDED: Rgb = Rgb(0x57, 0xAB, 0x5A);
pub(super) const REMOVED: Rgb = Rgb(0xE5, 0x53, 0x4B);
pub(super) const MODIFIED: Rgb = Rgb(0xD2, 0xA8, 0x3E);
pub(super) const RENAMED: Rgb = Rgb(0x53, 0x9B, 0xF5);

/// 加了多少行、删了多少行的标签，「+N」和「−N」。
pub(super) fn added_label(count: usize) -> Div {
    div().flex_none().text_color(hsla(ADDED)).child(format!("+{count}"))
}

pub(super) fn removed_label(count: usize) -> Div {
    div().flex_none().text_color(hsla(REMOVED)).child(format!("−{count}"))
}

pub(super) fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 右侧各栏实际画多宽，收着的为零。从左到右是预览栏、Git 面板、文件树。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PanelWidths {
    pub preview: f32,
    pub git: f32,
    pub files: f32,
}

impl PanelWidths {
    pub fn total(self) -> f32 {
        self.preview + self.git + self.files
    }
}

impl WindowView {
    pub(super) fn project_visible(&self) -> bool {
        self.git_shown || self.files_shown || self.preview_shown()
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
        // 主仓库是 worktree 时它的 git 目录、连同里面子模块的 git 目录都不在 `root` 下面；一个
        // 套在另一个里面的只听外面那个。
        let mut git_dirs: Vec<(PathBuf, PathBuf)> = project
            .git
            .iter()
            .flat_map(|git| git.iter())
            .map(|repo| {
                (repo.git_dir.clone(), fs::canonicalize(&repo.git_dir).unwrap_or_else(|_| repo.git_dir.clone()))
            })
            .filter(|(_, real)| !real.starts_with(&real_root))
            .collect();
        git_dirs.sort_by(|a, b| a.1.cmp(&b.1));
        git_dirs.dedup_by(|inner, outer| inner.1.starts_with(&outer.1));
        let git_dirs: Vec<_> = git_dirs.into_iter().map(|(dir, _)| dir).collect();
        if self.project_watch.as_ref().is_some_and(|watch| watch.root == root && watch.git_dirs == git_dirs) {
            return;
        }
        self.project_watch = ProjectWatch::new(root, git_dirs, self.project_events.clone());
        // 监听建好之前那次读的期间改了什么听不到，按有改动再读一次。
        if self.project_watch.is_some() {
            self.workspace_mut().project.stale = true;
        }
    }

    /// 监听到 `paths` 变了：有要紧的就重读。窗口在后台时只记下来，切到前台时再读。
    pub(super) fn project_changed(&mut self, paths: Vec<PathBuf>, active: bool, cx: &mut Context<Self>) {
        // 预览的文件变了就重读；窗口在后台时等切回前台再按修改时间判断。
        if active && self.preview().is_some_and(|preview| preview.affected_by(&paths)) {
            self.load_preview(cx);
        }
        // 分支、tag 变了时图表重读；工作区里的文件变了不重读。
        if self.git_shown {
            self.graph_refs_changed(&paths, cx);
        }
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
            // 切到右侧什么都不显示的 workspace 时，放掉上一个 workspace 的目录监听。
            self.sync_project_watch();
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
        // 其他工作树只在 Git 面板里显示，面板没开时不读；打开面板时 `toggle_git` 会重读一次。
        let options = runode_git::ReadOptions { worktrees: self.git_shown };
        let job = cx.background_spawn(async move { scan(dir, expanded, untracked, options) });
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
                    this.reload_stale_diff(cx);
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
        } else if self.git_shown && project.git.as_ref().is_some_and(|git| !git.worktrees.is_empty()) {
            let wait = WORKTREE_INTERVAL.max(project.scan_cost * POLL_BACKOFF);
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

    pub(super) fn toggle_git(&mut self, _: &ToggleGit, window: &mut Window, cx: &mut Context<Self>) {
        self.git_shown = !self.git_shown;
        // 收起时焦点还在提交说明框里的话，按键就没处去了，交回终端。
        if !self.git_shown && self.git_focus.contains_focused(window, cx) {
            window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        }
        if !self.git_shown {
            self.close_branch_picker(window, cx);
        }
        self.sync_project_watch();
        self.refresh_project(cx);
        self.save(cx);
        cx.notify();
    }

    pub(super) fn toggle_files(&mut self, _: &ToggleFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.files_shown = !self.files_shown;
        // 文件树收起时焦点还在里面的话，按键就没处去了，交回终端。
        if !self.files_shown && self.files_focus.contains_focused(window, cx) {
            window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        }
        self.sync_project_watch();
        self.refresh_project(cx);
        self.save(cx);
        cx.notify();
    }

    /// 右侧各栏实际画多宽，收着的为零。窗口窄时依次压预览栏、Git 面板，最后压文件树，
    /// 尽量给终端区留出 `MAIN_MIN_WIDTH`，但不窄于各自的下限。
    pub(super) fn right_panel_widths(&self, viewport: f32) -> PanelWidths {
        let sidebar = if self.sidebar_visible() { self.sidebar_width() } else { 0. };
        let room = viewport - sidebar - MAIN_MIN_WIDTH;
        let preview_shown = self.preview_shown();
        let files = if self.files_shown { self.files_width.unwrap_or(FILES_WIDTH) } else { 0. };
        let git = if self.git_shown { self.git_width.unwrap_or(GIT_WIDTH) } else { 0. };
        let preview = if preview_shown { self.preview_width.unwrap_or(PREVIEW_WIDTH) } else { 0. };
        let preview = if preview_shown { preview.min(room - files - git).max(PREVIEW_MIN_WIDTH) } else { 0. };
        let git = if self.git_shown { git.min(room - files - preview).max(GIT_MIN_WIDTH) } else { 0. };
        let files = if self.files_shown { files.min(room - preview - git).max(FILES_MIN_WIDTH) } else { 0. };
        PanelWidths { preview, git, files }
    }

    /// 拖动右侧面板左边的分隔线，左边跟到窗口里的横坐标 `x`。
    pub(super) fn resize_right_panel(&mut self, divider: Divider, x: f32, viewport: f32) {
        let sidebar = if self.sidebar_visible() { self.sidebar_width() } else { 0. };
        let room = viewport - sidebar - MAIN_MIN_WIDTH;
        let PanelWidths { preview, git, files } = self.right_panel_widths(viewport);
        match divider {
            Divider::Preview => {
                let width = (viewport - git - files - x).min(room - git - files).max(PREVIEW_MIN_WIDTH);
                self.preview_width = Some(width);
            }
            Divider::Git => {
                let width = (viewport - files - x).min(room - preview - files).max(GIT_MIN_WIDTH);
                self.git_width = Some(width);
            }
            Divider::Files => {
                let width = (viewport - x).min(room - preview - git).max(FILES_MIN_WIDTH);
                self.files_width = Some(width);
            }
            Divider::Split(..) | Divider::Sidebar => {}
        }
    }

    /// 右侧面板左边的分隔线把手，盖在窗口的最上层；`right` 是分隔线离窗口右边的距离。
    pub(super) fn render_right_handle(&self, divider: Divider, right: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = match divider {
            Divider::Preview => "preview-divider",
            Divider::Git => "git-divider",
            _ => "files-divider",
        };
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
                            Divider::Preview => this.preview_width = None,
                            Divider::Git => this.git_width = None,
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

    /// 标题栏右上角开关 Git 面板和文件树的两个按钮，打开着的底色亮一些。
    pub(super) fn render_panel_toggles(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        type Toggle = fn(&mut WindowView, &mut Window, &mut Context<WindowView>);
        let button = |id: &'static str, icon: &'static str, shown: bool, cx: &mut Context<Self>| {
            let (show, hide, action, toggle): (_, _, &dyn Action, Toggle) = match id {
                "toggle-git" => (
                    rust_i18n::t!("tooltip.show_git"),
                    rust_i18n::t!("tooltip.hide_git"),
                    &ToggleGit,
                    |this, window, cx| this.toggle_git(&ToggleGit, window, cx),
                ),
                _ => (
                    rust_i18n::t!("tooltip.show_files"),
                    rust_i18n::t!("tooltip.hide_files"),
                    &ToggleFiles,
                    |this, window, cx| this.toggle_files(&ToggleFiles, window, cx),
                ),
            };
            let tooltip = tooltip(if shown { hide } else { show }, Some(action), fg, bg);
            icon_toggle(id, icon, 16., shown, fg, bg)
                .w(px(TOGGLE_WIDTH))
                .h(px(TOGGLE_HEIGHT))
                .tooltip(tooltip)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        toggle(this, window, cx);
                    }),
                )
        };
        div()
            .absolute()
            .top(px((TITLEBAR_HEIGHT - TOGGLE_HEIGHT) / 2.))
            .right(px(TOGGLE_MARGIN))
            .flex()
            .gap(px(TOGGLE_GAP))
            .child(button("toggle-git", GIT_ICON, self.git_shown, cx))
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
            .border_color(divider_color(hsla(fg)))
            .on_mouse_down(MouseButton::Left, drag_window)
    }
}

/// 面板里居中的一句说明，比如不在仓库里、没有改动、预览不了。
pub(super) fn panel_message(text: String, fg: Rgb) -> Div {
    div().flex_1().flex().items_center().justify_center().px(px(16.)).text_color(hsla(fg).opacity(0.5)).child(text)
}

/// 右侧面板的外框：定宽、占满高度、竖着排，左边一条分隔线。底色和字号由调用方接着写。
pub(super) fn panel_shell(id: &'static str, width: f32, fg: Rgb) -> Stateful<Div> {
    div().id(id).flex_none().w(px(width)).h_full().flex().flex_col().border_l_1().border_color(divider_color(hsla(fg)))
}
