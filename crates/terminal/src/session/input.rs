//! 发给程序的输入：按键、输入法上屏的文字、粘贴、改写正在编辑的输入，以及清屏。

use std::time::Instant;

use libghostty_vt::{
    key,
    key::OptionAsAlt,
    paste,
    screen::Screen,
    terminal::{Mode, ScrollViewport},
};
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
        self.send_input(self.scratch.clone());
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
        self.send_input(bytes);
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
        self.send_input(text.as_bytes().to_vec());
    }

    /// 按终端当前模式粘贴剪贴板文本：不安全的控制字节换成空格，开了 bracketed paste 时用
    /// 括号序列包起来，没开时换行换成回车。
    ///
    /// 文本可能注入命令时（未开 bracketed paste 却含换行，或含 bracketed paste
    /// 结束序列），除非 `allow_unsafe`，否则什么也不写并返回 `Paste::NeedsConfirmation`，
    /// 由界面向用户确认后再带 `allow_unsafe` 重试。
    ///
    /// 这边的 VT 不注册 `on_pty_write`，所以不经 `Terminal::paste_text`，而是按它对纯文本粘贴
    /// 的同样规则自己编码：剪贴板读取回调没装，程序开了粘贴事件（mode 5522）时它也照样写文本。
    pub fn paste(&mut self, text: &str, allow_unsafe: bool) -> Paste {
        self.before_input();
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        // 括号包着时换行无妨，只有结束序列能逃出括号；没包时换行就会执行命令。
        let safe = !text.contains("\x1b[201~") && (bracketed || !text.contains('\n'));
        if !safe && !allow_unsafe {
            return Paste::NeedsConfirmation;
        }
        if text.is_empty() {
            return Paste::Done;
        }
        let mut data = text.as_bytes().to_vec();
        let mut encoded = vec![0u8; data.len() + 16];
        match paste::encode(&mut data, bracketed, &mut encoded) {
            Ok(len) => {
                encoded.truncate(len);
                self.send_input(encoded);
            }
            Err(err) => tracing::warn!("paste failed: {err}"),
        }
        Paste::Done
    }

    /// 把字节直接发给程序，用于映射成控制字符的快捷键（比如 ⌘← 发 Ctrl-A）。
    pub fn send_text(&mut self, bytes: &[u8]) {
        self.before_input();
        self.send_input(bytes.to_vec());
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

    /// 清屏（⌘K）：请宿主清掉屏幕和回滚历史，宿主把清屏要写进 VT 的字节当成一段输出发回来，
    /// 两份 VT 一起清。备用屏幕归全屏程序（vim、less 等）自己管，不动；宿主那边到时还会再看
    /// 一遍。
    pub fn clear_screen(&mut self) {
        if self.terminal.active_screen().is_ok_and(|s| s == Screen::Alternate) {
            return;
        }
        self.before_input();
        (self.sender)(super::Request::ClearScreen);
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
    use crate::testing::*;
    use runode_shared_types::{
        input::{Key, Mods},
        settings::{self, TermSettings},
    };

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
