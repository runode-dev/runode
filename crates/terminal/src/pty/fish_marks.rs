//! fish 自己发的提示符标记（`OSC 133;A;special_key=1`）：它连 `read` 的提示符也标，还在跑的命令的输出
//! 就在那里被切成两条。`--features no-mark-prompt` 能关掉它，fish 4.0 不认得这个功能名，照发不误。shell
//! 集成在提示符里自己标，所以输出里出现过集成的报告（`OSC 6973`）以后，把这种标记的结束符 BEL 原地换成
//! CAN：VT 遇到 CAN 就丢掉这条 OSC。各端收到的都是换过的字节；不缓存、不改长度，分几次读到也认得出来。

const REPORT: &[u8] = b"\x1b]6973;";
const MARK: &[u8] = b"\x1b]133;A;special_key=1";
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;

#[derive(Default)]
pub(super) struct FishMarks {
    /// 见过集成的报告，开始换掉 fish 的标记。
    armed: bool,
    /// `REPORT` 已经对上了几个字节。
    report: usize,
    /// `MARK` 已经对上了几个字节。
    mark: usize,
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
            if self.report == REPORT.len() {
                self.armed = true;
                self.report = 0;
            }
        }
    }
}

/// 对上了 `matched` 个字节以后又来了 `byte`，现在对上了几个。两个模式里只有开头的 ESC 会重新开始一次匹配。
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
    }

    #[test]
    fn marks_after_a_report_are_cancelled_even_when_split() {
        let report = b"\x1b]6973;token;cwd=/x\x07";
        let out = filtered(&[report, b"one\r\n\x1b]133;A;spec", b"ial_key=1", b"\x07read> "]);
        assert_eq!(out, [&report[..], b"one\r\n\x1b]133;A;special_key=1\x18read> "].concat());
    }

    #[test]
    fn other_marks_stay() {
        let report = b"\x1b]6973;token;end=\x07";
        let ours = b"\x1b]133;A;cl=line;redraw=1\x07P> \x1b]133;B\x07\x1b]133;A;special_key=12\x07";
        let longer = b"\x1b]133;A;special_key=1;x\x07";
        assert_eq!(filtered(&[report, ours, longer]), [&report[..], ours, longer].concat());
    }

    #[test]
    fn an_escape_restarts_the_match() {
        let out = filtered(&[b"\x1b]6973\x1b]6973;t;end=\x07\x1b]13\x1b]133;A;special_key=1\x07"]);
        assert_eq!(out.last(), Some(&CAN));
    }
}
