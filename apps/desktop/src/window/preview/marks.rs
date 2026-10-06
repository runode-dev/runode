//! 预览里行号旁的改动标记：文件相对 HEAD 的改动（已暂存的换算到工作区的行号上）换成每一行的
//! 新增、修改或上面删了行。

use std::{collections::HashMap, path::Path};

use runode_git::{self as git, LineKind, Section};

/// 行号旁的改动标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum Mark {
    Added,
    Modified,
    /// 这一行上面删掉了行。
    Removed,
}

/// 一个文件的改动换算成新文件里每一行的标记，键是从 1 数的行号。一串连着的删除和新增里，
/// 有删除的新增行算修改；只删不增的标在删除处的下一行，删在末尾时标在新文件的最后一行之后。
fn diff_marks(diff: &git::FileDiff) -> HashMap<u32, Mark> {
    let mut marks = HashMap::new();
    for hunk in &diff.hunks {
        let mut removed = 0;
        let mut added = Vec::new();
        let mut last_new = 0;
        let flush = |removed: &mut usize, added: &mut Vec<u32>, next: u32, marks: &mut HashMap<u32, Mark>| {
            if added.is_empty() && *removed > 0 {
                marks.entry(next.max(1)).or_insert(Mark::Removed);
            }
            let mark = if *removed > 0 { Mark::Modified } else { Mark::Added };
            for line in added.drain(..) {
                marks.insert(line, mark);
            }
            *removed = 0;
        };
        for line in &hunk.lines {
            match line.kind {
                LineKind::Removed => removed += 1,
                LineKind::Added => added.extend(line.new),
                LineKind::Context => flush(&mut removed, &mut added, line.new.unwrap_or(last_new + 1), &mut marks),
            }
            if let Some(new) = line.new {
                last_new = new;
            }
        }
        flush(&mut removed, &mut added, last_new + 1, &mut marks);
    }
    marks
}

/// 暂存区里的第 `line` 行在工作区里是第几行；工作区里删掉了就为空。`unstaged` 是工作区相对
/// 暂存区的改动。
fn index_to_worktree(unstaged: &git::FileDiff, line: u32) -> Option<u32> {
    let mut delta: i64 = 0;
    for diff_line in unstaged.hunks.iter().flat_map(|hunk| &hunk.lines) {
        match diff_line.old {
            Some(old) if old == line => {
                return if diff_line.kind == LineKind::Context { diff_line.new } else { None };
            }
            Some(old) if old > line => break,
            _ => {}
        }
        match diff_line.kind {
            LineKind::Added => delta += 1,
            LineKind::Removed => delta -= 1,
            LineKind::Context => {}
        }
    }
    u32::try_from(i64::from(line) + delta).ok().filter(|line| *line > 0)
}

/// 工作区里这个文件相对 HEAD 改了哪些行：未暂存的改动直接用，已暂存的换算到工作区的行号上。
pub(super) fn line_marks(snapshot: &git::Snapshot, rel: &Path) -> HashMap<u32, Mark> {
    let find = |section: Section| snapshot.files(section).iter().find(|file| file.path == rel);
    let unstaged = find(Section::Unstaged);
    let mut marks = HashMap::new();
    if let Some(staged) = find(Section::Staged) {
        for (line, mark) in diff_marks(staged) {
            let line = match unstaged {
                Some(unstaged) => index_to_worktree(unstaged, line),
                None => Some(line),
            };
            if let Some(line) = line {
                marks.insert(line, mark);
            }
        }
    }
    if let Some(unstaged) = unstaged {
        marks.extend(diff_marks(unstaged));
    }
    marks
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf};

    use git::{FileDiff, FileStatus, Hunk, Line};

    use super::*;

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> Line {
        Line { kind, old, new, text: String::new() }
    }

    fn diff(lines: Vec<Line>) -> FileDiff {
        FileDiff {
            path: "a.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            added: 0,
            removed: 0,
            hunks: vec![Hunk { header: String::new(), lines }],
            binary: false,
            truncated: false,
            gitlink: false,
        }
    }

    use LineKind::{Added as A, Context as C, Removed as R};

    #[test]
    fn marks_added_modified_and_removed_lines() {
        let file = diff(vec![
            line(C, Some(1), Some(1)),
            line(R, Some(2), None),
            line(A, None, Some(2)),
            line(C, Some(3), Some(3)),
            line(A, None, Some(4)),
            line(C, Some(4), Some(5)),
            line(R, Some(5), None),
            line(C, Some(6), Some(6)),
            line(R, Some(7), None),
        ]);
        let marks = diff_marks(&file);
        assert_eq!(marks.get(&2), Some(&Mark::Modified));
        assert_eq!(marks.get(&4), Some(&Mark::Added));
        assert_eq!(marks.get(&6), Some(&Mark::Removed));
        // 删在末尾：标在最后一行之后，画的时候挪到最后一行。
        assert_eq!(marks.get(&7), Some(&Mark::Removed));
        assert_eq!(marks.len(), 4);
    }

    #[test]
    fn maps_index_lines_through_unstaged_changes() {
        // 工作区在第 1 行后加了两行，删了原来的第 4 行。
        let unstaged = diff(vec![
            line(C, Some(1), Some(1)),
            line(A, None, Some(2)),
            line(A, None, Some(3)),
            line(C, Some(2), Some(4)),
            line(C, Some(3), Some(5)),
            line(R, Some(4), None),
            line(C, Some(5), Some(6)),
        ]);
        assert_eq!(index_to_worktree(&unstaged, 1), Some(1));
        assert_eq!(index_to_worktree(&unstaged, 2), Some(4));
        assert_eq!(index_to_worktree(&unstaged, 4), None);
        assert_eq!(index_to_worktree(&unstaged, 5), Some(6));
        // 改动块之外的行按前面增减的行数平移。
        assert_eq!(index_to_worktree(&unstaged, 20), Some(21));
    }

    #[test]
    fn combines_staged_and_unstaged_marks() {
        let mut staged = diff(vec![line(C, Some(1), Some(1)), line(A, None, Some(2)), line(C, Some(2), Some(3))]);
        staged.path = "a.rs".into();
        let unstaged = diff(vec![line(A, None, Some(1)), line(C, Some(1), Some(2)), line(C, Some(2), Some(3))]);
        let snapshot = git::Snapshot {
            root: "/repo".into(),
            git_dir: "/repo/.git".into(),
            prefix: PathBuf::new(),
            kind: git::RepoKind::Main,
            staged: vec![staged],
            unstaged: vec![unstaged],
            statuses: HashMap::new(),
            ignored: HashSet::new(),
            info: Default::default(),
        };
        let marks = line_marks(&snapshot, Path::new("a.rs"));
        // 工作区新加的第 1 行，以及暂存区里加的第 2 行，在工作区里是第 3 行。
        assert_eq!(marks.get(&1), Some(&Mark::Added));
        assert_eq!(marks.get(&3), Some(&Mark::Added));
        assert_eq!(marks.len(), 2);
    }
}
