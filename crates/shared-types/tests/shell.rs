//! 按程序名认出 shell。

use runode_shared_types::shell::Shell;

#[test]
fn shells_are_recognized_by_program_name() {
    assert_eq!(Shell::detect("/bin/zsh"), Some(Shell::Zsh));
    assert_eq!(Shell::detect("/opt/homebrew/bin/fish"), Some(Shell::Fish));
    assert_eq!(Shell::detect("-bash"), Some(Shell::Bash));
    assert_eq!(Shell::detect("/bin/sh"), None);
}
