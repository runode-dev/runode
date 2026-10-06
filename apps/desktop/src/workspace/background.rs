//! 后台会话：宿主里还活着、却没有窗口在显示的会话，比如存档里没记着的、恢复时没接上的。侧栏
//! 里列成「后台会话」一组：点一个就在当前 workspace 新开标签接上它，也可以全部结束。宿主跑在
//! app 里时没有这回事（app 退出会话就结束了），不列。
//!
//! 列表全 app 一份（`BackgroundSessions`）：问宿主有哪些会话（`host_client::list_sessions`），
//! 减去各窗口里的终端占着的、有别的界面连着的（`SessionInfo::claimed`）和 shell 已经退出的。列表
//! 里的会话不 attach，免得宿主把它们算成有界面连着。列表不空时每 `POLL_INTERVAL` 重问一次，空了
//! 就停；启动、接上、结束之后各问一次。问到 shell 已经退出、谁都没连着的会话（多半是列表里的后台
//! 会话自己退出了）顺手结束，免得它一直留在宿主里、挡着宿主空闲退出。

use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::Result;
use gpui::{
    App, Context, Div, Global, MouseButton, PromptLevel, SharedString, Stateful, Task, Window, div, prelude::*, px,
};
use runode_protocol::{SessionId, SessionInfo};
use runode_shared_types::{agent::AgentKind, color::Rgb};

use super::{AGENT_MARK_WIDTH, WindowView, agents::Mark, divider_color, model::display_dir, titlebar::agent_mark};
use crate::{
    host_client::{self, Mode},
    terminal_view::{DEFAULT_TITLE, TerminalView},
    ui::hsla,
};

/// 列表不空时隔这么久重问一次宿主。
const POLL_INTERVAL: Duration = Duration::from_secs(2);
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

/// 恢复完窗口以后开始看有没有后台会话。
pub fn watch(cx: &mut App) {
    refresh(cx);
}

/// 有后台会话：没被用户收起过的侧栏要显示出来，让人看得到它们。
pub(super) fn any() -> bool {
    ANY.load(Ordering::Relaxed)
}

/// 马上重问一次宿主。
fn refresh(cx: &mut App) {
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

/// 宿主里的会话；宿主跑在 app 里时为 `None`，没有后台会话可言。
fn live_sessions() -> Option<Result<Vec<SessionInfo>>> {
    (host_client::mode() != Mode::InProcess).then(host_client::list_sessions)
}

/// 按问到的会话更新列表，重画窗口；列表不空时排好下一次。问不到时列表不变。
fn apply(live: Option<Result<Vec<SessionInfo>>>, cx: &mut App) {
    let previous = cx.default_global::<BackgroundSessions>().sessions.clone();
    let sessions = match live {
        None => Vec::new(),
        Some(Ok(live)) => {
            let held = held_sessions(cx);
            let link = host_client::link();
            for id in orphans(&live, &held) {
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
    let poll = !sessions.is_empty();
    set(sessions, cx);
    if poll {
        schedule(POLL_INTERVAL, cx);
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

/// shell 已经退出、谁都没连着（没有界面，也没有命令行这类前端）、也不是窗口里的终端占着的会话，
/// 同一个会话只给一次。
fn orphans(live: &[SessionInfo], held: &HashSet<SessionId>) -> Vec<SessionId> {
    let mut seen = HashSet::new();
    live.iter()
        .filter(|session| session.exited && !session.claimed && session.clients == 0 && !held.contains(&session.id))
        .map(|session| session.id)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// 各窗口里的终端占着的会话。
fn held_sessions(cx: &App) -> HashSet<SessionId> {
    let mut held = HashSet::new();
    for window in cx.windows() {
        let Some(view) = window.downcast::<WindowView>().and_then(|window| window.read(cx).ok()) else {
            continue;
        };
        let views = view.workspaces.iter().flat_map(|workspace| &workspace.tabs).flat_map(|tab| tab.panes.values());
        held.extend(views.filter_map(|(view, _)| view.read(cx).session_id()));
    }
    held
}

/// 前台在跑 agent 的会话个数，不算 `AgentKind::Other`（用 OSC 9;4 报进度的普通程序）。
fn agent_count(sessions: &[SessionInfo]) -> usize {
    sessions.iter().filter(|session| session.meta.agent.is_some_and(|agent| agent.kind != AgentKind::Other)).count()
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
    pub(super) fn render_background(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let sessions = cx.try_global::<BackgroundSessions>()?.sessions.clone();
        if sessions.is_empty() {
            return None;
        }
        let rows: Vec<_> =
            sessions.iter().enumerate().map(|(ix, session)| background_row(ix, session, fg, bg, cx)).collect();
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let fg = hsla(fg);
        let header = div()
            .flex_none()
            .h(px(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(11.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(fg.opacity(0.45))
                    .child(rust_i18n::t!("background.title").into_owned()),
            )
            .child(
                div()
                    .id("background-end-all")
                    .flex_none()
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_color(fg.opacity(0.55))
                    .hover(|button| button.bg(hover_bg).text_color(fg))
                    .child(rust_i18n::t!("background.end_all").into_owned())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.end_background(window, cx);
                        }),
                    ),
            );
        Some(
            div()
                .id("background-sessions")
                .flex_none()
                .max_h(px(GROUP_MAX_HEIGHT))
                .overflow_y_scroll()
                .px(px(6.))
                .pt(px(4.))
                .border_t_1()
                .border_color(divider_color(fg))
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
fn background_row(ix: usize, session: &SessionInfo, fg: Rgb, bg: Rgb, cx: &mut Context<WindowView>) -> Stateful<Div> {
    let id = session.id;
    let hover_bg = hsla(bg.mix(fg, 0.06));
    let fg = hsla(fg);
    let mark = match session.meta.agent {
        Some(agent) => agent_mark(Mark::new(agent, false), ("background-agent", ix), fg),
        None => div().flex_none().w(px(AGENT_MARK_WIDTH)).into_any_element(),
    };
    let agent = session.meta.agent.map(|agent| {
        let status = Mark::new(agent, false).status.label();
        format!("{} · {status}", agent.kind.display_name())
    });
    let dir = session.meta.cwd.as_deref().map(display_dir);
    let detail = (agent.is_some() || dir.is_some()).then(|| {
        div()
            .flex()
            .gap(px(4.))
            .text_size(px(11.))
            .text_color(fg.opacity(0.45))
            .whitespace_nowrap()
            .children(agent.map(|agent| div().flex_none().child(agent)))
            // 路径长时留下结尾：最后几级目录最能区分。
            .children(dir.map(|dir| div().flex_1().min_w_0().overflow_hidden().text_ellipsis_start().child(dir)))
    });
    div()
        .id(("background-session", ix))
        .flex_none()
        .h(px(ROW_HEIGHT))
        .px(px(8.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(12.))
        .text_color(fg.opacity(0.7))
        .hover(|row| row.bg(hover_bg).text_color(fg))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.open_background(id, window, cx);
            }),
        )
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
        agent::{Agent, AgentState},
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
        assert_eq!(orphans(&live, &held), [SessionId(1)]);
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
