//! `WindowView` 和存档之间的转换，以及什么时候写存档。
//!
//! 布局一变就把新快照交给 `Saver`，攒一小会儿再写文件；退出时把各窗口最新的布局写一次。
//! 关掉最后一个窗口、一下关掉所有窗口也会退出，这时关掉的窗口都留下布局，下次启动恢复，和
//! 退出一样；还有别的窗口开着时单独关掉的窗口（里面的会话随之结束）和里面的终端都关完了的
//! 窗口不留。终端的目录随 `cd` 变化，不单独盯着：下次布局变化或退出时一并记下。
//!
//! 每个终端记着宿主里的会话。恢复时先问宿主还有哪些会话，接得上的接上，其余在原目录新开，
//! 见 `format::plan_restore`。

pub(super) mod format;

use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use gpui::{
    App, Bounds, Context, EntityId, Global, Task, WeakEntity, Window, WindowBounds, WindowOptions, point, px, size,
};
use runode_protocol::{SessionInfo, WindowLayout};
use runode_types::pane::{Node, Split};

use super::{
    WindowView,
    model::{Tab, Workspace, home_dir, workspace_name},
    project::{Project, SidePanel},
};
use crate::{
    host_client::{self, Mode},
    prespawn::Prespawned,
    terminal_view::TerminalView,
};
use format::{SavedBounds, SavedNode, SavedTab, SavedWindow, SavedWorkspace, State, WindowMode};

/// 布局变化后等这么久再写文件，拖动窗口、连续切标签时只写一次。
const WRITE_DELAY: Duration = Duration::from_millis(500);
/// 终端开出来后这么久之内不读它的目录：shell 还在读启动配置，插件管理器会临时切进插件
/// 目录。这段时间里存档记它的起始目录。
pub(super) const SHELL_STARTUP: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Saver {
    /// 开着的窗口和它们最近一次的布局，按打开的先后；关掉它们 app 随即退出的窗口（见 `closed`）
    /// 也留着，记关掉时的布局。
    windows: Vec<(WeakEntity<WindowView>, SavedWindow)>,
    /// 上次写进文件的内容，没变就不再写。
    written: Option<State>,
    /// 等着写文件的任务。
    pending: Option<Task<()>>,
    /// 最近一次开始写的编号，见 `write_in_order`。
    seq: u64,
    /// 已经开始退出，最后的布局已经写好（`freeze`）：之后窗口一个个被关掉、分屏随会话结束一个个
    /// 关掉，都不是用户改了布局，不再改存档。
    frozen: bool,
}

impl Global for Saver {}

impl Saver {
    /// 把窗口 `window` 的布局记成 `snapshot`：已经记着这个窗口时替换，没有时加在后面。
    /// 返回布局变了没有。
    fn upsert(&mut self, window: WeakEntity<WindowView>, snapshot: SavedWindow) -> bool {
        match self.windows.iter_mut().find(|(w, _)| w.entity_id() == window.entity_id()) {
            Some((_, saved)) if *saved == snapshot => false,
            Some((_, saved)) => {
                *saved = snapshot;
                true
            }
            None => {
                self.windows.push((window, snapshot));
                true
            }
        }
    }
}

/// 装上存档，退出时把各窗口最新的布局写一次。要在打开窗口之前调用。
pub(super) fn install(cx: &mut App) {
    cx.set_global(Saver::default());
    cx.on_app_quit(|cx| {
        freeze(cx);
        async {}
    })
    .detach();
}

/// 要退出了：把各窗口现在的布局写进存档，之后布局怎么变都不再写。退出时一定会做；会话要先于
/// app 结束时（让单独跑的宿主连会话一起退出），视图会先收到会话结束、一个个关掉分屏，要在那之前
/// 调，下次启动才能照原样在原目录新开。已经冻结了时什么都不做。
pub(super) fn freeze(cx: &mut App) {
    if cx.global::<Saver>().frozen {
        return;
    }
    // 还在等着报的布局变化先报给宿主，冻结以后就不报了，见 `layout_report`。
    super::layout_report::report_change(cx);
    let windows: Vec<_> = cx.global::<Saver>().windows.iter().map(|(window, _)| window.clone()).collect();
    let fresh: Vec<_> = windows.iter().map(|window| window.upgrade().map(|view| view.read(cx).snapshot(cx))).collect();
    let saver = cx.global_mut::<Saver>();
    for ((_, saved), fresh) in saver.windows.iter_mut().zip(fresh) {
        if let Some(fresh) = fresh {
            *saved = fresh;
        }
    }
    write(cx);
    cx.global_mut::<Saver>().frozen = true;
}

/// 已经开始退出，存档冻结了（`freeze`）。
pub(super) fn frozen(cx: &App) -> bool {
    cx.try_global::<Saver>().is_some_and(|saver| saver.frozen)
}

/// 存档里还留着窗口：窗口一个不剩时，是关窗口退出（下次启动恢复它们），不是用户把工作区都关完了。
pub(super) fn keeps_windows(cx: &App) -> bool {
    cx.try_global::<Saver>().is_some_and(|saver| !saver.windows.is_empty())
}

/// 冻结之后又不退出了（退出前没能把会话交出去，用户取消了）：存档重新跟着布局变。
pub(super) fn thaw(cx: &mut App) {
    cx.global_mut::<Saver>().frozen = false;
}

/// 上次存下的各个窗口，以及恢复时打开它们用的窗口选项（位置、大小、所在屏幕）。没有存档或
/// 读不了时为空；读不了时文件挪到一边（`format::load`），从默认布局开始。没开 app 时手机、命令行
/// 经宿主新开的终端、新建的工作区和改的名字按宿主手里的布局接进来（`format::adopt`）。终端记的会话按宿主里
/// 还活着的会话定下接不接（`format::plan_restore`）；存档记着、没人连着的会话中 shell 已经退出的和一直没
/// 启动的这时结束掉，存档里没记着的孤儿会话由 `watch_background` 问过看板再收。
pub fn saved_window_options(cx: &App) -> Vec<(SavedWindow, WindowOptions)> {
    let mut windows = format::load().map(|state| state.windows).unwrap_or_default();
    let live = live_sessions();
    format::adopt(&mut windows, &held_layout(), &live, workspace_name);
    let windows: Vec<_> = windows.into_iter().filter(|window| !window.workspaces.is_empty()).collect();
    let plan = format::plan_restore(windows, &live);
    let link = host_client::link();
    for id in plan.end {
        tracing::info!("ending the session {id}: its shell exited or never started");
        link.kill(id);
    }
    plan.windows
        .into_iter()
        .map(|window| {
            let options = window_options(&window.bounds, cx);
            (window, options)
        })
        .collect()
}

/// 宿主里还活着的会话。宿主跑在 app 里时它刚建好，没有上次的会话，不问；问不到时当作没有，
/// 终端都在原目录新开，没接上的会话留在宿主里成为后台会话。
fn live_sessions() -> Vec<SessionInfo> {
    if host_client::mode() == Mode::InProcess {
        return Vec::new();
    }
    host_client::list_sessions().unwrap_or_else(|err| {
        tracing::warn!("failed to list the host's sessions, starting every saved terminal over: {err:#}");
        Vec::new()
    })
}

/// 宿主手里的布局，见 `format::adopt`。宿主跑在 app 里时它刚建好，手里没有，不问；问不到时当作
/// 没有，没开 app 时新开的终端成为后台会话。
fn held_layout() -> Vec<WindowLayout> {
    if host_client::mode() == Mode::InProcess {
        return Vec::new();
    }
    host_client::held_layout().unwrap_or_else(|err| {
        tracing::warn!(
            "failed to get the host's layout, leaving what changed without the app in the background: {err:#}"
        );
        Vec::new()
    })
}

/// 按存档放窗口：回到原来那块屏幕的原来位置；那块屏幕不在了（比如拔掉了外接显示器）或者
/// 窗口已经不在屏幕上，就按默认位置开。
fn window_options(saved: &SavedBounds, cx: &App) -> WindowOptions {
    let displays = cx.displays();
    let display = match &saved.display {
        Some(uuid) => displays.iter().find(|display| display.uuid().is_ok_and(|id| id.to_string() == *uuid)).cloned(),
        None => cx.primary_display(),
    };
    let bounds = Bounds { origin: point(px(saved.x), px(saved.y)), size: size(px(saved.width), px(saved.height)) };
    let placed = display.filter(|display| {
        // 存的位置相对于窗口所在的屏幕。
        let screen = Bounds { origin: point(px(0.), px(0.)), size: display.bounds().size };
        saved.width >= 100. && saved.height >= 100. && screen.intersects(&bounds)
    });
    let mut options = super::open::window_options(cx);
    if let Some(display) = placed {
        options.display_id = Some(display.id());
        options.window_bounds = Some(match saved.mode {
            WindowMode::Windowed => WindowBounds::Windowed(bounds),
            WindowMode::Maximized => WindowBounds::Maximized(bounds),
            WindowMode::Fullscreen => WindowBounds::Fullscreen(bounds),
        });
    }
    options
}

/// 记下窗口的最新布局，和上次不同时稍后写文件。
pub(super) fn update(window: WeakEntity<WindowView>, snapshot: SavedWindow, cx: &mut App) {
    let saver = cx.global_mut::<Saver>();
    if saver.frozen {
        return;
    }
    if !saver.upsert(window, snapshot) {
        return;
    }
    if saver.pending.is_none() {
        let task = cx.spawn(async |cx| {
            cx.background_executor().timer(WRITE_DELAY).await;
            cx.update(write_later);
        });
        cx.global_mut::<Saver>().pending = Some(task);
    }
}

/// 窗口关掉时存档怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnClose {
    /// 不动存档。
    Ignore,
    /// 留下它关掉时的布局，下次启动恢复。
    Keep,
    /// 从存档里拿掉。
    Forget,
}

/// 关掉一个窗口时存档怎么办（见 `closed`）。`frozen`：已经冻结了（`freeze`）；`leaving`：关掉它
/// 以后一个窗口都不剩；`emptied`：它的 workspace 都关掉了。
fn on_close(frozen: bool, leaving: bool, emptied: bool) -> OnClose {
    // 关掉最后一个窗口的最后一个 workspace 时，退出要冻结存档，那时窗口里还留着这个 workspace。
    match () {
        _ if emptied => OnClose::Forget,
        _ if frozen => OnClose::Ignore,
        _ if leaving => OnClose::Keep,
        _ => OnClose::Forget,
    }
}

/// 窗口关掉时按 `closed` 更新存档。
pub(super) fn track(cx: &mut Context<WindowView>) {
    let window = cx.weak_entity();
    cx.on_release(move |view, cx| closed(view, window, cx)).detach();
}

/// 窗口关掉了。关掉它以后一个窗口都不剩时，是关掉了最后一个窗口或者一下关掉了所有窗口（这时
/// 各个窗口都先从 app 里拿掉，再一个个放掉），app 随即退出：和退出一样留下它关掉时的布局，下次
/// 启动恢复。还有别的窗口开着时是单独关掉了它，里面的会话随之结束，从存档里拿掉。所有
/// workspace 都关掉了的窗口不留。
fn closed(view: &mut WindowView, window: WeakEntity<WindowView>, cx: &mut App) {
    super::layout_report::changed(cx);
    let id = window.entity_id();
    match on_close(cx.global::<Saver>().frozen, super::terminal_windows(cx) == 0, view.emptied) {
        OnClose::Ignore => return,
        OnClose::Keep => {
            let snapshot = view.snapshot(cx);
            cx.global_mut::<Saver>().upsert(window, snapshot);
        }
        OnClose::Forget => cx.global_mut::<Saver>().windows.retain(|(w, _)| w.entity_id() != id),
    }
    write(cx);
}

/// 当场写存档：退出、关窗口时用，app 随即可能退出，要写完再走。
fn write(cx: &mut App) {
    let Some((seq, state)) = next_write(cx) else {
        return;
    };
    match write_in_order(seq, &state) {
        Ok(()) => cx.global_mut::<Saver>().written = Some(state),
        Err(err) => tracing::warn!("failed to save the window layout: {err}"),
    }
}

/// 在后台写存档：`replace_file` 要把文件刷到盘上，在界面线程上做时拖窗口、切标签会一卡一卡的。
fn write_later(cx: &mut App) {
    let Some((seq, state)) = next_write(cx) else {
        return;
    };
    let job = cx.background_executor().spawn({
        let state = state.clone();
        async move { write_in_order(seq, &state) }
    });
    cx.spawn(async move |cx| match job.await {
        Ok(()) => cx.update(|cx| {
            // 写的时候又开始了一次写，文件里是哪份要等那次说了算。
            let saver = cx.global_mut::<Saver>();
            if saver.seq == seq {
                saver.written = Some(state);
            }
        }),
        Err(err) => tracing::warn!("failed to save the window layout: {err}"),
    })
    .detach();
}

/// 要写的存档和它的编号；和上次写进文件的一样时不用写。
fn next_write(cx: &mut App) -> Option<(u64, State)> {
    let saver = cx.global_mut::<Saver>();
    saver.pending = None;
    let windows: Vec<_> = saver.windows.iter().map(|(_, saved)| saved.clone()).collect();
    let state = State::new(windows);
    if saver.written.as_ref() == Some(&state) {
        return None;
    }
    saver.seq += 1;
    Some((saver.seq, state))
}

/// 已经写进文件的那次写的编号。
static WRITTEN_SEQ: Mutex<u64> = Mutex::new(0);

/// 按编号写：后台的写和退出时的写会交错，开始得早的晚写完时不盖掉新的。
fn write_in_order(seq: u64, state: &State) -> io::Result<()> {
    let mut written = WRITTEN_SEQ.lock().unwrap_or_else(PoisonError::into_inner);
    if *written > seq {
        return Ok(());
    }
    format::write(state)?;
    *written = seq;
    Ok(())
}

impl WindowView {
    pub(super) fn snapshot(&self, cx: &App) -> SavedWindow {
        let (mode, bounds) = match self.bounds {
            WindowBounds::Windowed(bounds) => (WindowMode::Windowed, bounds),
            WindowBounds::Maximized(bounds) => (WindowMode::Maximized, bounds),
            WindowBounds::Fullscreen(bounds) => (WindowMode::Fullscreen, bounds),
        };
        let file_tree = self.file_tree.read(cx).options(cx);
        let git = self.git_panel.read(cx).options();
        SavedWindow {
            bounds: SavedBounds {
                display: self.display.clone(),
                mode,
                x: f32::from(bounds.origin.x),
                y: f32::from(bounds.origin.y),
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
            },
            workspaces: self.workspaces.iter().map(|workspace| self.save_workspace(workspace, cx)).collect(),
            active: self.active,
            sidebar: self.sidebar_shown,
            sidebar_width: self.sidebar_width,
            panel_width: self.panel_width,
            preview_width: self.preview_width,
            preview_wrap: self.preview.read(cx).wrap(),
            preview_source: self.preview.read(cx).source(),
            show_ignored: file_tree.show_ignored,
            hide_dotfiles: !file_tree.show_dotfiles,
            git_tree: git.tree,
            git_hide_subs: !git.show_subs,
            git_hide_worktrees: !git.show_worktrees,
            file_search_tree: file_tree.search_tree,
            file_search_options: file_tree.search,
            git_graph_collapsed: git.graph_collapsed,
            git_graph_height: git.graph_height,
            github_actions_heights: self.github_actions.read(cx).heights(),
        }
    }

    /// 按存档建出 workspace、标签和分屏：记着的会话接得上（`format::plan_restore` 留下了）就
    /// 接上，否则在记下的目录里开一个终端。开不起来的终端跳过，一个终端都没有的标签也跳过；
    /// workspace 没有标签也留着，和关掉最后一个标签时一样。新开的终端先不启动 shell，切到所在标签时由 `activate` 启动，所以启动
    /// 时只有窗口里显示的那个标签占进程。`shell` 是启动时在家目录提前拉起的 shell，交给显示的
    /// 标签里从家目录开始的新终端；没用上时还回去，由调用方处理。
    pub(super) fn restore_workspaces(
        &mut self,
        saved: SavedWindow,
        mut shell: Option<Prespawned>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Prespawned> {
        let home = home_dir();
        // 有终端起不来时后面的标签和 workspace 会往前挪，当前的那个按原来的位置找。
        let mut active = 0;
        for (wi, saved_workspace) in saved.workspaces.into_iter().enumerate() {
            let mut tabs = Vec::new();
            let mut active_tab = 0;
            for (ti, saved_tab) in saved_workspace.tabs.iter().enumerate() {
                let mut panes = HashMap::new();
                let mut leaves = Vec::new();
                let shown = wi == saved.active && ti == saved_workspace.active;
                let restored = self.restore_node(
                    &saved_tab.root,
                    &saved_workspace.dir,
                    home.as_deref(),
                    shown,
                    shown.then_some(&mut shell),
                    (&mut panes, &mut leaves),
                    window,
                    cx,
                );
                let Some(root) = restored else {
                    continue;
                };
                let (focused, collapsed) = saved_leaves(&leaves, saved_tab.focused, &saved_tab.collapsed);
                let focused = focused.unwrap_or(root.leaves()[0]);
                let collapsed = collapsed.into_iter().collect();
                if ti <= saved_workspace.active {
                    active_tab = tabs.len();
                }
                tabs.push(Tab {
                    id: self.next_id(),
                    zoomed: saved_tab.zoomed && !root.is_leaf(),
                    collapsed,
                    root,
                    panes,
                    focused,
                    bell: false,
                    done: Default::default(),
                });
            }
            let name = Some(saved_workspace.name)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| workspace_name(&saved_workspace.dir));
            if wi <= saved.active {
                active = self.workspaces.len();
            }
            let panel = if saved_workspace.files {
                Some(SidePanel::Files)
            } else {
                saved_workspace.git.then_some(SidePanel::Git)
            };
            let mut project = Project::default();
            project.tasks_last = saved_workspace.last_task;
            let id = self.next_id();
            self.workspaces.push(Workspace {
                id,
                name: name.into(),
                repo: Default::default(),
                canonical: super::model::canonical_dir_later(&saved_workspace.dir),
                dir: saved_workspace.dir,
                active: active_tab,
                tabs,
                tab_scroll: gpui::ScrollHandle::new(),
                project,
                panel,
                last_panel: panel.unwrap_or_default(),
                pinned_workflows: saved_workspace.pinned_workflows,
            });
        }
        if !self.workspaces.is_empty() {
            self.sidebar_shown = saved.sidebar;
            self.sidebar_width = saved.sidebar_width;
            self.panel_width = saved.panel_width;
            self.preview_width = saved.preview_width;
            self.preview.update(cx, |panel, _| panel.set_options(saved.preview_wrap, saved.preview_source));
            let file_tree = crate::file_tree::Options {
                show_ignored: saved.show_ignored,
                show_dotfiles: !saved.hide_dotfiles,
                search_tree: saved.file_search_tree,
                search: saved.file_search_options,
            };
            self.file_tree.update(cx, |tree, cx| tree.set_options(file_tree, cx));
            let git = crate::git_panel::Options {
                tree: saved.git_tree,
                show_subs: !saved.git_hide_subs,
                show_worktrees: !saved.git_hide_worktrees,
                graph_collapsed: saved.git_graph_collapsed,
                graph_height: saved.git_graph_height,
            };
            self.git_panel.update(cx, |panel, _| panel.set_options(git));
            self.github_actions.update(cx, |page, _| page.set_heights(saved.github_actions_heights));
            self.activate_workspace(active, window, cx);
        }
        shell
    }

    /// `shown`：这个标签显示在窗口里，接上的会话要看屏幕，否则只看状态；要新开的，显示着的马上在
    /// 宿主里开会话，看不见的等切过去时再开（`TerminalView::deferred`）。建出来的终端放进 `panes`；
    /// 存档里的每个叶子按先后在 `leaves` 里记一项，开不起来的记 `None`，见 `saved_leaves`。
    #[allow(clippy::too_many_arguments)]
    fn restore_node(
        &mut self,
        saved: &SavedNode,
        dir: &Path,
        home: Option<&Path>,
        shown: bool,
        mut shell: Option<&mut Option<Prespawned>>,
        (panes, leaves): (&mut Panes, &mut Vec<Option<EntityId>>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Node<EntityId>> {
        match saved {
            SavedNode::Leaf { cwd, session, .. } => {
                let reattached = (*session).and_then(|id| {
                    TerminalView::reattach(id, cwd.as_deref(), shown, window, cx)
                        .inspect_err(|err| {
                            tracing::warn!("failed to reattach session {id}, starting over in its directory: {err:#}");
                        })
                        .ok()
                });
                let view = match reattached {
                    Some(view) => view,
                    None => {
                        let start = Some(format::start_dir(cwd.as_deref(), dir));
                        // 提前拉起的 shell 在家目录里，只交给同样从家目录开始的终端。
                        let in_home = start.as_deref() == home;
                        let view = match shell.and_then(|shell| shell.take_if(|_| in_home)) {
                            Some(shell) => TerminalView::adopt(shell, window, cx),
                            None if shown => TerminalView::unstarted(start.as_deref(), Some(dir), window, cx),
                            // 看不见的标签切过去时才在宿主里开会话。
                            None => Ok(TerminalView::deferred(start.as_deref(), Some(dir), window, cx)),
                        };
                        let view = match view {
                            Ok(view) => view,
                            Err(err) => {
                                tracing::error!("failed to restore terminal session: {err:#}");
                                leaves.push(None);
                                return None;
                            }
                        };
                        self.record_spawn(&view, start.as_deref());
                        view
                    }
                };
                let (id, entry) = self.pane_entry(view, window, cx);
                panes.insert(id, entry);
                leaves.push(Some(id));
                Some(Node::Leaf(id))
            }
            SavedNode::Split { axis, ratio, first, second } => {
                let first =
                    self.restore_node(first, dir, home, shown, shell.as_deref_mut(), (panes, leaves), window, cx);
                let second = self.restore_node(second, dir, home, shown, shell, (panes, leaves), window, cx);
                match (first, second) {
                    (Some(first), Some(second)) => {
                        let id = self.next_id();
                        let mut node = Node::Split(Split {
                            id,
                            axis: *axis,
                            ratio: 0.5,
                            first: Box::new(first),
                            second: Box::new(second),
                        });
                        // 经 `set_ratio` 夹到合理范围，手改坏的存档也不会把一侧挤没。
                        if ratio.is_finite() {
                            node.set_ratio(id, *ratio);
                        }
                        Some(node)
                    }
                    (node, None) | (None, node) => node,
                }
            }
        }
    }
}

/// 一个标签里的终端和对它们事件的订阅，见 `Tab::panes`。
type Panes = HashMap<EntityId, (gpui::Entity<TerminalView>, gpui::Subscription)>;

/// 存档里标签的第 `focused` 个叶子和 `collapsed` 里那几个叶子恢复成了哪个终端。存档按叶子的先后记，
/// `restored` 是同样先后的每个叶子恢复出的终端（开不起来的为 `None`），开不起来的叶子不会让后面的错位；
/// 开不起来的那几个不算。
fn saved_leaves<T: Copy>(restored: &[Option<T>], focused: usize, collapsed: &[usize]) -> (Option<T>, Vec<T>) {
    let at = |ix: usize| restored.get(ix).copied().flatten();
    (at(focused), collapsed.iter().filter_map(|&ix| at(ix)).collect())
}

impl WindowView {
    fn save_workspace(&self, workspace: &Workspace, cx: &App) -> SavedWorkspace {
        SavedWorkspace {
            name: workspace.name.to_string(),
            dir: workspace.dir.clone(),
            tabs: workspace.tabs.iter().map(|tab| self.save_tab(tab, cx)).collect(),
            active: workspace.active,
            last_task: workspace.project.tasks_last.clone(),
            git: workspace.panel == Some(SidePanel::Git),
            files: workspace.panel == Some(SidePanel::Files),
            pinned_workflows: workspace.pinned_workflows.clone(),
        }
    }

    fn save_tab(&self, tab: &Tab, cx: &App) -> SavedTab {
        SavedTab {
            root: self.save_node(&tab.root, tab, cx),
            focused: tab.root.leaves().iter().position(|id| *id == tab.focused).unwrap_or(0),
            zoomed: tab.zoomed,
            collapsed: tab
                .root
                .leaves()
                .iter()
                .enumerate()
                .filter(|(_, id)| tab.collapsed.contains(id))
                .map(|(ix, _)| ix)
                .collect(),
        }
    }

    fn save_node(&self, node: &Node<EntityId>, tab: &Tab, cx: &App) -> SavedNode {
        match node {
            Node::Leaf(id) => {
                let view = tab.panes[id].0.read(cx);
                let starting = self.spawned.get(id).filter(|(at, _)| at.elapsed() < SHELL_STARTUP);
                let cwd = match starting {
                    Some((_, start)) => start.clone(),
                    None => view.cwd(),
                };
                SavedNode::Leaf { cwd, session: view.session_id(), unstarted: !view.started() }
            }
            Node::Split(split) => SavedNode::Split {
                axis: split.axis,
                // 拖分隔线时窗口宽度为零会算出 NaN，JSON 存不了。
                ratio: if split.ratio.is_finite() { split.ratio } else { 0.5 },
                first: Box::new(self.save_node(&split.first, tab, cx)),
                second: Box::new(self.save_node(&split.second, tab, cx)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 前面的叶子开不起来时，焦点和折叠的叶子仍按存档里的位置找，不往前错一位落到别的终端上。
    #[test]
    fn a_failed_leaf_does_not_shift_the_focus_or_the_folds() {
        let restored = [Some('a'), None, Some('c'), Some('d')];
        assert_eq!(saved_leaves(&restored, 2, &[1, 3]), (Some('c'), vec!['d']));
        assert_eq!(saved_leaves(&restored, 1, &[]), (None, vec![]));
        assert_eq!(saved_leaves(&restored, 9, &[9]), (None, vec![]));
    }

    /// 关掉的窗口不是最后一个时，里面的会话随之结束，从存档里拿掉；workspace 都关掉了的窗口也拿掉。
    #[test]
    fn a_window_closed_on_its_own_is_forgotten() {
        assert_eq!(on_close(false, false, false), OnClose::Forget);
        assert_eq!(on_close(false, false, true), OnClose::Forget);
        assert_eq!(on_close(false, true, true), OnClose::Forget);
    }

    /// 关掉以后一个窗口都不剩（关最后一个窗口、关掉所有窗口），app 随即退出：每个这样关掉的窗口
    /// 都留下。
    #[test]
    fn windows_closed_on_the_way_out_are_kept() {
        assert_eq!(on_close(false, true, false), OnClose::Keep);
    }

    /// 冻结以后（会话随宿主一起结束、视图一个个关掉分屏乃至窗口），不管怎么关都不动存档，留下
    /// 冻结时的布局；只有 workspace 都关掉了的窗口照样拿掉。
    #[test]
    fn closing_after_freezing_leaves_the_layout_alone() {
        for leaving in [false, true] {
            assert_eq!(on_close(true, leaving, false), OnClose::Ignore, "{leaving}");
            assert_eq!(on_close(true, leaving, true), OnClose::Forget, "{leaving}");
        }
    }
}
