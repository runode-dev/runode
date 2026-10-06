//! 分支：名字的合法性检查，建分支、切分支和检出远端分支。

mod common;

use common::TestRepo;
use runode_git::valid_branch_name;

#[test]
fn checks_branch_names() {
    assert!(valid_branch_name("feat/x"));
    assert!(valid_branch_name("修复"));
    for name in ["", "a b", "-x", "x..y", "@{-1}", "HEAD", "x.lock", "a~1", "x/"] {
        assert!(!valid_branch_name(name), "{name}");
    }
}

#[test]
fn creates_and_switches_branches() {
    let repo = TestRepo::new("branches");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = common::read(&repo).repo();
    handle.create_branch("feat").unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "feat");
    repo.commit_file("b.txt", "b\n", "on feat");

    let branches = handle.branches();
    let names: Vec<_> = branches.iter().map(|branch| (branch.name.as_str(), branch.current)).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&("feat", true)) && names.contains(&("main", false)));
    let feat = branches.iter().find(|branch| branch.name == "feat").unwrap();
    assert_eq!(feat.subject, "on feat");
    assert!(!feat.date.is_empty());

    let main = branches.iter().find(|branch| branch.name == "main").unwrap();
    handle.checkout(main).unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "main");
    assert!(handle.create_branch("feat").is_err());
}

#[test]
fn checks_out_remote_branches_with_tracking() {
    let remote = TestRepo::bare("branches-remote");
    let upstream = TestRepo::clone_of(&remote, "branches-upstream");
    upstream.commit_file("a.txt", "one\n", "init");
    upstream.git(&["push", "-q", "origin", "main"]);
    upstream.git(&["switch", "-q", "-c", "topic"]);
    upstream.commit_file("t.txt", "t\n", "topic work");
    upstream.git(&["push", "-q", "origin", "topic"]);

    let repo = TestRepo::clone_of(&remote, "branches-local");
    let handle = common::read(&repo).repo();
    let branches = handle.branches();
    let mut names: Vec<_> = branches.iter().map(|branch| (branch.name.as_str(), branch.remote)).collect();
    // 本地分支在前；`origin/HEAD` 不算。两个提交可能在同一秒，远端分支之间的先后不定。
    assert_eq!(names[0], ("main", false));
    names[1..].sort();
    assert_eq!(names[1..], [("origin/main", true), ("origin/topic", true)]);
    assert_eq!(branches[0].upstream.as_deref(), Some("origin/main"));
    let find = |name: &str| branches.iter().find(|branch| branch.name == name).unwrap();

    handle.checkout(find("origin/topic")).unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "topic");
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "topic@{upstream}"]), "origin/topic");

    // 本地已经有同名分支，就切过去而不是再建。
    handle.checkout(find("origin/main")).unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "main");
    handle.checkout(find("origin/topic")).unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "topic");
}
