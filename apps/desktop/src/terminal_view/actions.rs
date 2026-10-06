//! 终端绑定的动作：复制粘贴、清屏、全选、滚动、跳到提示符、发送文本、把屏幕写进文件，以及调整字号。

use std::collections::HashMap;

use gpui::{ClipboardItem, Context, PromptLevel, Window, px};
use runode_shared_types::grid::ViewportScroll;
use runode_terminal::session::{Paste as PasteResult, Session};

use super::{
    ClearScreen, DecreaseFontSize, IncreaseFontSize, JumpToPrompt, MAX_FONT_SIZE, MIN_FONT_SIZE, PasteSelection,
    ResetFontSize, ScreenFile, ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToSelection, ScrollToTop, SendText,
    TerminalView, WriteScreenFile,
};
use crate::ui::actions::{Copy, Paste, SelectAll};

impl TerminalView {
    pub(super) fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste_text(text, window, cx);
        }
    }

    pub(super) fn paste_selection(&mut self, _: &PasteSelection, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.screen.shown().and_then(Session::selection_text) {
            self.paste_text(text, window, cx);
        }
    }

    /// 把文字当作粘贴打进终端，可能直接执行命令时先问一句。没在看（重新连上的过程中、和宿主
    /// 断开了）时丢掉。
    pub fn paste_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.screen.live_mut() else {
            return;
        };
        if session.paste(&text, false) == PasteResult::Done {
            return;
        }
        // 可能直接执行命令的粘贴先让用户确认。
        let detail = rust_i18n::t!("paste.detail");
        let answer = window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("paste.title"),
            Some(&detail),
            &[&*rust_i18n::t!("paste.confirm"), &*rust_i18n::t!("paste.cancel")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |view, _| {
                    if let Some(session) = view.screen.live_mut() {
                        session.paste(&text, true);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.screen.shown().and_then(Session::selection_text) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn clear_screen(&mut self, _: &ClearScreen, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.live_mut() {
            session.clear_screen();
            cx.notify();
        }
    }

    pub(super) fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.select_all();
            cx.notify();
        }
    }

    pub(super) fn scroll_to_top(&mut self, _: &ScrollToTop, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.scroll_viewport(ViewportScroll::Top);
            cx.notify();
        }
    }

    pub(super) fn scroll_to_bottom(&mut self, _: &ScrollToBottom, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.scroll_viewport(ViewportScroll::Bottom);
            cx.notify();
        }
    }

    pub(super) fn scroll_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.scroll_viewport(ViewportScroll::Page(-1));
            cx.notify();
        }
    }

    pub(super) fn scroll_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.scroll_viewport(ViewportScroll::Page(1));
            cx.notify();
        }
    }

    pub(super) fn scroll_to_selection(&mut self, _: &ScrollToSelection, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.scroll_to_selection();
            cx.notify();
        }
    }

    pub(super) fn jump_to_prompt(&mut self, action: &JumpToPrompt, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.screen.shown_mut() {
            session.jump_to_prompt(action.0 < 0);
            cx.notify();
        }
    }

    pub(super) fn send_text(&mut self, action: &SendText, _: &mut Window, cx: &mut Context<Self>) {
        // 有建议时，跳到行尾（Ctrl-E，默认的 ⌘→）接受整条，按词前进（ESC f，默认的 Option+→）
        // 接受一个词：光标已经在输入末尾，这两个键原本也没有别的效果。
        let accepted = match action.0.as_str() {
            "\x05" => self.accept_suggestion(false),
            "\x1bf" => self.accept_suggestion(true),
            _ => false,
        };
        if !accepted && let Some(session) = self.screen.live_mut() {
            session.send_text(action.0.as_bytes());
        }
        cx.notify();
    }

    pub(super) fn write_screen_file(&mut self, action: &WriteScreenFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.screen.shown().and_then(Session::screen_text) else {
            return;
        };
        let path = match write_screen_file(&text) {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("failed to write the screen file: {err:#}");
                return;
            }
        };
        match action.0 {
            ScreenFile::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
            }
            ScreenFile::PastePath => self.paste_text(path.display().to_string(), window, cx),
            ScreenFile::Open => cx.open_with_system(&path),
        }
    }

    pub(super) fn increase_font_size(&mut self, _: &IncreaseFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(f32::from(self.font_size) + 1., cx);
    }

    pub(super) fn decrease_font_size(&mut self, _: &DecreaseFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(f32::from(self.font_size) - 1., cx);
    }

    pub(super) fn reset_font_size(&mut self, _: &ResetFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(self.config.font_size, cx);
    }

    /// 字号一变，单元格尺寸和已排版的字形都要作废；下一次 prepaint 会按新单元格
    /// 重新计算行列数并调整终端尺寸。
    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        let size = px(size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        if size == self.font_size {
            return;
        }
        self.font_size = size;
        self.metrics = None;
        self.glyphs.iter_mut().for_each(HashMap::clear);
        cx.notify();
    }
}

/// 屏幕内容写到临时目录下一个新文件里，返回它的路径。
fn write_screen_file(text: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("runode");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("screen-{}-{stamp}.txt", std::process::id()));
    // 屏幕上可能有密钥之类的内容，只让自己读写。
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&path)?, text.as_bytes())?;
    Ok(path)
}
