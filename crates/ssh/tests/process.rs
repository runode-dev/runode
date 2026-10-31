use std::{
    io::{Read as _, Write as _},
    process::{Command, Stdio},
};

use runode_ssh::process::{Piped, Pipes, output, spawn};

/// 原样把 stdin 抄到 stdout 的程序。
fn echo() -> Command {
    if cfg!(windows) {
        let mut command = Command::new("findstr");
        command.arg("^");
        command
    } else {
        Command::new("cat")
    }
}

/// 写进去的子进程读得到，关掉 stdin 后它退出、这边读到结尾：子进程那头没有留在 `Command` 里。
#[test]
fn pipes_reach_the_child_and_close_with_it() {
    let mut command = echo();
    let piped = spawn(&mut command, Pipes { stdin: true, stdout: true, stderr: false }).unwrap();
    let Piped { mut child, stdin, stdout, stderr } = piped;
    assert!(stderr.is_none());
    stdin.unwrap().write_all(b"hello\n").unwrap();
    let mut out = String::new();
    stdout.unwrap().read_to_string(&mut out).unwrap();
    assert_eq!(out.trim_end(), "hello");
    assert!(child.wait().unwrap().success());
}

#[test]
fn output_collects_both_streams() {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/c", "echo out& echo err 1>&2& exit 3"]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", "echo out; echo err >&2; exit 3"]);
        command
    };
    let output = output(command.stdin(Stdio::null())).unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), "out");
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), "err");
    assert_eq!(output.status.code(), Some(3));
}
