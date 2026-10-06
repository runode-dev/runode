//! 按 shell 集成标出的提示符读某条命令的输出（`read --command`）。

mod common;

use common::{PROMPT, idle_host};
use runode_shared_types::{grid::GridSize, settings::TermSettings};
use runode_terminal::host_session::HostSession;

/// 40 列 12 行的宿主会话。
fn host() -> HostSession {
    let mut session = idle_host();
    session.resize(GridSize { cols: 40, rows: 12, cell_width_px: 8, cell_height_px: 16 });
    session
}

/// 在提示符上敲 `command` 回车，shell 报告开始运行；接着是 `output`。
fn run(command: &str, output: &str) -> Vec<u8> {
    let mut bytes = PROMPT.to_vec();
    bytes.extend_from_slice(command.as_bytes());
    bytes.extend_from_slice(b"\r\n\x1b]133;C\x07");
    bytes.extend_from_slice(output.as_bytes());
    bytes
}

/// 命令结束（退出码 `exit`）。
fn end(exit: i32) -> Vec<u8> {
    format!("\x1b]133;D;{exit}\x07").into_bytes()
}

#[test]
fn the_last_commands_are_counted_from_the_bottom() {
    let mut session = host();
    session.feed(&run("echo hi", "hi\r\n"));
    session.feed(&end(0));
    session.feed(&run("ls", "a\r\nb\r\n\r\n"));
    session.feed(&end(1));
    // 回到提示符，用户已经敲了一半：正等着输入的提示符不算一条命令。
    session.feed(PROMPT);
    session.feed(b"git st");
    assert_eq!(session.command_output(1).unwrap(), ("a\nb\n".into(), false));
    assert_eq!(session.command_output(2).unwrap(), ("hi\n".into(), false));
    let err = session.command_output(3).unwrap_err().to_string();
    assert!(err.contains("only 2 commands"), "{err}");
}

#[test]
fn a_running_command_is_read_to_the_bottom() {
    let mut session = host();
    session.feed(&run("echo one", "one\r\n"));
    session.feed(&end(0));
    session.feed(&run("make", ""));
    // 刚回车、还没输出时是空的，不会读成上一条。
    assert_eq!(session.command_output(1).unwrap(), (String::new(), false));
    session.feed(b"compiling\r\nstill going");
    assert_eq!(session.command_output(1).unwrap(), ("compiling\nstill going\n".into(), false));
    assert_eq!(session.command_output(2).unwrap(), ("one\n".into(), false));
}

#[test]
fn a_long_command_line_is_not_part_of_the_output() {
    let mut session = host();
    // 输入比一行长，软折行到第二行。
    session.feed(&run(&"x".repeat(50), "out\r\n"));
    session.feed(&end(0));
    session.feed(PROMPT);
    assert_eq!(session.command_output(1).unwrap(), ("out\n".into(), false));
}

#[test]
fn without_marks_it_asks_for_shell_integration() {
    let mut session = host();
    session.feed(b"$ ls\r\na b c\r\n$ ");
    let err = session.command_output(1).unwrap_err().to_string();
    assert!(err.contains("needs shell integration"), "{err}");
    // 全屏程序占着屏幕时也读不了。
    session.feed(&run("vim", "\x1b[?1049h~"));
    let err = session.command_output(1).unwrap_err().to_string();
    assert!(err.contains("full-screen"), "{err}");
}

#[test]
fn output_whose_start_scrolled_away_is_marked_truncated() {
    let mut session = host();
    // 不留回滚历史：输出一长，命令的提示符就被挤掉了。
    session.apply_theme(&TermSettings { scrollback_limit: 0, ..TermSettings::default() });
    let output: String = (1..=20).map(|i| format!("line {i}\r\n")).collect();
    session.feed(&run("seq 20", &output));
    session.feed(&end(0));
    session.feed(PROMPT);
    let (text, truncated) = session.command_output(1).unwrap();
    assert!(truncated);
    assert!(text.ends_with("line 19\nline 20\n"), "{text:?}");
    assert!(!text.contains("seq 20"), "{text:?}");
    // 再往前就什么都没了。
    assert!(session.command_output(2).is_err());
}
