//! 把铺满窗口的看板接到窗口上。看板本身在 `kanban`：打开时建一个 `Kanban`，收起时丢掉。
//! 这里启动时把和宿主说话的办法交给它（`watch_todos`），每次画之前交给它各 workspace 的目录和名字、当前的
//! 目录、定时能不能在后台跑、任务会话所在分屏的 agent 上次换状态的时刻，画它的外框（和配对设备页共用 `render_page_body`）；卡片上 agent 的状态标记照
//! 标签上的画（`AGENT_MARKS`）。它要窗口办的事
//! （收起、换页、把任务会话接进窗口）经 `KanbanEvent` 交回来，它的对话框画在窗口最上层。
//!
//! 看板和配对设备页一样盖住标签和分屏：切到任何一个标签或 workspace、按 Esc 就收起。卡片的「打开终端」
//! 把任务的会话接进它目录所在的 workspace 的一个新标签，已经在哪个分屏里时切过去。

use std::path::{Path, PathBuf};

use crate::host_client::hosts::HostId;
use crate::kanban::{AgentMarks, Host, Input, Kanban, KanbanEvent, TodoDialog};
use gpui::{AnyElement, App, Context, Entity, Focusable as _, StyleRefinement, Subscription, Window, prelude::*};
use runode_protocol::{SessionId, todo::Todo};

use super::{
    AGENT_MARK_SIZE, NewTodo, ToggleKanban, ToggleSchedules, WindowView,
    agents::{self, Mark},
    remote,
    titlebar::agent_mark,
};
use crate::{config::AppConfig, host_client, terminal_view::TerminalView};

/// 开着的看板。
pub(super) struct OpenKanban {
    pub(super) view: Entity<Kanban>,
    _events: Subscription,
}

/// 卡片上 agent 的状态标记，和标签上的一样（不记干完了没看）。
const AGENT_MARKS: AgentMarks = AgentMarks {
    render: |agent, id, cx| agent_mark(Some(Mark::new(agent, false)), id, AGENT_MARK_SIZE, false, cx),
    describe: |agent| Mark::new(agent, false).describe(),
};

/// 在后台跟着看板，见 `kanban::watch`：经 `host_client` 和宿主说话，宿主说变了就重问。
pub fn watch_todos(cx: &mut App) {
    let host = Host {
        ask: |step| host_client::ask_todos(step),
        list_sessions: host_client::list_sessions,
        board: host_client::host_board,
        read_screen: host_client::read_screen,
        send_keys: host_client::send_keys,
    };
    crate::kanban::watch(host, host_client::todos_changed(), cx);
}

/// 任务会话该接进哪个 workspace（`dirs` 是各 workspace 的目录）：目录正好是任务的工作树、正好是任务的
/// 目录、是任务目录最深的上级（家目录不算，免得任务都塞进家目录那个），依次找；都没有时为空。
fn best_workspace(dirs: &[&Path], dir: &Path, worktree: Option<&Path>, home: Option<&Path>) -> Option<usize> {
    let exact = |wanted: &Path| dirs.iter().position(|candidate| *candidate == wanted);
    worktree.and_then(exact).or_else(|| exact(dir)).or_else(|| {
        dirs.iter()
            .enumerate()
            .filter(|(_, candidate)| dir.starts_with(candidate) && Some(**candidate) != home)
            .max_by_key(|(_, candidate)| candidate.components().count())
            .map(|(ix, _)| ix)
    })
}

impl WindowView {
    /// 打开或收起看板；开着的是定时页时换到任务页，开着看板的对话框时不理。
    pub(super) fn toggle_kanban(&mut self, _: &ToggleKanban, window: &mut Window, cx: &mut Context<Self>) {
        if self.todo_dialog(cx).is_some() {
            return;
        }
        if self.showing_todo_page(false, cx) {
            self.close_kanban(window, cx);
        } else {
            self.show_kanban(false, window, cx);
        }
    }

    /// 打开或收起定时页；开着的是任务页时换到定时页，开着看板的对话框时不理。
    pub(super) fn toggle_schedules(&mut self, _: &ToggleSchedules, window: &mut Window, cx: &mut Context<Self>) {
        if self.todo_dialog(cx).is_some() {
            return;
        }
        if self.showing_todo_page(true, cx) {
            self.close_kanban(window, cx);
        } else {
            self.show_kanban(true, window, cx);
        }
    }

    /// 打开新建任务的对话框，上回收起时写到一半的接着写（`todo_draft`）。看板没开着时看板只为它而开，没存就关掉
    /// 时一起收起。
    pub(super) fn new_todo(&mut self, _: &NewTodo, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_todo_page(window, cx) {
            return;
        }
        let alone = self.kanban_page().is_none();
        self.show_kanban(false, window, cx);
        let draft = self.todo_draft.take();
        if let Some(board) = self.kanban_page() {
            board.view.update(cx, |board, cx| board.new_todo(alone, draft, window, cx));
        }
    }

    /// 用户按键、拖文件夹这类要去别处：新建任务的对话框收起来留着（`todo_draft`），看板只为它开着时一起收起，
    /// 否则露出看板；开着编辑的对话框时为假，留在原处，免得丢掉写了一半的内容。
    pub(super) fn leave_todo_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(board) = self.kanban_page().map(|board| board.view.clone()) else { return true };
        let alone = board.read(cx).creating();
        if let Some(draft) = board.update(cx, |board, cx| board.take_draft(window, cx)) {
            self.todo_draft = Some(draft);
            if alone {
                self.close_kanban(window, cx);
            }
        }
        !self.dialog_open(cx)
    }

    /// 看板开着新建任务的对话框，离开时能收起来留着（`leave_todo_page`）。
    pub(super) fn drafting(&self, cx: &App) -> bool {
        self.kanban_page().is_some_and(|board| board.view.read(cx).drafting(cx))
    }

    /// 看板的新建、编辑对话框开着：换页、收起或再开一个会丢掉写了一半的内容。
    pub(super) fn dialog_open(&self, cx: &App) -> bool {
        self.kanban_page().is_some_and(|board| board.view.read(cx).editing())
    }

    /// 关掉看板里新建、编辑任务的对话框，新建的收起来留着；没开着时为假。
    pub(super) fn close_todo_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(board) = self.kanban_page().map(|board| board.view.clone()) else { return false };
        if let Some(draft) = board.update(cx, |board, cx| board.take_draft(window, cx)) {
            self.todo_draft = Some(draft);
            return true;
        }
        board.update(cx, |board, cx| board.close_page(window, cx))
    }

    /// 看板开着，显示的是定时页（`schedules`）或任务页；开着新建任务的对话框时都不算。
    fn showing_todo_page(&self, schedules: bool, cx: &App) -> bool {
        self.kanban_page().is_some_and(|board| {
            let board = board.view.read(cx);
            !board.drafting(cx) && board.showing_schedules() == schedules
        })
    }

    /// 打开看板的定时页（`schedules`）或任务页，焦点给它。
    pub(super) fn show_kanban(&mut self, schedules: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_mobile(cx);
        self.drop_settings(cx);
        self.leave_screen_page(window, cx);
        match self.kanban_page() {
            Some(board) => board.view.update(cx, |board, cx| board.show_schedules(schedules, cx)),
            None => {
                let view = cx.new(|cx| Kanban::new(AGENT_MARKS, schedules, cx));
                // 先交一次输入：从「新建任务」打开时对话框当场就要填工作区，等不到画的时候。`resident` 画定时页时
                // 才查，这里先当没开。
                let input = self.kanban_input(false, window, cx);
                view.update(cx, |board, _| board.set_input(input));
                let events = cx.subscribe_in(&view, window, Self::on_kanban_event);
                self.page = Some(super::Page::Kanban(OpenKanban { view, _events: events }));
            }
        }
        if let Some(board) = self.kanban_page() {
            window.focus(&board.view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn close_kanban(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drop_kanban() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 看板要窗口办的事。
    fn on_kanban_event(
        &mut self,
        _: &Entity<Kanban>,
        event: &KanbanEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            KanbanEvent::Close => self.close_kanban(window, cx),
            KanbanEvent::PageChanged => cx.notify(),
            KanbanEvent::OpenSession { session, todo } => self.open_todo_session(*session, todo, window, cx),
            KanbanEvent::NewTodo => self.new_todo(&NewTodo, window, cx),
            KanbanEvent::StashDraft(dialog) => self.todo_draft = Some(dialog.clone()),
        }
    }

    /// 切到任务会话：已经在哪个分屏里就切过去，否则接进它目录所在 workspace 的一个新标签（没有这样的
    /// workspace 时新开一个），看板随之收起。
    fn open_todo_session(&mut self, id: SessionId, todo: &Todo, window: &mut Window, cx: &mut Context<Self>) {
        let own = self.pane_with_session(id, cx).map(|(pane, _)| pane);
        if let Some(pane) = own {
            self.drop_kanban();
            self.show_pane(pane, window, cx);
            return;
        }
        // 当前窗口正在更新，`find_session` 读不到它，找到的是别的窗口。
        if let Some((target, pane)) = remote::find_session(&HostId::Local, id, cx) {
            self.close_kanban(window, cx);
            agents::reveal(target, pane, cx);
            return;
        }
        let cwd = todo.worktree_dir.as_deref().unwrap_or(&todo.spec.dir).to_owned();
        let view = match TerminalView::reattach(&HostId::Local, id, Some(&cwd), None, true, window, cx) {
            Ok(view) => view,
            Err(err) => {
                tracing::warn!("failed to open the session of todo {}: {err:#}", todo.id);
                return;
            }
        };
        self.drop_kanban();
        let (dir, worktree) = (todo.spec.dir.clone(), todo.worktree_dir.clone());
        // 目录也许在挂死的网络卷上，规范化在别的线程上做（`inspect_dir`），做好再摆。
        cx.spawn_in(window, async move |this, cx| {
            let real = async |dir: Option<PathBuf>| match dir {
                Some(dir) => Some(super::model::inspect_dir(dir).await.1),
                None => None,
            };
            let todo_dir = real(Some(dir.clone())).await.unwrap_or_default();
            let worktree = real(worktree).await;
            let home = real(super::model::home_dir()).await;
            this.update_in(cx, |this, window, cx| {
                // 任务会话在本机，只接进本机的 workspace。
                let local: Vec<(usize, &Path)> = (this.workspaces.iter().enumerate())
                    .filter(|(_, workspace)| workspace.host.is_local())
                    .map(|(ix, workspace)| (ix, workspace.real_dir()))
                    .collect();
                let dirs: Vec<&Path> = local.iter().map(|&(_, dir)| dir).collect();
                let found = best_workspace(&dirs, &todo_dir, worktree.as_deref(), home.as_deref());
                match found.map(|found| local[found].0) {
                    Some(ix) => {
                        this.activate_workspace(ix, window, cx);
                        this.insert_tab(this.workspace().active + 1, view, window, cx);
                    }
                    None => this.insert_workspace(this.active + 1, dir, None, view, window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// 看板的任务在本机的宿主上跑，只交这台电脑上的工作区；当前工作区在远端主机上时，新建任务先填第一个
    /// 本机工作区的目录，一个都没有时填 home 目录。
    fn kanban_input(&self, resident: bool, window: &Window, cx: &App) -> Input {
        let local: Vec<_> = self.workspaces.iter().filter(|workspace| workspace.host.is_local()).collect();
        let current = self.workspace();
        let dir = if current.host.is_local() {
            Some(current.dir.clone())
        } else {
            local.first().map(|workspace| workspace.dir.clone())
        };
        Input {
            workspaces: local.iter().map(|workspace| (workspace.dir.clone(), workspace.name.clone())).collect(),
            dir: dir.or_else(|| runode_paths::Dirs::from_env().home).unwrap_or_default(),
            shell_path: self.terminal_path(Some(&HostId::Local), cx),
            resident,
            agent_changed: self
                .agent_entries(window, cx)
                .into_iter()
                .filter_map(|entry| {
                    let view = entry.view.read(cx);
                    view.host().is_local().then(|| view.session_id()).flatten().map(|id| (id, entry.changed_at))
                })
                .collect(),
        }
    }

    /// 看板开着时窗口的内容，摆法和配对设备页一样。
    pub(super) fn render_todo_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(board) = self.kanban_page().map(|board| board.view.clone()) else { return Vec::new() };
        // 要读自启文件，只在定时那一页显示着时才查：窗口几乎每帧都重画。
        let resident = board.read(cx).showing_schedules()
            && cx.global::<AppConfig>().0.terminal_host
            && runode_autostart::is_installed(runode_autostart::Kind::Host, &runode_paths::Dirs::from_env());
        let input = self.kanban_input(resident, window, cx);
        if board.update(cx, |board, _| board.set_input(input)) {
            // 画的时候发的通知不会再要一帧，等这一帧画完再通知。
            let board = board.clone();
            cx.defer(move |cx| board.update(cx, |_, cx| cx.notify()));
        }
        // 缓存：窗口跟着终端输出、标题、转圈几乎每帧都重画，看板只在自己的数据、悬停、动画变了时重画。辅助工具
        // 开着时不缓存，理由同 `cached_slot`。
        let page = if window.is_a11y_active() {
            board.into_any_element()
        } else {
            board.cached(StyleRefinement::default().size_full()).into_any_element()
        };
        self.render_page_body(page, true, window, cx)
    }

    /// 看板开着的新建、编辑对话框，画在窗口最上层。
    pub(super) fn todo_dialog(&self, cx: &App) -> Option<Entity<TodoDialog>> {
        self.kanban_page().and_then(|board| board.view.read(cx).dialog())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_workspace_for_a_task_session() {
        let dirs =
            [Path::new("/Users/me"), Path::new("/src"), Path::new("/src/app"), Path::new("/src/app/.worktrees/fix")];
        let home = Some(Path::new("/Users/me"));
        // 工作树优先，其次正好是任务目录，再次最深的上级；家目录不算。
        assert_eq!(
            best_workspace(&dirs, Path::new("/src/app"), Some(Path::new("/src/app/.worktrees/fix")), home),
            Some(3)
        );
        assert_eq!(best_workspace(&dirs, Path::new("/src/app"), None, home), Some(2));
        assert_eq!(best_workspace(&dirs, Path::new("/src/app/web"), None, home), Some(2));
        assert_eq!(best_workspace(&dirs, Path::new("/Users/me/notes"), None, home), None);
        assert_eq!(best_workspace(&dirs, Path::new("/elsewhere"), None, home), None);
    }
}
