//! 窗口里的数据：workspace、标签和分屏布局，以及 `WindowView` 增删、查找、切换和关闭它们的操作。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

use gpui::{
    App, Bounds, Context, Entity, EntityId, Focusable, Pixels, ScrollHandle, SharedString, Subscription, Window,
};
use runode_shared_types::pane::{Node, SplitId};

use super::{WindowView, persistence, project::Project};
use crate::{
    persist,
    terminal_view::{TerminalEvent, TerminalView},
};

/// 标签的标识，标签挪动位置后不变。
pub(super) type TabId = u64;

pub(super) struct Tab {
    pub(super) id: TabId,
    /// 分屏布局；只有一个终端时是单个叶子。
    pub(super) root: Node<EntityId>,
    /// 这个标签里的终端，以及对它们事件的订阅。
    pub(super) panes: HashMap<EntityId, (Entity<TerminalView>, Subscription)>,
    /// 当前（切走之前最后）获得焦点的终端。
    pub(super) focused: EntityId,
    /// 当前终端放大占满整个标签，其他分屏暂时不画。
    pub(super) zoomed: bool,
    /// 不在前台时响过铃，切过去后清掉。agent 停下来不算，那由 agent 的标记表示。
    pub(super) bell: bool,
    /// agent 干完了、用户还没看过的分屏，见 `Mark::new`；用户看到那个分屏时清掉。
    pub(super) done: HashSet<EntityId>,
}

impl Tab {
    pub(super) fn focused_view(&self) -> &Entity<TerminalView> {
        &self.panes[&self.focused].0
    }
}

/// 上一帧各个终端和分屏节点在窗口里的位置，按方向切换和拖动分隔线时用。
#[derive(Default)]
pub(super) struct PaneLayout {
    pub(super) panes: HashMap<EntityId, Bounds<Pixels>>,
    pub(super) splits: HashMap<SplitId, Bounds<Pixels>>,
}

pub(super) fn home_dir() -> Option<PathBuf> {
    runode_paths::Dirs::from_env().home
}

/// 新 workspace 的名字：家目录叫 `~`；在 git 仓库里取仓库根的目录名，其余取目录名。
/// 往上找仓库根时不看家目录本身，免得家目录是个管配置文件的仓库时个个都叫用户名。
pub(super) fn workspace_name(dir: &Path) -> String {
    let home = home_dir();
    if home.as_deref() == Some(dir) {
        return "~".into();
    }
    let root =
        dir.ancestors().take_while(|d| Some(*d) != home.as_deref()).find(|d| d.join(".git").exists()).unwrap_or(dir);
    root.file_name().map_or_else(|| dir.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// 侧栏里显示的目录，家目录写成 `~`。
pub(super) fn display_dir(dir: &Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}

/// workspace 的标识，挪动位置后不变。
pub(super) type WorkspaceId = u64;

/// 一个项目目录，以及在其中打开的一组标签。
pub(super) struct Workspace {
    pub(super) id: WorkspaceId,
    /// 侧栏里显示的名字，新建时按目录取，可以改。
    pub(super) name: SharedString,
    /// 项目目录：新终端取不到当前终端的目录时从这里开始。
    pub(super) dir: PathBuf,
    /// 至少有一个；最后一个关掉时 workspace 跟着关。
    pub(super) tabs: Vec<Tab>,
    pub(super) active: usize,
    /// 标签条的滚动位置，切换标签时把当前标签滚进视野。
    pub(super) tab_scroll: ScrollHandle,
    /// Git 面板和文件树显示的内容。
    pub(super) project: Project,
}

impl Workspace {
    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// 有标签响过铃，还没切过去看。
    pub(super) fn bell(&self) -> bool {
        self.tabs.iter().any(|tab| tab.bell)
    }

    fn pane_count(&self) -> usize {
        self.tabs.iter().map(|tab| tab.panes.len()).sum()
    }
}

impl WindowView {
    pub(super) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub(super) fn workspace(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    pub(super) fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    /// 窗口里正显示的标签。
    pub(super) fn tab(&self) -> &Tab {
        self.workspace().active_tab()
    }

    pub(super) fn tab_mut(&mut self) -> &mut Tab {
        let workspace = &mut self.workspaces[self.active];
        &mut workspace.tabs[workspace.active]
    }

    pub(super) fn pane_entry(
        &mut self,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (EntityId, (Entity<TerminalView>, Subscription)) {
        let events = cx.subscribe_in(&view, window, Self::handle_terminal_event);
        (view.entity_id(), (view, events))
    }

    /// 记下刚开出来的终端从哪个目录开始，`start` 为空表示家目录。
    pub(super) fn record_spawn(&mut self, view: &Entity<TerminalView>, start: Option<&Path>) {
        self.spawned.retain(|_, (at, _)| at.elapsed() < persistence::SHELL_STARTUP);
        let start = start.map(Path::to_path_buf).or_else(home_dir);
        self.spawned.insert(view.entity_id(), (Instant::now(), start));
    }

    /// 在 `cwd` 里启动一个终端，为空时在家目录；启动失败时记日志，返回 `None`。
    pub(super) fn spawn_terminal(
        &mut self,
        cwd: Option<&Path>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Entity<TerminalView>> {
        match TerminalView::spawn(cwd, window, cx) {
            Ok(view) => {
                self.record_spawn(&view, cwd);
                Some(view)
            }
            Err(err) => {
                tracing::error!("failed to start terminal session: {err:#}");
                None
            }
        }
    }

    /// 只有 `view` 一个终端的新标签。
    pub(super) fn single_pane_tab(
        &mut self,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let (id, entry) = self.pane_entry(view, window, cx);
        Tab {
            id: self.next_id(),
            root: Node::Leaf(id),
            panes: HashMap::from([(id, entry)]),
            focused: id,
            zoomed: false,
            bell: false,
            done: HashSet::new(),
        }
    }

    pub(super) fn insert_tab(
        &mut self,
        ix: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.single_pane_tab(view, window, cx);
        self.workspace_mut().tabs.insert(ix, tab);
        self.activate(ix, window, cx);
    }

    /// 在第 `ix` 个位置放一个目录是 `dir` 的新 workspace，`view` 是它的第一个终端，并切过去。
    pub(super) fn insert_workspace(
        &mut self,
        ix: usize,
        dir: PathBuf,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.single_pane_tab(view, window, cx);
        let workspace = Workspace {
            id: self.next_id(),
            name: workspace_name(&dir).into(),
            dir,
            tabs: vec![tab],
            active: 0,
            tab_scroll: ScrollHandle::new(),
            project: Project::default(),
        };
        self.workspaces.insert(ix, workspace);
        self.activate_workspace(ix, window, cx);
    }

    /// 装着这个终端的 workspace 和标签。
    pub(super) fn locate(&self, pane: EntityId) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(wi, workspace)| {
            let ti = workspace.tabs.iter().position(|tab| tab.panes.contains_key(&pane))?;
            Some((wi, ti))
        })
    }

    /// 第 `wi` 个 workspace 的第 `ti` 个标签正显示在窗口里。
    pub(super) fn is_shown(&self, wi: usize, ti: usize) -> bool {
        wi == self.active && ti == self.workspaces[wi].active
    }

    fn handle_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = view.entity_id();
        let Some((wi, ti)) = self.locate(id) else {
            return;
        };
        let shown = self.is_shown(wi, ti);
        match event {
            TerminalEvent::TitleChanged => {
                if shown && self.tab().focused == id {
                    self.sync_window_title(window, cx);
                }
                self.forget_stale_done(id, wi, ti, cx);
                cx.notify();
            }
            TerminalEvent::Focused => {
                let tab = &mut self.workspaces[wi].tabs[ti];
                if tab.focused != id {
                    tab.focused = id;
                    if shown {
                        self.sync_window_title(window, cx);
                    }
                    self.save(cx);
                    cx.notify();
                }
                self.mark_seen(window, cx);
            }
            // agent 的分屏响铃不点亮提示点：agent 停下来要人处理时已经有它自己的标记。
            TerminalEvent::Bell => {
                window.play_system_bell();
                if !shown && view.read(cx).agent().is_none() {
                    self.workspaces[wi].tabs[ti].bell = true;
                    cx.notify();
                }
            }
            TerminalEvent::AgentFinished => self.agent_finished(id, wi, ti, window, cx),
            TerminalEvent::AgentBlocked => self.agent_blocked(id, wi, ti, window, cx),
            TerminalEvent::Exited => self.close_pane_by_id(id, window, cx),
        }
    }

    /// 切到当前 workspace 的第 `ix` 个标签。
    pub(super) fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace_mut();
        workspace.active = ix;
        workspace.tabs[ix].bell = false;
        workspace.tab_scroll.scroll_to_item(ix);
        self.start_shown(cx);
        window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        self.sync_window_title(window, cx);
        self.mark_seen(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// 启动显示中的标签里还没启动 shell 的终端（恢复布局时看不见的终端都等到这时）。
    fn start_shown(&mut self, cx: &mut Context<Self>) {
        let unstarted: Vec<_> =
            self.tab().panes.values().map(|(view, _)| view.clone()).filter(|view| !view.read(cx).started()).collect();
        for view in unstarted {
            let start = view.read(cx).cwd();
            view.update(cx, |view, cx| view.start(cx));
            // shell 现在才开始读启动配置，存档时这段时间仍记起始目录。
            self.record_spawn(&view, start.as_deref());
        }
    }

    /// 切到第 `ix` 个 workspace，显示它切走之前的那个标签。
    pub(super) fn activate_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        self.sidebar_scroll.scroll_to_item(ix);
        self.refresh_project(cx);
        // 目录监听只跟着当前 workspace，切走期间这个 workspace 预览的文件可能变过。
        self.refresh_preview_if_changed(cx);
        self.activate(self.workspaces[ix].active, window, cx);
    }

    fn sync_window_title(&self, window: &mut Window, cx: &App) {
        window.set_window_title(self.tab().focused_view().read(cx).title());
    }

    /// 把当前布局交给存档，有变化时稍后写进文件。
    pub(super) fn save(&self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        persistence::update(cx.weak_entity(), snapshot, cx);
    }

    /// 关掉第 `wi` 个 workspace 的第 `ti` 个标签；关掉的是它的当前标签时切到右边那个（没有
    /// 就左边）。workspace 里只剩这一个标签时关掉整个 workspace。
    pub(super) fn close_tab_at(&mut self, wi: usize, ti: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces[wi].tabs.len() == 1 {
            self.close_workspace_at(wi, window, cx);
            return;
        }
        let workspace = &mut self.workspaces[wi];
        let tab = workspace.tabs.remove(ti);
        super::agents::dismiss_alerts(tab.panes.keys().copied(), cx);
        if ti < workspace.active || workspace.active == workspace.tabs.len() {
            workspace.active -= 1;
        }
        let active = workspace.active;
        if wi == self.active {
            self.activate(active, window, cx);
        } else {
            self.save(cx);
            cx.notify();
        }
    }

    pub(super) fn close_tab_by_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.workspace().tabs.iter().position(|tab| tab.id == id) {
            self.close_tab_at(self.active, ix, window, cx);
        }
    }

    /// 关掉一个终端：它的兄弟分屏顶替上来，焦点交给兄弟一侧离它最近的终端；
    /// 标签里只剩它时关掉整个标签。
    pub(super) fn close_pane_by_id(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate(pane) else {
            return;
        };
        crate::agent_alert::dismiss(&super::agents::notification_tag(pane), cx);
        let shown = self.is_shown(wi, ti);
        let tab = &mut self.workspaces[wi].tabs[ti];
        tab.done.remove(&pane);
        let Some(next) = tab.root.remove(pane) else {
            self.close_tab_at(wi, ti, window, cx);
            return;
        };
        tab.panes.remove(&pane);
        // 关的是别的分屏（比如后台的 shell 自己退出了）时，放大和焦点都不动。
        if tab.focused != pane {
            self.save(cx);
            cx.notify();
            return;
        }
        tab.focused = next;
        tab.zoomed = false;
        if shown {
            self.activate(ti, window, cx);
        } else {
            self.save(cx);
            cx.notify();
        }
    }

    /// 关掉第 `ix` 个 workspace，里面的终端都随之结束；关掉的是当前 workspace 时切到下面那个
    /// （没有就上面）。最后一个关掉时关窗口。
    fn close_workspace_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.as_ref().is_some_and(|renaming| renaming.id == self.workspaces[ix].id) {
            self.renaming = None;
        }
        let panes = self.workspaces[ix].tabs.iter().flat_map(|tab| tab.panes.keys().copied()).collect::<Vec<_>>();
        super::agents::dismiss_alerts(panes, cx);
        if self.workspaces.len() == 1 {
            self.emptied = true;
            window.remove_window();
            return;
        }
        self.workspaces.remove(ix);
        let active =
            if ix < self.active || self.active == self.workspaces.len() { self.active - 1 } else { self.active };
        self.activate_workspace(active, window, cx);
    }

    /// 关掉第 `ix` 个 workspace；里面不止一个终端时先问一句，免得误点关掉一整个项目。
    pub(super) fn confirm_close_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = &self.workspaces[ix];
        let count = workspace.pane_count();
        if count <= 1 {
            self.close_workspace_at(ix, window, cx);
            return;
        }
        let id = workspace.id;
        let title = rust_i18n::t!("workspace.close_title", name = workspace.name);
        let detail = rust_i18n::t!("workspace.close_detail", count = count);
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &title,
            Some(&detail),
            &[&*rust_i18n::t!("workspace.close_confirm"), &*rust_i18n::t!("workspace.close_cancel")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                // 问的时候 workspace 可能已经挪了位置或者自己关掉了。
                if let Some(ix) = this.workspaces.iter().position(|workspace| workspace.id == id) {
                    this.close_workspace_at(ix, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 把拖动的标签挪到第 `to` 个位置，并切到它。
    pub(super) fn move_tab(&mut self, id: TabId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = &mut self.workspace_mut().tabs;
        let Some(from) = tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = tabs.remove(from);
        let to = to.min(tabs.len());
        tabs.insert(to, tab);
        self.activate(to, window, cx);
    }

    /// 把拖动的 workspace 挪到第 `to` 个位置，并切到它。
    pub(super) fn move_workspace(&mut self, id: WorkspaceId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.workspaces.iter().position(|workspace| workspace.id == id) else {
            return;
        };
        let workspace = self.workspaces.remove(from);
        let to = to.min(self.workspaces.len());
        self.workspaces.insert(to, workspace);
        self.activate_workspace(to, window, cx);
    }

    /// 新终端从当前终端的 shell 所在目录开始。
    pub(super) fn spawn_beside_focused(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        // 目录已经被删掉时 shell 起不来，退回 workspace 的目录，再退回家目录。
        let cwd = self.tab().focused_view().read(cx).cwd();
        let cwd = persist::start_dir(cwd.as_deref(), &self.workspace().dir);
        self.spawn_terminal(cwd.as_deref(), window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_name_uses_the_repository_root() {
        let base = std::env::temp_dir().join(format!("runode-workspace-name-{}", std::process::id()));
        let sub = base.join("repo/crates/app");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        std::fs::create_dir_all(base.join("plain")).unwrap();
        assert_eq!(workspace_name(&sub), "repo");
        assert_eq!(workspace_name(&base.join("repo")), "repo");
        assert_eq!(workspace_name(&base.join("plain")), "plain");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
