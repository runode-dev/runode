//! 退出应用、关掉窗口时宿主里的会话怎么办，以及要结束正在干活的 agent 之前先问一句。
//!
//! 会话留下还是结束由宿主怎么跑（`host_client::Mode`）、配置项 `terminal-host`（见
//! `host_client::quit_keeps_sessions`）和用户做了什么（`QuitAction`）决定，决策在 `quit_plan`。
//! 要结束会话、又有 agent 在跑时才弹框确认；结束会话之前先冻结存档（`persist::freeze`），视图随后
//! 关掉分屏时才不会把存档清空。
//!
//! 关掉的窗口不是最后一个时，app 不退出，结束这个窗口里的会话（`WindowView::end_sessions`），不问。
//! 不管关的是哪个窗口、退不退出，要关掉的窗口里预览栏有没存的修改时都先问要不要放弃（`discard_unsaved`）。
//! 弹框之前都先推迟到当前的更新结束：回调运行时触发它的窗口正被借出，读不到它里面的 agent，也没法
//! 在它上面弹框。
//!
//! 从 Dock 退出、注销、关机这类系统发起的退出拦不下也弹不了框：`install` 装的观察者在这次退出没经过
//! `run` 时只做 `unprompted` 定下的那部分，有 agent 在跑、有没存的修改也不问。

use std::{collections::HashSet, time::Duration};

use gpui::{AnyWindowHandle, App, Global, PromptLevel, Window};
use runode_protocol::{ClientMsg, SessionId, SessionInfo};
use runode_types::agent::AgentKind;

use super::{WindowView, model::Closing, persist, remote};
use crate::{
    host_client::{self, Mode},
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
    /// 重启以装上下好的新版本（`crate::update`）。
    Update,
}

/// 退出时宿主里的会话怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// 留在单独跑的宿主里，下次启动接回来。
    Keep,
    /// 宿主跑在 app 里，随 app 退出一起结束。
    WithApp,
    /// 先让单独跑的宿主连会话一起退出（`ClientMsg::Shutdown`），再退出。
    ShutdownHost,
    /// 单独跑的宿主还连着别的桌面：只结束这个 app 窗口里的会话（`ClientMsg::Kill`），宿主和别的会话
    /// 留给那个桌面，见 `host_client::other_desktops`。
    EndOwn,
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
    /// 框里有「留在后台并退出」（默认的按钮）：开关关着、会话本来要结束，但留得下。
    pub(super) offer_keep: bool,
}

/// 重启以更新时会话留得下：Windows 上不交接会话，安装程序还要换掉单独跑的宿主正用着的 runode.exe，
/// 所以宿主连会话一起退出。
const UPDATE_KEEPS_SESSIONS: bool = cfg!(not(windows));

/// `action` 会结束宿主里所有的会话，连别的桌面的也算：「退出并结束所有会话」，以及留不下会话的重启以更新。
fn ends_all(action: QuitAction) -> bool {
    match action {
        QuitAction::QuitAndEndSessions => true,
        QuitAction::Update => !UPDATE_KEEPS_SESSIONS,
        QuitAction::Quit | QuitAction::CloseLastWindow | QuitAction::CloseAllWindows => false,
    }
}

/// `action` 让 app 退出时会话怎么办；`keeping` 是配置项 `terminal-host` 开着，见
/// `host_client::quit_keeps_sessions`；`shared` 是宿主还连着别的桌面，见 `host_client::other_desktops`。
/// `ends_all` 的那些照样让宿主连所有会话一起退出。
pub(super) fn ending(mode: Mode, keeping: bool, action: QuitAction, shared: bool) -> Ending {
    let keep = match action {
        QuitAction::QuitAndEndSessions => false,
        QuitAction::Update => UPDATE_KEEPS_SESSIONS,
        QuitAction::Quit | QuitAction::CloseLastWindow | QuitAction::CloseAllWindows => keeping,
    };
    match mode {
        Mode::InProcess => Ending::WithApp,
        Mode::Standalone if keep => Ending::Keep,
        Mode::Standalone if shared && !ends_all(action) => Ending::EndOwn,
        Mode::Standalone => Ending::ShutdownHost,
    }
}

/// `action` 让 app 退出时怎么做；`agents` 是退出会结束掉的会话里有几个 agent 在跑（让宿主连会话一起
/// 退出时连没在窗口里显示的会话也算）。会话留下时不问，会结束会话又有 agent 时先确认；不是「退出并
/// 结束所有会话」、会话又留得下时，框里给一个留下的选项。
pub(super) fn quit_plan(mode: Mode, keeping: bool, action: QuitAction, shared: bool, agents: usize) -> Plan {
    let ending = ending(mode, keeping, action, shared);
    let prompt = match ending {
        _ if agents == 0 => None,
        Ending::Keep => None,
        Ending::WithApp | Ending::EndOwn => Some(Prompt::Quit),
        Ending::ShutdownHost if action == QuitAction::QuitAndEndSessions => Some(Prompt::EndAll),
        Ending::ShutdownHost => Some(Prompt::QuitEndingAll),
    };
    let offer_keep =
        matches!(prompt, Some(Prompt::Quit | Prompt::QuitEndingAll)) && !ends_all(action) && mode == Mode::Standalone;
    Plan { ending, prompt, offer_keep }
}

/// 菜单里有没有「退出并结束所有会话」：只在退出会把会话留下时才有，否则和退出一样。
pub(super) fn offers_end_sessions(mode: Mode, keeping: bool) -> bool {
    ending(mode, keeping, QuitAction::Quit, false) == Ending::Keep
}

/// 菜单里要不要放「退出并结束所有会话」，见 `offers_end_sessions`。
pub fn end_sessions_in_menu() -> bool {
    offers_end_sessions(host_client::mode(), host_client::quit_keeps_sessions())
}

/// 退出确认框开着（或者正问宿主有几个 agent）；这时再按退出或者关窗口不再弹第二个。
#[derive(Default)]
struct Prompting(bool);

impl Global for Prompting {}

fn prompting(cx: &App) -> bool {
    cx.try_global::<Prompting>().is_some_and(|prompting| prompting.0)
}

/// 这次退出经过了 `run`，会话怎么办已经办了：系统发起的退出的收尾（`install`）不再做一遍。经过了
/// `run` 却没退成（重启以更新拉不起新进程）时复位，见 `quit_to_update`。
#[derive(Default)]
struct Settled(bool);

impl Global for Settled {}

/// 系统发起的退出要不要做 `install` 的收尾：这次退出没经过 `run`，或者经过了却没退成、已经复位。
fn needs_ending(settled: Option<&Settled>) -> bool {
    !settled.is_some_and(|settled| settled.0)
}

/// 装上系统发起的退出（从 Dock 退出、注销、关机）的收尾，见模块说明。要在打开窗口之前调用。
pub(super) fn install(cx: &mut App) {
    cx.on_app_quit(|cx| {
        if needs_ending(cx.try_global::<Settled>()) {
            end_unprompted(cx);
        }
        async {}
    })
    .detach();
}

/// 没经过 `run` 的退出：同步做 `unprompted` 定下的那部分。
fn end_unprompted(cx: &mut App) {
    let mode = host_client::mode();
    let keeping = host_client::quit_keeps_sessions();
    // 不用结束会话时不去问宿主连着几个桌面：问要在主线程上等，最多 `launch::probe` 的超时。
    if ending(mode, keeping, QuitAction::Quit, false) != Ending::ShutdownHost {
        return;
    }
    let Some(ending) = unprompted(mode, keeping, host_client::other_desktops()) else {
        return;
    };
    tracing::info!("quitting without going through the quit menu: {ending:?}");
    // 同 `run`：会话结束之前定下存档。
    persist::freeze(cx);
    match ending {
        Ending::EndOwn => end_own(cx),
        _ => shutdown_host(),
    }
}

/// 弹不了框的退出里要做的事：按退出（`QuitAction::Quit`）的决策，不留会话、宿主单独跑时让它连会话
/// 一起退出（`Ending::ShutdownHost`），还连着别的桌面时只结束窗口里的（`Ending::EndOwn`）；会话留在
/// 单独跑的宿主里、或者随 app 结束时什么都不用做，为 `None`。
fn unprompted(mode: Mode, keeping: bool, shared: bool) -> Option<Ending> {
    match ending(mode, keeping, QuitAction::Quit, shared) {
        ending @ (Ending::ShutdownHost | Ending::EndOwn) => Some(ending),
        Ending::Keep | Ending::WithApp => None,
    }
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

/// 退出应用，装上下好的新版本后重新打开（`crate::update::restart`）。会话留得下就留在后台，
/// 不看配置项 `terminal-host`；留不下又有 agent 在跑时先确认，取消了就不重启。
pub fn quit_to_update(cx: &mut App) {
    cx.defer(|cx| {
        let window = front_window(cx);
        run(QuitAction::Update, window, cx, |cx| {
            crate::update::restart(cx);
            // 退出已经排进主线程的队列，这个任务排在它后面；轮到了就是没退成（GPUI 拉不起重启的
            // 脚本），复位 `Settled` 让之后从 Dock 退出照常收尾。冻结了的存档不解冻：会话已经交出或
            // 结束，解冻会拿缺了终端的布局盖掉要用的那份。
            cx.spawn(async |cx| cx.update(|cx| cx.set_global(Settled(false)))).detach();
        });
    });
}

/// 关掉 `window`。还有别的窗口时结束它里面的会话；它是最后一个窗口时应用会跟着退出，这时跟
/// 退出一样。
pub fn close_window(window: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| {
        let remove = move |cx: &mut App| {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        };
        // 不是终端窗口的，直接关。
        let Some(view) = window.downcast::<WindowView>() else {
            remove(cx);
            return;
        };
        let windows = terminal_windows(cx);
        if windows > 1 {
            discard_unsaved(vec![window], Some(window), cx, move |cx| {
                view.update(cx, |view, _, cx| view.end_sessions(Closing::Window { windows }, cx)).ok();
                remove(cx);
            });
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

/// 点了窗口的关闭按钮：不是最后一个窗口、预览栏里也没有没存的修改时结束它里面的会话、照常关；否则先不关，
/// 交给 `close_window` 问过再关。
pub fn should_close(window: &mut Window, cx: &mut App) -> bool {
    let Some(Some(view)) = window.root::<WindowView>() else {
        return true;
    };
    let windows = terminal_windows(cx);
    if windows > 1 && view.read(cx).preview.read(cx).unsaved(None) == 0 {
        view.update(cx, |view, cx| view.end_sessions(Closing::Window { windows }, cx));
        return true;
    }
    close_window(window.window_handle(), cx);
    false
}

/// 开着的终端窗口有几个。
pub fn terminal_windows(cx: &App) -> usize {
    remote::windows(cx).len()
}

/// 确认框弹在当前窗口上；当前没有窗口在前台时弹在第一个窗口上。
fn front_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.active_window().or_else(|| cx.windows().into_iter().next())
}

/// 预览栏里有没存的修改时先问要不要放弃（`discard_unsaved`），再按 `quit_plan` 做 `action`：该问就在 `window`
/// 上问，确认了（或者不用问）再做 `then`，要让宿主连会话一起退出时先让它退出。
fn run(action: QuitAction, window: Option<AnyWindowHandle>, cx: &mut App, then: impl FnOnce(&mut App) + 'static) {
    if prompting(cx) {
        return;
    }
    discard_unsaved(cx.windows(), window, cx, move |cx| end_sessions_then(action, window, cx, then));
}

/// `windows` 的预览栏里有没存的修改时，在 `window` 上问要不要放弃；放弃了先丢掉修改再做 `then`，取消了
/// 什么都不做。没有要问的或没有窗口能弹框时直接做 `then`。
fn discard_unsaved(
    windows: Vec<AnyWindowHandle>,
    window: Option<AnyWindowHandle>,
    cx: &mut App,
    then: impl FnOnce(&mut App) + 'static,
) {
    if prompting(cx) {
        return;
    }
    let views: Vec<_> = windows.into_iter().filter_map(|window| window.downcast::<WindowView>()).collect();
    let unsaved: usize =
        views.iter().filter_map(|view| view.read_with(cx, |view, cx| view.preview.read(cx).unsaved(None)).ok()).sum();
    let answer = window.filter(|_| unsaved > 0).and_then(|window| {
        window
            .update(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    &rust_i18n::t!("preview.unsaved_title", count = unsaved),
                    Some(&rust_i18n::t!("preview.unsaved_detail")),
                    &[&*rust_i18n::t!("preview.discard_edits"), &*rust_i18n::t!("quit.cancel")],
                    cx,
                )
            })
            .ok()
    });
    let Some(answer) = answer else {
        then(cx);
        return;
    };
    cx.set_global(Prompting(true));
    cx.spawn(async move |cx| {
        let discard = answer.await.ok() == Some(0);
        cx.update(|cx| {
            cx.set_global(Prompting(false));
            if !discard {
                return;
            }
            for view in views {
                view.update(cx, |view, _, cx| view.preview.update(cx, |panel, cx| panel.discard_edits(None, cx))).ok();
            }
            then(cx);
        });
    })
    .detach();
}

/// 按 `quit_plan` 结束会话、做 `then`，见 `run`。
fn end_sessions_then(
    action: QuitAction,
    window: Option<AnyWindowHandle>,
    cx: &mut App,
    then: impl FnOnce(&mut App) + 'static,
) {
    // 走到 `then` 时会话怎么办已经办了，接着的退出不用 `install` 的收尾再办。
    let then = move |cx: &mut App| {
        cx.set_global(Settled(true));
        then(cx);
    };
    let mode = host_client::mode();
    let keeping = host_client::quit_keeps_sessions();
    if ending(mode, keeping, action, false) == Ending::Keep {
        then(cx);
        return;
    }
    // 没被窗口里的终端占着的会话（丢掉视图后留在宿主里的、后台会话）也会被结束，里面的 agent
    // 也要算，得问宿主；还得问宿主还连着别的桌面没有。问它最多要等上 `host_client::list_sessions`
    // 的超时，放到后台线程，期间不再接退出。问不到时只数窗口里的。
    cx.set_global(Prompting(true));
    let asked = cx.background_executor().spawn(async { (host_client::list_sessions(), host_client::other_desktops()) });
    cx.spawn(async move |cx| {
        let (sessions, shared) = asked.await;
        cx.update(|cx| {
            cx.set_global(Prompting(false));
            let sessions = sessions
                .inspect_err(|err| {
                    tracing::warn!("failed to list the host's sessions, counting the agents in windows: {err:#}");
                })
                .unwrap_or_default();
            let shown = window_agents(cx);
            let held = held_sessions(cx);
            // 只结束窗口里的会话时，后台的不算。
            let hidden = match ending(mode, keeping, action, shared) {
                Ending::EndOwn => Vec::new(),
                _ => hidden_agents(&sessions, &held),
            };
            let count = shown.len() + hidden.len();
            let plan = quit_plan(mode, keeping, action, shared, count);
            let text =
                plan.prompt.map(|prompt| prompt_text(prompt, &agent_names(&shown, &hidden), count, plan.offer_keep));
            confirm(window, text, cx, move |answer, cx| match (answer, plan.ending) {
                (Answer::Keep, _) => {
                    keep_from_now_on(cx);
                    then(cx);
                }
                (Answer::End, Ending::ShutdownHost) => {
                    // 宿主连会话一起退出后，视图会先收到会话结束、关掉分屏，存档要在那之前定下来，
                    // 下次启动才能照原样在原目录新开。
                    persist::freeze(cx);
                    shutdown_host();
                    then(cx);
                }
                (Answer::End, Ending::EndOwn) => {
                    // 同上：会话结束之前定下存档。
                    persist::freeze(cx);
                    end_own(cx);
                    then(cx);
                }
                (Answer::End, _) => then(cx),
            });
        });
    })
    .detach();
}

/// 在退出确认框里选了「留在后台并退出」：打开配置项 `terminal-host`，以后退出都留下会话。写不了
/// 配置文件时只记一笔，这次照样留下。
fn keep_from_now_on(cx: &mut App) {
    if let Err(err) = crate::config::set("terminal-host", "true", cx) {
        tracing::warn!("failed to turn terminal-host on: {err:#}");
    }
}

/// 让单独跑的宿主连会话一起退出，等它读完（最多 `SHUTDOWN_FLUSH`）。
fn shutdown_host() {
    let link = host_client::link();
    link.send(ClientMsg::Shutdown { kill_sessions: true });
    if !link.flush(SHUTDOWN_FLUSH) {
        tracing::warn!("the host did not take the shutdown in time");
    }
}

/// 结束这个 app 窗口里的会话，等宿主读完（最多 `SHUTDOWN_FLUSH`），见 `Ending::EndOwn`。
fn end_own(cx: &App) {
    let link = host_client::link();
    for id in held_sessions(cx) {
        link.kill(id);
    }
    if !link.flush(SHUTDOWN_FLUSH) {
        tracing::warn!("the host did not take the kills in time");
    }
}

/// 确认框的标题、说明和按钮。
struct PromptText {
    title: String,
    detail: String,
    /// 「留在后台并退出」，有时是默认的（第一个）按钮。
    keep: Option<String>,
    /// 结束会话、退出的按钮。
    confirm: String,
}

/// 确认框里选了什么，取消不算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// 「留在后台并退出」。
    Keep,
    /// 结束会话、退出；没问就退出时也是它。
    End,
}

/// `names` 是 `Prompt::Quit` 要列出的 agent，`count` 是别的几种给出的个数。`offer_keep` 时多一个
/// 「留在后台并退出」，标题改成问要不要留下。
fn prompt_text(prompt: Prompt, names: &[String], count: usize, offer_keep: bool) -> PromptText {
    let t = |key: &str| rust_i18n::t!(key).into_owned();
    let agents = names.join(rust_i18n::t!("quit.separator").as_ref());
    if offer_keep {
        let running = match prompt {
            Prompt::Quit => rust_i18n::t!("quit.keep_running", agents = agents),
            Prompt::QuitEndingAll | Prompt::EndAll => rust_i18n::t!("quit.keep_running_count", count = count),
        };
        return PromptText {
            title: t("quit.keep_title"),
            detail: format!("{running}\n\n{}", t("quit.keep_detail")),
            keep: Some(t("quit.keep")),
            confirm: t("quit.end_confirm"),
        };
    }
    match prompt {
        Prompt::Quit => PromptText {
            title: t("quit.title"),
            detail: rust_i18n::t!("quit.detail", agents = agents).into_owned(),
            keep: None,
            confirm: t("quit.confirm"),
        },
        Prompt::QuitEndingAll => PromptText {
            title: t("quit.title"),
            detail: rust_i18n::t!("quit.end_detail", count = count).into_owned(),
            keep: None,
            confirm: t("quit.confirm"),
        },
        Prompt::EndAll => PromptText {
            title: t("quit.end_title"),
            detail: rust_i18n::t!("quit.end_detail", count = count).into_owned(),
            keep: None,
            confirm: t("quit.end_confirm"),
        },
    }
}

/// 没有要问的（`text` 为空）或者没有窗口能弹框时直接做 `then`（`Answer::End`）；否则在 `window`
/// 上弹框，选了保留或者结束再做，取消了什么都不做。确认框已经开着时什么都不做。
fn confirm(
    window: Option<AnyWindowHandle>,
    text: Option<PromptText>,
    cx: &mut App,
    then: impl FnOnce(Answer, &mut App) + 'static,
) {
    if prompting(cx) {
        return;
    }
    let (Some(window), Some(text)) = (window, text) else {
        then(Answer::End, cx);
        return;
    };
    let cancel = rust_i18n::t!("quit.cancel");
    // 有「留在后台并退出」时它在第一个，是默认的按钮。
    let answers: Vec<(&str, Option<Answer>)> = text
        .keep
        .as_deref()
        .map(|keep| (keep, Some(Answer::Keep)))
        .into_iter()
        .chain([(&*text.confirm, Some(Answer::End)), (&*cancel, None)])
        .collect();
    let labels: Vec<&str> = answers.iter().map(|(label, _)| *label).collect();
    let answer = window
        .update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &text.title, Some(&text.detail), &labels, cx));
    let Ok(answer) = answer else {
        return;
    };
    let choices: Vec<Option<Answer>> = answers.iter().map(|(_, choice)| *choice).collect();
    cx.set_global(Prompting(true));
    cx.spawn(async move |cx| {
        let chosen = answer.await.ok().and_then(|index| choices.get(index).copied().flatten());
        cx.update(|cx| {
            cx.set_global(Prompting(false));
            if let Some(chosen) = chosen {
                then(chosen, cx);
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
        .filter(|kind| kind.is_known())
        .collect()
}

/// 窗口里一个前台在跑 agent 的终端。
struct ShownAgent {
    kind: AgentKind,
    /// 所在 workspace 的名字。
    workspace: String,
}

/// 各窗口里的终端，对每个终端调 `f`（带上所在 workspace 的名字）；`skip_emptied` 时跳过 workspace 都关掉了
/// 的窗口（`WindowView::emptied`）。
fn for_each_view(cx: &App, skip_emptied: bool, mut f: impl FnMut(&TerminalView, &str)) {
    let windows = remote::windows(cx).into_iter().filter_map(|window| window.read(cx).ok());
    for view in windows.filter(|view| !(skip_emptied && view.emptied)) {
        for workspace in &view.workspaces {
            for tab in &workspace.tabs {
                for (view, _) in tab.panes.values() {
                    f(view.read(cx), &workspace.name);
                }
            }
        }
    }
}

/// 各窗口里前台在跑 agent 的终端，每个一项。关掉了最后一个 workspace 的窗口不算：那些终端关的时候问过、
/// 已经结束了，等着退出的这会儿视图还在。
fn window_agents(cx: &App) -> Vec<ShownAgent> {
    let mut agents = Vec::new();
    for_each_view(cx, true, |view, workspace| {
        if let Some(kind) = view.agent().map(|agent| agent.kind).filter(|kind| kind.is_known()) {
            agents.push(ShownAgent { kind, workspace: workspace.to_owned() });
        }
    });
    agents
}

/// 各窗口里的终端占着的会话。
pub(super) fn held_sessions(cx: &App) -> HashSet<SessionId> {
    let mut held = HashSet::new();
    // 结束了的会话宿主可能还没收掉，照样算占着，免得当成后台会话再问一遍。
    for_each_view(cx, false, |view, _| {
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
    const STANDALONE: Mode = Mode::Standalone;

    /// 开关开着。
    const KEEP: bool = true;
    /// 开关关着。
    const END: bool = false;

    const QUITTING: [QuitAction; 3] = [QuitAction::Quit, QuitAction::CloseLastWindow, QuitAction::CloseAllWindows];

    fn plan(ending: Ending, prompt: Option<Prompt>, offer_keep: bool) -> Plan {
        Plan { ending, prompt, offer_keep }
    }

    /// 系统发起的退出只在没经过 `run`（或者经过了却没退成、已复位）时收尾。
    #[test]
    fn a_system_quit_ends_sessions_unless_already_settled() {
        assert!(needs_ending(None));
        assert!(!needs_ending(Some(&Settled(true))));
        assert!(needs_ending(Some(&Settled(false))));
    }

    /// 开关开着、宿主单独跑：退出、关最后一个窗口、关所有窗口都把会话留在宿主里，不问。
    #[test]
    fn a_standalone_host_keeps_sessions_on_quit() {
        for action in QUITTING {
            assert_eq!(quit_plan(STANDALONE, KEEP, action, false, 0), plan(Ending::Keep, None, false), "{action:?}");
            assert_eq!(quit_plan(STANDALONE, KEEP, action, false, 3), plan(Ending::Keep, None, false), "{action:?}");
        }
    }

    /// 「退出并结束所有会话」不管开关都结束会话，有 agent 才问，不给留下的选项。
    #[test]
    fn ending_sessions_ends_them_whatever_the_switch() {
        let action = QuitAction::QuitAndEndSessions;
        assert_eq!(quit_plan(STANDALONE, KEEP, action, false, 0), plan(Ending::ShutdownHost, None, false));
        assert_eq!(
            quit_plan(STANDALONE, KEEP, action, false, 2),
            plan(Ending::ShutdownHost, Some(Prompt::EndAll), false)
        );
        assert_eq!(quit_plan(IN_PROCESS, KEEP, action, false, 0), plan(Ending::WithApp, None, false));
        assert_eq!(quit_plan(IN_PROCESS, KEEP, action, false, 2), plan(Ending::WithApp, Some(Prompt::Quit), false));
    }

    /// 开关关着、宿主单独跑：退出时让宿主连会话一起退出，有 agent 才问，框里可以选留下。
    #[test]
    fn a_standalone_host_is_shut_down_when_not_keeping() {
        for action in QUITTING {
            assert_eq!(
                quit_plan(STANDALONE, END, action, false, 0),
                plan(Ending::ShutdownHost, None, false),
                "{action:?}"
            );
            assert_eq!(
                quit_plan(STANDALONE, END, action, false, 1),
                plan(Ending::ShutdownHost, Some(Prompt::QuitEndingAll), true),
                "{action:?}"
            );
        }
    }

    /// 开关关着、宿主还连着别的桌面：只结束窗口里的会话，有 agent 才问，框里可以选留下；「退出并结束
    /// 所有会话」照样让宿主连所有会话一起退出，重启以更新照样留下。
    #[test]
    fn a_shared_host_only_loses_this_apps_sessions() {
        for action in QUITTING {
            assert_eq!(quit_plan(STANDALONE, END, action, true, 0), plan(Ending::EndOwn, None, false), "{action:?}");
            assert_eq!(quit_plan(STANDALONE, END, action, true, 1), plan(Ending::EndOwn, Some(Prompt::Quit), true));
            assert_eq!(quit_plan(STANDALONE, KEEP, action, true, 1), plan(Ending::Keep, None, false));
        }
        let all = QuitAction::QuitAndEndSessions;
        assert_eq!(quit_plan(STANDALONE, END, all, true, 1), plan(Ending::ShutdownHost, Some(Prompt::EndAll), false));
        if UPDATE_KEEPS_SESSIONS {
            assert_eq!(quit_plan(STANDALONE, END, QuitAction::Update, true, 1), plan(Ending::Keep, None, false));
        }
    }

    /// 宿主跑在 app 里：不管开关，会话都随 app 退出结束，有 agent 时先问，不给留下的选项。
    #[test]
    fn an_in_process_host_ends_sessions_with_the_app() {
        for keeping in [KEEP, END] {
            for action in QUITTING {
                assert_eq!(
                    quit_plan(IN_PROCESS, keeping, action, false, 0),
                    plan(Ending::WithApp, None, false),
                    "{action:?}"
                );
                assert_eq!(
                    quit_plan(IN_PROCESS, keeping, action, false, 2),
                    plan(Ending::WithApp, Some(Prompt::Quit), false)
                );
            }
        }
    }

    /// 重启以更新不看开关：宿主单独跑就留下，不问；跑在 app 里时随 app 结束，有 agent 才问，不给留下
    /// 的选项。
    #[cfg(not(windows))]
    #[test]
    fn updating_keeps_sessions_whenever_it_can() {
        let action = QuitAction::Update;
        assert_eq!(quit_plan(STANDALONE, END, action, false, 2), plan(Ending::Keep, None, false));
        assert_eq!(quit_plan(STANDALONE, KEEP, action, false, 2), plan(Ending::Keep, None, false));
        assert_eq!(quit_plan(IN_PROCESS, KEEP, action, false, 0), plan(Ending::WithApp, None, false));
        assert_eq!(quit_plan(IN_PROCESS, END, action, false, 1), plan(Ending::WithApp, Some(Prompt::Quit), false));
    }

    /// Windows 上重启以更新留不下会话：不看开关，宿主连所有会话一起退出（连着别的桌面也一样），有 agent
    /// 时先问，不给留下的选项。
    #[cfg(windows)]
    #[test]
    fn updating_on_windows_ends_every_session() {
        let action = QuitAction::Update;
        for keeping in [KEEP, END] {
            for shared in [false, true] {
                assert_eq!(quit_plan(STANDALONE, keeping, action, shared, 0), plan(Ending::ShutdownHost, None, false));
                assert_eq!(
                    quit_plan(STANDALONE, keeping, action, shared, 2),
                    plan(Ending::ShutdownHost, Some(Prompt::QuitEndingAll), false)
                );
            }
        }
        assert_eq!(quit_plan(IN_PROCESS, KEEP, action, false, 0), plan(Ending::WithApp, None, false));
    }

    /// 系统发起的退出弹不了框：单独跑的宿主不留会话时照样让它退出（还连着别的桌面时只结束窗口里的），
    /// 有没有 agent 都一样；会话留在单独跑的宿主里、随 app 结束时什么都不做。
    #[test]
    fn a_system_quit_does_what_needs_no_prompt() {
        assert_eq!(unprompted(STANDALONE, END, false), Some(Ending::ShutdownHost));
        assert_eq!(unprompted(STANDALONE, END, true), Some(Ending::EndOwn));
        assert_eq!(unprompted(STANDALONE, KEEP, true), None);
        assert_eq!(unprompted(IN_PROCESS, KEEP, false), None);
        assert_eq!(unprompted(IN_PROCESS, END, true), None);
    }

    /// 只有退出会把会话留下时菜单里才有「退出并结束所有会话」，跟着开关变。
    #[test]
    fn the_end_sessions_item_is_only_offered_when_quitting_keeps_sessions() {
        assert!(offers_end_sessions(STANDALONE, KEEP));
        assert!(!offers_end_sessions(STANDALONE, END));
        assert!(!offers_end_sessions(IN_PROCESS, KEEP));
        assert!(!offers_end_sessions(IN_PROCESS, END));
    }

    /// 给留下的选项时它是第一个按钮，标题改成问要不要留下，说明里照样列出 agent 但不说会随之结束，
    /// 结束的按钮写明会结束会话。
    #[test]
    fn the_keep_button_comes_first_when_offered() {
        let names = ["Claude".to_owned()];
        let offered = prompt_text(Prompt::Quit, &names, 1, true);
        let plain = prompt_text(Prompt::Quit, &names, 1, false);
        assert!(offered.keep.is_some());
        assert!(plain.keep.is_none());
        assert_ne!(offered.title, plain.title);
        assert_ne!(offered.confirm, plain.confirm);
        assert!(offered.detail.contains("Claude") && !offered.detail.contains(&plain.detail));
    }

    #[test]
    fn only_live_agents_out_of_windows_count() {
        use runode_types::{
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
