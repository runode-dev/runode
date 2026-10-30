//! 右侧各栏共用的项目状态：当前终端所在仓库的 git 改动和文件树。面板显示时监听仓库目录，
//! 有文件变了才在后台重读，监听不了时定时重读。文件树、Git 面板和 GitHub Actions 页合在右侧面板里，顶上一排
//! 标签切换；面板的开关，右侧各栏的宽度、分隔线，以及开关按钮也在这里。
//!
//! 在后台读 git 状态和文件树要显示的目录在 `scan`，每个 workspace 记着的状态和换上读到的结果在 `state`，
//! 监听目录在 `watch`；这三处不碰界面。文件树本身在 `file_tree`，读到的目录交给它，见 `files`。

mod scan;
mod state;
mod watch;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use super::{
    CARD_GAP, DIVIDER_GRAB_WIDTH, Divider, TITLEBAR_HEIGHT, ToggleFiles, ToggleGit, ToggleGitHubActions, WindowView,
    cards,
};
use crate::ui::button::Toggle;
use crate::ui::menu::MenuHost as _;
use gpui::{
    Action, AnyElement, App, Context, CursorStyle, Div, Focusable, MouseButton, MouseDownEvent, Role, Stateful, Window,
    div, prelude::*, px,
};
// 面板外框在 `runode_ui::panel`，窗口里各面板从这里引。
use crate::ui::a11y::A11yPress;
pub(super) use crate::ui::panel::{panel_message, panel_shell};
use crate::{
    assets::{ACTIONS_WORKFLOW_ICON, FILES_ICON, GIT_ICON, PANEL_RIGHT_ICON},
    ui::tooltip::tooltip,
};
use scan::scan;

pub(super) use state::Project;
pub(super) use watch::{ProjectChanges, ProjectWatch};

/// 显示右侧面板时隔这么久看一次终端换没换目录。
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 面板都收着时，隔多久再问一次当前目录在不在 git 仓库里（`git init` 之后按钮才出得来）。
const REPO_PROBE_INTERVAL: Duration = Duration::from_secs(5);
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
/// 右侧各栏的默认宽度，以及拖动的下限。
const PANEL_WIDTH: f32 = 280.;
const PANEL_MIN_WIDTH: f32 = 220.;
/// 右侧面板再宽也给终端区留这么宽。
const MAIN_MIN_WIDTH: f32 = 240.;
/// 标题栏右上角开关按钮和右侧面板顶上标签的尺寸和间距。
pub(super) const TOGGLE_WIDTH: f32 = 28.;
pub(super) const TOGGLE_HEIGHT: f32 = 24.;
const TOGGLE_GAP: f32 = 4.;
pub(super) const TOGGLE_MARGIN: f32 = 10.;
/// 标题栏右上角那组按钮的个数：「更多」（没有菜单栏的系统上才有）、命令和面板开关。
const TOGGLE_COUNT: f32 = if super::more_menu::SHOWN { 3. } else { 2. };
/// 右侧面板都收着时标题栏右边给右上角那组按钮让出的宽度，窗口按钮另算。
pub(super) const PANEL_TOGGLES_INSET: f32 =
    TOGGLE_WIDTH * TOGGLE_COUNT + TOGGLE_GAP * (TOGGLE_COUNT - 1.) + TOGGLE_MARGIN + 6.;

/// 右侧面板显示的那一页。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SidePanel {
    #[default]
    Files,
    Git,
    GitHubActions,
}

/// 右侧实际画多宽，收着的为零。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PanelWidths {
    pub panel: f32,
}

impl PanelWidths {
    pub fn total(self) -> f32 {
        self.panel
    }
}

impl WindowView {
    pub(super) fn project_visible(&self, cx: &App) -> bool {
        self.workspace().panel.is_some() || self.preview_shown(cx)
    }

    pub(super) fn files_shown(&self) -> bool {
        self.workspace().panel == Some(SidePanel::Files)
    }

    pub(super) fn git_shown(&self) -> bool {
        self.workspace().panel == Some(SidePanel::Git)
    }

    /// 右侧面板读哪个目录：当前终端的目录，取不到、或者后台看过它不是目录（删掉了，ssh 报来的远端目录）时是
    /// workspace 的目录。在不在由 `check_cwd` 在后台看：这里每秒都跑，目录在挂死的网络卷上时 stat 会卡住，
    /// 不能在界面线程上看。还没看过时沿用上次读的目录，不先拿它去读，免得每 cd 进一个不存在的目录面板就闪一下。
    pub(super) fn project_dir(&self, cx: &Context<Self>) -> PathBuf {
        let workspace = self.workspace();
        let Some(cwd) = self.focused_view().and_then(|view| view.read(cx).cwd()) else {
            return workspace.dir.clone();
        };
        match &workspace.project.cwd {
            Some((dir, true)) if *dir == cwd => cwd,
            Some((dir, false)) if *dir == cwd => workspace.dir.clone(),
            _ => workspace.project.dir.clone().unwrap_or_else(|| workspace.dir.clone()),
        }
    }

    /// 在单独的线程上（`fs_probe::off_thread`，不占后台线程池）看当前终端的目录是不是目录，记下来；
    /// `project_dir` 因此变了时按新目录再检查一遍。
    fn check_cwd(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.focused_view().and_then(|view| view.read(cx).cwd()) else {
            return;
        };
        let workspace = &mut self.workspaces[self.active];
        if workspace.project.cwd_checking.as_ref() == Some(&cwd) {
            return;
        }
        workspace.project.cwd_checking = Some(cwd.clone());
        let id = workspace.id;
        // 起不了线程时当它是目录，照终端报的目录读。
        let job = crate::fs_probe::off_thread(
            {
                let cwd = cwd.clone();
                move || cwd.is_dir()
            },
            true,
        );
        cx.spawn(async move |this, cx| {
            let is_dir = job.await;
            this.update(cx, |this, cx| {
                // 看的时候 workspace 可能已经关掉了。
                let Some(ix) = this.workspaces.iter().position(|workspace| workspace.id == id) else {
                    return;
                };
                let before = this.project_dir(cx);
                let project = &mut this.workspaces[ix].project;
                if project.cwd_checking.as_ref() == Some(&cwd) {
                    project.cwd_checking = None;
                }
                project.cwd = Some((cwd, is_dir));
                if ix == this.active && this.project_dir(cx) != before {
                    this.poll_project(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 正在监听当前 workspace 的文件树根目录。
    fn watching(&self) -> bool {
        let root = self.workspace().project.root.as_ref();
        self.project_watch.as_ref().is_some_and(|watch| Some(&watch.root) == root)
    }

    /// 让监听跟上当前 workspace 的文件树根目录；右侧面板都收着时不听。
    /// 建监听要看、要解析目录，目录在挂死的网络卷上时会一直卡着，所以在单独的线程上建（同
    /// `canonical_dir_later`），建好之前照旧的监听或定时重读。
    pub(super) fn sync_project_watch(&mut self, cx: &mut Context<Self>) {
        let project = &self.workspace().project;
        let root = project.root.clone().filter(|_| self.project_visible(cx));
        let Some(root) = root else {
            self.project_watch = None;
            self.building_watch = None;
            return;
        };
        let target = (root, project.watch_git_dirs.clone());
        let built =
            self.project_watch.as_ref().is_some_and(|watch| (&watch.root, &watch.git_dirs) == (&target.0, &target.1));
        if built || self.building_watch.as_ref() == Some(&target) {
            return;
        }
        self.building_watch = Some(target.clone());
        let (done, watch) = futures::channel::oneshot::channel();
        let (root, git_dirs, events) = (target.0.clone(), target.1.clone(), self.project_events.clone());
        // 起不了线程时 `done` 跟着丢掉，下面收到的是空的。
        let _ = std::thread::Builder::new()
            .name("project-watch".into())
            .spawn(move || done.send(ProjectWatch::new(root, git_dirs, events)).ok());
        cx.spawn(async move |this, cx| {
            let watch = watch.await.ok().flatten();
            this.update(cx, |this, cx| {
                // 建的时候又换了要听的目录：这个不要了。
                if this.building_watch.as_ref() != Some(&target) {
                    return;
                }
                this.building_watch = None;
                this.project_watch = watch;
                // 监听建好之前那次读的期间改了什么听不到，按有改动再读一次。
                if this.project_watch.is_some() && this.workspace().project.root.as_ref() == Some(&target.0) {
                    this.workspace_mut().project.stale = true;
                    this.refresh_if_due(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 监听到 `changes`：有要紧的就重读。窗口在后台时只记下来，切到前台时再读。说不准哪里变了
    /// （`ProjectChanges::everything`）时按路径筛的几样都当作变了。
    pub(super) fn project_changed(&mut self, changes: ProjectChanges, active: bool, cx: &mut Context<Self>) {
        let ProjectChanges { paths, everything } = changes;
        // 预览的文件变了就重读；窗口在后台时等切回前台再按修改时间判断。说不准哪里变了时都当作变了、直接重读：
        // 丢了事件时按修改时间比不出同样大小、修改时间又被设回去的改写，防抖并进来的几批里明确变了的文件也在内。
        if active {
            if everything {
                self.preview_all_files_changed(cx);
            } else {
                self.preview_files_changed(&paths, cx);
            }
        }
        // 分支、tag 变了时图表重读；工作区里的文件变了不重读。Git 页收着时也要标：读过的图表留着，切回来时
        // 只看这个标记决定重不重读。
        let id = self.workspace().id;
        if everything {
            self.with_git_panel(cx, |panel, cx| panel.all_refs_changed(id, cx));
        } else {
            self.with_git_panel(cx, |panel, cx| panel.refs_changed(id, &paths, cx));
        }
        let Some(watch) = &self.project_watch else {
            return;
        };
        let (id, git) = (self.workspace().id, self.workspace().project.git.as_deref());
        let tree = self.file_tree.read(cx);
        let listed = |dir: &std::path::Path| tree.is_listed(id, dir);
        if !self.watching() || !(everything || paths.iter().any(|path| watch.affects(path, git, &listed))) {
            return;
        }
        // 命令从这几个文件里来，它们变了就重列。
        if everything || paths.iter().any(|path| super::tasks::is_task_file(path)) {
            self.workspace_mut().project.tasks_stale = true;
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
        // 终端换了目录时，看过它在不在之后 `check_cwd` 再按新目录读。
        self.check_cwd(cx);
        if !self.project_visible(cx) {
            // 切到右侧什么都不显示的 workspace 时，放掉上一个 workspace 的目录监听。
            self.sync_project_watch(cx);
            self.probe_repo(cx);
            return;
        }
        let dir = self.project_dir(cx);
        let expanded = self.file_tree.read(cx).expanded_dirs(self.workspace().id);
        // 其他工作树只在 Git 面板里显示，面板没开或者藏起来了时不读；打开面板时 `toggle_git` 会重读一次。
        let worktrees = self.git_shown() && self.git_panel.read(cx).options().show_worktrees;
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
        let untracked = std::mem::take(&mut project.untracked);
        let started = crate::file_tree::read_ticket();
        let job = cx.background_spawn(async move { scan(dir, expanded, untracked, worktrees) });
        cx.spawn(async move |this, cx| {
            let scan = job.await;
            this.update(cx, |this, cx| {
                // 读的时候 workspace 可能已经关掉了。
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                workspace.project.refreshing = false;
                workspace.project.scan_cost = scan.cost;
                let (changed, tree) = workspace.apply_scan(scan, started);
                let git = workspace.project.git.clone();
                if changed {
                    cx.notify();
                }
                // 文件树换上读到的目录；是它正显示着的 workspace 时它自己重画，有搜索词时重搜。
                this.file_tree.update(cx, |page, cx| page.apply_scan(id, tree, cx));
                // 预览栏按新的 git 状态重算改动标记，diff 标签记下要不要重读。
                this.sync_preview_git(id, cx);
                // Git 面板按新的 git 状态重排。面板收着时也交：预览栏里按块暂存要经它在这份状态上跑。
                this.git_panel.update(cx, |panel, cx| panel.set_git(id, git, cx));
                if this.workspace().id == id {
                    this.sync_project_watch(cx);
                    this.reload_stale_diff(cx);
                    this.list_tasks(cx);
                    this.refresh_if_due(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 面板都收着时不读整份状态，只在终端换了目录、或离上次问过 `REPO_PROBE_INTERVAL` 之后
    /// 问一下当前目录在不在 git 仓库里，展开时据此决定显不显示 Git 标签。
    fn probe_repo(&mut self, cx: &mut Context<Self>) {
        let dir = self.project_dir(cx);
        let workspace = &mut self.workspaces[self.active];
        // 侧栏的分支徽标每轮都在问当前 workspace 的分支，看得出来时用它的。
        if workspace.repo.covers_repo_dir(&workspace.dir, &dir) {
            // 记着的那次问的结论已经不是 `in_repo` 了，回到那个目录时要重问。
            workspace.project.probed = None;
            if workspace.project.in_repo != Some(true) {
                workspace.project.in_repo = Some(true);
                cx.notify();
            }
            return;
        }
        let project = &mut workspace.project;
        let fresh =
            project.probed.as_ref().is_some_and(|(probed, at)| *probed == dir && at.elapsed() < REPO_PROBE_INTERVAL);
        if project.probing || fresh {
            return;
        }
        project.probing = true;
        let id = workspace.id;
        let job = cx.background_spawn({
            let dir = dir.clone();
            async move { runode_git::in_repo(&dir) }
        });
        cx.spawn(async move |this, cx| {
            let in_repo = job.await;
            this.update(cx, |this, cx| {
                // 问的时候 workspace 可能已经关掉了。
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                let project = &mut workspace.project;
                project.probing = false;
                if project.in_repo != Some(in_repo) {
                    project.in_repo = Some(in_repo);
                    cx.notify();
                }
                project.probed = Some((dir, Instant::now()));
            })
            .ok();
        })
        .detach();
    }

    /// 定时检查：终端换了目录时重读；有没读的改动时按 `refresh_if_due` 重读；监听不了时
    /// 退回定时重读，离上次开始读至少隔 `FALLBACK_INTERVAL`，上次读得慢时按耗时拉长。
    pub(super) fn poll_project(&mut self, cx: &mut Context<Self>) {
        self.check_cwd(cx);
        self.list_tasks(cx);
        self.poll_github_actions(cx);
        self.refresh_repo_badges(cx);
        if !self.project_visible(cx) {
            self.probe_repo(cx);
            return;
        }
        let dir = self.project_dir(cx);
        let project = &self.workspace().project;
        if project.dir.as_ref() != Some(&dir) {
            self.refresh_project(cx);
        } else if project.stale {
            self.refresh_if_due(cx);
        } else {
            // 监听不了目录，或者有其他工作树（它们的工作目录监听不到）时定时重读。
            let interval = if !self.watching() {
                Some(FALLBACK_INTERVAL)
            } else if self.git_shown() && project.git.as_ref().is_some_and(|git| !git.worktrees.is_empty()) {
                Some(WORKTREE_INTERVAL)
            } else {
                None
            };
            if let Some(interval) = interval {
                let wait = interval.max(project.scan_cost * POLL_BACKOFF);
                if project.refreshed_at.is_none_or(|at| at.elapsed() >= wait) {
                    self.refresh_project(cx);
                }
            }
        }
    }

    pub(super) fn toggle_git(&mut self, _: &ToggleGit, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel(SidePanel::Git, window, cx);
    }

    pub(super) fn toggle_files(&mut self, _: &ToggleFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel(SidePanel::Files, window, cx);
    }

    pub(super) fn toggle_github_actions(
        &mut self,
        _: &ToggleGitHubActions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_panel(SidePanel::GitHubActions, window, cx);
    }

    /// 右侧面板切到 `page` 这一页，已经在这一页时收起。
    fn toggle_panel(&mut self, page: SidePanel, window: &mut Window, cx: &mut Context<Self>) {
        self.set_panel((self.workspace().panel != Some(page)).then_some(page), window, cx);
    }

    /// 右侧面板切到 `panel` 这一页，为空时收起。
    fn set_panel(&mut self, panel: Option<SidePanel>, window: &mut Window, cx: &mut Context<Self>) {
        let (git_was_shown, files_were_shown) = (self.git_shown(), self.files_shown());
        let workspace = self.workspace_mut();
        workspace.panel = panel;
        if let Some(panel) = panel {
            workspace.last_panel = panel;
        }
        // 收起或切走时焦点还在提交说明框或文件树里的话，按键就没处去了，交回终端。
        if git_was_shown && !self.git_shown() {
            // 菜单存在页上，页不画了菜单也画不出来：先关掉，焦点回到页上再一起交回终端。
            self.git_panel.update(cx, |page, cx| page.close_menu(window, cx));
            if self.git_panel.focus_handle(cx).contains_focused(window, cx) {
                window.focus(&self.focus_handle(cx), cx);
            }
            self.close_branch_picker(window, cx);
        }
        if files_were_shown && !self.files_shown() {
            // 菜单存在页上，页不画了菜单也画不出来：先关掉，焦点回到树上再一起交回终端。
            self.file_tree.update(cx, |page, cx| page.close_menu(window, cx));
            if self.file_tree.focus_handle(cx).contains_focused(window, cx) {
                window.focus(&self.focus_handle(cx), cx);
            }
        }
        if !self.github_actions_shown() {
            self.cancel_github_actions_edit(window, cx);
        }
        self.sync_project_watch(cx);
        self.refresh_project(cx);
        self.save(cx);
        cx.notify();
    }

    /// 右侧面板显示着的那一页，收着时为空。
    pub(super) fn render_side_panel(&mut self, width: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.workspace().panel? {
            SidePanel::Files => Some(self.render_files_panel(width, cx).into_any_element()),
            SidePanel::Git => Some(self.render_git_panel(width, cx).into_any_element()),
            SidePanel::GitHubActions => Some(self.render_github_actions_panel(width, cx).into_any_element()),
        }
    }

    /// 右侧实际画多宽，收着的为零。窗口窄时压右侧面板，尽量给终端区留出 `MAIN_MIN_WIDTH`，但不窄于
    /// 它的下限。
    pub(super) fn right_panel_widths(&self, viewport: f32) -> PanelWidths {
        let sidebar = if self.sidebar_visible() { self.sidebar_width() } else { 0. };
        let room = viewport - sidebar - MAIN_MIN_WIDTH;
        let panel = match self.workspace().panel {
            Some(_) => self.panel_width.unwrap_or(PANEL_WIDTH).min(room).max(PANEL_MIN_WIDTH),
            None => 0.,
        };
        PanelWidths { panel }
    }

    /// 拖动右侧面板左边的分隔线，左边跟到窗口里的横坐标 `x`。
    pub(super) fn resize_right_panel(&mut self, x: f32, viewport: f32) {
        let sidebar = if self.sidebar_visible() { self.sidebar_width() } else { 0. };
        let room = viewport - sidebar - MAIN_MIN_WIDTH;
        self.panel_width = Some((viewport - x).min(room).max(PANEL_MIN_WIDTH));
    }

    /// 右侧面板左边的分隔线把手，盖在窗口的最上层；`right` 是分隔线离窗口右边的距离。
    pub(super) fn render_right_handle(&self, divider: Divider, right: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("panel-divider")
            .absolute()
            // 卡片样式下标题栏横跨右侧面板，分隔线从标题栏下面开始，不挡着拖动窗口。
            .top(px(if cards(cx) { TITLEBAR_HEIGHT } else { 0. }))
            .bottom_0()
            .right(px(right - DIVIDER_GRAB_WIDTH / 2.))
            .w(px(DIVIDER_GRAB_WIDTH))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        // 双击恢复默认宽度。
                        this.panel_width = None;
                        this.save(cx);
                    } else {
                        this.dragging_divider = Some(divider);
                    }
                    cx.notify();
                }),
            )
    }

    /// 当前目录问过或读过了且不在 git 仓库里时藏起 Git 标签；正显示着 Git 时仍留着。
    pub(super) fn git_button_visible(&self) -> bool {
        self.git_shown() || self.workspace().project.in_repo != Some(false)
    }

    /// 右侧面板左边的分隔线离窗口右边多远：经典样式下是面板的宽度；卡片样式下还要加上卡片到窗口右边的
    /// 间距，分隔线落在终端区和面板两张卡片中间。
    pub(super) fn right_divider_offset(&self, widths: PanelWidths, cards: bool) -> f32 {
        if cards { widths.panel + CARD_GAP * 1.5 } else { widths.panel }
    }

    /// 标题栏右上角的按钮：没有菜单栏的系统上先是「更多」按钮，然后是命令按钮和开关右侧面板的按钮，
    /// 展开时底色亮一些，打开的是上次显示的那一页。位置由调用方接着写。
    pub(super) fn render_panel_toggles(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let shown = self.workspace().panel.is_some();
        let text = if shown { rust_i18n::t!("tooltip.hide_panel") } else { rust_i18n::t!("tooltip.show_panel") };
        let view = cx.entity().downgrade();
        let toggle = Toggle::new("toggle-panel", PANEL_RIGHT_ICON, text.clone().into_owned())
            .pressed(shown)
            .aria_expanded(shown)
            .w(px(TOGGLE_WIDTH))
            .h(px(TOGGLE_HEIGHT))
            .tooltip(tooltip(text, None))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    let panel = if this.workspace().panel.is_some() { None } else { Some(this.workspace().last_panel) };
                    this.set_panel(panel, window, cx);
                }),
            )
            // 按下鼠标就办，没有 on_click，辅助工具的按下另外登记。
            .on_a11y_press(view, move |this, window, cx| {
                let panel = if this.workspace().panel.is_some() { None } else { Some(this.workspace().last_panel) };
                this.set_panel(panel, window, cx);
            });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(TOGGLE_GAP))
            .when(super::more_menu::SHOWN, |toggles| toggles.child(self.render_more_button(window, cx)))
            .child(self.render_tasks_button(window, cx))
            .child(toggle)
    }

    /// 右侧面板顶上那排切换文件树和 Git 的标签，显示着的那个底色亮一些。经典样式下标题栏右上角的
    /// 开关按钮盖在这一条的右端。
    pub(super) fn render_panel_tabs(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let tab = |page: SidePanel, cx: &mut Context<Self>| {
            let (id, icon, text, action): (_, _, _, &dyn Action) = match page {
                SidePanel::Files => ("tab-files", FILES_ICON, rust_i18n::t!("tooltip.show_files"), &ToggleFiles),
                SidePanel::Git => ("tab-git", GIT_ICON, rust_i18n::t!("tooltip.show_git"), &ToggleGit),
                SidePanel::GitHubActions => (
                    "tab-github-actions",
                    ACTIONS_WORKFLOW_ICON,
                    rust_i18n::t!("tooltip.show_github_actions"),
                    &ToggleGitHubActions,
                ),
            };
            let view = cx.entity().downgrade();
            Toggle::new(id, icon, text.clone().into_owned())
                .pressed(self.workspace().panel == Some(page))
                .role(Role::Tab)
                .aria_selected(self.workspace().panel == Some(page))
                .w(px(TOGGLE_WIDTH))
                .h(px(TOGGLE_HEIGHT))
                .tooltip(tooltip(text, Some(action)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.set_panel(Some(page), window, cx);
                    }),
                )
                .on_a11y_press(view, move |this, window, cx| this.set_panel(Some(page), window, cx))
        };
        self.panel_header(cx)
            .id("panel-tabs")
            .role(Role::TabList)
            .px(px(6.))
            .gap(px(TOGGLE_GAP))
            .child(tab(SidePanel::Files, cx))
            .when(self.git_button_visible(), |tabs| tabs.child(tab(SidePanel::Git, cx)))
            .when(self.github_actions_tab_visible(cx), |tabs| tabs.child(tab(SidePanel::GitHubActions, cx)))
    }

    /// 右侧面板顶上的一条，见 `runode_ui::panel::panel_header`。
    pub(super) fn panel_header(&self, cx: &App) -> Div {
        crate::ui::panel::panel_header(cards(cx), cx)
    }
}
