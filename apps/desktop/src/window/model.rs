//! 窗口里的数据：workspace、标签和分屏布局，以及 `WindowView` 增删、查找、切换和关闭它们的操作。

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

use gpui::{
    App, Bounds, Context, Entity, EntityId, Focusable, Pixels, ScrollHandle, SharedString, Subscription, Window,
};
use runode_shared_types::{
    grid::GridSize,
    pane::{Node, SplitId},
};

use super::{
    WindowView,
    persist::{self, format},
    project::Project,
    sidebar::RepoBadge,
};
use crate::terminal_view::{PROVISIONAL_SIZE, SHOW_WAIT, TerminalEvent, TerminalView};

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

/// 路径的最后一段，用在标签、提示和列表里；没有最后一段（比如 `/`）时写整个路径。
pub(super) fn base_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
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
    /// 侧栏这一行上的当前分支和 GitHub 头像。
    pub(super) repo: RepoBadge,
    /// 可以没有：最后一个关掉后 workspace 留着，显示一个空的标签区，等用户新开。
    pub(super) tabs: Vec<Tab>,
    /// 当前标签；没有标签时为 0。
    pub(super) active: usize,
    /// 标签条的滚动位置，切换标签时把当前标签滚进视野。
    pub(super) tab_scroll: ScrollHandle,
    /// Git 面板和文件树显示的内容。
    pub(super) project: Project,
}

impl Workspace {
    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
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

    /// 窗口里正显示的标签；当前 workspace 里没有标签时为空。
    pub(super) fn tab(&self) -> Option<&Tab> {
        self.workspace().active_tab()
    }

    pub(super) fn tab_mut(&mut self) -> Option<&mut Tab> {
        let workspace = &mut self.workspaces[self.active];
        workspace.tabs.get_mut(workspace.active)
    }

    /// 窗口里有焦点的终端；当前 workspace 里没有标签时为空。
    pub(super) fn focused_view(&self) -> Option<&Entity<TerminalView>> {
        self.tab().map(Tab::focused_view)
    }

    /// 按某个终端的尺寸启动不显示的新终端：显示着的那个，没有时窗口里随便哪个，窗口里一个终端
    /// 都没有时用默认尺寸（新终端一显示出来就按实际尺寸改）。
    pub(super) fn reference_size(&self, cx: &App) -> GridSize {
        self.focused_view()
            .or_else(|| {
                self.workspaces
                    .iter()
                    .flat_map(|w| &w.tabs)
                    .find_map(|tab| tab.panes.values().next())
                    .map(|(view, _)| view)
            })
            .map_or(PROVISIONAL_SIZE, |view| view.read(cx).size())
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
        self.spawned.retain(|_, (at, _)| at.elapsed() < persist::SHELL_STARTUP);
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
        // 没有标签的 workspace 里 `active` 是 0，调用方按「当前标签右边」给的 1 落在末尾之外。
        let tabs = &mut self.workspace_mut().tabs;
        let ix = ix.min(tabs.len());
        tabs.insert(ix, tab);
        self.activate(ix, window, cx);
    }

    /// 在第 `ix` 个位置放一个目录是 `dir` 的新 workspace，`view` 是它的第一个终端，并切过去。
    /// `name` 为空时按目录取名。
    pub(super) fn insert_workspace(
        &mut self,
        ix: usize,
        dir: PathBuf,
        name: Option<String>,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_workspace(ix, dir, name, view, window, cx);
        self.activate_workspace(ix, window, cx);
    }

    /// 在第 `ix` 个位置放一个目录是 `dir` 的新 workspace，`view` 是它的第一个终端，不切过去：当前的
    /// workspace 还是原来那个。`name` 为空时按目录取名。
    pub(super) fn add_workspace(
        &mut self,
        ix: usize,
        dir: PathBuf,
        name: Option<String>,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.single_pane_tab(view, window, cx);
        let workspace = Workspace {
            id: self.next_id(),
            name: name.unwrap_or_else(|| workspace_name(&dir)).into(),
            repo: RepoBadge::default(),
            dir,
            tabs: vec![tab],
            active: 0,
            tab_scroll: ScrollHandle::new(),
            project: Project::default(),
        };
        self.workspaces.insert(ix, workspace);
        // 插在当前 workspace 前面时它往后挪了一位；窗口里本来没有 workspace 时调用方接着切过去。
        if self.workspaces.len() > 1 && ix <= self.active {
            self.active += 1;
        }
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
                if shown && self.tab().is_some_and(|tab| tab.focused == id) {
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

    /// 切到当前 workspace 的第 `ix` 个标签；workspace 里没有标签时显示空的标签区。开着的手机端引导页
    /// 随之收起。
    pub(super) fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(ix, true, window, cx);
    }

    /// 同 `activate`，只是 `reveal_now` 为假时标签条等下一帧按新排好的位置把标签滚进来：刚切过来的 workspace
    /// 上次画出时的布局可能已经过时（开了侧栏、缩了窗口），按它算的偏移不准。
    fn show_tab(&mut self, ix: usize, reveal_now: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.mobile = None;
        let workspace = self.workspace_mut();
        workspace.active = ix;
        // 焦点要交给终端，盖着终端的放大预览栏还原。
        workspace.project.previews.maximized = false;
        if let Some(tab) = workspace.tabs.get_mut(ix) {
            tab.bell = false;
            if reveal_now {
                reveal_tab(&workspace.tab_scroll, ix, workspace.tabs.len());
            } else {
                workspace.tab_scroll.scroll_to_item(ix);
            }
        }
        self.start_shown(cx);
        self.sync_visibility(window, cx);
        window.focus(&self.focus_handle(cx), cx);
        self.sync_window_title(window, cx);
        self.mark_seen(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// 启动显示中的标签里还没启动 shell 的终端（恢复布局时看不见的终端都等到这时）。
    fn start_shown(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tab() else {
            return;
        };
        let unstarted: Vec<_> =
            tab.panes.values().map(|(view, _)| view.clone()).filter(|view| !view.read(cx).started()).collect();
        for view in unstarted {
            let start = view.read(cx).cwd();
            view.update(cx, |view, cx| view.start(cx));
            // shell 现在才开始读启动配置，存档时这段时间仍记起始目录。
            self.record_spawn(&view, start.as_deref());
        }
    }

    /// 告诉窗口里的每个终端它显示着没有：当前 workspace 当前标签里的分屏都算显示着（被放大的
    /// 分屏挡住的也算，取消放大时要马上画得出来），其余的离开显示一段时间后丢掉界面这份 VT、只看
    /// 状态，见 `TerminalView::request_visible`。显示的标签变了、往不显示的标签里加了终端时调。
    ///
    /// 要重新要屏幕的先一起发出去，再按同一个截止时间依次等，一次显示好几个分屏时总共最多等
    /// `SHOW_WAIT`，不是每个各等一份。
    pub(super) fn sync_visibility(&self, window: &mut Window, cx: &mut Context<Self>) {
        let deadline = Instant::now() + SHOW_WAIT;
        let mut waiting = Vec::new();
        for (wi, workspace) in self.workspaces.iter().enumerate() {
            for (ti, tab) in workspace.tabs.iter().enumerate() {
                let shown = self.is_shown(wi, ti);
                for (view, _) in tab.panes.values() {
                    if view.read(cx).visible() != shown && view.update(cx, |view, cx| view.request_visible(shown, cx)) {
                        waiting.push(view.clone());
                    }
                }
            }
        }
        for view in waiting {
            view.update(cx, |view, cx| view.wait_for_screen(deadline, window, cx));
        }
    }

    /// 切到第 `ix` 个 workspace，显示它切走之前的那个标签。
    pub(super) fn activate_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.active == ix;
        self.active = ix;
        self.sidebar_scroll.scroll_to_item(ix);
        self.follow_workspace_in_file_search(self.workspaces[ix].id, cx);
        self.refresh_project(cx);
        // 目录监听只跟着当前 workspace，切走期间这个 workspace 预览的文件可能变过。
        self.refresh_preview_if_changed(cx);
        self.show_tab(self.workspaces[ix].active, shown, window, cx);
    }

    /// 窗口标题跟着有焦点的终端；没有终端时用 workspace 的名字。
    fn sync_window_title(&self, window: &mut Window, cx: &App) {
        match self.focused_view() {
            Some(view) => window.set_window_title(view.read(cx).title()),
            None => window.set_window_title(&self.workspace().name),
        }
    }

    /// 把当前布局交给存档，有变化时稍后写进文件。
    pub(super) fn save(&self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        persist::update(cx.weak_entity(), snapshot, cx);
        super::layout_report::changed(cx);
    }

    /// 关掉第 `wi` 个 workspace 的第 `ti` 个标签；关掉的是它的当前标签时切到右边那个（没有
    /// 就左边）。关掉的是最后一个标签时 workspace 留着，显示空的标签区。
    pub(super) fn close_tab_at(&mut self, wi: usize, ti: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.end_sessions(Closing::Tab { workspace: wi, tab: ti }, cx);
        let workspace = &mut self.workspaces[wi];
        let tab = workspace.tabs.remove(ti);
        super::agents::dismiss_alerts(tab.panes.keys().copied(), cx);
        if workspace.active > 0 && (ti < workspace.active || workspace.active == workspace.tabs.len()) {
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

    /// 关掉标签 `id`；里面还有程序在跑时先问一句（见 `confirm_closing`）。
    pub(super) fn close_tab_by_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate_tab(id) else {
            return;
        };
        let running = running_programs(self.workspaces[wi].tabs[ti].panes.values().map(|(view, _)| view), cx);
        let title = rust_i18n::t!("close.tab_title");
        self.confirm_closing(&title, None, &running, window, cx, move |this, window, cx| {
            // 问的时候标签可能已经挪了位置或者自己关掉了。
            if let Some((wi, ti)) = this.locate_tab(id) {
                this.close_tab_at(wi, ti, window, cx);
            }
        });
    }

    fn locate_tab(&self, id: TabId) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(wi, workspace)| Some((wi, workspace.tabs.iter().position(|tab| tab.id == id)?)))
    }

    /// 用户要关掉终端 `pane`；它前台还有程序在跑时先问一句（见 `confirm_closing`）。shell 自己退出时
    /// 不经这里，直接 `close_pane_by_id`。
    pub(super) fn confirm_close_pane(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate(pane) else {
            return;
        };
        let running = running_programs(self.workspaces[wi].tabs[ti].panes.get(&pane).map(|(view, _)| view), cx);
        let title = rust_i18n::t!("close.pane_title");
        self.confirm_closing(&title, None, &running, window, cx, move |this, window, cx| {
            this.close_pane_by_id(pane, window, cx);
        });
    }

    /// `running` 为空、`detail` 也没有时直接做 `close`；否则弹框，说明是 `detail` 加上哪些程序还在跑，
    /// 确认了再做。
    fn confirm_closing(
        &mut self,
        title: &str,
        detail: Option<&str>,
        running: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
        close: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let running = (!running.is_empty()).then(|| {
            rust_i18n::t!("close.running", programs = running.join(rust_i18n::t!("close.separator").as_ref()))
                .into_owned()
        });
        let detail = match (detail, running) {
            (None, None) => {
                close(self, window, cx);
                return;
            }
            (Some(detail), Some(running)) => format!("{detail}\n\n{running}"),
            (Some(detail), None) => detail.to_owned(),
            (None, Some(running)) => running,
        };
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            title,
            Some(&detail),
            &[&*rust_i18n::t!("close.confirm"), &*rust_i18n::t!("close.cancel")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| close(this, window, cx)).ok();
        })
        .detach();
    }

    /// 结束关掉 `closing` 时要结束的会话（见 `sessions_to_end`）。要在把终端从窗口里拿掉之前调。
    pub(super) fn end_sessions(&self, closing: Closing<EntityId>, cx: &mut App) {
        let layout: Vec<Vec<Vec<EntityId>>> = self
            .workspaces
            .iter()
            .map(|workspace| workspace.tabs.iter().map(|tab| tab.root.leaves()).collect())
            .collect();
        let ending = sessions_to_end(&layout, closing);
        for tab in self.workspaces.iter().flat_map(|workspace| &workspace.tabs) {
            for (id, (view, _)) in &tab.panes {
                if ending.contains(id) {
                    view.update(cx, |view, _| view.end());
                }
            }
        }
    }

    /// 关掉一个终端：它的兄弟分屏顶替上来，焦点交给兄弟一侧离它最近的终端；
    /// 标签里只剩它时关掉整个标签。
    pub(super) fn close_pane_by_id(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate(pane) else {
            return;
        };
        self.end_sessions(Closing::Pane(pane), cx);
        super::agents::alert::dismiss(&super::agents::notification_tag(pane), cx);
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
        self.end_sessions(Closing::Workspace(ix), cx);
        if self.renaming.as_ref().is_some_and(|renaming| renaming.id == self.workspaces[ix].id) {
            self.renaming = None;
        }
        super::agents::dismiss_alerts(self.workspaces[ix].tabs.iter().flat_map(|tab| tab.panes.keys().copied()), cx);
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

    /// 关掉第 `ix` 个 workspace；里面不止一个终端或者还有程序在跑时先问一句，免得误点关掉一整个项目。
    pub(super) fn confirm_close_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = &self.workspaces[ix];
        let count = workspace.pane_count();
        let id = workspace.id;
        let title = rust_i18n::t!("workspace.close_title", name = workspace.name);
        let detail = (count > 1).then(|| rust_i18n::t!("workspace.close_detail", count = count));
        let running =
            running_programs(workspace.tabs.iter().flat_map(|tab| tab.panes.values().map(|(view, _)| view)), cx);
        self.confirm_closing(&title, detail.as_deref(), &running, window, cx, move |this, window, cx| {
            // 问的时候 workspace 可能已经挪了位置或者自己关掉了。
            if let Some(ix) = this.workspaces.iter().position(|workspace| workspace.id == id) {
                this.close_workspace_at(ix, window, cx);
            }
        });
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
        // 目录已经被删掉时 shell 起不来，退回 workspace 的目录，再退回家目录；workspace 里没有终端
        // 时从它的目录开始。
        let cwd = self.focused_view().and_then(|view| view.read(cx).cwd());
        let cwd = format::start_dir(cwd.as_deref(), &self.workspace().dir);
        self.spawn_terminal(cwd.as_deref(), window, cx)
    }
}

/// `views` 里前台还在跑的程序（见 `TerminalView::running`），同名的只列一次。
fn running_programs<'a>(views: impl IntoIterator<Item = &'a Entity<TerminalView>>, cx: &App) -> Vec<String> {
    let mut names = Vec::new();
    for view in views {
        if let Some(name) = view.read(cx).running()
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// 用户要关掉的东西，见 `sessions_to_end`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Closing<T> {
    /// 一个分屏。
    Pane(T),
    /// 第 `workspace` 个 workspace 的第 `tab` 个标签。
    Tab { workspace: usize, tab: usize },
    /// 第几个 workspace。
    Workspace(usize),
    /// 整个窗口；`windows` 是关之前一共开着几个窗口。
    Window { windows: usize },
}

/// 关掉 `closing` 时要结束哪些会话（按分屏给出），`layout` 是窗口里各个 workspace 各个标签的分屏。
/// 关分屏、标签、workspace 是用户明确不要这些终端了，结束它们（连带关掉的标签、workspace 和窗口
/// 里也只有这些）；关窗口时还有别的窗口才结束这个窗口里的全部，关最后一个窗口时 app 跟着退出，
/// 会话怎么办归退出管，见 `quit::quit_plan`。
pub(super) fn sessions_to_end<T: Copy + PartialEq>(layout: &[Vec<Vec<T>>], closing: Closing<T>) -> Vec<T> {
    match closing {
        Closing::Pane(pane) => {
            if layout.iter().flatten().flatten().any(|id| *id == pane) {
                vec![pane]
            } else {
                Vec::new()
            }
        }
        Closing::Tab { workspace, tab } => {
            layout.get(workspace).and_then(|tabs| tabs.get(tab)).cloned().unwrap_or_default()
        }
        Closing::Workspace(workspace) => layout.get(workspace).map(|tabs| tabs.concat()).unwrap_or_default(),
        Closing::Window { windows } if windows > 1 => layout.iter().flatten().flatten().copied().collect(),
        Closing::Window { .. } => Vec::new(),
    }
}

/// 把第 `ix` 个标签滚进标签条，连同两边各一个邻居一起露出来：点挤在边上的标签，标签条把它滚到
/// 正中，一次往那边多露出几个，看得到后面还有什么。按上一帧排好的位置算；标签数对不上（刚开、刚关）
/// 时退回只露出它自己。
fn reveal_tab(scroll: &ScrollHandle, ix: usize, tab_count: usize) {
    let item = |ix: usize| scroll.bounds_for_item(ix).map(|b| (f32::from(b.left()), f32::from(b.right())));
    let (Some(tab), true) = (item(ix), scroll.children_count() == tab_count) else {
        scroll.scroll_to_item(ix);
        return;
    };
    let view = scroll.bounds();
    let before = ix.checked_sub(1).and_then(item).unwrap_or(tab);
    let after = item(ix + 1).unwrap_or(tab);
    let offset = scroll.offset();
    let x = reveal_offset(
        f32::from(offset.x),
        (f32::from(view.left()), f32::from(view.right())),
        tab,
        (before.0, after.1),
        f32::from(scroll.max_offset().x),
    );
    scroll.set_offset(gpui::point(gpui::px(x), offset.y));
}

/// 横向滚动的偏移（向右滚为负）：`span`（标签连同邻居）已经都在 `view` 里时不动，否则把 `tab`
/// 滚到 `view` 正中，最后限制在 `[-max, 0]` 里。坐标都是没滚动时的位置。
fn reveal_offset(x: f32, view: (f32, f32), tab: (f32, f32), span: (f32, f32), max: f32) -> f32 {
    let x = if span.0 + x >= view.0 && span.1 + x <= view.1 { x } else { (view.0 + view.1 - tab.0 - tab.1) / 2. };
    x.clamp(-max.max(0.), 0.)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [u32; 0] = [];

    /// 两个 workspace：第一个有两个标签（分屏 1、2 在同一个标签里，3 单独一个），第二个只有分屏 4。
    fn layout() -> Vec<Vec<Vec<u32>>> {
        vec![vec![vec![1, 2], vec![3]], vec![vec![4]]]
    }

    #[test]
    fn closing_a_pane_ends_only_that_pane() {
        assert_eq!(sessions_to_end(&layout(), Closing::Pane(2)), [2]);
        // 标签里只剩它时连标签、workspace 一起关，要结束的还是只有它。
        assert_eq!(sessions_to_end(&layout(), Closing::Pane(4)), [4]);
        assert_eq!(sessions_to_end(&layout(), Closing::Pane(9)), NONE);
    }

    #[test]
    fn closing_a_tab_ends_its_panes() {
        assert_eq!(sessions_to_end(&layout(), Closing::Tab { workspace: 0, tab: 0 }), [1, 2]);
        assert_eq!(sessions_to_end(&layout(), Closing::Tab { workspace: 1, tab: 0 }), [4]);
        assert_eq!(sessions_to_end(&layout(), Closing::Tab { workspace: 1, tab: 1 }), NONE);
    }

    #[test]
    fn closing_a_workspace_ends_every_tab_in_it() {
        assert_eq!(sessions_to_end(&layout(), Closing::Workspace(0)), [1, 2, 3]);
        // 窗口里最后一个 workspace 关掉时窗口跟着关，它里面的会话也结束：这是用户点名要关的。
        assert_eq!(sessions_to_end(&[vec![vec![7]]], Closing::Workspace(0)), [7]);
    }

    #[test]
    fn closing_one_of_several_windows_ends_all_of_it() {
        assert_eq!(sessions_to_end(&layout(), Closing::Window { windows: 2 }), [1, 2, 3, 4]);
    }

    #[test]
    fn closing_the_last_window_leaves_it_to_quitting() {
        assert_eq!(sessions_to_end(&layout(), Closing::Window { windows: 1 }), NONE);
    }

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

    // 视口 [0, 500]，标签宽 100，第 n 个在 [100n, 100n + 100]，内容共 10 个标签。
    fn tab(n: f32) -> (f32, f32) {
        (100. * n, 100. * n + 100.)
    }

    fn reveal(x: f32, n: f32) -> f32 {
        let span = (tab((n - 1.).max(0.)).0, tab((n + 1.).min(9.)).1);
        reveal_offset(x, (0., 500.), tab(n), span, 500.)
    }

    #[test]
    fn reveal_offset_centres_tabs_at_the_edge() {
        // 点最右边露出来的第 4 个，滚到正中，一次多露出两个。
        assert_eq!(reveal(0., 4.), -200.);
        // 露着第 5 到 9 个时点最左边的第 5 个，同样滚到正中。
        assert_eq!(reveal(-500., 5.), -300.);
        // 邻居都看得到，不动。
        assert_eq!(reveal(-300., 5.), -300.);
        // 两头的标签滚不到正中，不越界。
        assert_eq!(reveal(-300., 0.), 0.);
        assert_eq!(reveal(0., 9.), -500.);
    }
}
