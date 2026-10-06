//! 退出应用、关掉窗口时宿主里的会话怎么办，以及要结束正在干活的 agent 之前先问一句。
//!
//! 会话结不结束看宿主怎么跑（`session_host::Mode`）和用户做了什么（`QuitAction`），决策在
//! `quit_plan`：宿主跑在 app 里时，退出（包括关掉最后一个窗口、关掉所有窗口）必然结束所有会话；
//! 宿主单独一个进程时，退出只是不再看它们，会话留在宿主里等下次启动接回来，要连会话一起结束用
//! 「退出并结束所有会话」；配置项 `terminal-host` 已经关了、这次只是接回上次留下的会话时，退出
//! 让宿主连会话一起退出。只有会结束会话、又有 agent 在跑时才弹框确认，免得一按 cmd+q 把正在干活
//! 或者攒着上下文的会话一起结束掉。让宿主连会话一起退出之前先冻结存档（`persistence::freeze`）：
//! 视图会先收到会话结束、一个个关掉分屏，冻结了才不会把存档清空，下次启动照原样在原目录新开。
//!
//! 关掉的窗口不是最后一个时，app 不退出，结束这个窗口里的会话（`WindowView::end_sessions`），不问。
//!
//! 弹框之前都先推迟到当前的更新结束：动作和关闭按钮的回调运行时，触发它的窗口正被借出，这时
//! 既读不到它里面的 agent，也没法在它上面弹框。

use std::{collections::HashSet, time::Duration};

use gpui::{AnyWindowHandle, App, Global, PromptLevel, Window};
use runode_protocol::{ClientMsg, SessionId, SessionInfo};
use runode_shared_types::agent::AgentKind;

use super::{WindowView, model::Closing, persistence};
use crate::{
    session_host::{self, Mode},
    terminal_view::TerminalView,
};

/// 让单独跑的宿主连会话一起退出后，最多等它这么久读完之前发的消息。
const SHUTDOWN_FLUSH: Duration = Duration::from_millis(200);

/// 用户做的会让 app 退出的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QuitAction {
    /// 退出应用（cmd+q）。
    Quit,
    /// 关掉最后一个窗口，app 跟着退出。
    CloseLastWindow,
    /// 关掉所有窗口。
    CloseAllWindows,
    /// 菜单里的「退出并结束所有会话」。
    QuitAndEndSessions,
}

/// 退出时宿主里的会话怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// 留在宿主里，下次启动接回来。
    Keep,
    /// 宿主跑在 app 里，随 app 退出一起结束。
    WithApp,
    /// 先让单独跑的宿主连会话一起退出（`ClientMsg::Shutdown`），再退出。
    ShutdownHost,
}

/// 退出前弹哪种确认框。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prompt {
    /// 退出会结束各窗口里的 agent，按 workspace 列出它们。
    Quit,
    /// 退出会结束所有会话（包括没在窗口里显示的），给出 agent 的个数。
    QuitEndingAll,
    /// 「退出并结束所有会话」，给出 agent 的个数。
    EndAll,
}

/// 一次退出怎么做。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) ending: Ending,
    /// 先弹框确认，确认了才退出；`None` 时直接退出。
    pub(super) prompt: Option<Prompt>,
}

/// `action` 让 app 退出时会话怎么办。
pub(super) fn ending(mode: Mode, action: QuitAction) -> Ending {
    match (mode, action) {
        (Mode::InProcess, _) => Ending::WithApp,
        (Mode::Standalone { end_on_quit: true }, _) | (Mode::Standalone { .. }, QuitAction::QuitAndEndSessions) => {
            Ending::ShutdownHost
        }
        (Mode::Standalone { end_on_quit: false }, _) => Ending::Keep,
    }
}

/// `action` 让 app 退出时怎么做；`agents` 是退出会结束掉的会话里有几个 agent 在跑（宿主单独跑时
/// 连没在窗口里显示的会话也算）。会话留下时不问，会结束会话又有 agent 时先确认。
pub(super) fn quit_plan(mode: Mode, action: QuitAction, agents: usize) -> Plan {
    let ending = ending(mode, action);
    let prompt = match ending {
        _ if agents == 0 => None,
        Ending::Keep => None,
        Ending::WithApp => Some(Prompt::Quit),
        Ending::ShutdownHost if action == QuitAction::QuitAndEndSessions => Some(Prompt::EndAll),
        Ending::ShutdownHost => Some(Prompt::QuitEndingAll),
    };
    Plan { ending, prompt }
}

/// 菜单里有没有「退出并结束所有会话」：只在退出会把会话留下时才有，否则和退出一样。
pub(super) fn offers_end_sessions(mode: Mode) -> bool {
    ending(mode, QuitAction::Quit) == Ending::Keep
}

/// 菜单里要不要放「退出并结束所有会话」，见 `offers_end_sessions`。
pub fn end_sessions_in_menu() -> bool {
    offers_end_sessions(session_host::mode())
}

/// 退出确认框开着（或者正问宿主有几个 agent）；这时再按退出或者关窗口不再弹第二个。
#[derive(Default)]
struct Prompting(bool);

impl Global for Prompting {}

fn prompting(cx: &App) -> bool {
    cx.try_global::<Prompting>().is_some_and(|prompting| prompting.0)
}

/// 退出应用，会话怎么办见 `quit_plan`。
pub fn quit(cx: &mut App) {
    cx.defer(|cx| {
        let window = front_window(cx);
        run(QuitAction::Quit, window, cx, |cx| cx.quit());
    });
}

/// 退出应用并结束宿主里所有的会话，包括没在窗口里显示的；有 agent 在跑时先确认。
pub fn quit_and_end_sessions(cx: &mut App) {
    cx.defer(|cx| {
        let window = front_window(cx);
        run(QuitAction::QuitAndEndSessions, window, cx, |cx| cx.quit());
    });
}

/// 关掉 `window`。还有别的窗口时结束它里面的会话；它是最后一个窗口时应用会跟着退出，这时跟
/// 退出一样。
pub fn close_window(window: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| {
        let remove = move |cx: &mut App| {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        };
        let windows = cx.windows().len();
        if windows > 1 {
            if let Some(window) = window.downcast::<WindowView>() {
                window.update(cx, |view, _, cx| view.end_sessions(Closing::Window { windows }, cx)).ok();
            }
            remove(cx);
        } else {
            run(QuitAction::CloseLastWindow, Some(window), cx, remove);
        }
    });
}

/// 关掉所有窗口，应用跟着退出，会话怎么办和退出一样。
pub fn close_all_windows(cx: &mut App) {
    cx.defer(|cx| {
        let window = front_window(cx);
        run(QuitAction::CloseAllWindows, window, cx, |cx| {
            for window in cx.windows() {
                window.update(cx, |_, window, _| window.remove_window()).ok();
            }
        });
    });
}

/// 点了窗口的关闭按钮：不是最后一个窗口时结束它里面的会话、照常关；是最后一个时先不关，交给
/// `close_window` 按退出处理。
pub fn should_close(window: &mut Window, cx: &mut App) -> bool {
    let windows = cx.windows().len();
    if windows > 1 {
        if let Some(Some(view)) = window.root::<WindowView>() {
            view.update(cx, |view, cx| view.end_sessions(Closing::Window { windows }, cx));
        }
        return true;
    }
    close_window(window.window_handle(), cx);
    false
}

/// 确认框弹在当前窗口上；当前没有窗口在前台时弹在第一个窗口上。
fn front_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.active_window().or_else(|| cx.windows().into_iter().next())
}

/// 按 `quit_plan` 做 `action`：该问就在 `window` 上问，确认了（或者不用问）再做 `then`，要让宿主
/// 连会话一起退出时先让它退出。
fn run(action: QuitAction, window: Option<AnyWindowHandle>, cx: &mut App, then: impl FnOnce(&mut App) + 'static) {
    if prompting(cx) {
        return;
    }
    let mode = session_host::mode();
    let ending = ending(mode, action);
    if ending == Ending::Keep {
        then(cx);
        return;
    }
    // 没被窗口里的终端占着的会话（丢掉视图后留在宿主里的、后台会话）也会被结束，里面的 agent
    // 也要算，得问宿主；问它最多要等上 `session_host::list_sessions` 的超时，放到后台线程，期间
    // 不再接退出。问不到时只数窗口里的。
    cx.set_global(Prompting(true));
    let sessions = cx.background_executor().spawn(async { session_host::list_sessions() });
    cx.spawn(async move |cx| {
        let sessions = sessions.await;
        cx.update(|cx| {
            cx.set_global(Prompting(false));
            let sessions = sessions
                .inspect_err(|err| {
                    tracing::warn!("failed to list the host's sessions, counting the agents in windows: {err:#}");
                })
                .unwrap_or_default();
            let shown = window_agents(cx);
            let held = held_sessions(cx);
            let hidden = hidden_agents(&sessions, &held);
            let count = shown.len() + hidden.len();
            let plan = quit_plan(mode, action, count);
            let text = plan.prompt.map(|prompt| prompt_text(prompt, &agent_names(&shown, &hidden), count));
            if ending == Ending::ShutdownHost {
                confirm(window, text, cx, move |cx| {
                    // 宿主连会话一起退出后，视图会先收到会话结束、关掉分屏，存档要在那之前定下来，
                    // 下次启动才能照原样在原目录新开。
                    persistence::freeze(cx);
                    shutdown_host();
                    then(cx);
                });
            } else {
                confirm(window, text, cx, then);
            }
        });
    })
    .detach();
}

/// 让单独跑的宿主连会话一起退出，等它读完（最多 `SHUTDOWN_FLUSH`）。
fn shutdown_host() {
    let link = session_host::link();
    link.send(ClientMsg::Shutdown { kill_sessions: true });
    if !link.flush(SHUTDOWN_FLUSH) {
        tracing::warn!("the host did not take the shutdown in time");
    }
}

/// 确认框的标题、说明和确认按钮。
struct PromptText {
    title: String,
    detail: String,
    confirm: String,
}

/// `names` 是 `Prompt::Quit` 要列出的 agent，`count` 是别的几种给出的个数。
fn prompt_text(prompt: Prompt, names: &[String], count: usize) -> PromptText {
    let t = |key: &str| rust_i18n::t!(key).into_owned();
    match prompt {
        Prompt::Quit => PromptText {
            title: t("quit.title"),
            detail: rust_i18n::t!("quit.detail", agents = names.join(rust_i18n::t!("quit.separator").as_ref()))
                .into_owned(),
            confirm: t("quit.confirm"),
        },
        Prompt::QuitEndingAll => PromptText {
            title: t("quit.title"),
            detail: rust_i18n::t!("quit.end_detail", count = count).into_owned(),
            confirm: t("quit.confirm"),
        },
        Prompt::EndAll => PromptText {
            title: t("quit.end_title"),
            detail: rust_i18n::t!("quit.end_detail", count = count).into_owned(),
            confirm: t("quit.end_confirm"),
        },
    }
}

/// 没有要问的（`text` 为空）或者没有窗口能弹框时直接做 `then`；否则在 `window` 上弹框，确认了
/// 再做。确认框已经开着时什么都不做。
fn confirm(
    window: Option<AnyWindowHandle>,
    text: Option<PromptText>,
    cx: &mut App,
    then: impl FnOnce(&mut App) + 'static,
) {
    if prompting(cx) {
        return;
    }
    let (Some(window), Some(text)) = (window, text) else {
        then(cx);
        return;
    };
    let answer = window.update(cx, |_, window, cx| {
        window.prompt(
            PromptLevel::Warning,
            &text.title,
            Some(&text.detail),
            &[&*text.confirm, &*rust_i18n::t!("quit.cancel")],
            cx,
        )
    });
    let Ok(answer) = answer else {
        return;
    };
    cx.set_global(Prompting(true));
    cx.spawn(async move |cx| {
        let confirmed = answer.await.ok() == Some(0);
        cx.update(|cx| {
            cx.set_global(Prompting(false));
            if confirmed {
                then(cx);
            }
        });
    })
    .detach();
}

/// 宿主的会话里没被窗口里的终端占着（不在 `held` 里）、前台在跑的 agent，已经退出的不算。
fn hidden_agents(sessions: &[SessionInfo], held: &HashSet<SessionId>) -> Vec<AgentKind> {
    sessions
        .iter()
        .filter(|session| !session.exited && !held.contains(&session.id))
        .filter_map(|session| session.meta.agent.map(|agent| agent.kind))
        .filter(|kind| is_agent(Some(*kind)))
        .collect()
}

/// 不算 `AgentKind::Other`：那是用 OSC 9;4 报进度的普通程序，不是 agent 会话。
fn is_agent(kind: Option<AgentKind>) -> bool {
    kind.is_some_and(|kind| kind != AgentKind::Other)
}

/// 窗口里一个前台在跑 agent 的终端。
struct ShownAgent {
    kind: AgentKind,
    /// 所在 workspace 的名字。
    workspace: String,
}

/// 各窗口里的终端，对每个终端调 `f`（带上所在 workspace 的名字）。
fn for_each_view(cx: &App, mut f: impl FnMut(&TerminalView, &str)) {
    for window in cx.windows() {
        let Some(view) = window.downcast::<WindowView>().and_then(|window| window.read(cx).ok()) else {
            continue;
        };
        for workspace in &view.workspaces {
            for tab in &workspace.tabs {
                for (view, _) in tab.panes.values() {
                    f(view.read(cx), &workspace.name);
                }
            }
        }
    }
}

/// 各窗口里前台在跑 agent 的终端，每个一项。
fn window_agents(cx: &App) -> Vec<ShownAgent> {
    let mut agents = Vec::new();
    for_each_view(cx, |view, workspace| {
        if let Some(kind) = view.agent().map(|agent| agent.kind).filter(|kind| is_agent(Some(*kind))) {
            agents.push(ShownAgent { kind, workspace: workspace.to_owned() });
        }
    });
    agents
}

/// 各窗口里的终端占着的会话。
fn held_sessions(cx: &App) -> HashSet<SessionId> {
    let mut held = HashSet::new();
    for_each_view(cx, |view, _| {
        held.extend(view.session_id());
    });
    held
}

/// 确认框里列出的 agent：窗口里的写成「名字（workspace）」，同一个 workspace 里同种 agent 只列
/// 一次；没在窗口里的写成「名字（后台）」，同种只列一次。
fn agent_names(shown: &[ShownAgent], hidden: &[AgentKind]) -> Vec<String> {
    let all = shown
        .iter()
        .map(|agent| (agent.kind, Some(agent.workspace.as_str())))
        .chain(hidden.iter().map(|kind| (*kind, None)));
    let mut seen: Vec<(AgentKind, Option<&str>)> = Vec::new();
    for agent in all {
        if !seen.contains(&agent) {
            seen.push(agent);
        }
    }
    seen.into_iter()
        .map(|(kind, workspace)| match workspace {
            Some(workspace) => {
                rust_i18n::t!("quit.agent", agent = kind.display_name(), workspace = workspace).into_owned()
            }
            None => rust_i18n::t!("quit.background_agent", agent = kind.display_name()).into_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN_PROCESS: Mode = Mode::InProcess;
    const KEEPING: Mode = Mode::Standalone { end_on_quit: false };
    const LEFTOVER: Mode = Mode::Standalone { end_on_quit: true };

    const QUITTING: [QuitAction; 3] = [QuitAction::Quit, QuitAction::CloseLastWindow, QuitAction::CloseAllWindows];

    fn plan(ending: Ending, prompt: Option<Prompt>) -> Plan {
        Plan { ending, prompt }
    }

    /// 开关开着：退出、关最后一个窗口、关所有窗口都把会话留在宿主里，不问。
    #[test]
    fn a_standalone_host_keeps_sessions_on_quit() {
        for action in QUITTING {
            assert_eq!(quit_plan(KEEPING, action, 0), plan(Ending::Keep, None), "{action:?}");
            assert_eq!(quit_plan(KEEPING, action, 3), plan(Ending::Keep, None), "{action:?}");
        }
    }

    /// 开关开着时「退出并结束所有会话」让宿主连会话一起退出，有 agent 才问。
    #[test]
    fn ending_sessions_shuts_the_host_down() {
        assert_eq!(quit_plan(KEEPING, QuitAction::QuitAndEndSessions, 0), plan(Ending::ShutdownHost, None));
        assert_eq!(
            quit_plan(KEEPING, QuitAction::QuitAndEndSessions, 2),
            plan(Ending::ShutdownHost, Some(Prompt::EndAll))
        );
    }

    /// 接回上次留下的会话（开关已经关了）：退出时让宿主连会话一起退出，有 agent 才问。
    #[test]
    fn a_leftover_host_is_shut_down_on_quit() {
        for action in QUITTING {
            assert_eq!(quit_plan(LEFTOVER, action, 0), plan(Ending::ShutdownHost, None), "{action:?}");
            assert_eq!(
                quit_plan(LEFTOVER, action, 1),
                plan(Ending::ShutdownHost, Some(Prompt::QuitEndingAll)),
                "{action:?}"
            );
        }
        // 菜单里没有这一项，万一触发了也和退出一样结束会话。
        assert_eq!(quit_plan(LEFTOVER, QuitAction::QuitAndEndSessions, 0), plan(Ending::ShutdownHost, None));
        assert_eq!(
            quit_plan(LEFTOVER, QuitAction::QuitAndEndSessions, 1),
            plan(Ending::ShutdownHost, Some(Prompt::EndAll))
        );
    }

    /// 开关关着：会话随 app 退出结束，有 agent 时先问，和以前一样。
    #[test]
    fn an_in_process_host_ends_sessions_with_the_app() {
        for action in QUITTING.into_iter().chain([QuitAction::QuitAndEndSessions]) {
            assert_eq!(quit_plan(IN_PROCESS, action, 0), plan(Ending::WithApp, None), "{action:?}");
            assert_eq!(quit_plan(IN_PROCESS, action, 2), plan(Ending::WithApp, Some(Prompt::Quit)), "{action:?}");
        }
    }

    /// 只有退出会把会话留下时菜单里才有「退出并结束所有会话」。
    #[test]
    fn the_end_sessions_item_is_only_offered_when_quitting_keeps_sessions() {
        assert!(offers_end_sessions(KEEPING));
        assert!(!offers_end_sessions(LEFTOVER));
        assert!(!offers_end_sessions(IN_PROCESS));
    }

    #[test]
    fn only_live_agents_out_of_windows_count() {
        use runode_shared_types::{
            agent::{Agent, AgentState},
            grid::GridSize,
            session::SessionMeta,
        };

        let session = |id: u128, kind: Option<AgentKind>, exited: bool| SessionInfo {
            id: SessionId(id),
            size: GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 },
            meta: SessionMeta {
                agent: kind.map(|kind| Agent { kind, state: AgentState::Working }),
                ..SessionMeta::default()
            },
            clients: 0,
            claimed: false,
            exited,
            size_owner: None,
        };
        let sessions = [
            session(1, Some(AgentKind::Claude), false),
            session(2, Some(AgentKind::Codex), false),
            session(3, Some(AgentKind::Claude), true),
            session(4, Some(AgentKind::Other), false),
            session(5, None, false),
            session(6, Some(AgentKind::Codex), false),
        ];
        // 窗口里的终端占着 2，它已经按窗口里的算过了。
        let held = HashSet::from([SessionId(2)]);
        assert_eq!(hidden_agents(&sessions, &held), [AgentKind::Claude, AgentKind::Codex]);
        assert_eq!(hidden_agents(&sessions, &HashSet::new()).len(), 3);
    }
}
