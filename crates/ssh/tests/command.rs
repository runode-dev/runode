//! 拼出来的 ssh 命令行。

use std::{path::PathBuf, process::Command};

use runode_ssh::{Destination, Ssh};

fn args(command: &Command) -> Vec<String> {
    command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
}

/// 建连接的调用不重复绑定用户配置里的转发；加转发的那条不能清，清了要加的那条也没了。
#[test]
fn only_connecting_commands_clear_forwardings() {
    let ssh = Ssh { program: PathBuf::from("ssh"), control: Some(PathBuf::from("/tmp/c/%C")), persist_secs: 60 };
    let dest = Destination::parse("me@box").unwrap();
    let clears = |command: &Command| args(command).iter().any(|arg| arg == "ClearAllForwardings=yes");
    assert!(clears(&ssh.master(&dest, true)));
    assert!(clears(&ssh.exec(&dest, "true", true)));
    let forward = ssh.forward(&dest, 4000, 3000, false);
    assert!(!clears(&forward));
    assert!(args(&forward).windows(2).any(|pair| pair == ["-L", "127.0.0.1:4000:localhost:3000"]));
}

/// 远端命令是最后一个参数，选项都在目标之前：目标之后的参数会被算进远端命令。
#[test]
fn options_come_before_the_destination() {
    let ssh = Ssh { program: PathBuf::from("ssh"), control: None, persist_secs: 60 };
    let dest = Destination::parse("me@box:2222").unwrap();
    let args = args(&ssh.exec_compressed(&dest, "uptime", true));
    assert_eq!(args.last().map(String::as_str), Some("uptime"));
    let target = args.iter().position(|arg| arg == "me@box").unwrap();
    let compression = args.iter().position(|arg| arg == "Compression=yes").unwrap();
    assert!(compression < target, "{args:?}");
}
