//! 从程序输出的原始字节里留下最近一次 OSC 9 报告的原文。
//!
//! 终端解析 OSC 9;4 进度时只留下解析后的状态和百分比，规则要比的是原文（比如 `4;1;-1` 和
//! `4;0;0` 在解析后分不出来），所以在输出进 VT 之前另扫一遍，只认 OSC 序列，别的字节跳过。

/// 留下的原文最多这么多个字符；原文是程序给的，不可信，限制长度。
const MAX_CHARS: usize = 256;
/// 一个 OSC 序列最多攒这么多字节，再长的整条丢掉。
const MAX_BODY: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// 刚读到 ESC。
    Escape,
    /// 在 OSC 序列里。
    Body,
    /// OSC 序列里读到 ESC，后面跟 `\` 就是结束。
    BodyEscape,
    /// 太长的 OSC 序列，跳到它结束为止。
    Discard,
    DiscardEscape,
}

/// 最近一次 OSC 9 报告的原文。
#[derive(Debug, Default)]
pub struct ProgressCapture {
    state: State,
    body: Vec<u8>,
    latest: String,
}

impl ProgressCapture {
    /// 扫一段输出。序列可以跨好几段。
    pub fn observe(&mut self, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            if self.state == State::Ground {
                // 大部分输出里没有 ESC，直接跳到下一个 ESC。
                match bytes[i..].iter().position(|&b| b == 0x1b) {
                    Some(at) => {
                        i += at + 1;
                        self.state = State::Escape;
                    }
                    None => return,
                }
                continue;
            }
            let b = bytes[i];
            i += 1;
            self.state = match self.state {
                State::Ground => State::Ground,
                State::Escape => self.after_escape(b),
                State::Body => match b {
                    0x07 => self.finish(),
                    0x1b => State::BodyEscape,
                    _ => self.push(b),
                },
                State::BodyEscape => match b {
                    b'\\' => self.finish(),
                    // ESC 打断了这条 OSC，它本身可能是下一个序列的开头。
                    _ => self.after_escape(b),
                },
                State::Discard => match b {
                    0x07 => State::Ground,
                    0x1b => State::DiscardEscape,
                    _ => State::Discard,
                },
                State::DiscardEscape => match b {
                    b'\\' => State::Ground,
                    0x1b => State::DiscardEscape,
                    _ => State::Discard,
                },
            };
        }
    }

    /// 最近一次 OSC 9 报告 `9;` 后面的部分，去掉了控制字符；没有过时为空。
    pub fn latest(&self) -> &str {
        &self.latest
    }

    /// 忘掉留下的原文，换了一个程序时用，免得它沿用上一个程序的报告。
    pub fn clear(&mut self) {
        self.latest.clear();
    }

    fn after_escape(&mut self, b: u8) -> State {
        match b {
            b']' => {
                self.body.clear();
                State::Body
            }
            0x1b => State::Escape,
            _ => State::Ground,
        }
    }

    fn push(&mut self, b: u8) -> State {
        if self.body.len() >= MAX_BODY {
            self.body.clear();
            return State::Discard;
        }
        self.body.push(b);
        State::Body
    }

    fn finish(&mut self) -> State {
        if let Some(payload) = self.body.strip_prefix(b"9;") {
            self.latest =
                String::from_utf8_lossy(payload).chars().filter(|c| !c.is_control()).take(MAX_CHARS).collect();
        }
        self.body.clear();
        State::Ground
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_latest_osc_9_payload() {
        let mut capture = ProgressCapture::default();
        assert_eq!(capture.latest(), "");
        capture.observe(b"text\x1b]0;title\x07\x1b]9;4;1;-1\x07more");
        assert_eq!(capture.latest(), "4;1;-1");
        // 用 ST 结束、跨段到达都行；别的 OSC 不影响。
        capture.observe(b"\x1b]9;4;");
        capture.observe(b"0;0\x1b");
        capture.observe(b"\\\x1b]2;x\x07");
        assert_eq!(capture.latest(), "4;0;0");
        capture.clear();
        assert_eq!(capture.latest(), "");
    }

    #[test]
    fn interrupted_and_oversized_sequences_are_dropped() {
        let mut capture = ProgressCapture::default();
        // OSC 被一个新的 ESC ] 打断，算新的那条。
        capture.observe(b"\x1b]9;4;3\x1b\x1b]9;4;0\x07");
        assert_eq!(capture.latest(), "4;0");
        let mut long = b"\x1b]9;".to_vec();
        long.extend(std::iter::repeat_n(b'x', MAX_BODY + 10));
        long.extend(b"\x07\x1b]9;4;3\x07");
        capture.observe(&long);
        assert_eq!(capture.latest(), "4;3");
    }
}
