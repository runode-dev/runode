//! 发给程序的输入：按键、输入法上屏的文字、粘贴、改写正在编辑的输入，以及清屏。

use std::time::Instant;

use libghostty_vt::{Error, key, key::OptionAsAlt, paste::PasteSource, screen::Screen, terminal::ScrollViewport};
use runode_shared_types::input::KeyInput;

use super::{
    Session,
    convert::{ghostty_key, ghostty_mods},
    log_err,
};

impl Session {
    /// 最近一次向程序发输入（按键、文本、粘贴等）的时刻；从没发过时为 `None`。
    pub fn last_input(&self) -> Option<Instant> {
        self.input_at
    }

    /// 经 libghostty 编码一次按键，它会遵守运行中程序要求的模式（应用光标键、
    /// Kitty 键盘协议、modifyOtherKeys 等）。按键没有产生字节时返回 false，
    /// 调用方可以交给平台处理。
    pub fn key(&mut self, input: &KeyInput) -> bool {
        // Option 当作 Alt 时不能算「已消耗」，编码器才会改用未加修饰的字符并加 ESC 前缀。
        let right = input.mods.right_alt;
        let option_is_alt = match self.option_as_alt {
            OptionAsAlt::True => true,
            OptionAsAlt::Left => !right,
            OptionAsAlt::Right => right,
            _ => false,
        };
        let mut consumed = input.consumed_mods;
        if option_is_alt {
            consumed.alt = false;
        }
        self.key_event
            .set_action(key::Action::Press)
            .set_key(ghostty_key(input.key))
            .set_mods(ghostty_mods(input.mods))
            .set_consumed_mods(ghostty_mods(consumed))
            .set_unshifted_codepoint(input.unshifted)
            .set_utf8(input.text.as_deref());
        self.scratch.clear();
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.terminal)
            .set_macos_option_as_alt(self.option_as_alt)
            .encode_to_vec(&self.key_event, &mut self.scratch);
        if let Err(err) = encoded {
            tracing::warn!("key encode failed: {err}");
            return false;
        }
        if self.scratch.is_empty() {
            return false;
        }
        self.before_input();
        self.writer.write(&self.scratch);
        true
    }

    /// 改写 shell 正在编辑的输入：先按 `backspace` 下退格，然后把 `text` 当作普通文字写进去
    /// （不走粘贴），一次发给程序。编码不出退格键时什么也不发，返回 false。
    pub fn edit_input(&mut self, backspace: usize, text: &str) -> bool {
        let mut bytes = Vec::new();
        if backspace > 0 {
            let Some(key) = self.encode_key(key::Key::Backspace) else {
                return false;
            };
            bytes.extend(key.repeat(backspace));
        }
        bytes.extend_from_slice(text.as_bytes());
        if bytes.is_empty() {
            return true;
        }
        self.before_input();
        self.writer.write(&bytes);
        true
    }

    /// 往左（负数）或往右按 `steps` 下方向键的编码。
    pub(super) fn arrow_keys(&mut self, steps: isize) -> Option<Vec<u8>> {
        let key = if steps < 0 { key::Key::ArrowLeft } else { key::Key::ArrowRight };
        Some(self.encode_key(key)?.repeat(steps.unsigned_abs()))
    }

    /// 不带修饰键按一下 `key` 的编码，跟随终端当前的键盘模式。
    pub(super) fn encode_key(&mut self, key: key::Key) -> Option<Vec<u8>> {
        self.key_event
            .set_action(key::Action::Press)
            .set_key(key)
            .set_mods(key::Mods::empty())
            .set_consumed_mods(key::Mods::empty())
            .set_unshifted_codepoint('\0')
            .set_utf8(None::<String>);
        self.scratch.clear();
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&self.key_event, &mut self.scratch);
        log_err("key encode", encoded)?;
        (!self.scratch.is_empty()).then(|| self.scratch.clone())
    }

    /// 输入法上屏的文本，按原样发送。
    pub fn commit_text(&mut self, text: &str) {
        self.before_input();
        self.writer.write(text.as_bytes());
    }

    /// 按终端当前模式粘贴剪贴板文本（bracketed paste、粘贴事件等由 libghostty 处理）。
    ///
    /// 文本可能注入命令时（未开 bracketed paste 却含换行，或含 bracketed paste
    /// 结束序列），除非 `allow_unsafe`，否则什么也不写并返回 `Paste::NeedsConfirmation`，
    /// 由界面向用户确认后再带 `allow_unsafe` 重试。
    pub fn paste(&mut self, text: &str, allow_unsafe: bool) -> Paste {
        self.before_input();
        match self
            .terminal
            .paste_text(text, PasteSource::Clipboard, allow_unsafe)
        {
            Ok(_) => Paste::Done,
            Err(Error::Rejected) => Paste::NeedsConfirmation,
            Err(err) => {
                tracing::warn!("paste failed: {err}");
                Paste::Done
            }
        }
    }

    /// 把字节直接发给程序，用于映射成控制字符的快捷键（比如 ⌘← 发 Ctrl-A）。
    pub fn send_text(&mut self, bytes: &[u8]) {
        self.before_input();
        self.writer.write(bytes);
    }

    /// 向程序发输入之前：记下时刻，回到最底部，并清掉选区。
    pub(super) fn before_input(&mut self) {
        self.input_at = Some(Instant::now());
        if self.terminal.selection().is_ok_and(|s| s.is_some()) {
            log_err("selection clear", self.terminal.set_selection(None));
        }
        if !self.terminal.viewport_active().unwrap_or(true) {
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
        }
        // 打字时回到底部对齐整行，不留半行错开。
        self.scroll_offset = 0.;
    }

    /// 清屏（⌘K）：清掉屏幕和回滚历史。备用屏幕归全屏程序（vim、less 等）自己管，不动。
    ///
    /// 前台是 shell 时它多半停在提示符，整屏清掉后发一个 FF（Ctrl-L）让它在顶上重画提示符，
    /// 已经敲了一半的命令也会保留。前台在跑别的程序时不能给它塞 FF，只删掉光标以上的行，
    /// 光标所在行顶到第一行。
    pub fn clear_screen(&mut self) {
        if self.terminal.active_screen().is_ok_and(|s| s == Screen::Alternate) {
            return;
        }
        self.before_input();
        if self.pty.foreground_is_shell() {
            // ED 3 放在最后：先 ED 2 时被推进回滚历史的内容也一起清掉。
            self.terminal.vt_write(b"\x1b[H\x1b[2J\x1b[3J");
            self.writer.write(b"\x0c");
        } else {
            self.clear_above_cursor();
        }
    }

    /// 清掉回滚历史和光标以上的行，光标所在行及以下顶到最上面，光标留在原来的列。
    fn clear_above_cursor(&mut self) {
        let x = self.terminal.cursor_x().unwrap_or(0);
        let y = self.terminal.cursor_y().unwrap_or(0);
        // DL 从第一行起删掉 y 行，下面的内容跟着上移。
        let seq = if y > 0 {
            format!("\x1b[3J\x1b[H\x1b[{y}M\x1b[1;{}H", x + 1)
        } else {
            "\x1b[3J".to_owned()
        };
        self.terminal.vt_write(seq.as_bytes());
    }
}

/// `Session::paste` 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paste {
    Done,
    /// 内容可能直接执行命令，需要用户确认。
    NeedsConfirmation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testing::*;
    use runode_shared_types::{
        input::{Key, Mods},
        settings::{self, TermSettings},
    };

    #[test]
    fn clear_screen_at_the_shell_clears_everything() {
        // `idle_session` 的「shell」就是前台进程，走 shell 那一支。
        let mut session = idle_session();
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n$ ls");
        assert!(scrollback_rows(&session) > 0);
        session.clear_screen();
        assert_eq!(scrollback_rows(&session), 0);
        let frame = session.frame();
        assert!((0..4).all(|y| row_text(&frame, y).is_empty()));
    }

    #[test]
    fn clear_above_cursor_keeps_the_cursor_row_at_the_top() {
        let mut session = idle_session();
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\nrunning");
        session.clear_above_cursor();
        assert_eq!(scrollback_rows(&session), 0);
        assert_eq!(session.terminal.cursor_y().unwrap(), 0);
        assert_eq!(session.terminal.cursor_x().unwrap(), 7);
        let frame = session.frame();
        assert_eq!(row_text(&frame, 0), "running");
        assert!((1..4).all(|y| row_text(&frame, y).is_empty()));
    }

    #[test]
    fn clear_screen_leaves_the_alternate_screen_alone() {
        let mut session = idle_session();
        session.feed(b"\x1b[?1049hvim");
        session.clear_screen();
        assert_eq!(row_text(&session.frame(), 0), "vim");
    }

    #[test]
    fn multiline_paste_needs_confirmation_unless_bracketed() {
        let mut session = idle_session();
        assert_eq!(session.paste("ls", false), Paste::Done);
        assert_eq!(session.paste("rm -rf x\nls", false), Paste::NeedsConfirmation);
        assert_eq!(session.paste("rm -rf x\nls", true), Paste::Done);

        // 程序开启 bracketed paste 后，换行不会被直接执行，无需确认。
        session.feed(b"\x1b[?2004h");
        assert_eq!(session.paste("a\nb", false), Paste::Done);
    }

    #[test]
    fn option_as_alt_follows_the_configured_side() {
        /// 按一次 Option+s（美式布局下打出 ß），返回编码结果。
        fn option_s(session: &mut Session, right: bool) -> Vec<u8> {
            let alt = Mods { alt: true, ..Mods::default() };
            session.key(&KeyInput {
                key: Key::S,
                mods: Mods { right_alt: right, ..alt },
                consumed_mods: alt,
                unshifted: 's',
                text: Some("ß".into()),
            });
            session.scratch.clone()
        }

        let mut session = idle_session();
        assert_eq!(option_s(&mut session, false), "ß".as_bytes());

        session.apply_config(&TermSettings {
            option_as_alt: settings::OptionAsAlt::Left,
            ..TermSettings::default()
        });
        assert_eq!(option_s(&mut session, false), b"\x1bs");
        assert_eq!(option_s(&mut session, true), "ß".as_bytes());
    }
}
