//! fish 自己发的提示符标记（`OSC 133;A;special_key=1`）：它连 `read` 的提示符也标，还在跑的命令的输出
//! 就在那里被切成两条。`--features no-mark-prompt` 能关掉它，fish 4.0 不认得这个功能名，照发不误。fish 的
//! shell 集成在提示符里自己标、带着 `runode=fish`（VT 不认这个键，略过），所以从这样的标记到下一个命令结束
//! 标记（`OSC 133;D`）之间，把 fish 自己那种标记的结束符 BEL 原地换成 CAN：VT 遇到 CAN 就丢掉这条 OSC。
//! fish 自己每跑完一条命令也发 `133;D`，`read` 不发，所以命令里 `read` 的提示符标记被换掉，集成的 fish 里
//! `exec` 或再起的没接集成的 fish 跑完一条命令以后照常标自己的提示符，到集成的 fish 下一个提示符才又开始换。
//! 只认 fish 的集成：zsh、bash 里再起的、没接集成的 fish（包括 ssh 到别处的）照常标自己的提示符。各端收到的
//! 都是换过的字节；不缓存、不改长度，分几次读到也认得出来。
//! shortcut: 集成的 fish 里再起的 fish 跑第一条命令之前，它的提示符标记还是被换掉；真碰上再找认得出它启动的
//! 标志。

const REPORT: &[u8] = b"\x1b]133;A;runode=fish;";
const MARK: &[u8] = b"\x1b]133;A;special_key=1";
const END: &[u8] = b"\x1b]133;D";
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;

#[derive(Default)]
pub(super) struct FishMarks {
    /// 见过 fish 集成的提示符标记，之后还没见过命令结束标记，换掉 fish 自己的标记。
    armed: bool,
    /// `REPORT` 已经对上了几个字节。
    report: usize,
    /// `MARK` 已经对上了几个字节。
    mark: usize,
    /// `END` 已经对上了几个字节。
    end: usize,
}

impl FishMarks {
    pub(super) fn filter(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            if self.mark == MARK.len() {
                if self.armed && *byte == BEL {
                    *byte = CAN;
                }
                self.mark = 0;
            }
            self.report = advance(REPORT, self.report, *byte);
            self.mark = advance(MARK, self.mark, *byte);
            self.end = advance(END, self.end, *byte);
            if self.report == REPORT.len() {
                self.armed = true;
                self.report = 0;
            }
            if self.end == END.len() {
                self.armed = false;
                self.end = 0;
            }
        }
    }
}

/// 对上了 `matched` 个字节以后又来了 `byte`，现在对上了几个。各个模式里只有开头的 ESC 会重新开始一次匹配。
fn advance(pattern: &[u8], matched: usize, byte: u8) -> usize {
    if pattern[matched] == byte { matched + 1 } else { usize::from(byte == pattern[0]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> Vec<u8> {
        let mut marks = FishMarks::default();
        let mut out = Vec::new();
        for chunk in chunks {
            let mut chunk = chunk.to_vec();
            marks.filter(&mut chunk);
            out.extend(chunk);
        }
        out
    }

    #[test]
    fn marks_pass_until_the_integration_reports() {
        let mark = b"\x1b]133;A;special_key=1\x07read> ";
        assert_eq!(filtered(&[mark]), mark);
        // 别的 shell 的集成不算：zsh 里起的 fish 照常标。
        let zsh = b"\x1b]6973;token;cwd=/x\x07\x1b]133;A;cl=line;redraw=1\x07% ";
        assert_eq!(filtered(&[zsh, mark]), [&zsh[..], mark].concat());
    }

    #[test]
    fn marks_after_a_report_are_cancelled_even_when_split() {
        let report = b"\x1b]133;A;runode=fish;cl=line;redraw=1\x07> ";
        let out = filtered(&[report, b"one\r\n\x1b]133;A;spec", b"ial_key=1", b"\x07read> "]);
        assert_eq!(out, [&report[..], b"one\r\n\x1b]133;A;special_key=1\x18read> "].concat());
    }

    #[test]
    fn other_marks_stay() {
        let report = b"\x1b]133;A;runode=fish;cl=line;redraw=1\x07> ";
        let ours = b"\x1b]133;A;cl=line;redraw=1\x07P> \x1b]133;B\x07\x1b]133;A;special_key=12\x07";
        let longer = b"\x1b]133;A;special_key=1;x\x07";
        assert_eq!(filtered(&[report, ours, longer]), [&report[..], ours, longer].concat());
    }

    #[test]
    fn a_command_end_stops_cancelling_until_the_next_report() {
        let report = b"\x1b]133;A;runode=fish;cl=line;redraw=1\x07> ";
        let read = b"\x1b]133;A;special_key=1\x07read> ";
        // 集成的 fish 里 `exec fish`：新的 fish 跑完一条命令发了 133;D，之后它自己的提示符标记照常留着。
        let nested = b"\x1b]133;A;special_key=1\x07> ls\r\n\x1b]133;C\x07a\r\n\x1b]133;D;0\x07";
        let out = filtered(&[report, nested, read]);
        assert_eq!(
            out,
            [&report[..], b"\x1b]133;A;special_key=1\x18> ls\r\n\x1b]133;C\x07a\r\n\x1b]133;D;0\x07", read].concat()
        );
        // 回到集成的 fish 的提示符以后又开始换。
        let out = filtered(&[report, b"\x1b]133;D;0\x07", report, read]);
        assert_eq!(out.iter().filter(|&&byte| byte == CAN).count(), 1);
    }

    #[test]
    fn an_escape_restarts_the_match() {
        let out = filtered(&[b"\x1b]133;A;ru\x1b]133;A;runode=fish;\x07\x1b]13\x1b]133;A;special_key=1\x07"]);
        assert_eq!(out.last(), Some(&CAN));
    }
}
