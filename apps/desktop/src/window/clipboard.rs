//! 终端里的程序读写系统剪贴板（OSC 52）：宿主认出请求、按配置定了办不办，包成
//! `ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard` 经 `HostMsg::UiRequest` 转过来（见
//! `remote`），这里在主线程上写系统剪贴板；读之前按宿主的意思先问用户，问在显示那个终端的窗口上，
//! 说清是哪个终端、哪个程序要读。日志里不记剪贴板的内容。

use gpui::{App, ClipboardItem, PromptLevel, WindowHandle};
use runode_protocol::{HostMsg, SessionId};
use runode_shared_types::clipboard::MAX_CLIPBOARD_BYTES;

use super::{
    WindowView,
    remote::{find_session, front_window},
};
use crate::host_client::{self, UiTicket};

/// 程序要写剪贴板：照写，回 `Done`（`req` 为 0，见 `ClientMsg::WriteClipboard`）。
pub(super) fn write(text: String, cx: &mut App) -> HostMsg {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    HostMsg::Done { req: 0 }
}

/// 程序要读剪贴板，`ticket` 是宿主转来的请求的回执。不用问时当场读；要问时在显示这个会话的窗口上
/// （没有时在最前面的窗口上）弹框，用户点了允许才读。都用 `Link::ui_reply` 回 `ClipboardText`，
/// 不让读、没有窗口可问、剪贴板里没有文字时 `text` 为空。
pub(super) fn read(ticket: UiTicket, id: SessionId, ask: bool, program: Option<String>, cx: &mut App) {
    let reply = move |text: Option<String>| {
        host_client::link().ui_reply(ticket, HostMsg::ClipboardText { id, text: text.map(Into::into) });
    };
    if !ask {
        reply(clipboard_text(cx));
        return;
    }
    let target = match find_session(id, cx) {
        Some((window, _)) => Some(window),
        None => front_window(cx),
    };
    let Some(window) = target else {
        tracing::info!("no window to ask whether session {id} may read the clipboard");
        reply(None);
        return;
    };
    let terminal = terminal_title(window, id, cx);
    let title = rust_i18n::t!("clipboard.read_title");
    let detail = match program.filter(|program| !program.is_empty()) {
        Some(program) => rust_i18n::t!("clipboard.read_detail", terminal = terminal, program = program),
        None => rust_i18n::t!("clipboard.read_detail_unknown", terminal = terminal),
    };
    // 不允许排在前面：回车按的是它，免得用户正打着字时一个回车就把剪贴板交出去。
    let answers = [&*rust_i18n::t!("clipboard.deny"), &*rust_i18n::t!("clipboard.allow")];
    let answer =
        window.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(&detail), &answers, cx));
    let Ok(answer) = answer else {
        reply(None);
        return;
    };
    cx.spawn(async move |cx| {
        // 窗口关了、框没了时按不允许。
        let allowed = answer.await.ok() == Some(1);
        let text = if allowed { cx.update(clipboard_text) } else { None };
        reply(text);
    })
    .detach();
}

/// 剪贴板里的文字；没有文字或者超过 `MAX_CLIPBOARD_BYTES` 时为 `None`。
fn clipboard_text(cx: &mut App) -> Option<String> {
    let text = cx.read_from_clipboard().and_then(|item| item.text())?;
    if text.len() > MAX_CLIPBOARD_BYTES {
        tracing::info!("did not hand over {} bytes of clipboard text, over the limit", text.len());
        return None;
    }
    Some(text)
}

/// 问的时候怎么称呼这个终端：它所在分屏的标题；不在 `window` 里显示时用会话标识的开头。
fn terminal_title(window: WindowHandle<WindowView>, id: SessionId, cx: &App) -> String {
    let title = window.read(cx).ok().and_then(|view| {
        view.workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|(terminal, _)| terminal.read(cx))
            .find(|terminal| terminal.session_id() == Some(id))
            .map(|terminal| terminal.title().to_owned())
    });
    title.unwrap_or_else(|| id.to_string()[..8].to_owned())
}
