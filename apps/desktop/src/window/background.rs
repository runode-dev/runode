//! 后台会话：宿主里还活着、却没有窗口在显示的会话，比如存档里没记着的、恢复时没接上的。侧栏
//! 里列成「后台会话」一组：点一个就在当前 workspace 新开标签接上它，也可以全部结束。宿主跑在
//! app 里时没有这回事（app 退出会话就结束了），不列。
//!
//! 列表全 app 一份（`BackgroundSessions`）：问宿主有哪些会话（`host_client::list_sessions`），
//! 减去各窗口里的终端占着的、有别的界面连着的（`SessionInfo::claimed`）和 shell 已经退出的。列表
//! 里的会话不 attach，免得宿主把它们算成有界面连着。列表不空时每 `POLL_INTERVAL` 重问一次，空了
//! 就停；启动、接上、结束之后各问一次。问到 shell 已经退出、谁都没连着的会话（多半是列表里的后台
//! 会话自己退出了）顺手结束，免得它一直留在宿主里、挡着宿主空闲退出；任务的会话不结束，要不要结束
//! 现问宿主的任务板，问不到时这一轮一个都不结束。

use crate::ui::button::{Button, ButtonSize};
use crate::ui::theme::ActiveTheme as _;
use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::Result;
use gpui::{App, Context, Div, Global, PromptLevel, Role, SharedString, Stateful, Task, Window, div, prelude::*, px};
use runode_protocol::{ClientMsg, HostMsg, SessionId, SessionInfo};

use super::{
    AGENT_MARK_WIDTH, PressDown, WindowView, agents::Mark, cards, quit::held_sessions, titlebar::styled_agent_mark,
};
use crate::{
    host_client::{self, Mode},
    terminal_view::{DEFAULT_TITLE, TerminalView},
    ui::{
        display_dir,
        font_size::{FontSize, scaled},
    },
};

/// 列表不空时隔这么久重问一次宿主。
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 列表空着、只因为任务板没问到才再问时隔这么久：库是新版本写的这类情况会一直问不到，别每两秒唤醒宿主。
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// 每个会话一行：标题和目录各占一行，和 workspace 的行一样高。
const ROW_HEIGHT: f32 = 40.;
/// 这一组最高这么高，再多就在组里滚动，不挤掉上面的 workspace 列表。
const GROUP_MAX_HEIGHT: f32 = 220.;

/// 有没有后台会话，和 `BackgroundSessions` 的列表同步更新。侧栏默认显不显示
/// （`WindowView::sidebar_visible`）要看它，那里拿不到 `App`。
static ANY: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct BackgroundSessions {
    /// 后台会话，已经列着的保持原来的先后。
    sessions: Vec<SessionInfo>,
    /// 下一次问宿主的任务；换掉它就取消了还没开始的那次。
    next: Option<Task<()>>,
}

impl Global for BackgroundSessions {}

/// 有后台会话：没被用户收起过的侧栏要显示出来，让人看得到它们。
pub(super) fn any() -> bool {
    ANY.load(Ordering::Relaxed)
}

/// 马上重问一次宿主。恢复完窗口以后由它开始看有没有后台会话。
pub fn refresh(cx: &mut App) {
    schedule(Duration::ZERO, cx);
}

/// 过 `delay` 问一次宿主，更新列表；取代还没开始的那次。
fn schedule(delay: Duration, cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        if !delay.is_zero() {
            cx.background_executor().timer(delay).await;
        }
        // 问宿主最多要等上 `host_client::list_sessions` 的超时，放到后台线程。
        let live = cx.background_executor().spawn(async { live_sessions() }).await;
        cx.update(|cx| apply(live, cx));
    });
    cx.default_global::<BackgroundSessions>().next = Some(task);
}

/// 问宿主得到的：宿主里的会话，和任务用到的会话（见 `todo_sessions`）。
struct Live {
    sessions: Vec<SessionInfo>,
    todos: Option<HashSet<SessionId>>,
}

/// 宿主里的会话，连同任务板；宿主跑在 app 里时为 `None`，没有后台会话可言。任务板每次都现问：任务的会话
/// 不列进来，「全部结束」也就不会结束正在干活的任务。
fn live_sessions() -> Option<Result<Live>> {
    (host_client::mode() != Mode::InProcess).then(|| {
        let sessions = host_client::list_sessions()?;
        Ok(Live { sessions, todos: todo_sessions() })
    })
}

/// 任务用到的会话，现问宿主的任务板：任务没做完时宿主特意留着 shell 已经退出的会话，给人看 agent 最后
/// 的输出，不能当成没人要的结束掉。看板那份（`todo_board::todo_sessions`）启动时还没问到、开始了
/// 新任务时还没更新，靠不住。宿主不认得任务板时没有任务；问不到时为 `None`。
fn todo_sessions() -> Option<HashSet<SessionId>> {
    let link = host_client::link();
    if link.connected() && !link.host_knows_todos() {
        return Some(HashSet::new());
    }
    match host_client::ask_todos(|req| ClientMsg::ListTodos { req }) {
        Ok(HostMsg::Todos { todos, .. }) => Some(todos.iter().filter_map(|todo| todo.session).collect()),
        Ok(other) => {
            tracing::debug!("unexpected answer to the task list: {other:?}");
            None
        }
        Err(err) => {
            tracing::debug!("failed to list the tasks before ending background sessions: {err:#}");
            None
        }
    }
}

/// 按问到的会话更新列表，重画窗口；列表不空、或者任务板没问到又有会话等着结束时排好下一次。问不到时列表不变。
fn apply(live: Option<Result<Live>>, cx: &mut App) {
    let previous = cx.default_global::<BackgroundSessions>().sessions.clone();
    // 任务板没问到时，有会话等着结束也要再问，不然列表空着就不再轮询，它们一直留在宿主里。
    let mut retry = false;
    let sessions = match live {
        None => Vec::new(),
        Some(Ok(Live { sessions: live, todos })) => {
            // 任务的会话在任务板上，不列在这里；没问到任务板时用看板那份兜底。
            let mut held = held_sessions(cx);
            held.extend(crate::todo_board::todo_sessions(cx));
            held.extend(todos.iter().flatten());
            retry = todos.is_none() && live.iter().any(|session| session.exited);
            let link = host_client::link();
            for id in orphans(&live, &held, todos.as_ref()) {
                tracing::info!("ending the background session {id}: its shell exited");
                link.kill(id);
            }
            background(&live, &held, &previous)
        }
        Some(Err(err)) => {
            tracing::debug!("failed to list the host's sessions for the background list: {err:#}");
            previous
        }
    };
    let poll = if !sessions.is_empty() { Some(POLL_INTERVAL) } else { retry.then_some(RETRY_INTERVAL) };
    set(sessions, cx);
    if let Some(delay) = poll {
        schedule(delay, cx);
    } else {
        cx.global_mut::<BackgroundSessions>().next = None;
    }
}

/// 换上新列表，变了时重画各窗口。
fn set(sessions: Vec<SessionInfo>, cx: &mut App) {
    ANY.store(!sessions.is_empty(), Ordering::Relaxed);
    let state = cx.default_global::<BackgroundSessions>();
    if state.sessions != sessions {
        state.sessions = sessions;
        cx.refresh_windows();
    }
}

/// 从列表里拿掉这些会话（刚接上或者结束了的），不等下次问宿主。
fn forget(ids: &[SessionId], cx: &mut App) {
    let mut sessions = cx.default_global::<BackgroundSessions>().sessions.clone();
    sessions.retain(|session| !ids.contains(&session.id));
    set(sessions, cx);
}

/// 后台会话：`live` 里 shell 还没退出、没有界面连着、也不是窗口里的终端占着（`held`）的会话，
/// 同一个会话只列一次。`previous` 里已经列着的保持原来的先后，新出现的排在后面，每次刷新列表
/// 不会来回跳。
fn background(live: &[SessionInfo], held: &HashSet<SessionId>, previous: &[SessionInfo]) -> Vec<SessionInfo> {
    let mut seen = HashSet::new();
    let mut sessions: Vec<SessionInfo> = live
        .iter()
        .filter(|session| !session.exited && !session.claimed && !held.contains(&session.id))
        .filter(|session| seen.insert(session.id))
        .cloned()
        .collect();
    let rank = |id: SessionId| previous.iter().position(|session| session.id == id).unwrap_or(usize::MAX);
    sessions.sort_by_key(|session| (rank(session.id), session.id));
    sessions
}

/// shell 已经退出、谁都没连着（没有界面，也没有命令行这类前端）、也不是窗口里的终端占着（`held`）、
/// 不是任务用到的（`todos`）会话，同一个会话只给一次。不知道任务用到哪些会话时一个都不给。
fn orphans(live: &[SessionInfo], held: &HashSet<SessionId>, todos: Option<&HashSet<SessionId>>) -> Vec<SessionId> {
    let Some(todos) = todos else { return Vec::new() };
    let mut seen = HashSet::new();
    live.iter()
        .filter(|session| session.exited && !session.claimed && session.clients == 0)
        .filter(|session| !held.contains(&session.id) && !todos.contains(&session.id))
        .map(|session| session.id)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// 前台在跑 agent 的会话个数，不算 `AgentKind::Other`（用 OSC 9;4 报进度的普通程序）。
fn agent_count(sessions: &[SessionInfo]) -> usize {
    sessions.iter().filter(|session| session.meta.agent.is_some_and(|agent| agent.kind.is_known())).count()
}

/// 会话的标题：程序设置的标题，没有时是前台程序名或者目录名。
fn title(session: &SessionInfo) -> SharedString {
    let meta = &session.meta;
    meta.title.as_deref().or(meta.fallback_title.as_deref()).unwrap_or(DEFAULT_TITLE).to_owned().into()
}

impl WindowView {
    /// 在当前 workspace 当前标签的右边新开一个标签，接上后台会话 `id`。
    fn open_background(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = cx
            .try_global::<BackgroundSessions>()
            .and_then(|state| state.sessions.iter().find(|session| session.id == id))
            .and_then(|session| session.meta.cwd.clone());
        match TerminalView::reattach(id, cwd.as_deref(), true, window, cx) {
            Ok(view) => {
                forget(&[id], cx);
                self.insert_tab(self.workspace().active + 1, view, window, cx);
            }
            Err(err) => tracing::warn!("failed to open the background session {id}: {err:#}"),
        }
        refresh(cx);
    }

    /// 结束所有后台会话；里面有 agent 在跑时先问一句。
    fn end_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = cx.try_global::<BackgroundSessions>() else {
            return;
        };
        let ids: Vec<SessionId> = state.sessions.iter().map(|session| session.id).collect();
        let agents = agent_count(&state.sessions);
        if agents == 0 {
            end_sessions(&ids, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("background.end_title"),
            Some(&rust_i18n::t!("background.end_detail", count = agents)),
            &[&*rust_i18n::t!("background.end_all"), &*rust_i18n::t!("quit.cancel")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await.ok() == Some(0) {
                cx.update(|cx| end_sessions(&ids, cx));
            }
        })
        .detach();
    }

    /// 侧栏里的「后台会话」一组；没有后台会话时为空。
    pub(super) fn render_background(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let sessions = cx.try_global::<BackgroundSessions>()?.sessions.clone();
        if sessions.is_empty() {
            return None;
        }
        let rows: Vec<_> = sessions.iter().enumerate().map(|(ix, session)| background_row(ix, session, cx)).collect();
        let fg = cx.theme().foreground;
        let header = div()
            .flex_none()
            .h(scaled(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(FontSize::Small)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(fg.opacity(0.45))
                    .child(rust_i18n::t!("background.title").into_owned()),
            )
            .child(
                Button::new("background-end-all", rust_i18n::t!("background.end_all"))
                    .with_size(ButtonSize::Sm)
                    .h_auto()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_color(fg.opacity(0.55))
                    .on_press_down(cx, |this, window, cx| this.end_background(window, cx)),
            );
        Some(
            div()
                .id("background-sessions")
                .role(Role::Group)
                .aria_label(rust_i18n::t!("background.title"))
                .flex_none()
                .max_h(scaled(GROUP_MAX_HEIGHT))
                .overflow_y_scroll()
                .px(px(6.))
                .pt(px(4.))
                .border_t_1()
                .border_color(cx.theme().border)
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(header)
                .children(rows),
        )
    }
}

/// 结束这些会话。
fn end_sessions(ids: &[SessionId], cx: &mut App) {
    let link = host_client::link();
    for id in ids {
        link.kill(*id);
    }
    forget(ids, cx);
    refresh(cx);
}

/// 「后台会话」里的一行：agent 标记，标题，下面是 agent 和状态、目录。点了在当前 workspace 新开
/// 标签接上它。
fn background_row(ix: usize, session: &SessionInfo, cx: &mut Context<WindowView>) -> Stateful<Div> {
    let id = session.id;
    let hover_bg = cx.theme().mix(0.06);
    let fg = cx.theme().foreground;
    let mark = match session.meta.agent {
        Some(agent) => styled_agent_mark(Mark::new(agent, false), ("background-agent", ix), fg, cards(cx)),
        None => div().flex_none().w(px(AGENT_MARK_WIDTH)).into_any_element(),
    };
    let agent = session.meta.agent.map(|agent| Mark::new(agent, false).describe());
    let dir = session.meta.cwd.as_deref().map(display_dir);
    let description = [agent.clone(), dir.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let detail = (agent.is_some() || dir.is_some()).then(|| {
        div()
            .flex()
            .gap(px(4.))
            .text_size(FontSize::Small)
            .text_color(fg.opacity(0.45))
            .whitespace_nowrap()
            .children(agent.map(|agent| div().flex_none().child(agent)))
            // 路径长时留下结尾：最后几级目录最能区分。
            .children(dir.map(|dir| div().flex_1().min_w_0().overflow_hidden().text_ellipsis_start().child(dir)))
    });
    div()
        .id(("background-session", ix))
        .role(Role::Button)
        .aria_label(title(session))
        .when(!description.is_empty(), |row| row.aria_description(description))
        .flex_none()
        .h(scaled(ROW_HEIGHT))
        .px(px(8.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(FontSize::Body)
        .text_color(fg.opacity(0.7))
        .hover(|row| row.bg(hover_bg).text_color(fg))
        .on_press_down(cx, move |this, window, cx| this.open_background(id, window, cx))
        .child(mark)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(div().truncate().child(title(session)))
                .children(detail),
        )
}

#[cfg(test)]
mod tests {
    use runode_shared_types::{
        agent::{Agent, AgentKind, AgentState},
        grid::GridSize,
        session::SessionMeta,
    };

    use super::*;

    fn session(id: u128, claimed: bool, exited: bool) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            size: GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 },
            meta: SessionMeta::default(),
            clients: 0,
            claimed,
            exited,
            size_owner: None,
        }
    }

    fn ids(sessions: &[SessionInfo]) -> Vec<u128> {
        sessions.iter().map(|session| session.id.0).collect()
    }

    #[test]
    fn background_sessions_are_live_ones_nobody_shows() {
        let live = [
            session(1, false, false),
            session(2, true, false),
            session(3, false, true),
            session(4, false, false),
            session(5, false, false),
            session(4, false, false),
        ];
        // 5 是窗口里的终端占着的；2 有界面连着；3 的 shell 退出了；4 列了两次。
        let held = HashSet::from([SessionId(5)]);
        assert_eq!(ids(&background(&live, &held, &[])), [1, 4]);
        assert!(background(&[], &held, &[]).is_empty());
    }

    #[test]
    fn exited_sessions_nobody_holds_are_orphans() {
        let attached = SessionInfo { clients: 1, ..session(4, false, true) };
        let live = [
            session(1, false, true),
            session(2, true, true),
            session(3, false, false),
            attached,
            session(5, false, true),
            session(1, false, true),
        ];
        // 2 有界面连着；3 还在跑；4 有命令行连着；5 是窗口里的终端占着的。
        let held = HashSet::from([SessionId(5)]);
        assert_eq!(orphans(&live, &held, Some(&HashSet::new())), [SessionId(1)]);
        // 1 是任务的会话；不知道任务用到哪些会话时一个都不结束。
        assert!(orphans(&live, &held, Some(&HashSet::from([SessionId(1)]))).is_empty());
        assert!(orphans(&live, &held, None).is_empty());
    }

    #[test]
    fn listed_sessions_keep_their_place() {
        let previous = [session(9, false, false), session(3, false, false)];
        let live = [session(1, false, false), session(3, false, false), session(9, false, false)];
        assert_eq!(ids(&background(&live, &HashSet::new(), &previous)), [9, 3, 1]);
    }

    #[test]
    fn only_real_agents_count_for_the_prompt() {
        let with_agent = |id: u128, kind: AgentKind| {
            let mut session = session(id, false, false);
            session.meta.agent = Some(Agent { kind, state: AgentState::Working });
            session
        };
        let sessions = [with_agent(1, AgentKind::Claude), with_agent(2, AgentKind::Other), session(3, false, false)];
        assert_eq!(agent_count(&sessions), 1);
    }
}
