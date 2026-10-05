//! 按命令历史统计各命令和子命令的常用程度。

use runode_completion::usage::Usage;

#[test]
fn counts_commands_and_subcommands() {
    let usage = Usage::from_commands([
        "git status",
        "git status -s",
        "git -C x log",
        "FOO=1 /usr/bin/git commit -m x",
        "cargo build",
    ]);
    assert_eq!(usage.command("git"), 4);
    assert_eq!(usage.command("cargo"), 1);
    assert_eq!(usage.command("ls"), 0);
    assert_eq!(usage.subcommand("git", "status"), 2);
    // `-C` 后面的 `x` 被当成子命令：统计是粗略的，只用来排先后。
    assert_eq!(usage.subcommand("git", "x"), 1);
    assert_eq!(usage.subcommand("git", "commit"), 1);
}
