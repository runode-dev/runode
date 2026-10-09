//! 整篇 diff：把块按块头的行号嵌进新文件的全文（行号对齐、对不上时放弃），以及从工作区、
//! 暂存区和提交里读一个文件的整篇 diff。

mod common;

use std::path::Path;

use common::{TestRepo, read};
use runode_git::{DiffRow, DiffSide, FileDiff, FileStatus, Hunk, Line, LineKind, merge_rows};

fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> Line {
    Line { kind, old, new, text: text.into() }
}

fn file(hunks: Vec<Hunk>) -> FileDiff {
    FileDiff {
        path: "a.txt".into(),
        old_path: None,
        status: FileStatus::Modified,
        added: 0,
        removed: 0,
        hunks,
        binary: false,
        truncated: false,
        gitlink: false,
    }
}

fn lines(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|&text| text.to_owned()).collect()
}

use DiffRow::{Context, Header};
use LineKind::{Added as A, Context as C, Removed as R};

fn hunk(hunk: usize, line: usize) -> DiffRow {
    DiffRow::Hunk { hunk, line }
}

#[test]
fn merges_hunks_into_the_whole_file() {
    // 旧文件 1..6 行；第 2 行改成 two，第 5 行删掉。
    let diff = file(vec![
        Hunk {
            header: "@@ -1,3 +1,3 @@".into(),
            lines: vec![
                line(C, Some(1), Some(1), "1"),
                line(R, Some(2), None, "2"),
                line(A, None, Some(2), "two"),
                line(C, Some(3), Some(3), "3"),
            ],
        },
        Hunk { header: "@@ -5 +4,0 @@".into(), lines: vec![line(R, Some(5), None, "5")] },
    ]);
    let new = lines(&["1", "two", "3", "4", "6"]);
    let rows = merge_rows(&diff, &new).unwrap();
    assert_eq!(
        rows,
        [
            Header(0),
            hunk(0, 0),
            hunk(0, 1),
            hunk(0, 2),
            hunk(0, 3),
            Context { old: 4, new: 4 },
            Header(1),
            hunk(1, 0),
            // 删了一行以后，旧行号比新行号多一。
            Context { old: 6, new: 5 },
        ]
    );
}

#[test]
fn shifts_old_numbers_after_additions_and_keeps_tabs() {
    let diff = file(vec![Hunk {
        header: "@@ -1,0 +2,2 @@".into(),
        lines: vec![line(A, None, Some(2), "    x"), line(A, None, Some(3), "y")],
    }]);
    let new = lines(&["a", "\tx", "y", "b"]);
    let rows = merge_rows(&diff, &new).unwrap();
    assert_eq!(rows, [Context { old: 1, new: 1 }, Header(0), hunk(0, 0), hunk(0, 1), Context { old: 2, new: 4 }]);
}

#[test]
fn refuses_hunks_that_do_not_match_the_file() {
    let diff = file(vec![Hunk { header: "@@ -1 +1 @@".into(), lines: vec![line(C, Some(1), Some(1), "old")] }]);
    assert!(merge_rows(&diff, &lines(&["new"])).is_none());
    let beyond = file(vec![Hunk { header: "@@ -9 +9 @@".into(), lines: vec![line(A, None, Some(9), "x")] }]);
    assert!(merge_rows(&beyond, &lines(&["a"])).is_none());
    // 没有块的文件就是全文。
    assert_eq!(merge_rows(&file(Vec::new()), &lines(&["a"])).unwrap(), [Context { old: 1, new: 1 }]);
}

/// 一行行地列出整篇 diff：块头写成 `@@`，块里的行带正负号，块外的行带两个行号。
fn render(view: &runode_git::DiffView) -> Vec<String> {
    view.rows
        .iter()
        .map(|row| match *row {
            Header(_) => "@@".to_owned(),
            DiffRow::Hunk { hunk, line } => {
                let line = &view.file.hunks[hunk].lines[line];
                let sign = match line.kind {
                    A => '+',
                    R => '-',
                    C => ' ',
                };
                format!("{sign}{}", line.text)
            }
            Context { old, new } => format!("{old}/{new} {}", view.new_line(new).unwrap()),
        })
        .collect()
}

const BASE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n";

#[test]
fn reads_whole_file_diffs() {
    let repo = TestRepo::new("view");
    repo.commit_file("a.txt", BASE, "init");
    let handle = read(&repo).repo();
    let path = Path::new("a.txt");
    assert!(handle.file_view(path, None, &DiffSide::Worktree).unwrap().is_none(), "没有改动");

    repo.write("a.txt", "1\ntwo\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n");
    let view = handle.file_view(path, None, &DiffSide::Worktree).unwrap().unwrap();
    assert_eq!(view.new_lines.len(), 12);
    let shown = render(&view);
    assert_eq!(shown[..6], ["@@", " 1", "-2", "+two", " 3", " 4"]);
    assert_eq!(shown.last().unwrap(), "12/12 12");
    assert_eq!(shown.len(), 1 + 12 + 1);

    // 暂存以后工作区没有改动了，暂存区比 HEAD 有。
    repo.git(&["add", "a.txt"]);
    assert!(handle.file_view(path, None, &DiffSide::Worktree).unwrap().is_none());
    let staged = handle.file_view(path, None, &DiffSide::Index).unwrap().unwrap();
    assert_eq!(render(&staged)[..4], ["@@", " 1", "-2", "+two"]);

    // 提交里的改动和第一个父提交比；第一个提交整篇都是加的。
    repo.git(&["commit", "-q", "-m", "two"]);
    let ids = repo.git(&["rev-list", "HEAD"]);
    let ids: Vec<_> = ids.lines().collect();
    let commit = DiffSide::Commit { id: ids[0].into(), parent: Some(ids[1].into()) };
    assert_eq!(render(&handle.file_view(path, None, &commit).unwrap().unwrap())[2..4], ["-2", "+two"]);
    let first = DiffSide::Commit { id: ids[1].into(), parent: None };
    let first = handle.file_view(path, None, &first).unwrap().unwrap();
    assert_eq!(first.file.status, FileStatus::Added);
    assert!(render(&first)[1..].iter().all(|line| line.starts_with('+')));

    // 未跟踪的文件整篇是加的，删掉的文件整篇是删的。
    repo.write("new.txt", "x\n\ty\n");
    let new = handle.file_view(Path::new("new.txt"), None, &DiffSide::Worktree).unwrap().unwrap();
    assert_eq!(render(&new), ["@@", "+x", "+    y"]);
    assert_eq!(new.new_line(2), Some("\ty"));
    std::fs::remove_file(repo.path().join("a.txt")).unwrap();
    let gone = handle.file_view(path, None, &DiffSide::Worktree).unwrap().unwrap();
    assert_eq!(gone.file.status, FileStatus::Deleted);
    assert!(gone.new_lines.is_empty());
    assert_eq!(render(&gone).len(), 13);
}

/// 只改了名、内容没变的文件没有块，全文照样读出来，按没改的行排。
#[test]
fn a_pure_rename_reads_the_whole_file() {
    let repo = TestRepo::new("view-rename");
    repo.commit_file("old.txt", "a\nb\n", "init");
    repo.git(&["mv", "old.txt", "new.txt"]);
    let handle = read(&repo).repo();
    let view = handle.file_view(Path::new("new.txt"), Some(Path::new("old.txt")), &DiffSide::Index).unwrap().unwrap();
    assert_eq!(view.file.status, FileStatus::Renamed);
    assert!(view.file.hunks.is_empty());
    assert_eq!(render(&view), ["1/1 a", "2/2 b"]);
}

/// 未跟踪的大文件和已跟踪的一样不读进来，只标成截断。
#[test]
fn a_large_untracked_file_is_truncated_without_reading_it() {
    let repo = TestRepo::new("view-large-untracked");
    repo.commit_file("a.txt", "a\n", "init");
    // 只有一行，按行数不会截断，只能按大小认出来。
    repo.write("big.txt", &format!("{}\n", "x".repeat(2 * 1024 * 1024)));
    let handle = read(&repo).repo();
    let view = handle.file_view(Path::new("big.txt"), None, &DiffSide::Worktree).unwrap().unwrap();
    assert_eq!(view.file.status, FileStatus::Untracked);
    assert!(view.file.truncated);
    assert!(view.file.hunks.is_empty() && view.new_lines.is_empty());
}

#[test]
fn reads_both_sides_of_a_change() {
    let repo = TestRepo::new("both-sides");
    repo.commit_file("a.bin", "v1", "one");
    let handle = read(&repo).repo();
    let path = Path::new("a.bin");
    let bytes = |text: &str| Some(text.as_bytes().to_vec());

    // 工作区：改之前是暂存区那份，改之后是磁盘上的。
    repo.write("a.bin", "v2");
    assert_eq!(handle.old_bytes(path, None, &DiffSide::Worktree), bytes("v1"));
    assert_eq!(handle.new_bytes(path, &DiffSide::Worktree), bytes("v2"));

    // 暂存区：改之前是 HEAD 那份。
    repo.git(&["add", "a.bin"]);
    assert_eq!(handle.old_bytes(path, None, &DiffSide::Index), bytes("v1"));
    assert_eq!(handle.new_bytes(path, &DiffSide::Index), bytes("v2"));

    // 提交：改之前是父提交那份；第一个提交没有父提交，没有旧的。
    repo.git(&["commit", "-q", "-m", "two"]);
    let ids = repo.git(&["rev-list", "HEAD"]);
    let ids: Vec<_> = ids.lines().collect();
    let commit = DiffSide::Commit { id: ids[0].into(), parent: Some(ids[1].into()) };
    assert_eq!(handle.old_bytes(path, None, &commit), bytes("v1"));
    assert_eq!(handle.new_bytes(path, &commit), bytes("v2"));
    assert_eq!(handle.old_bytes(path, None, &DiffSide::Commit { id: ids[1].into(), parent: None }), None);

    // 改了名的按原来的路径找旧的；未跟踪的没有旧的。
    repo.git(&["mv", "a.bin", "b.bin"]);
    assert_eq!(handle.old_bytes(Path::new("b.bin"), Some(path), &DiffSide::Index), bytes("v2"));
    repo.write("new.bin", "n");
    assert_eq!(handle.old_bytes(Path::new("new.bin"), None, &DiffSide::Worktree), None);
}
