//! 把铺满窗口的看板接到窗口上。看板本身在 `kanban`：打开时建一个 `Kanban`，收起时丢掉。
//! 这里启动时把和宿主说话的办法交给它（`watch_todos`），每次画之前交给它各 workspace 的目录和名字、当前的
//! 目录、定时能不能在后台跑，画它的外框（和手机端引导页一样，见 `render_page_body`）和侧栏顶上新建任务、定时、看板三个
//! 入口（按着修饰键时右边写着快捷键）；卡片上 agent 的状态标记照标签上的画（`AGENT_MARKS`）。它要窗口办的事
//! （收起、换页、把任务会话接进窗口）经 `KanbanEvent` 交回来，它的对话框画在窗口最上层。
//!
//! 看板和手机端引导页一样盖住标签和分屏：切到任何一个标签或 workspace、按 Esc 就收起。卡片的「打开终端」
//! 把任务的会话接进它目录所在的 workspace 的一个新标签，已经在哪个分屏里时切过去。

use std::path::{Path, PathBuf};

use crate::kanban::{AgentMarks, Host, Input, Kanban, KanbanEvent, TodoDialog};
use gpui::{
    Action, AnyElement, App, Context, Div, Entity, Focusable as _, Modifiers, Stateful, StyleRefinement, Subscription,
    Window, div, prelude::*,
};
use runode_protocol::{SessionId, todo::Todo};

use super::{
    NewTodo, ToggleKanban, ToggleSchedules, WindowView,
    agents::{self, Mark},
    remote,
    sidebar::shortcut_modifiers,
    titlebar::styled_agent_mark,
};
use crate::{
    assets::{CLOCK_ICON, KANBAN_ICON, PLUS_ICON},
    config::AppConfig,
    host_client,
    terminal_view::TerminalView,
    ui::{
        font_size::FontSize,
        theme::ActiveTheme as _,
        tooltip::{shortcut_text, tooltip},
    },
};

/// 开着的看板。
pub(super) struct OpenKanban {
    pub(super) view: Entity<Kanban>,
    _events: Subscription,
}

/// 卡片上 agent 的状态标记，和标签上的一样（不记干完了没看）。
const AGENT_MARKS: AgentMarks = AgentMarks {
    render: |agent, id, cx| styled_agent_mark(Mark::new(agent, false), id, cx.theme().foreground, super::cards(cx)),
    describe: |agent| Mark::new(agent, false).describe(),
};

/// 在后台跟着看板，见 `kanban::watch`：经 `host_client` 和宿主说话，宿主说变了就重问。
pub fn watch_todos(cx: &mut App) {
    let host = Host { ask: |step| host_client::ask_todos(step), list_sessions: host_client::list_sessions };
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
    /// 打开或收起看板；开着的是定时页时换到任务页。
    pub(super) fn toggle_kanban(&mut self, _: &ToggleKanban, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_open(cx) {
            return;
        }
        if self.showing_todo_page(false, cx) {
            self.close_kanban(window, cx);
        } else {
            self.show_kanban(false, window, cx);
        }
    }

    /// 打开或收起定时页；开着的是任务页时换到定时页。
    pub(super) fn toggle_schedules(&mut self, _: &ToggleSchedules, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_open(cx) {
            return;
        }
        if self.showing_todo_page(true, cx) {
            self.close_kanban(window, cx);
        } else {
            self.show_kanban(true, window, cx);
        }
    }

    /// 打开看板的任务页，弹出新建任务的对话框。
    pub(super) fn new_todo(&mut self, _: &NewTodo, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog_open(cx) {
            return;
        }
        self.show_kanban(false, window, cx);
        if let Some(board) = &self.kanban {
            board.view.update(cx, |board, cx| board.new_todo(window, cx));
        }
    }

    /// 看板的新建、编辑对话框开着。它画在窗口根上，窗口的快捷键在里面照样触发，这时换页、收起或再开一个
    /// 都会把写了一半的内容丢掉，所以这几个快捷键不理。
    fn dialog_open(&self, cx: &App) -> bool {
        self.todo_dialog(cx).is_some()
    }

    /// 看板开着，显示的是定时页（`schedules`）或任务页。
    fn showing_todo_page(&self, schedules: bool, cx: &App) -> bool {
        self.kanban.as_ref().is_some_and(|board| board.view.read(cx).showing_schedules() == schedules)
    }

    /// 打开看板的定时页（`schedules`）或任务页，焦点给它。
    pub(super) fn show_kanban(&mut self, schedules: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_mobile(cx);
        self.drop_settings(cx);
        match &self.kanban {
            Some(board) => board.view.update(cx, |board, cx| board.show_schedules(schedules, cx)),
            None => {
                let view = cx.new(|cx| Kanban::new(AGENT_MARKS, schedules, cx));
                let events = cx.subscribe_in(&view, window, Self::on_kanban_event);
                self.kanban = Some(OpenKanban { view, _events: events });
            }
        }
        if let Some(board) = &self.kanban {
            window.focus(&board.view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn close_kanban(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.kanban.take().is_some() {
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
            self.kanban = None;
            self.show_pane(pane, window, cx);
            return;
        }
        // 当前窗口正在更新，`find_session` 读不到它，找到的是别的窗口。
        if let Some((target, pane)) = remote::find_session(id, cx) {
            self.close_kanban(window, cx);
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
        self.kanban = None;
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

    /// 侧栏顶上的新建任务入口：打开任务页，弹出新建任务的对话框。
    pub(super) fn render_new_todo_entry(&self, modifiers: Modifiers, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("kanban.new_todo").into_owned();
        super::mobile::page_entry("new-todo-entry", PLUS_ICON, label.clone(), false, cx, |this, window, cx| {
            this.new_todo(&NewTodo, window, cx);
        })
        .tooltip(tooltip(label, Some(&NewTodo)))
        .children(entry_shortcut(&NewTodo, modifiers, cx))
    }

    /// 侧栏顶上的定时入口，看板开着定时页时高亮。
    pub(super) fn render_schedules_entry(&self, modifiers: Modifiers, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("kanban.schedules").into_owned();
        let active = self.showing_todo_page(true, cx);
        super::mobile::page_entry("schedules-entry", CLOCK_ICON, label.clone(), active, cx, |this, window, cx| {
            this.show_kanban(true, window, cx);
        })
        .tooltip(tooltip(label, Some(&ToggleSchedules)))
        .children(entry_shortcut(&ToggleSchedules, modifiers, cx))
    }

    /// 侧栏顶上的看板入口，看板开着任务页时高亮；有需要你处理的任务时右边是它们的数。
    pub(super) fn render_kanban_entry(&self, modifiers: Modifiers, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("kanban.title").into_owned();
        let active = self.showing_todo_page(false, cx);
        let count = crate::kanban::needs_you(cx);
        let shortcut = entry_shortcut(&ToggleKanban, modifiers, cx);
        let pushed = shortcut.is_some();
        super::mobile::page_entry("kanban-entry", KANBAN_ICON, label.clone(), active, cx, |this, window, cx| {
            this.show_kanban(false, window, cx);
        })
        .tooltip(tooltip(label, Some(&ToggleKanban)))
        .children(shortcut)
        .when(count > 0, |entry| {
            entry
                .aria_description(rust_i18n::t!("kanban.entry_needs_you", count = count).into_owned())
                .child(crate::kanban::needs_you_badge(count).when(!pushed, |badge| badge.ml_auto()))
        })
    }

    /// 看板开着时窗口的内容，摆法和手机端引导页一样。
    pub(super) fn render_todo_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(board) = self.kanban.as_ref().map(|board| board.view.clone()) else { return Vec::new() };
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
        self.render_page_body(page, window, cx)
    }

    /// 看板开着的新建、编辑对话框，画在窗口最上层。
    pub(super) fn todo_dialog(&self, cx: &App) -> Option<Entity<TodoDialog>> {
        self.kanban.as_ref().and_then(|board| board.view.read(cx).dialog())
    }
}

/// 侧栏入口右边淡淡的快捷键，从键位表里查，快捷键改了也跟着变；没绑时没有。和 workspace 那几行一样，只在
/// 按着它的修饰键或者单按 ⌘ 时显示，平时只在悬停提示里写着。不按时不占位置，看板的徽标一直贴着右边。
fn entry_shortcut(action: &dyn Action, modifiers: Modifiers, cx: &App) -> Option<Div> {
    let held = modifiers == Modifiers::command() || shortcut_modifiers(action, action, false, cx) == Some(modifiers);
    shortcut_text(action, cx).filter(|_| held).map(|text| {
        div()
            .flex_none()
            .ml_auto()
            .text_size(FontSize::Small)
            .text_color(cx.theme().foreground.opacity(0.5))
            .child(text)
    })
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
