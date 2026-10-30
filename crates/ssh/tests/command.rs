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
