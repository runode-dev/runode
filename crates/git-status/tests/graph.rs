//! 提交历史和图表：lane 怎么排（分叉、合并、多个根、章鱼合并、空出来的 lane 再用），以及从
//! 仓库里读历史、引用、单个提交的改动，切到某个提交和从它新建分支。

mod common;

use std::collections::HashMap;

use common::{TestRepo, paths_of, read};
use runode_git_status::{Commit, FileStatus, GraphLine, Half, RefKind, graph_layout, refs_changed};

fn commit(id: &str, parents: &[&str]) -> Commit {
    Commit {
        id: id.into(),
        parents: parents.iter().map(|&parent| parent.into()).collect(),
        subject: id.into(),
        author: String::new(),
        date: String::new(),
        refs: Vec::new(),
    }
}

fn top(from: usize, to: usize, color: usize) -> GraphLine {
    GraphLine { half: Half::Top, from, to, color }
}

fn bottom(from: usize, to: usize, color: usize) -> GraphLine {
    GraphLine { half: Half::Bottom, from, to, color }
}

/// 每行的点所在的列、颜色、宽度和线段。
fn layout(commits: &[Commit]) -> Vec<(usize, usize, usize, Vec<GraphLine>)> {
    graph_layout(commits).into_iter().map(|row| (row.column, row.color, row.width, row.lines)).collect()
}

#[test]
fn lays_out_a_straight_line() {
    let rows = layout(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])]);
    assert_eq!(
        rows,
        [
            (0, 0, 1, vec![bottom(0, 0, 0)]),
            (0, 0, 1, vec![top(0, 0, 0), bottom(0, 0, 0)]),
            (0, 0, 1, vec![top(0, 0, 0)]),
        ]
    );
}

#[test]
fn lays_out_a_fork() {
    // feat 和 main 都从 b 分出来：后出现的 main 在第二条 lane，下半段并回 b 那条。
    let rows = layout(&[commit("feat", &["b"]), commit("main", &["b"]), commit("b", &["a"]), commit("a", &[])]);
    assert_eq!(
        rows,
        [
            (0, 0, 1, vec![bottom(0, 0, 0)]),
            (1, 1, 2, vec![top(0, 0, 0), bottom(0, 0, 0), bottom(1, 0, 1)]),
            (0, 0, 1, vec![top(0, 0, 0), bottom(0, 0, 0)]),
            (0, 0, 1, vec![top(0, 0, 0)]),
        ]
    );
}

#[test]
fn lays_out_a_merge() {
    // m 合并了 b：第二个父提交另占一条 lane、换一种颜色，b 再斜着并回 a 那条。
    let rows = layout(&[commit("m", &["a", "b"]), commit("b", &["a"]), commit("a", &[])]);
    assert_eq!(
        rows,
        [
            (0, 0, 2, vec![bottom(0, 0, 0), bottom(0, 1, 1)]),
            (1, 1, 2, vec![top(0, 0, 0), top(1, 1, 1), bottom(0, 0, 0), bottom(1, 0, 1)]),
            (0, 0, 1, vec![top(0, 0, 0)]),
        ]
    );
}

#[test]
fn lays_out_an_octopus_merge() {
    let rows = layout(&[commit("o", &["a", "b", "c"]), commit("c", &[]), commit("b", &[]), commit("a", &[])]);
    assert_eq!(rows[0], (0, 0, 3, vec![bottom(0, 0, 0), bottom(0, 1, 1), bottom(0, 2, 2)]));
    assert_eq!(rows[1], (2, 2, 3, vec![top(0, 0, 0), top(1, 1, 1), top(2, 2, 2), bottom(0, 0, 0), bottom(1, 1, 1)]));
    assert_eq!(rows[2], (1, 1, 2, vec![top(0, 0, 0), top(1, 1, 1), bottom(0, 0, 0)]));
    assert_eq!(rows[3], (0, 0, 1, vec![top(0, 0, 0)]));
}

#[test]
fn lays_out_several_roots_and_reuses_free_lanes() {
    // 两段不相干的历史交错着出现，各占一条 lane。
    let rows = layout(&[commit("a1", &["a0"]), commit("b1", &["b0"]), commit("a0", &[]), commit("b0", &[])]);
    assert_eq!(rows[0], (0, 0, 1, vec![bottom(0, 0, 0)]));
    assert_eq!(rows[1], (1, 1, 2, vec![top(0, 0, 0), bottom(0, 0, 0), bottom(1, 1, 1)]));
    assert_eq!(rows[2], (0, 0, 2, vec![top(0, 0, 0), top(1, 1, 1), bottom(1, 1, 1)]));
    assert_eq!(rows[3], (1, 1, 2, vec![top(1, 1, 1)]));

    // 中间那条 lane 空出来以后，下一个新分支先用它，右边的不往左挪。
    let rows = layout(&[
        commit("x", &["r"]),
        commit("y", &["y0"]),
        commit("z", &["r"]),
        commit("y0", &[]),
        commit("w", &["r"]),
        commit("r", &[]),
    ]);
    let columns: Vec<_> = rows.iter().map(|row| row.0).collect();
    assert_eq!(columns, [0, 1, 2, 1, 1, 0]);
    // z 在第三条 lane，下半段并回第一条 r。
    assert!(rows[2].3.contains(&bottom(2, 0, 2)));
    // y0 是根，它那条空出来，w 用上它。
    assert_eq!(rows[4].2, 2);
}

#[test]
fn a_merge_into_a_lane_that_is_already_waiting() {
    // feat 先走到了 b；m 合并 b 时不另占 lane，斜着连到 feat 那条。
    let rows = layout(&[commit("feat", &["b"]), commit("m", &["a", "b"]), commit("b", &["a"]), commit("a", &[])]);
    assert_eq!(rows[1], (1, 1, 2, vec![top(0, 0, 0), bottom(0, 0, 0), bottom(1, 1, 1), bottom(1, 0, 0)]));
    assert_eq!(rows[2], (0, 0, 2, vec![top(0, 0, 0), top(1, 1, 1), bottom(1, 1, 1), bottom(0, 1, 0)]));
    assert_eq!(rows[3], (1, 1, 2, vec![top(1, 1, 1)]));
}

/// 按说明首行找提交。
fn by_subject(history: &runode_git_status::History) -> HashMap<&str, &Commit> {
    history.commits.iter().map(|commit| (commit.subject.as_str(), commit)).collect()
}

#[test]
fn reads_history_refs_and_changes() {
    let remote = TestRepo::bare("graph-remote");
    let repo = TestRepo::new("graph");
    let handle = read(&repo).repo();
    assert!(handle.history(50).unwrap().commits.is_empty(), "还没有提交");

    repo.commit_file("a.txt", "one\n", "init");
    repo.git(&["tag", "v1"]);
    repo.git(&["switch", "-q", "-c", "feat"]);
    repo.commit_file("f.txt", "f\n", "feature");
    repo.git(&["switch", "-q", "main"]);
    repo.commit_file("a.txt", "two\n", "second");
    repo.git(&["merge", "-q", "--no-ff", "-m", "merge feat", "feat"]);
    repo.git(&["remote", "add", "origin", &remote.path().to_string_lossy()]);
    repo.git(&["push", "-q", "-u", "origin", "main"]);
    repo.commit_file("a.txt", "three\n", "local");
    // 另一个本地分支，没合进 main。
    repo.git(&["switch", "-q", "-c", "side", "v1"]);
    repo.commit_file("s.txt", "s\n", "side work");
    repo.git(&["switch", "-q", "main"]);

    let history = handle.history(50).unwrap();
    assert_eq!(history.commits.len(), 6);
    assert!(!history.more);
    assert_eq!(history.rows.len(), 6);
    // 父提交总在子提交后面。
    let position: HashMap<_, _> = history.commits.iter().enumerate().map(|(ix, c)| (c.id.as_str(), ix)).collect();
    for (ix, commit) in history.commits.iter().enumerate() {
        assert!(commit.parents.iter().all(|parent| position[parent.as_str()] > ix), "{}", commit.subject);
    }
    let commits = by_subject(&history);
    let kinds = |subject: &str| commits[subject].refs.iter().map(|r| (r.name.clone(), r.kind)).collect::<Vec<_>>();
    assert_eq!(kinds("local"), [("main".into(), RefKind::CurrentBranch)]);
    assert_eq!(kinds("merge feat"), [("origin/main".into(), RefKind::Remote)]);
    assert_eq!(kinds("init"), [("v1".into(), RefKind::Tag)]);
    assert_eq!(kinds("feature"), [("feat".into(), RefKind::Branch)]);
    assert_eq!(kinds("side work"), [("side".into(), RefKind::Branch)]);
    assert_eq!(commits["merge feat"].parents.len(), 2);
    assert_eq!(commits["local"].author, "t");
    assert_eq!(commits["local"].short_id().len(), 7);

    // 只读两个：后面还有。
    let first = handle.history(2).unwrap();
    assert_eq!((first.commits.len(), first.rows.len(), first.more), (2, 2, true));

    // 合并提交和第一个父提交比，只有合进来的文件；第一个提交和空的树比。
    let merge = handle.commit_changes(commits["merge feat"]).unwrap();
    assert_eq!(paths_of(&merge), ["f.txt"]);
    assert_eq!(merge[0].status, FileStatus::Added);
    let second = handle.commit_changes(commits["second"]).unwrap();
    assert_eq!((second[0].added, second[0].removed), (1, 1));
    assert_eq!(second[0].hunks[0].lines.len(), 2);
    let init = handle.commit_changes(commits["init"]).unwrap();
    assert_eq!((paths_of(&init), init[0].status), (vec!["a.txt".to_owned()], FileStatus::Added));

    // 切到某个提交成为分离头指针，再从某个提交新建分支。
    let second_id = commits["second"].id.clone();
    handle.checkout_detached(&second_id).unwrap();
    let detached = handle.history(50).unwrap();
    let head = detached.commits.iter().find(|commit| commit.id == second_id).unwrap();
    assert_eq!(head.refs[0].kind, RefKind::Head);
    assert!(repo.try_git(&["symbolic-ref", "-q", "HEAD"]).is_none());
    handle.create_branch_at("fix", &commits["init"].id).unwrap();
    assert_eq!(repo.git(&["branch", "--show-current"]), "fix");
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), commits["init"].id);
    assert!(handle.checkout_detached("--orphan").is_err());
}

#[test]
fn tells_ref_changes_from_other_files() {
    let repo = TestRepo::new("graph-refs");
    repo.commit_file("a.txt", "one\n", "init");
    let git_dir = read(&repo).git_dir;
    for (rel, expected) in [
        ("HEAD", true),
        ("refs/heads/feat", true),
        ("refs/tags/v1", true),
        ("packed-refs", true),
        ("refs/heads/feat.lock", false),
        ("index", false),
        ("logs/HEAD", false),
        ("objects/ab/cdef", false),
    ] {
        assert_eq!(refs_changed(&git_dir, &git_dir.join(rel)), expected, "{rel}");
    }
    assert!(!refs_changed(&git_dir, &repo.path().join("refs/x.txt")), "工作区里叫 refs 的目录不算");

    // worktree 自己的 git 目录里只有 HEAD，分支和 tag 在共用的 git 目录里。
    let worktree = repo.path().join("wt");
    repo.git(&["worktree", "add", "-q", "-b", "wt", &worktree.to_string_lossy()]);
    let wt_dir = runode_git_status::snapshot(&worktree, &mut Default::default()).unwrap().git_dir;
    assert_ne!(wt_dir, git_dir);
    assert!(refs_changed(&wt_dir, &wt_dir.join("HEAD")));
    assert!(refs_changed(&wt_dir, &git_dir.join("refs/tags/v2")));
    assert!(!refs_changed(&wt_dir, &git_dir.join("HEAD")));
}
