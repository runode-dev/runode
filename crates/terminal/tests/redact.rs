//! 宿主转给别的进程的输出里抹掉 shell 集成报告的内容，报告被切在哪里都一样；宿主升级时
//! 在哪里切开、换一个 `ReportRedactor` 接着抹也一样。

use runode_terminal::host_session::{RedactorState, ReportRedactor};

/// 一次喂完和任意切成两块喂，结果一样。
fn redact_split(data: &[u8]) -> Vec<u8> {
    let mut whole = ReportRedactor::new();
    let expected = whole.redact(data).unwrap_or_else(|| data.to_vec());
    for at in 0..=data.len() {
        let mut redactor = ReportRedactor::new();
        let mut out = Vec::new();
        for part in [&data[..at], &data[at..]] {
            out.extend(redactor.redact(part).unwrap_or_else(|| part.to_vec()));
        }
        assert_eq!(out, expected, "split at {at}");
        // 切开的地方换成另一个，从前一个的状态接着抹。
        let mut before = ReportRedactor::new();
        let mut out = before.redact(&data[..at]).unwrap_or_else(|| data[..at].to_vec());
        let mut after = ReportRedactor::from_state(before.state());
        assert_eq!(after.state(), before.state());
        out.extend(after.redact(&data[at..]).unwrap_or_else(|| data[at..].to_vec()));
        assert_eq!(out, expected, "handed over at {at}");
    }
    expected
}

#[test]
fn report_contents_are_removed_wherever_the_output_is_split() {
    assert_eq!(
        redact_split(b"a\x1b]6973;secret;cwd=/tmp\x07b\x1b]6973;secret;path=/bin\x1b\\c"),
        b"a\x1b]6973;\x07b\x1b]6973;\x1b\\c"
    );
    // CAN、SUB 取消序列；序列里别的 C0 控制字符 VT 不理，一起抹掉。
    assert_eq!(redact_split(b"\x1b]6973;s\x01ecret\x18x\x1b]6973;t\x1ay"), b"\x1b]6973;\x18x\x1b]6973;\x1ay");
    // 报告里又来一条报告：ESC 结束了前一条。
    assert_eq!(redact_split(b"\x1b]6973;a\x1b]6973;b\x07"), b"\x1b]6973;\x1b]6973;\x07");
}

#[test]
fn other_output_passes_through_untouched() {
    let mut redactor = ReportRedactor::new();
    for data in [
        &b"plain text"[..],
        b"\x1b[31mred\x1b[0m \x1b]0;title\x07",
        b"\x1b]69730;not a report\x07\x1b]697;x\x07\x1b]6973\x07",
        b"\x1b\x1b]6972;x\x07",
    ] {
        assert_eq!(redactor.redact(data), None, "{data:?}");
    }
    // 没结束的报告留到下一块接着抹。
    assert_eq!(redactor.redact(b"\x1b]6973;tok"), Some(b"\x1b]6973;".to_vec()));
    assert_eq!(redactor.redact(b"en;cwd=/"), Some(Vec::new()));
    assert_eq!(redactor.redact(b"\x07after"), None);
}

/// 从重放建起来的 VT 要补喂的字节：对上了几个字节的开头补几个，在报告里面补完整的开头。
#[test]
fn the_resume_bytes_follow_the_state() {
    let mut redactor = ReportRedactor::new();
    assert_eq!(redactor.state(), RedactorState::default());
    assert_eq!(redactor.state().resume_bytes(), b"");
    redactor.redact(b"x\x1b]69");
    assert_eq!(redactor.state(), RedactorState { matched: 4, inside: false });
    assert_eq!(redactor.state().resume_bytes(), b"\x1b]69");
    redactor.redact(b"73;");
    assert_eq!(redactor.state(), RedactorState { matched: 0, inside: true });
    assert_eq!(redactor.state().resume_bytes(), b"\x1b]6973;");
    // 报告被下一条序列的 ESC 结束：停在这个 ESC 上。
    redactor.redact(b"tok\x1b");
    assert_eq!(redactor.state().resume_bytes(), b"\x1b");
    // 不合理的状态按能对上的最多算。
    let odd = ReportRedactor::from_state(RedactorState { matched: 200, inside: false });
    assert_eq!(odd.state(), RedactorState { matched: 6, inside: false });
    assert_eq!(RedactorState { matched: 200, inside: false }.resume_bytes(), b"\x1b]6973");
    let odd = ReportRedactor::from_state(RedactorState { matched: 3, inside: true });
    assert_eq!(odd.state(), RedactorState { matched: 0, inside: true });
}
