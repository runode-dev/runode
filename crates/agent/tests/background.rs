//! 哪些进程算 agent 用来跑命令的 shell。

use runode_agent::{ForegroundProcess, background::is_tool_shell};

fn process(name: &str, argv: &[&str]) -> ForegroundProcess {
    ForegroundProcess {
        pid: 1,
        name: name.into(),
        exe: None,
        argv0: argv.first().map(|arg| (*arg).into()),
        argv: Some(argv.iter().map(|arg| (*arg).into()).collect()),
    }
}

#[test]
fn shells_running_a_command_are_tool_shells() {
    assert!(is_tool_shell(&process("zsh", &["/bin/zsh", "-c", "-l", "npm run dev"])));
    assert!(is_tool_shell(&process("bash", &["bash", "-lc", "make"])));
    assert!(is_tool_shell(&process("fish", &["fish", "-c", "make"])));
}

#[test]
fn other_processes_are_not_tool_shells() {
    assert!(!is_tool_shell(&process("sh", &["sh"])));
    assert!(!is_tool_shell(&process("bash", &["bash", "--norc"])));
    assert!(!is_tool_shell(&process("bash", &["bash", "--rcfile", "x"])));
    assert!(!is_tool_shell(&process("node", &["node", "-c", "x"])));
    assert!(!is_tool_shell(&process("cmd.exe", &["cmd", "/c", "dir"])));
    assert!(!is_tool_shell(&ForegroundProcess { argv: None, ..process("zsh", &[]) }));
}
