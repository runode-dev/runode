//! `Repo` 的写操作：暂存、撤回暂存、丢弃、提交、撤销提交、同步远端和 stash，含嵌套的仓库。

mod common;

use std::{fs, path::PathBuf};

use common::{TestRepo, read, read_all};
use runode_git::{CommitOptions, Section};

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn stages_and_unstages_files() {
    let repo = TestRepo::new("ops-stage");
    repo.commit_file("a.txt", "one\n", "init");
    repo.commit_file("b.txt", "b\n", "b");
    repo.write("a.txt", "two\n");
    fs::remove_file(repo.path().join("b.txt")).unwrap();
    repo.write("dir/c [1].txt", "c\n");
    let handle = read(&repo).repo();
    handle.stage(&paths(&["a.txt", "b.txt", "dir/c [1].txt"])).unwrap();
    assert_eq!(repo.status(), ["M  a.txt", "D  b.txt", "A  \"dir/c [1].txt\""]);
    handle.unstage(&paths(&["a.txt", "b.txt", "dir/c [1].txt"])).unwrap();
    assert_eq!(repo.status(), [" M a.txt", " D b.txt", "?? \"dir/c [1].txt\""]);

    handle.stage_all().unwrap();
    assert_eq!(repo.status(), ["M  a.txt", "D  b.txt", "A  \"dir/c [1].txt\""]);
    handle.unstage_all().unwrap();
    assert_eq!(repo.status(), [" M a.txt", " D b.txt", "?? \"dir/c [1].txt\""]);

    assert!(handle.stage(&paths(&["../outside.txt"])).is_err());
    assert!(handle.stage(&paths(&["missing.txt"])).is_err());
}

#[test]
fn unstages_before_the_first_commit() {
    let repo = TestRepo::new("ops-unborn");
    repo.write("a.txt", "one\n");
    repo.write("b.txt", "b\n");
    let handle = read(&repo).repo();
    handle.stage(&paths(&["a.txt", "b.txt"])).unwrap();
    assert_eq!(repo.status(), ["A  a.txt", "A  b.txt"]);
    // 还没有 HEAD 时暂存段和空树比，暂存了的新文件也在里面。
    assert_eq!(common::paths_of(&read(&repo).staged), ["a.txt", "b.txt"]);
    let staged = handle.file_view("a.txt".as_ref(), None, &runode_git::DiffSide::Index).unwrap().unwrap();
    assert_eq!(staged.new_lines, ["one"]);
    // 暂存后又改过，暂存区和工作区、HEAD 都不一样也能撤回。
    repo.write("a.txt", "two\n");
    handle.unstage(&paths(&["a.txt"])).unwrap();
    assert_eq!(repo.status(), ["A  b.txt", "?? a.txt"]);
    assert_eq!(repo.read("a.txt"), "two\n");
    handle.unstage_all().unwrap();
    assert_eq!(repo.status(), ["?? a.txt", "?? b.txt"]);
    handle.unstage_all().unwrap();
}

#[test]
fn stages_inside_nested_repositories() {
    let repo = TestRepo::new("ops-nested");
    repo.commit_file("top.txt", "top\n", "init");
    let inner = repo.nested("wt/inner");
    repo.write("wt/inner/a.txt", "two\n");
    repo.write("wt/inner/new.txt", "n\n");
    repo.write("top.txt", "changed\n");
    let handle = read(&repo).repo();
    // 顶层仓库的句柄拿到嵌套仓库里的路径，转到那个仓库里暂存。
    handle.stage(&paths(&["wt/inner/a.txt"])).unwrap();
    let inner_status = || repo.git(&["-C", "wt/inner", "status", "--porcelain=v1", "--untracked-files=all"]);
    assert_eq!(inner_status(), "M  a.txt\n?? new.txt");
    assert_eq!(repo.status(), [" M top.txt", "?? wt/inner/"]);

    // 全部暂存只管自己这个仓库，也不把嵌套的仓库当子模块记进来。
    handle.stage_all().unwrap();
    assert_eq!(inner_status(), "M  a.txt\n?? new.txt");
    assert_eq!(repo.status(), ["M  top.txt", "?? wt/inner/"]);
    handle.unstage_all().unwrap();
    assert_eq!(inner_status(), "M  a.txt\n?? new.txt");
    assert_eq!(repo.status(), [" M top.txt", "?? wt/inner/"]);

    // 嵌套仓库自己的句柄管它自己的。
    let inner_handle = read_all(&repo).subs[0].repo();
    inner_handle.stage_all().unwrap();
    assert_eq!(inner_status(), "M  a.txt\nA  new.txt");
    inner_handle.unstage_all().unwrap();
    assert_eq!(inner_status(), " M a.txt\n?? new.txt");
    assert_eq!(repo.status(), [" M top.txt", "?? wt/inner/"]);

    handle.commit("top", CommitOptions { stage_all: true, ..Default::default() }).unwrap();
    assert_eq!(repo.git(&["ls-files"]), "top.txt");
    assert!(inner.join(".git").exists());
}

#[test]
fn discards_unstaged_changes() {
    let repo = TestRepo::new("ops-discard");
    repo.commit_file("a.txt", "one\n", "init");
    repo.commit_file("b.txt", "b\n", "b");
    repo.commit_file("keep/k.txt", "k\n", "k");
    // a.txt 暂存了 two，工作区里又改成 three；b.txt 删了；还有未跟踪和 `add -N` 的文件。
    repo.write("a.txt", "two\n");
    repo.git(&["add", "a.txt"]);
    repo.write("a.txt", "three\n");
    fs::remove_file(repo.path().join("b.txt")).unwrap();
    repo.write("d/e/new.txt", "n\n");
    repo.write("keep/new.txt", "n\n");
    repo.write("ita.txt", "i\n");
    repo.git(&["add", "-N", "ita.txt"]);
    let snapshot = read(&repo);
    assert_eq!(snapshot.unstaged.len(), 5, "{:?}", snapshot.unstaged);
    snapshot.repo().discard(&snapshot.unstaged).unwrap();
    assert_eq!(repo.status(), ["M  a.txt"]);
    assert_eq!(repo.read("a.txt"), "two\n");
    assert_eq!(repo.read("b.txt"), "b\n");
    // 删掉文件后变空的目录一起删掉，原本就有别的文件的目录留着。
    assert!(!repo.path().join("d").exists());
    assert!(repo.path().join("keep/k.txt").exists());
    assert!(!repo.path().join("ita.txt").exists());
}

#[test]
fn discards_inside_nested_repositories() {
    let repo = TestRepo::new("ops-discard-nested");
    repo.commit_file("top.txt", "top\n", "init");
    repo.nested("wt/inner");
    repo.write("wt/inner/a.txt", "changed\n");
    repo.write("wt/inner/extra.txt", "x\n");
    let repos = read_all(&repo);
    assert!(repos.main.unstaged.is_empty());
    let inner = &repos.subs[0];
    inner.repo().discard(inner.files(Section::Unstaged)).unwrap();
    assert_eq!(repo.read("wt/inner/a.txt"), "one\n");
    assert!(!repo.path().join("wt/inner/extra.txt").exists());
    assert!(repo.path().join("wt/inner/.git").exists());
}

#[test]
fn reads_pending_diff_and_recent_messages() {
    let repo = TestRepo::new("ops-pending");
    let handle = read(&repo).repo();
    assert_eq!(handle.recent_messages(5).unwrap(), Vec::<String>::new());
    repo.commit_file("a.txt", "one\n", "first\n\nbody");
    repo.commit_file("a.txt", "two\n", "second");
    assert_eq!(handle.recent_messages(5).unwrap(), ["second", "first\n\nbody"]);
    assert_eq!(handle.recent_messages(1).unwrap(), ["second"]);

    repo.write("a.txt", "three\n");
    repo.write("new.txt", "n\n");
    assert_eq!(handle.pending_diff(true).unwrap(), "");
    let all = handle.pending_diff(false).unwrap();
    assert!(all.contains("-two\n+three\n") && all.ends_with("untracked: new.txt\n"), "{all}");
    handle.stage(&paths(&["a.txt"])).unwrap();
    let staged = handle.pending_diff(true).unwrap();
    assert!(staged.contains("+three\n") && !staged.contains("new.txt"), "{staged}");
}

#[test]
fn commits_and_amends() {
    let repo = TestRepo::new("ops-commit");
    let handle = read(&repo).repo();
    assert_eq!(repo.try_git(&["log", "-1", "--format=%B"]), None);
    repo.write("a.txt", "one\n");
    handle.stage(&paths(&["a.txt"])).unwrap();
    let message = "first\n\nwith a body, \"quotes\" and\n-dashes";
    handle.commit(message, CommitOptions::default()).unwrap();
    assert_eq!(repo.git(&["log", "-1", "--format=%B"]), message);

    // 没有暂存的改动时报 git 的错。
    let error = handle.commit("nothing", CommitOptions::default()).unwrap_err();
    assert!(!error.message.is_empty());
    assert!(!error.message.lines().any(|line| line.starts_with("hint:")), "{error}");

    // 说明为空的 amend 沿用原来的说明，只把新暂存的改动并进去。
    repo.write("b.txt", "b\n");
    handle.stage(&paths(&["b.txt"])).unwrap();
    handle.commit("", CommitOptions { amend: true, ..Default::default() }).unwrap();
    assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(repo.git(&["ls-files"]), "a.txt\nb.txt");
    assert_eq!(repo.git(&["log", "-1", "--format=%B"]), message);

    handle.commit("renamed", CommitOptions { amend: true, ..Default::default() }).unwrap();
    assert_eq!(repo.git(&["log", "-1", "--format=%B"]), "renamed");
    assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "1");

    // 先全部暂存再提交，含未跟踪的文件。
    repo.write("a.txt", "two\n");
    repo.write("c.txt", "c\n");
    handle.commit("second", CommitOptions { stage_all: true, ..Default::default() }).unwrap();
    assert!(repo.status().is_empty());
    assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]), "2");
}

#[test]
fn undoes_the_last_commit() {
    let repo = TestRepo::new("ops-undo");
    let handle = read(&repo).repo();
    assert!(handle.undo_last_commit().is_err());
    repo.commit_file("a.txt", "one\n", "first");
    repo.commit_file("b.txt", "b\n", "second\n\nbody");
    assert_eq!(handle.undo_last_commit().unwrap(), "second\n\nbody");
    assert_eq!(repo.git(&["log", "--format=%s"]), "first");
    assert_eq!(repo.status(), ["A  b.txt"]);

    // 只剩一个提交：撤销后回到还没有提交的样子，文件都在暂存区里。
    assert_eq!(handle.undo_last_commit().unwrap(), "first");
    assert!(repo.try_git(&["rev-parse", "--verify", "--quiet", "HEAD"]).is_none());
    assert_eq!(repo.git(&["branch", "--show-current"]), "main");
    assert_eq!(repo.status(), ["A  a.txt", "A  b.txt"]);
    assert_eq!(read(&repo).staged.len(), 2);
}

#[test]
fn pushes_pulls_and_syncs() {
    let remote = TestRepo::bare("ops-remote");
    let repo = TestRepo::new("ops-local");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = read(&repo).repo();
    assert!(handle.push().is_err(), "还没有远端");

    // 有两个远端时选 origin；第一次推送设上游。
    let spare = TestRepo::bare("ops-spare");
    repo.git(&["remote", "add", "aaa", &spare.path().to_string_lossy()]);
    repo.git(&["remote", "add", "origin", &remote.path().to_string_lossy()]);
    handle.push().unwrap();
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "main@{upstream}"]), "origin/main");
    assert_eq!(remote.git(&["log", "--format=%s", "main"]), "init");
    assert!(spare.try_git(&["rev-parse", "--verify", "--quiet", "main"]).is_none());

    // 别人推了一个提交：fetch 后落后一个，pull 后拿到。
    let other = TestRepo::clone_of(&remote, "ops-other");
    other.commit_file("b.txt", "b\n", "from other");
    other.git(&["push", "-q"]);
    handle.fetch().unwrap();
    assert_eq!(read(&repo).info.behind, 1);
    handle.pull().unwrap();
    assert_eq!(repo.read("b.txt"), "b\n");

    // 两边各有新提交：sync 先合并再推上去。
    other.commit_file("c.txt", "c\n", "other again");
    other.git(&["push", "-q"]);
    repo.commit_file("d.txt", "d\n", "local");
    handle.sync().unwrap();
    let info = read(&repo).info;
    assert_eq!((info.ahead, info.behind), (0, 0));
    assert_eq!(repo.read("c.txt"), "c\n");
    other.git(&["pull", "-q"]);
    assert_eq!(other.read("d.txt"), "d\n");

    // 新分支还没有上游：sync 只推送并设上游。
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.commit_file("e.txt", "e\n", "feat");
    handle.sync().unwrap();
    assert_eq!(repo.git(&["rev-parse", "--abbrev-ref", "feat@{upstream}"]), "origin/feat");
}

#[test]
fn stashes_and_restores_changes() {
    let repo = TestRepo::new("ops-stash");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = read(&repo).repo();
    repo.write("a.txt", "two\n");
    handle.stash(Some("tracked only"), false).unwrap();
    assert!(repo.status().is_empty());

    // 不含未跟踪的文件时新文件留着，也就没有可收的改动。
    repo.write("new.txt", "n\n");
    handle.stash(None, false).unwrap();
    assert_eq!(repo.status(), ["?? new.txt"]);
    handle.stash(Some(""), true).unwrap();
    assert!(repo.status().is_empty());
    let stashes = read(&repo).info.stashes;
    assert_eq!(stashes.len(), 2);
    assert_eq!(stashes[1].message, "On main: tracked only");

    handle.stash_apply(1).unwrap();
    assert_eq!(repo.read("a.txt"), "two\n");
    assert_eq!(read(&repo).info.stashes.len(), 2);
    repo.git(&["checkout", "--", "a.txt"]);

    handle.stash_pop(0).unwrap();
    assert_eq!(repo.read("new.txt"), "n\n");
    assert_eq!(read(&repo).info.stashes.len(), 1);
    handle.stash_drop(0).unwrap();
    assert!(read(&repo).info.stashes.is_empty());
    assert!(handle.stash_drop(0).is_err());
}

#[test]
fn stash_keeps_nested_repositories() {
    let repo = TestRepo::new("ops-stash-nested");
    repo.commit_file("top.txt", "top\n", "init");
    let inner = repo.nested("wt/inner");
    repo.write("new.txt", "n\n");
    let handle = read(&repo).repo();
    handle.stash(None, true).unwrap();
    assert!(inner.join(".git").exists() && inner.join("a.txt").exists());
    assert!(!repo.path().join("new.txt").exists());
    handle.stash_pop(0).unwrap();
    assert_eq!(repo.read("new.txt"), "n\n");
    assert!(inner.join(".git").exists() && inner.join("a.txt").exists());
}
