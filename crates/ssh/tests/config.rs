//! 从 `~/.ssh/config` 里列主机：展开 Include，跳过通配和取反的模式，去重。

mod common;

use std::fs;

use common::TempDir;
use runode_ssh::list_hosts;

#[test]
fn lists_concrete_hosts_through_includes() {
    let home = TempDir::new("config");
    let ssh = home.0.join(".ssh");
    fs::create_dir_all(ssh.join("conf.d")).unwrap();
    fs::write(
        ssh.join("config"),
        "# comment\nInclude conf.d/*.conf\nHost devbox gpu-*\n  HostName 10.0.0.2\nHost=\"with space\" !bad\nMatch host x\nHost *\n  User me\nHost devbox\n",
    )
    .unwrap();
    fs::write(ssh.join("conf.d/a.conf"), "host alpha\n").unwrap();
    fs::write(ssh.join("conf.d/b.conf"), "Host beta\nInclude ~/.ssh/config\n").unwrap();
    fs::write(ssh.join("conf.d/c.txt"), "Host skipped\n").unwrap();
    assert_eq!(list_hosts(&home.0), ["alpha", "beta", "devbox", "with space"]);
    assert!(list_hosts(&home.0.join("nowhere")).is_empty());
}

#[test]
fn includes_expand_wildcards_in_every_segment() {
    let home = TempDir::new("config-dirs");
    let ssh = home.0.join(".ssh");
    for dir in ["hosts/work", "hosts/home", "hosts/.hidden"] {
        fs::create_dir_all(ssh.join(dir)).unwrap();
        let name = dir.rsplit('/').next().unwrap().trim_start_matches('.');
        fs::write(ssh.join(dir).join("config"), format!("Host {name}\n")).unwrap();
    }
    fs::write(ssh.join("config"), "Include hosts/*/config ~/.ssh/h?sts/w*/config\n").unwrap();
    assert_eq!(list_hosts(&home.0), ["home", "work"]);
}
