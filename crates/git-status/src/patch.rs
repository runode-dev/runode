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
/// 冲突的文件和二进制文件只能整个操作；子模块那样的 gitlink 只能整个暂存或撤回。
pub fn hunk_actionable(file: &FileDiff) -> bool {
    file.status == FileStatus::Modified && !file.binary && !file.gitlink
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
            .chain(["--no-textconv", "--src-prefix=a/", "--dst-prefix=b/", "--ignore-submodules=dirty"])
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
