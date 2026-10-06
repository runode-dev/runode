//! 别的进程（命令行）要发给程序的控制键和粘贴：按宿主那份 VT 当前的模式编成字节。宿主的 VT
//! 是权威的，程序打开的应用光标键（DECCKM）、Kitty 键盘协议、括号粘贴（mode 2004）都记在它
//! 那里，所以在这里编，和用户在界面里按同一个键时程序收到的一样。

use anyhow::Result;
use libghostty_vt::{key, paste, terminal::Mode};
use runode_shared_types::input::{KeyChord, Mods};

use super::HostSession;
use crate::session::convert::{ghostty_key, ghostty_mods};

impl HostSession {
    /// 依次按下 `keys` 时程序该收到的字节，按 VT 当前的键盘模式编码。Alt 一律当 Meta（加 ESC
    /// 前缀，或者按 Kitty 键盘协议报修饰键），不当 macOS 的 Option 打特殊字符。编不出字节的键
    /// （当前模式下这个键不发东西）跳过。
    pub fn encode_keys(&self, keys: &[KeyChord]) -> Result<Vec<u8>> {
        let mut encoder = key::Encoder::new()?;
        encoder.set_options_from_terminal(&self.terminal).set_macos_option_as_alt(key::OptionAsAlt::True);
        let mut event = key::Event::new()?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64];
        for chord in keys {
            // 平台给的按键事件带着这个键打出的字：Ctrl 按着时编码器自己按没按 Shift 的字符处理，
            // Shift 打出的大写字母、符号算用掉了 Shift。Alt 不改打出的字，`alt-shift-.` 发的是
            // ESC 加 `>`。
            let shift_typed = chord.mods.shift && !chord.mods.ctrl;
            let text = if shift_typed { chord.key.shifted_char() } else { chord.key.unshifted_char() };
            let consumed =
                if shift_typed && text.is_some() { Mods { shift: true, ..Mods::default() } } else { Mods::default() };
            event
                .set_action(key::Action::Press)
                .set_key(ghostty_key(chord.key))
                .set_mods(ghostty_mods(chord.mods))
                .set_consumed_mods(ghostty_mods(consumed))
                .set_unshifted_codepoint(chord.key.unshifted_char().unwrap_or('\0'))
                .set_utf8(text.map(String::from));
            // 不用 `encode_to_vec`：它在 Vec 里已经有东西时按剩余容量算错要扩多少，长的序列会报空间不够。
            let len = match encoder.encode(&event, &mut buf) {
                Err(libghostty_vt::error::Error::OutOfSpace { required }) => {
                    buf.resize(required, 0);
                    encoder.encode(&event, &mut buf)?
                }
                len => len?,
            };
            out.extend_from_slice(&buf[..len]);
        }
        Ok(out)
    }

    /// 粘贴 `text` 时程序该收到的字节：程序开着括号粘贴（mode 2004）时用括号序列包起来，没开时
    /// 换行换成回车；不安全的控制字节（包括能提前结束括号的 ESC）换成空格。和界面里粘贴的规则
    /// 一样，只是不问用户：发粘贴的程序自己知道要粘什么。
    pub fn encode_paste(&self, text: &str) -> Result<Vec<u8>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        // 括号序列前后各 6 个字节。
        let mut encoded = vec![0u8; data.len() + 16];
        let len = paste::encode(&mut data, bracketed, &mut encoded)?;
        encoded.truncate(len);
        Ok(encoded)
    }
}
