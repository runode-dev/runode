//! 分支：名字的合法性检查，建分支、切分支和检出远端分支。

mod common;

use common::TestRepo;
use runode_git::{current_branch, origin_url, valid_branch_name};

#[test]
fn checks_branch_names() {
    assert!(valid_branch_name("feat/x"));
    assert!(valid_branch_name("修复"));
    for name in ["", "a b", "-x", "x..y", "@{-1}", "HEAD", "x.lock", "a~1", "x/"] {
        assert!(!valid_branch_name(name), "{name}");
    }
}

#[test]
fn reads_current_branch_and_origin() {
    let repo = TestRepo::new("current-branch");
    // 还没有提交时也认得出分支。
    assert_eq!(current_branch(repo.path()).as_deref(), Some("main"));
    repo.commit_file("a.txt", "one\n", "init");
    let sha = repo.git(&["rev-parse", "--short", "HEAD"]);
    repo.git(&["checkout", "-q", "--detach"]);
    assert_eq!(current_branch(repo.path()), Some(sha));
    assert_eq!(origin_url(repo.path()), None);
    repo.git(&["remote", "add", "origin", "git@github.com:runode-dev/runode.git"]);
    assert_eq!(origin_url(repo.path()).as_deref(), Some("git@github.com:runode-dev/runode.git"));
    assert_eq!(current_branch(&repo.path().join("..")), None);
}

#[test]
fn creates_and_switches_branches() {
    let repo = TestRepo::new("branches");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = common::read(&repo).repo();
    handle.create_branch("feat", None).unwrap();
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
    assert!(handle.create_branch("feat", None).is_err());
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

/// 有和远端分支同名的标签（`origin/topic`）时，按简写起点建跟踪分支有歧义；照样建得出来。
#[test]
fn checks_out_a_remote_branch_shadowed_by_a_tag() {
    let remote = TestRepo::bare("branches-tag-remote");
    let upstream = TestRepo::clone_of(&remote, "branches-tag-upstream");
    upstream.commit_file("a.txt", "one\n", "init");
    upstream.git(&["push", "-q", "origin", "main"]);
    upstream.git(&["switch", "-q", "-c", "topic"]);
    upstream.commit_file("t.txt", "t\n", "topic work");
    upstream.git(&["push", "-q", "origin", "topic"]);

    let repo = TestRepo::clone_of(&remote, "branches-tag-local");
    repo.git(&["tag", "origin/topic", "main"]);
    let handle = common::read(&repo).repo();
    let topic = handle.branches().into_iter().find(|branch| branch.name == "origin/topic").unwrap();
    handle.checkout(&topic).unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "topic");
    assert_eq!(repo.git(&["config", "branch.topic.merge"]), "refs/heads/topic");
    assert_eq!(repo.git(&["rev-parse", "topic"]), repo.git(&["rev-parse", "refs/remotes/origin/topic"]));
}

/// 远端上 `-` 开头的分支（`update-ref` 建得出来）切掉远端名后不当成 `git switch` 的选项。
#[test]
fn refuses_a_remote_branch_named_like_an_option() {
    let remote = TestRepo::bare("branches-dash-remote");
    let upstream = TestRepo::clone_of(&remote, "branches-dash-upstream");
    upstream.commit_file("a.txt", "one\n", "init");
    upstream.git(&["push", "-q", "origin", "main"]);
    upstream.git(&["push", "-q", "origin", "main:refs/heads/-d"]);

    let repo = TestRepo::clone_of(&remote, "branches-dash-local");
    repo.git(&["update-ref", "refs/heads/-d", "HEAD"]);
    let handle = common::read(&repo).repo();
    let dash = handle.branches().into_iter().find(|branch| branch.name == "origin/-d").unwrap();
    assert!(handle.checkout(&dash).is_err());
    assert_eq!(repo.git(&["branch", "--show-current"]), "main");
}
