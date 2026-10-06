//! 按块暂存、撤回暂存和丢弃：`Repo::apply_hunk` 和 `hunk_actionable`。

mod common;

use std::path::Path;

use common::{TestRepo, read, read_all};
use runode_git_status::{FileDiff, HunkAction, Section, Snapshot, hunk_actionable};

fn file<'a>(snapshot: &'a Snapshot, section: Section, path: &str) -> &'a FileDiff {
    snapshot.files(section).iter().find(|file| file.path == Path::new(path)).unwrap()
}

/// 十二行的文件改第 2 行和第 11 行，中间隔得够远，`git diff` 给两块。
const BASE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";
const CHANGED: &str = "1\ntwo\n3\n4\n5\n6\n7\n8\n9\n10\neleven\n12\n";
const SECOND_ONLY: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\neleven\n12";

fn two_hunks(name: &str) -> TestRepo {
    let repo = TestRepo::new(name);
    repo.commit_file("a.txt", BASE, "init");
    repo.write("a.txt", CHANGED);
    repo
}

#[test]
fn stages_and_unstages_one_hunk() {
    let repo = two_hunks("hunk-stage");
    let snapshot = read(&repo);
    let a = file(&snapshot, Section::Unstaged, "a.txt");
    assert_eq!(a.hunks.len(), 2);
    assert!(hunk_actionable(a));
    snapshot.repo().apply_hunk(a, 1, HunkAction::Stage).unwrap();
    assert_eq!(repo.git(&["show", ":a.txt"]), SECOND_ONLY);
    assert_eq!(repo.read("a.txt"), CHANGED);

    // 暂存了第二块后再暂存第一块，块头的行号和刚才读的对得上。
    let snapshot = read(&repo);
    let a = file(&snapshot, Section::Unstaged, "a.txt");
    assert_eq!(a.hunks.len(), 1);
    snapshot.repo().apply_hunk(a, 0, HunkAction::Stage).unwrap();
    assert_eq!(repo.status(), ["M  a.txt"]);

    let snapshot = read(&repo);
    let staged = file(&snapshot, Section::Staged, "a.txt");
    snapshot.repo().apply_hunk(staged, 0, HunkAction::Unstage).unwrap();
    assert_eq!(repo.git(&["show", ":a.txt"]), SECOND_ONLY);
    assert_eq!(repo.read("a.txt"), CHANGED);
}

#[test]
fn discards_one_hunk() {
    let repo = two_hunks("hunk-discard");
    let snapshot = read(&repo);
    snapshot.repo().apply_hunk(file(&snapshot, Section::Unstaged, "a.txt"), 0, HunkAction::Discard).unwrap();
    assert_eq!(repo.read("a.txt"), format!("{SECOND_ONLY}\n"));
    assert_eq!(repo.git(&["show", ":a.txt"]), BASE.trim_end());
}

#[test]
fn keeps_tabs_and_missing_final_newline() {
    let repo = TestRepo::new("hunk-raw");
    let base = "\tfirst\n2\n3\n4\n5\n6\n7\n8\n9\nlast";
    repo.commit_file("a.txt", base, "init");
    repo.write("a.txt", "\tFIRST\n2\n3\n4\n5\n6\n7\n8\n9\nLAST");
    let snapshot = read(&repo);
    let a = file(&snapshot, Section::Unstaged, "a.txt");
    assert_eq!(a.hunks.len(), 2);
    assert_eq!(a.hunks[0].lines[1].text, "    FIRST");
    snapshot.repo().apply_hunk(a, 1, HunkAction::Stage).unwrap();
    assert_eq!(repo.git(&["cat-file", "blob", ":a.txt"]), "\tfirst\n2\n3\n4\n5\n6\n7\n8\n9\nLAST");
    snapshot.repo().apply_hunk(a, 0, HunkAction::Stage).unwrap();
    assert_eq!(repo.status(), ["M  a.txt"]);

    let snapshot = read(&repo);
    let staged = file(&snapshot, Section::Staged, "a.txt");
    snapshot.repo().apply_hunk(staged, 1, HunkAction::Unstage).unwrap();
    repo.write("a.txt", "\tFIRST\n2\n3\n4\n5\n6\n7\n8\n9\nLAST\n");
    let snapshot = read(&repo);
    let a = file(&snapshot, Section::Unstaged, "a.txt");
    snapshot.repo().apply_hunk(a, 0, HunkAction::Discard).unwrap();
    // 工作区回到暂存区的样子：第一行已暂存，最后一行没有，也没有结尾的换行。
    assert_eq!(repo.read("a.txt"), "\tFIRST\n2\n3\n4\n5\n6\n7\n8\n9\nlast");
}

#[test]
fn refuses_stale_hunks_and_other_files() {
    let repo = two_hunks("hunk-stale");
    repo.write("new.txt", "n\n");
    let snapshot = read(&repo);
    let a = file(&snapshot, Section::Unstaged, "a.txt").clone();
    let new = file(&snapshot, Section::Unstaged, "new.txt");
    assert!(!hunk_actionable(new));
    assert!(snapshot.repo().apply_hunk(new, 0, HunkAction::Stage).is_err());
    assert!(snapshot.repo().apply_hunk(&a, 2, HunkAction::Stage).is_err());
    // 读完之后文件又改了。
    repo.write("a.txt", &CHANGED.replace("two", "TWO"));
    assert!(snapshot.repo().apply_hunk(&a, 0, HunkAction::Stage).is_err());
    assert_eq!(repo.status(), [" M a.txt", "?? new.txt"]);
}

#[test]
fn stages_hunks_in_nested_repositories() {
    let repo = TestRepo::new("hunk-nested");
    repo.commit_file("top.txt", "top\n", "init");
    repo.nested("wt/inner");
    repo.write("wt/inner/a.txt", BASE);
    repo.git(&["-C", "wt/inner", "commit", "-q", "-am", "base"]);
    repo.write("wt/inner/a.txt", CHANGED);
    let repos = read_all(&repo);
    let inner = &repos.subs[0];
    let a = file(inner, Section::Unstaged, "a.txt");
    inner.repo().apply_hunk(a, 0, HunkAction::Stage).unwrap();
    assert_eq!(repo.git(&["-C", "wt/inner", "show", ":a.txt"]), "1\ntwo\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12");
}
