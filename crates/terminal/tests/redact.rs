//! 宿主转给别的进程的输出里抹掉 shell 集成报告的内容，报告被切在哪里都一样。

use runode_terminal::host_session::ReportRedactor;

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
