//! 按块暂存、撤回和丢弃：从文件的改动里只挑一块交给 `git apply`。

use std::{ffi::OsStr, path::Path};

use crate::{FileDiff, FileStatus, GitError, Hunk, LineKind, MAX_DIFF_BYTES, Repo, Result, expand_tabs, ops::run};

/// 对一块改动做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkAction {
    /// 把未暂存段里的这块放进暂存区。
    Stage,
    /// 把已暂存段里的这块撤回到工作区。
    Unstage,
    /// 把未暂存段里的这块从工作区里去掉，回到暂存区里的样子。
    Discard,
}

/// 这个文件能不能按块操作：只有普通文本文件的修改可以。新增、删除、改名、未跟踪、
/// 冲突的文件和二进制文件只能整个操作。
pub fn hunk_actionable(file: &FileDiff) -> bool {
    file.status == FileStatus::Modified && !file.binary
}

/// 原样的 `git diff` 输出切成文件头和一块一块，每一行都带着结尾的换行。
struct RawDiff<'a> {
    header: Vec<&'a [u8]>,
    hunks: Vec<Vec<&'a [u8]>>,
}

fn split_raw(output: &[u8]) -> RawDiff<'_> {
    let mut raw = RawDiff { header: Vec::new(), hunks: Vec::new() };
    for line in output.split_inclusive(|b| *b == b'\n') {
        // 块里的行都以空格、`+`、`-` 或 `\` 开头，以 `@@ ` 开头的只能是块头。
        if line.starts_with(b"@@ ") {
            raw.hunks.push(vec![line]);
        } else if let Some(hunk) = raw.hunks.last_mut() {
            hunk.push(line);
        } else {
            raw.header.push(line);
        }
    }
    raw
}

/// 一行去掉结尾的换行，按 `parse_diff` 读 `FileDiff` 时的样子转成文字。
fn line_text(line: &[u8]) -> String {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}

/// 原样的一块和 `FileDiff` 里的这块是不是同一块：块头一样，行也一样。`truncated` 时
/// `FileDiff` 里的行可能没记全，只比记下的那些。
fn same_hunk(raw: &[&[u8]], hunk: &Hunk, truncated: bool) -> bool {
    let Some((head, body)) = raw.split_first() else {
        return false;
    };
    if line_text(head) != hunk.header {
        return false;
    }
    let lines: Vec<_> = body
        .iter()
        .filter_map(|line| {
            let kind = match line.first() {
                Some(b'+') => LineKind::Added,
                Some(b'-') => LineKind::Removed,
                Some(b' ') => LineKind::Context,
                // 「\ No newline at end of file」。
                _ => return None,
            };
            Some((kind, expand_tabs(&line_text(&line[1..]))))
        })
        .collect();
    if lines.len() < hunk.lines.len() || (!truncated && lines.len() != hunk.lines.len()) {
        return false;
    }
    hunk.lines.iter().zip(&lines).all(|(line, (kind, text))| line.kind == *kind && line.text == *text)
}

impl Repo {
    /// 对 `file` 的第 `hunk` 块（`file.hunks` 的下标）做 `action`。暂存和丢弃时 `file` 来自
    /// 未暂存段，撤回时来自已暂存段；只接受 `hunk_actionable` 的文件。
    ///
    /// `FileDiff` 里的行把制表符展开了，也丢了「\ No newline at end of file」，不能拿来拼补丁：
    /// 这里重新跑一次这个文件的 `git diff` 取原样的输出，按块头找到同一块、核对每一行，再把
    /// 文件头加这一块交给 `git apply`。找不到（文件在这之后又改过）时报错，刷新后重试即可。
    pub fn apply_hunk(&self, file: &FileDiff, hunk: usize, action: HunkAction) -> Result {
        if !hunk_actionable(file) {
            return Err(GitError::new(format!("{} 不能按块操作", file.path.display())));
        }
        let target = file.hunks.get(hunk).ok_or_else(|| GitError::new("没有这一块改动"))?;
        let (dir, rel) = self.locate(&file.path)?;
        // 和 `diff` 读 `FileDiff` 时的参数一样，块才切得一样。
        let threshold = format!("core.bigFileThreshold={MAX_DIFF_BYTES}");
        let mut args: Vec<&OsStr> = ["--literal-pathspecs", "-c", &threshold, "diff", "--no-color", "--no-ext-diff"]
            .into_iter()
            .chain(["--no-textconv", "--src-prefix=a/", "--dst-prefix=b/"])
            .map(OsStr::new)
            .collect();
        if action == HunkAction::Unstage {
            args.extend([OsStr::new("--cached"), OsStr::new("HEAD")]);
        }
        args.extend([OsStr::new("--"), rel.as_os_str()]);
        let output = run(&dir, args, None)?;
        let raw = split_raw(&output);
        let chosen = raw
            .hunks
            .iter()
            .find(|raw| same_hunk(raw, target, file.truncated))
            .ok_or_else(|| GitError::new(format!("{} 已经变了，刷新后再试", file.path.display())))?;
        let patch: Vec<u8> = raw.header.iter().chain(chosen).flat_map(|line| line.iter().copied()).collect();
        apply(&dir, action, &patch)
    }
}

/// 在 `dir` 里把补丁交给 `git apply`：暂存是正着打进暂存区，撤回是反着打进暂存区，丢弃是
/// 反着打进工作区。只打一块时另外几块不在，git 按块头里的行号和上下文对位置。
fn apply(dir: &Path, action: HunkAction, patch: &[u8]) -> Result {
    let mut args = vec!["apply", "--whitespace=nowarn"];
    match action {
        HunkAction::Stage => args.push("--cached"),
        HunkAction::Unstage => args.extend(["--cached", "-R"]),
        HunkAction::Discard => args.push("-R"),
    }
    args.push("-");
    run(dir, args, Some(patch)).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_repo::TestRepo;
    use crate::{Section, Snapshot, UntrackedCache, snapshot};

    fn read(repo: &TestRepo) -> Snapshot {
        snapshot(repo.path(), &mut UntrackedCache::default()).unwrap()
    }

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
        let inner = repo.path().join("wt/inner");
        std::fs::create_dir_all(&inner).unwrap();
        repo.git(&["-C", "wt/inner", "init", "-q", "-b", "main"]);
        for (key, value) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false")] {
            repo.git(&["-C", "wt/inner", "config", key, value]);
        }
        repo.write("wt/inner/a.txt", BASE);
        repo.git(&["-C", "wt/inner", "add", "a.txt"]);
        repo.git(&["-C", "wt/inner", "commit", "-q", "-m", "init"]);
        repo.write("wt/inner/a.txt", CHANGED);
        let snapshot = read(&repo);
        let a = file(&snapshot, Section::Unstaged, "wt/inner/a.txt");
        snapshot.repo().apply_hunk(a, 0, HunkAction::Stage).unwrap();
        assert_eq!(repo.git(&["-C", "wt/inner", "show", ":a.txt"]), "1\ntwo\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12");
    }
}
