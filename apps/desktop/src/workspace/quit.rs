//! 退出应用，以及关掉最后一个窗口（关掉后应用跟着退出）：各窗口里还有 claude、codex 这类 agent
//! 在跑时先问一句，免得一按 cmd+q 或者点一下关闭按钮，把正在干活或者攒着上下文的会话一起结束掉。
//!
//! 弹框之前都先推迟到当前的更新结束：动作和关闭按钮的回调运行时，触发它的窗口正被借出，这时
//! 既读不到它里面的 agent，也没法在它上面弹框。

use gpui::{AnyWindowHandle, App, Global, PromptLevel, Window};
use runode_shared_types::agent::AgentKind;

use super::WindowView;

/// 退出确认框开着；这时再按退出或者关窗口不再弹第二个。
#[derive(Default)]
struct Prompting(bool);

impl Global for Prompting {}

fn prompting(cx: &App) -> bool {
    cx.try_global::<Prompting>().is_some_and(|prompting| prompting.0)
}

/// 退出应用；有 agent 在跑时先弹框确认，确认了才退出。
pub fn quit(cx: &mut App) {
    cx.defer(|cx| {
        // 弹在当前窗口上；当前没有窗口在前台时弹在第一个窗口上。
        let window = cx.active_window().or_else(|| cx.windows().into_iter().next());
        confirm(window, cx, |cx| cx.quit());
    });
}

/// 关掉 `window`。它是最后一个窗口时应用会跟着退出，这时跟退出一样，有 agent 在跑先确认。
pub fn close_window(window: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| {
        let remove = move |cx: &mut App| {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        };
        if cx.windows().len() > 1 {
            remove(cx);
        } else {
            confirm(Some(window), cx, remove);
        }
    });
}

/// 关掉所有窗口，应用跟着退出；有 agent 在跑时先确认。
pub fn close_all_windows(cx: &mut App) {
    cx.defer(|cx| {
        let window = cx.active_window().or_else(|| cx.windows().into_iter().next());
        confirm(window, cx, |cx| {
            for window in cx.windows() {
                window.update(cx, |_, window, _| window.remove_window()).ok();
            }
        });
    });
}

/// 点了窗口的关闭按钮：不是最后一个窗口时照常关；是最后一个时先不关，交给 `close_window` 确认后再关。
pub fn should_close(window: &mut Window, cx: &mut App) -> bool {
    if cx.windows().len() > 1 {
        return true;
    }
    close_window(window.window_handle(), cx);
    false
}

/// 没有 agent 在跑时直接做 `then`；有的话在 `window` 上弹框，确认了再做。确认框已经开着时
/// 什么都不做。
fn confirm(window: Option<AnyWindowHandle>, cx: &mut App, then: impl FnOnce(&mut App) + 'static) {
    if prompting(cx) {
        return;
    }
    let agents = running_agents(cx);
    let Some(window) = window.filter(|_| !agents.is_empty()) else {
        then(cx);
        return;
    };
    let title = rust_i18n::t!("quit.title");
    let detail = rust_i18n::t!("quit.detail", agents = agents.join(rust_i18n::t!("quit.separator").as_ref()));
    let answer = window.update(cx, |_, window, cx| {
        window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[&*rust_i18n::t!("quit.confirm"), &*rust_i18n::t!("quit.cancel")],
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

/// 各窗口里前台在跑的 agent，写成「名字（workspace）」；同一个 workspace 里同种 agent 只列一次。
/// 不算 `AgentKind::Other`：那是用 OSC 9;4 报进度的普通程序，不是 agent 会话。
fn running_agents(cx: &App) -> Vec<String> {
    let mut agents = Vec::new();
    for window in cx.windows() {
        let Some(view) = window.downcast::<WindowView>().and_then(|window| window.read(cx).ok()) else {
            continue;
        };
        for workspace in &view.workspaces {
            let mut kinds: Vec<AgentKind> = Vec::new();
            for tab in &workspace.tabs {
                for id in tab.root.leaves() {
                    let Some(agent) = tab.panes[&id].0.read(cx).agent() else {
                        continue;
                    };
                    if agent.kind != AgentKind::Other && !kinds.contains(&agent.kind) {
                        kinds.push(agent.kind);
                    }
                }
            }
            agents.extend(kinds.into_iter().map(|kind| {
                rust_i18n::t!("quit.agent", agent = kind.display_name(), workspace = workspace.name).into_owned()
            }));
        }
    }
    agents
}
