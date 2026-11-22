//! 起 git 时用的 `PATH`（`set_path`）：git 拉起的过滤器和钩子照它找程序。设了就改不回来，单独一个测试文件。
#![cfg(unix)]

mod common;

use std::{os::unix::fs::PermissionsExt, path::PathBuf};

use common::TestRepo;
use runode_git::Repo;

/// 检出要跑的过滤器只装在本进程 `PATH` 以外的目录里（像从访达打开的 app 找不到的 brew 目录）：设 `set_path`
/// 之前开工作树失败，设了以后开得出来。
#[test]
fn filters_run_from_the_given_path() {
    let repo = TestRepo::new("path-filter");
    repo.write(".gitattributes", "*.txt filter=only-there\n");
    repo.write("a.txt", "hello\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "init"]);
    let bin = repo.path().with_extension("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let filter = bin.join("only-there-filter");
    std::fs::write(&filter, "#!/bin/sh\nexec cat\n").unwrap();
    std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o755)).unwrap();
    repo.git(&["config", "filter.only-there.smudge", "only-there-filter"]);
    repo.git(&["config", "filter.only-there.required", "true"]);
    let opened = Repo::open(repo.path()).unwrap();
    let worktree = |name: &str| -> PathBuf { repo.path().with_extension(name) };

    assert!(opened.add_worktree(&worktree("wt1"), "one", None).is_err());

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(bin.clone()).chain(std::env::split_paths(&inherited));
    runode_git::set_path(std::env::join_paths(dirs).unwrap());
    assert!(opened.add_worktree(&worktree("wt2"), "two", None).is_ok());
    assert_eq!(std::fs::read_to_string(worktree("wt2").join("a.txt")).unwrap(), "hello\n");

    for dir in [bin, worktree("wt1"), worktree("wt2")] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
