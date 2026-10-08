mod common;

use std::{path::PathBuf, sync::atomic::AtomicBool};

use common::TestRepo;
use runode_git::{GrepMatch, grep, list_files};

#[test]
fn lists_tracked_and_untracked_files_but_not_ignored_ones() {
    let repo = TestRepo::new("search-list");
    repo.commit_file(".gitignore", "build/\n", "init");
    repo.write("src/main.rs", "fn main() {}\n");
    repo.write("build/out.o", "x");
    let mut files = list_files(repo.path()).unwrap();
    files.sort();
    assert_eq!(files, [PathBuf::from(".gitignore"), PathBuf::from("src/main.rs")]);
    assert!(list_files(&std::env::temp_dir()).is_none());
}

#[test]
fn greps_literal_text_and_stops_at_the_limit() {
    let repo = TestRepo::new("search-grep");
    repo.commit_file(".gitignore", "build/\n", "init");
    repo.write("a.txt", "Hello (world)\r\nbye\nhello again\n");
    repo.write("build/b.txt", "hello\n");
    let hit = |path: &str, line, text: &str| GrepMatch { path: path.into(), line, text: text.into() };
    // 括号按字面找，不当正则；被忽略的目录不找；CRLF 行尾的 `\r` 去掉。
    assert_eq!(grep(repo.path(), "(world)", false, 10, &AtomicBool::new(false)), [hit("a.txt", 1, "Hello (world)")]);
    assert_eq!(grep(repo.path(), "hello", false, 10, &AtomicBool::new(false)), [hit("a.txt", 3, "hello again")]);
    assert_eq!(grep(repo.path(), "hello", true, 10, &AtomicBool::new(false)).len(), 2);
    assert_eq!(grep(repo.path(), "hello", true, 1, &AtomicBool::new(false)).len(), 1);
    assert!(grep(repo.path(), "", true, 10, &AtomicBool::new(false)).is_empty());
    // 取消了的立刻返回。
    assert!(grep(repo.path(), "hello", true, 10, &AtomicBool::new(true)).len() <= 2);
}

#[test]
fn greps_a_directory_outside_any_repo() {
    let repo = TestRepo::new("search-plain");
    repo.write("note.md", "needle\n");
    std::fs::remove_dir_all(repo.path().join(".git")).unwrap();
    assert_eq!(grep(repo.path(), "needle", false, 10, &AtomicBool::new(false)).len(), 1);
}
