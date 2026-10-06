//! 随快照读进来的仓库信息：分支、HEAD、上游和远端，做到一半的合并、变基和拣选，以及 stash 列表。

mod common;

use common::TestRepo;
use runode_git::{Operation, RepoInfo, Stash, UntrackedCache, snapshot};

fn info(repo: &TestRepo) -> RepoInfo {
    snapshot(repo.path(), &mut UntrackedCache::default()).unwrap().info
}

#[test]
fn reads_repo_info() {
    let repo = TestRepo::new("info");
    // 还没有提交。
    let empty = info(&repo);
    assert_eq!((empty.branch.as_deref(), empty.head.as_deref(), empty.has_remote), (Some("main"), None, false));

    repo.commit_file("a.txt", "one\n", "init");
    let first = info(&repo);
    assert_eq!(first.head, Some(repo.git(&["rev-parse", "--short", "HEAD"])));
    assert_eq!((first.upstream.as_deref(), first.operation), (None, None));

    // 本地 bare 远端：推上去设上游，再在远端那边多一个提交、本地多两个提交。
    let remote = TestRepo::bare("info-remote");
    repo.git(&["remote", "add", "origin", &remote.path().to_string_lossy()]);
    repo.git(&["push", "-q", "-u", "origin", "main"]);
    let other = TestRepo::clone_of(&remote, "info-other");
    other.commit_file("b.txt", "b\n", "remote");
    other.git(&["push", "-q", "origin", "main"]);
    repo.commit_file("a.txt", "two\n", "local 1");
    repo.commit_file("a.txt", "three\n", "local 2");
    repo.git(&["fetch", "-q"]);
    let tracking = info(&repo);
    assert!(tracking.has_remote);
    assert_eq!(tracking.upstream.as_deref(), Some("origin/main"));
    assert_eq!((tracking.ahead, tracking.behind), (2, 1));

    // 合并停在冲突上。
    other.commit_file("a.txt", "theirs\n", "conflict");
    other.git(&["push", "-q", "origin", "main"]);
    repo.git(&["fetch", "-q"]);
    assert!(repo.try_git(&["merge", "-q", "origin/main"]).is_none());
    assert_eq!(info(&repo).operation, Some(Operation::Merge));
    repo.git(&["merge", "--abort"]);

    // 分离头指针。
    repo.git(&["checkout", "-q", "--detach", "HEAD~1"]);
    let detached = info(&repo);
    assert_eq!((detached.branch, detached.upstream), (None, None));
    assert_eq!(detached.head, Some(repo.git(&["rev-parse", "--short", "HEAD"])));
}

#[test]
fn reads_rebase_and_cherry_pick_markers() {
    let repo = TestRepo::new("info-ops");
    repo.commit_file("a.txt", "one\n", "init");
    repo.git(&["switch", "-q", "-c", "side"]);
    repo.commit_file("a.txt", "side\n", "side");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_file("a.txt", "main\n", "main");
    assert!(repo.try_git(&["cherry-pick", "side"]).is_none());
    assert_eq!(info(&repo).operation, Some(Operation::CherryPick));
    repo.git(&["cherry-pick", "--abort"]);
    assert!(repo.try_git(&["rebase", "side"]).is_none());
    assert_eq!(info(&repo).operation, Some(Operation::Rebase));
    repo.git(&["rebase", "--abort"]);
    assert_eq!(info(&repo).operation, None);
}

#[test]
fn lists_stashes_newest_first() {
    let repo = TestRepo::new("info-stash");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = common::read(&repo).repo();
    std::fs::write(repo.path().join("a.txt"), "two\n").unwrap();
    handle.stash(Some("first"), false).unwrap();
    std::fs::write(repo.path().join("a.txt"), "three\n").unwrap();
    handle.stash(None, false).unwrap();
    let stashes = info(&repo).stashes;
    assert_eq!(stashes.len(), 2);
    assert_eq!(stashes[1], Stash { index: 1, message: "On main: first".into() });
    assert!(stashes[0].message.starts_with("WIP on main: "), "{:?}", stashes[0]);
}
