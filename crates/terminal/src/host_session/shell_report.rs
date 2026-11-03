//! shell 集成报告：集成脚本经私有 OSC 告诉宿主的当前目录、PATH、函数名和命令的开始结束。这里只管格式和
//! 认口令，不改会话的状态。

use std::{ffi::OsString, path::PathBuf};

/// shell 集成在显示提示符时、内容和上次报告的不一样时用的私有 OSC：
/// `ESC ] 6973;<口令>;<字段>=<百分号编码的值> BEL`。字段是 `cwd`（shell 的当前目录，每次显示
/// 提示符前都发）、`path`（PATH）或 `functions`（名字之间用空格分开），别的字段不认识，丢掉。另有 `command`：shell 开始运行命令时紧接在 OSC 133;C 前面发，值是命令
/// 原文（拿不到时为空）；`end`：紧接在 OSC 133;D 前面发，值为空；`version`：集成脚本的版本，每条命令
/// 开始时和 `command` 一起发，2 起有 `end`。
///
/// 报告运行完的命令（`HostMsg::CommandFinished`）同样只认这条报告，133;C 自带的命令原文一律
/// 不用：伪造的命令会让 `runode wait` 这类等命令的前端以为命令跑完了。
///
/// 任何打印到终端的内容（`cat` 一个文件、ssh 远端的输出）都能写出这样的序列，而报告的 PATH
/// 会被界面拿去找命令行工具（gh）来跑，所以只认带着本 shell 口令的报告。口令经一个用完就删的文件交给 shell（不经
/// 环境变量），之后只留在 shell 自己不导出的变量里，在 shell 里运行的
/// 程序正常拿不到它。只看是否处在提示符状态挡不住伪造：输出里可以先伪造一个 OSC 133;D。
pub(crate) const SHELL_REPORT: &[u8] = super::redact::REPORT_START.split_at(2).1;

/// 一条认得的报告。
#[derive(Debug, PartialEq)]
pub(super) enum Report {
    /// shell 开始运行命令，命令原文；拿不到时为空。
    Command(Option<String>),
    /// 命令运行结束，紧接着是 OSC 133;D。
    End,
    /// 集成脚本的版本，2 起命令结束时有 `End`。
    Version(u32),
    Path(OsString),
    /// shell 的当前目录；报告的是空值时为空。
    Cwd(Option<PathBuf>),
    Functions(Vec<String>),
}

/// 解开一条报告 `<口令>;<字段>=<百分号编码的值>`。口令不是 `token`、缺口令、格式不对或者不认识的字段时为空。
pub(super) fn parse(token: &str, report: &[u8]) -> Option<Report> {
    let semicolon = report.iter().position(|&b| b == b';')?;
    let (given, report) = (&report[..semicolon], &report[semicolon + 1..]);
    if !same_token(token.as_bytes(), given) {
        return None;
    }
    let eq = report.iter().position(|&b| b == b'=')?;
    let (field, value) = (&report[..eq], percent_decode(&report[eq + 1..]));
    Some(match field {
        b"command" => Report::Command((!value.is_empty()).then(|| String::from_utf8_lossy(&value).into_owned())),
        b"end" => Report::End,
        b"version" => Report::Version(std::str::from_utf8(&value).ok()?.parse().ok()?),
        b"path" => Report::Path(os_string(value)),
        b"cwd" => Report::Cwd((!value.is_empty()).then(|| os_string(value).into())),
        b"functions" => {
            Report::Functions(String::from_utf8_lossy(&value).split_whitespace().map(str::to_owned).collect())
        }
        _ => return None,
    })
}

/// 两个口令是否相同。逐字节比完全部内容再下结论，耗时不随第一个不同字节的位置变化，
/// 不会因此泄露口令的内容。
fn same_token(expected: &[u8], given: &[u8]) -> bool {
    expected.len() == given.len() && expected.iter().zip(given).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// 解开百分号编码；`%` 后面不是两位十六进制数时原样保留。
fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) =
                (bytes.get(i + 1).and_then(|&b| hex(b)), bytes.get(i + 2).and_then(|&b| hex(b)))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// shell 报告的路径。Unix 上按原样的字节；Windows 上的路径不是字节串，按 UTF-8 解，解不开的字换成
/// 替换符。
#[cfg(unix)]
fn os_string(bytes: Vec<u8>) -> std::ffi::OsString {
    std::os::unix::ffi::OsStringExt::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_string(bytes: Vec<u8>) -> std::ffi::OsString {
    String::from_utf8_lossy(&bytes).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_compared_whole() {
        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abc", b"abd") && !same_token(b"abc", b"ab") && !same_token(b"", b"a"));
    }

    #[test]
    fn percent_escapes_that_are_not_hex_stay() {
        assert_eq!(percent_decode(b"a%2fb%zz%4"), b"a/b%zz%4");
    }

    #[test]
    fn reads_the_fields_it_knows() {
        assert_eq!(parse("t", b"t;cwd=/a%20b"), Some(Report::Cwd(Some("/a b".into()))));
        assert_eq!(parse("t", b"t;command="), Some(Report::Command(None)));
        assert_eq!(parse("t", b"t;version=x"), None);
        assert_eq!(parse("t", b"t;other=1"), None);
        assert_eq!(parse("t", b"u;end="), None);
        assert_eq!(parse("t", b"cwd=/x"), None);
    }
}
