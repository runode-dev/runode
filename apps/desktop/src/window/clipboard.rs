//! 终端里的程序读写系统剪贴板（OSC 52）：宿主认出请求、按配置定了办不办，包成
//! `ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard` 经 `HostMsg::UiRequest` 转过来（见
//! `remote`），这里在主线程上写系统剪贴板；读之前按宿主的意思先问用户，问在显示那个终端的窗口上，
//! 说清是哪个终端、哪个程序要读。日志里不记剪贴板的内容。

use gpui::{App, ClipboardItem, EntityId, PromptLevel, WindowHandle};
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
    let (target, pane) = match find_session(id, cx) {
        Some((window, pane)) => (Some(window), Some(pane)),
        None => (front_window(cx), None),
    };
    let Some(window) = target else {
        tracing::info!("no window to ask whether session {id} may read the clipboard");
        reply(None);
        return;
    };
    let detail = read_detail(&terminal_title(window, pane, id, cx), program.as_deref());
    let title = rust_i18n::t!("clipboard.read_title");
    // 不允许排在前面（`ALLOW` 是第二个）：回车按的是它，免得用户正打着字时一个回车就把剪贴板交出去。
    let answers = [&*rust_i18n::t!("clipboard.deny"), &*rust_i18n::t!("clipboard.allow")];
    let answer =
        window.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(&detail), &answers, cx));
    let Ok(answer) = answer else {
        reply(None);
        return;
    };
    cx.spawn(async move |cx| {
        let text = if allowed(answer.await.ok()) { cx.update(clipboard_text) } else { None };
        reply(text);
    })
    .detach();
}

/// 询问框里「允许」是第几个按钮。
const ALLOW: usize = 1;

/// 用户在询问框里的回答让不让读：只有点了「允许」才让，窗口关了、框没了（`None`）时按不允许。
fn allowed(answer: Option<usize>) -> bool {
    answer == Some(ALLOW)
}

/// 询问框的说明：哪个终端（`terminal`）里的哪个程序要读。名字都是终端里的程序说了算（标题能用 OSC
/// 设），经 `shown` 去掉控制字符、截短，免得被拿来冒充别的说明或者把按钮挤出去；程序名没有或者
/// 清理后为空时不提程序。
fn read_detail(terminal: &str, program: Option<&str>) -> String {
    let terminal = shown(terminal);
    match program.map(shown).filter(|program| !program.is_empty()) {
        Some(program) => rust_i18n::t!("clipboard.read_detail", terminal = terminal, program = program),
        None => rust_i18n::t!("clipboard.read_detail_unknown", terminal = terminal),
    }
    .into_owned()
}

/// 剪贴板里的文字；没有文字或者超过 `MAX_CLIPBOARD_BYTES` 时为 `None`。
fn clipboard_text(cx: &mut App) -> Option<String> {
    within_limit(cx.read_from_clipboard().and_then(|item| item.text())?)
}

/// 交得出去的剪贴板文字：超过 `MAX_CLIPBOARD_BYTES` 时为 `None`。
fn within_limit(text: String) -> Option<String> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        tracing::info!("did not hand over {} bytes of clipboard text, over the limit", text.len());
        return None;
    }
    Some(text)
}

/// 询问框里的名字最多这么多个字符，再长截掉、末尾加省略号。
const SHOWN_CHARS: usize = 40;

/// 放进询问框的名字：去掉控制字符和改变文字方向、看不见的格式字符，空白压成一个空格，截到
/// `SHOWN_CHARS` 个字符。
fn shown(name: &str) -> String {
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    };
    let cleaned: String =
        name.chars().filter_map(|c| if c.is_whitespace() { Some(' ') } else { (!invisible(c)).then_some(c) }).collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let joined = words.join(" ");
    if joined.chars().count() <= SHOWN_CHARS {
        return joined;
    }
    let mut cut: String = joined.chars().take(SHOWN_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// 问的时候怎么称呼这个终端：`window` 里分屏 `pane` 的标题；没有这个分屏时用会话标识的开头。
fn terminal_title(window: WindowHandle<WindowView>, pane: Option<EntityId>, id: SessionId, cx: &App) -> String {
    let title = pane.and_then(|pane| {
        let view = window.read(cx).ok()?;
        let (wi, ti) = view.locate(pane)?;
        Some(view.workspaces[wi].tabs[ti].panes[&pane].0.read(cx).title().to_owned())
    });
    title.unwrap_or_else(|| id.to_string()[..8].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_the_prompt_are_cleaned_and_cut() {
        assert_eq!(shown("vim"), "vim");
        assert_eq!(shown("a\x1b[31mb\x07\n\tc"), "a[31mb c");
        assert_eq!(shown("evil\u{202e}txt.exe\u{200b}"), "eviltxt.exe");
        let long = "长".repeat(100);
        let cut = shown(&long);
        assert_eq!(cut.chars().count(), SHOWN_CHARS);
        assert!(cut.ends_with('…'));
        assert_eq!(shown(&"x".repeat(SHOWN_CHARS)), "x".repeat(SHOWN_CHARS));
    }

    /// 只有点了「允许」才读；点了「不允许」（排在前面、回车按的那个）或者窗口关了都不读。
    #[test]
    fn only_allow_lets_the_program_read() {
        assert!(allowed(Some(ALLOW)));
        assert!(!allowed(Some(0)));
        assert!(!allowed(None));
    }

    /// 说明里有清理过的终端名和程序名；没有程序名或者清理后为空时用不提程序的那句。
    #[test]
    fn the_prompt_names_the_terminal_and_the_program() {
        let detail = read_detail("build\x1b]0;x", Some("vim\u{202e}"));
        assert!(detail.contains("build]0;x") && detail.contains("vim"), "{detail}");
        assert!(!detail.contains('\x1b') && !detail.contains('\u{202e}'), "{detail}");
        let unknown = read_detail("build", None);
        assert!(unknown.contains("build"), "{unknown}");
        assert_eq!(read_detail("build", Some("\x07 \u{200b}")), unknown);
        assert_ne!(read_detail("build", Some("vim")), unknown);
    }

    /// 超过上限的剪贴板文字不交出去，正好到上限的照交。
    #[test]
    fn clipboard_text_over_the_limit_is_withheld() {
        let full = "x".repeat(MAX_CLIPBOARD_BYTES);
        assert_eq!(within_limit(full.clone()).as_deref(), Some(full.as_str()));
        assert_eq!(within_limit(full + "x"), None);
        assert_eq!(within_limit(String::new()).as_deref(), Some(""));
    }
}
