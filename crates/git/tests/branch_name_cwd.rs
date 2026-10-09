//! 检查分支名不依赖进程的当前目录。要把整个进程的当前目录换到一个删掉了的目录，会影响同一个测试
//! 程序里并行跑的别的测试，所以单独一个文件、只有这一个测试。

use runode_git::valid_branch_name;

#[test]
fn checks_branch_names_from_a_deleted_working_directory() {
    let dir = std::env::temp_dir().join(format!("runode-git-gone-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_current_dir(&dir).unwrap();
    std::fs::remove_dir(&dir).unwrap();
    // 在当前目录里跑 git 时它连当前目录都读不到，合法的名字也判成不合法。
    assert!(valid_branch_name("feat/x"));
    assert!(!valid_branch_name("x..y"));
}
