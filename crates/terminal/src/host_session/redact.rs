//! 抹掉 PTY 输出里 shell 集成报告的内容，见 `ReportRedactor`。

use super::effects::SHELL_REPORT;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// 取消正在进行的序列的两个 C0 控制字符。
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// 报告开头的 `ESC ] 6973;` 有几个字节。
const PREFIX_LEN: usize = 2 + SHELL_REPORT.len();

/// 报告开头的第 `i` 个字节。
const fn prefix_byte(i: usize) -> u8 {
    match i {
        0 => ESC,
        1 => b']',
        _ => SHELL_REPORT[i - 2],
    }
}

/// 报告的开头 `ESC ] 6973;`：抹过的报告只剩它和结束序列。
pub(super) const REPORT_START: [u8; PREFIX_LEN] = {
    let mut start = [0; PREFIX_LEN];
    let mut i = 0;
    while i < PREFIX_LEN {
        start[i] = prefix_byte(i);
        i += 1;
    }
    start
};

/// 把 PTY 输出转给别的进程前，抹掉 shell 集成报告（`ESC ] 6973;<口令>;<字段>=<值> BEL`）
/// 的内容：报告带着这个 shell 的口令，口令不能出宿主。
///
/// 只抹 `6973;` 之后到序列结束前的字节，开头的 `ESC ] 6973;` 和结束序列的字节都留着：VT 收到
/// 的仍是一条内容为 `6973;` 的未知 OSC，照样忽略，前后的状态和收到原来那条时一样，前端的 VT
/// 不会因此和宿主的分叉。序列在 BEL、ESC（ST 的开头，或者直接开始下一条序列）、CAN、SUB 处
/// 结束，和 VT 处理 OSC 的方式一致；ESC 在任何状态下都会开始一条新序列，所以只认 `ESC ]`
/// 开头的报告就够了，shell 集成脚本也只这样发。
///
/// 输出是一块一块到的，报告可能被切在两块之间，状态跨块保留：要从输出流的开头起，每一块都
/// 按先后交给同一个 `ReportRedactor`。
#[derive(Debug, Default)]
pub struct ReportRedactor {
    /// 已经对上了报告开头的几个字节。
    matched: usize,
    /// 正在报告里面，之后的字节到序列结束前都抹掉。
    inside: bool,
}

impl ReportRedactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 输出流正停在一条报告里：开头的 `ESC ] 6973;` 已经过去了，结束序列还没到。这时宿主那份
    /// VT 也停在这条报告中间，没写完的报告连着口令都在它的续接里，给别的进程的屏幕要另外处理，
    /// 见 `HostSession::redacted_snapshot`。
    pub fn in_report(&self) -> bool {
        self.inside
    }

    /// 处理接下来的一块输出：有要抹的内容时返回抹过的一份，没有时返回 `None`，原样转发即可。
    pub fn redact(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        let mut out: Option<Vec<u8>> = None;
        let mut i = 0;
        while i < data.len() {
            // 不在报告里、也没对上开头时，直接跳到下一个 ESC。
            if !self.inside && self.matched == 0 {
                let skip = data[i..].iter().position(|&b| b == ESC).unwrap_or(data.len() - i);
                if let Some(out) = &mut out {
                    out.extend_from_slice(&data[i..i + skip]);
                }
                i += skip;
                if i == data.len() {
                    break;
                }
            }
            let byte = data[i];
            if self.inside {
                if matches!(byte, BEL | ESC | CAN | SUB) {
                    self.inside = false;
                    self.matched = usize::from(byte == ESC);
                } else {
                    out.get_or_insert_with(|| data[..i].to_vec());
                    i += 1;
                    continue;
                }
            } else if byte == prefix_byte(self.matched) {
                self.matched += 1;
                if self.matched == PREFIX_LEN {
                    self.matched = 0;
                    self.inside = true;
                }
            } else {
                self.matched = usize::from(byte == ESC);
            }
            if let Some(out) = &mut out {
                out.push(byte);
            }
            i += 1;
        }
        out
    }
}
