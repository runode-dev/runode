//! 把铺满窗口的任务板接到窗口上。任务板本身在 `runode_todo_board`：打开时建一个 `TodoBoard`，收起时丢掉。
//! 这里启动时把和宿主说话的办法交给它（`watch_todos`），每次画之前交给它各 workspace 的目录和名字、当前的
//! 目录、定时能不能在后台跑，画它的外框（和手机端引导页一样，见 `render_page_body`）和侧栏顶上的入口；卡片上
//! agent 的状态标记照标签上的画（`AGENT_MARKS`）。它要窗口办的事（收起、把任务会话接进窗口）经
//! `TodoBoardEvent` 交回来，它的对话框画在窗口最上层。
//!
//! 看板和手机端引导页一样盖住标签和分屏：切到任何一个标签或 workspace、按 Esc 就收起。卡片的「打开终端」
//! 把任务的会话接进它目录所在的 workspace 的一个新标签，已经在哪个分屏里时切过去。

use std::path::{Path, PathBuf};

use gpui::{AnyElement, App, Context, Div, Entity, Focusable as _, Stateful, Subscription, Window, prelude::*};
use runode_protocol::{SessionId, todo::Todo};
use runode_todo_board::{AgentMarks, Host, Input, TodoBoard, TodoBoardEvent, TodoDialog};

use super::{
    ToggleTodoBoard, WindowView,
    agents::{self, Mark},
    remote,
    titlebar::styled_agent_mark,
};
use crate::{
    assets::CHECK_ICON, config::AppConfig, host_client, terminal_view::TerminalView, ui::theme::ActiveTheme as _,
};

/// 开着的任务板。
pub(super) struct OpenTodoBoard {
    pub(super) view: Entity<TodoBoard>,
    _events: Subscription,
}

/// 卡片上 agent 的状态标记，和标签上的一样（不记干完了没看）。
const AGENT_MARKS: AgentMarks = AgentMarks {
    render: |agent, id, cx| styled_agent_mark(Mark::new(agent, false), id, cx.theme().foreground, super::cards(cx)),
    describe: |agent| Mark::new(agent, false).describe(),
};

/// 在后台跟着任务板，见 `runode_todo_board::watch`：经 `host_client` 和宿主说话，宿主说变了就重问。
pub fn watch_todos(cx: &mut App) {
    let host = Host { ask: |step| host_client::ask_todos(step), list_sessions: host_client::list_sessions };
    runode_todo_board::watch(host, host_client::todos_changed(), cx);
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
    /// 打开或收起任务板。
    pub(super) fn toggle_todo_board(&mut self, _: &ToggleTodoBoard, window: &mut Window, cx: &mut Context<Self>) {
        if self.todo_board.is_some() {
            self.close_todo_board(window, cx);
        } else {
            self.show_todo_board(window, cx);
        }
    }

    /// 打开任务板，焦点给它；马上重问一次任务会话的状态，开着时接着轮询。
    pub(super) fn show_todo_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mobile = None;
        self.drop_settings(cx);
        if self.todo_board.is_none() {
            let view = cx.new(|cx| TodoBoard::new(AGENT_MARKS, cx));
            let events = cx.subscribe_in(&view, window, Self::on_todo_board_event);
            self.todo_board = Some(OpenTodoBoard { view, _events: events });
        }
        if let Some(board) = &self.todo_board {
            window.focus(&board.view.focus_handle(cx), cx);
        }
        runode_todo_board::poll_sessions(cx);
        cx.notify();
    }

    pub(super) fn close_todo_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.todo_board.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 任务板要窗口办的事。
    fn on_todo_board_event(
        &mut self,
        _: &Entity<TodoBoard>,
        event: &TodoBoardEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TodoBoardEvent::Close => self.close_todo_board(window, cx),
            TodoBoardEvent::OpenSession { session, todo } => self.open_todo_session(*session, todo, window, cx),
        }
    }

    /// 切到任务会话：已经在哪个分屏里就切过去，否则接进它目录所在 workspace 的一个新标签（没有这样的
    /// workspace 时新开一个），看板随之收起。
    fn open_todo_session(&mut self, id: SessionId, todo: &Todo, window: &mut Window, cx: &mut Context<Self>) {
        let own = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find_map(|(pane, (terminal, _))| (terminal.read(cx).session_id() == Some(id)).then_some(*pane));
        if let Some(pane) = own {
            self.todo_board = None;
            self.show_pane(pane, window, cx);
            return;
        }
        // 当前窗口正在更新，`find_session` 读不到它，找到的是别的窗口。
        if let Some((target, pane)) = remote::find_session(id, cx) {
            agents::reveal(target, pane, cx);
            return;
        }
        let cwd = todo.worktree_dir.as_deref().unwrap_or(&todo.spec.dir).to_owned();
        let view = match TerminalView::reattach(id, Some(&cwd), true, window, cx) {
            Ok(view) => view,
            Err(err) => {
                tracing::warn!("failed to open the session of todo {}: {err:#}", todo.id);
                return;
            }
        };
        self.todo_board = None;
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let dirs: Vec<PathBuf> = self.workspaces.iter().map(|workspace| canonical(&workspace.dir)).collect();
        let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        let home = super::model::home_dir().map(|home| canonical(&home));
        let worktree = todo.worktree_dir.as_deref().map(canonical);
        match best_workspace(&dir_refs, &canonical(&todo.spec.dir), worktree.as_deref(), home.as_deref()) {
            Some(ix) => {
                self.activate_workspace(ix, window, cx);
                self.insert_tab(self.workspace().active + 1, view, window, cx);
            }
            None => self.insert_workspace(self.active + 1, todo.spec.dir.clone(), None, view, window, cx),
        }
    }

    /// 侧栏顶上的入口，任务板开着时高亮。
    pub(super) fn render_todo_board_entry(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("todo_board.title").into_owned();
        let active = self.todo_board.is_some();
        super::mobile::page_entry("todo-board-entry", CHECK_ICON, label, active, cx, |this, window, cx| {
            this.show_todo_board(window, cx);
        })
    }

    /// 任务板开着时窗口的内容，摆法和手机端引导页一样。
    pub(super) fn render_todo_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(board) = self.todo_board.as_ref().map(|board| board.view.clone()) else { return Vec::new() };
        // 要读自启文件，只在定时那一页显示着时才查：窗口几乎每帧都重画。
        let resident = board.read(cx).showing_schedules()
            && cx.global::<AppConfig>().0.terminal_host
            && runode_autostart::is_installed(runode_autostart::Kind::Host, &runode_paths::Dirs::from_env());
        let input = Input {
            workspaces: self
                .workspaces
                .iter()
                .map(|workspace| (workspace.dir.clone(), workspace.name.clone()))
                .collect(),
            dir: self.workspace().dir.clone(),
            resident,
        };
        board.update(cx, |board, _| board.set_input(input));
        self.render_page_body(board.into_any_element(), window, cx)
    }

    /// 任务板开着的新建、编辑对话框，画在窗口最上层。
    pub(super) fn todo_dialog(&self, cx: &App) -> Option<Entity<TodoDialog>> {
        self.todo_board.as_ref().and_then(|board| board.view.read(cx).dialog())
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
