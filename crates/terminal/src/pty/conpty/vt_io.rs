//! ConPTY 管道和外层 VT 之间的兼容处理。Kitty 和 modifyOtherKeys 的请求会穿过 ConPTY，
//! 它们的按键编码却不能原样传回程序。把这些 CSI 的结束字节改成 CAN，让各端的 VT 取消请求，
//! 一致使用 ConPTY 能传递的传统编码。已发出的字节和控制字符照常保留，不缓存整条序列。

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi(Csi),
}

#[derive(Default)]
struct Csi {
    prefix: Option<u8>,
    first_param: u16,
    params_started: bool,
    first_param_done: bool,
    extra_intermediates: bool,
    ignored: bool,
}

impl Csi {
    fn parameter(&mut self, byte: u8) {
        match byte {
            b'<'..=b'?' => {
                if self.prefix.is_none() && !self.params_started && !self.extra_intermediates {
                    self.prefix = Some(byte);
                } else {
                    self.ignored = true;
                }
            }
            b'0'..=b'9' | b';' | b':' => {
                self.params_started = true;
                self.ignored |= self.extra_intermediates;
                if matches!(byte, b';' | b':') {
                    self.first_param_done = true;
                } else if !self.first_param_done {
                    self.first_param = self.first_param.saturating_mul(10).saturating_add(u16::from(byte - b'0'));
                }
            }
            0x20..=0x2f => self.extra_intermediates = true,
            _ => self.ignored = true,
        }
    }

    fn keyboard_request(&self, final_byte: u8) -> bool {
        if self.ignored || self.extra_intermediates {
            return false;
        }
        match final_byte {
            b'u' => matches!(self.prefix, Some(b'>' | b'=' | b'<' | b'?')),
            b'm' => matches!(self.prefix, Some(b'>' | b'?')) && self.first_param == 4,
            _ => false,
        }
    }
}

#[derive(Default)]
pub(super) struct OutputFilter {
    state: State,
}

impl OutputFilter {
    pub(super) fn filter(&mut self, output: &mut [u8]) {
        for byte in output {
            // ESC 在所有控制序列和字符串里都会中断原序列；后面的 '[' 可以开始新的 CSI。
            // CAN、SUB 则直接回到 ground。这些字节必须先交给 VT，不能连着请求一起删掉。
            match *byte {
                0x1b => {
                    self.state = State::Escape;
                    continue;
                }
                0x18 | 0x1a => {
                    self.state = State::Ground;
                    continue;
                }
                _ => {}
            }
            match &mut self.state {
                State::Ground => {}
                State::Escape => match *byte {
                    b'[' | 0x9b => self.state = State::Csi(Csi::default()),
                    0x00..=0x1f | 0x7f => {}
                    _ => self.state = State::Ground,
                },
                State::Csi(csi) => match *byte {
                    // VT 执行 C0、忽略 DEL，但仍然在同一条 CSI 里。
                    0x00..=0x1f | 0x7f => {}
                    0x40..=0x7e => {
                        if csi.keyboard_request(*byte) {
                            *byte = 0x18;
                        }
                        self.state = State::Ground;
                    }
                    0x9b => self.state = State::Csi(Csi::default()),
                    0x80..=0xff => self.state = State::Ground,
                    byte => csi.parameter(byte),
                },
            }
        }
    }
}

/// 本机旧版 ConPTY 会把 OSC 回复里的颜色值等内容当成输入。只拦截已确认不安全的 OSC；
/// DCS（包括 XTVERSION、DECRQSS 的应答）可以传递，不能因为同属字符串协议就一起丢弃。
pub(crate) fn reply_supported(data: &[u8]) -> bool {
    !data.windows(2).any(|pair| pair == b"\x1b]")
}

#[cfg(test)]
mod tests;
