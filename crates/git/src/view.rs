//! 预览栏里看一个文件的整篇 diff：没改的行照常排着，删掉的行插在原来的位置，每块改动前面一行
//! 块头。块和工作区改动列表里的一样（同样的 `git diff` 参数，默认的上下文行数），按块暂存、丢弃
//! 用的就是这些块；再读新的那一边的全文，按块头的行号把块嵌进去。

use std::{fs, path::Path};

use crate::{
    FileDiff, FileStatus, LineKind, Repo, Result, git,
    parse::expand_tabs,
    snapshot::{MAX_DIFF_BYTES, diff, read_untracked, worktree_bytes},
};

/// 新的那一边超过这么多行时不读全文，只显示各块。
const MAX_VIEW_LINES: usize = 50_000;

/// 和什么比：工作区比暂存区，暂存区比 HEAD，或者一个提交比它的第一个父提交（第一个提交和空的树比）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DiffSide {
    Worktree,
    Index,
    Commit { id: String, parent: Option<String> },
}

/// 整篇 diff 里的一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffRow {
    /// 第几块（`FileDiff::hunks` 的下标）的块头。
    Header(usize),
    /// 块里的一行：`FileDiff::hunks[hunk].lines[line]`。
    Hunk { hunk: usize, line: usize },
    /// 块外面没改的一行，旧文件和新文件里的行号（从 1 数）。
    Context { old: u32, new: u32 },
}

/// `Repo::new_bytes` 的选项。
#[derive(Clone, Copy, Debug)]
pub struct ReadOptions {
    /// 内容比这么多字节大时不要，为 `TooLarge`。
    pub max_bytes: u64,
}

/// 内容比 `ReadOptions::max_bytes` 大，没给。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge;

/// 一个文件的整篇 diff。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffView {
    pub file: FileDiff,
    /// 新的那一边的全文，原样（制表符没展开），给按文件类型高亮；删掉的文件、二进制、太大或者和
    /// 块对不上时为空，`rows` 里只有各块。
    pub new_lines: Vec<String>,
    pub rows: Vec<DiffRow>,
}

impl DiffView {
    /// 新文件第 `new` 行（从 1 数）的原文；没读全文时为空。
    pub fn new_line(&self, new: u32) -> Option<&str> {
        self.new_lines.get(usize::try_from(new).ok()?.checked_sub(1)?).map(String::as_str)
    }
}

/// `-a,b` 或 `+a,b` 里的起始行号和行数，省略行数时是 1。
fn range(part: &str) -> Option<(u32, u32)> {
    let part = part.get(1..)?;
    match part.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((part.parse().ok()?, 1)),
    }
}

/// 块头 `@@ -a,b +c,d @@` 里旧文件和新文件的第一行：行数为 0 的那边（只有加或只有删）块头
/// 写的是块前面那一行，块从它的下一行开始。
pub(crate) fn first_lines(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.strip_prefix("@@ ")?.split(' ');
    let (old, old_count) = range(parts.next()?)?;
    let (new, new_count) = range(parts.next()?)?;
    Some((old + u32::from(old_count == 0), new + u32::from(new_count == 0)))
}

/// 把 `file` 的各块按块头的行号嵌进新文件的全文 `new_lines`：块之间没改的行按新文件排，旧行号
/// 随前面的块加减的行数平移。块里没改的行和加的行要和全文对得上（制表符展开后比），对不上（读
/// 全文时文件又变了）或者块头的行号越界时为空。
pub fn merge_rows(file: &FileDiff, new_lines: &[String]) -> Option<Vec<DiffRow>> {
    let mut rows = Vec::with_capacity(new_lines.len() + file.hunks.len());
    let (mut next_new, mut offset) = (1u32, 0i64);
    let context = |old: i64, new: u32| DiffRow::Context { old: u32::try_from(old).unwrap_or(0), new };
    let total = u32::try_from(new_lines.len()).ok()?;
    for (hi, hunk) in file.hunks.iter().enumerate() {
        let (mut old, mut new) = first_lines(&hunk.header)?;
        if new < next_new || new > total + 1 {
            return None;
        }
        rows.extend((next_new..new).map(|n| context(i64::from(n) + offset, n)));
        rows.push(DiffRow::Header(hi));
        for (li, line) in hunk.lines.iter().enumerate() {
            if line.kind != LineKind::Removed {
                let text = new_lines.get(usize::try_from(new).ok()?.checked_sub(1)?)?;
                if expand_tabs(text) != line.text {
                    return None;
                }
                new += 1;
            }
            if line.kind != LineKind::Added {
                old += 1;
            }
            rows.push(DiffRow::Hunk { hunk: hi, line: li });
        }
        next_new = new;
        offset = i64::from(old) - i64::from(new);
    }
    rows.extend((next_new..=total).map(|n| context(i64::from(n) + offset, n)));
    Some(rows)
}

/// 只有各块的行，没读全文时用。
fn hunk_rows(file: &FileDiff) -> Vec<DiffRow> {
    file.hunks
        .iter()
        .enumerate()
        .flat_map(|(hi, hunk)| {
            std::iter::once(DiffRow::Header(hi))
                .chain((0..hunk.lines.len()).map(move |li| DiffRow::Hunk { hunk: hi, line: li }))
        })
        .collect()
}

/// 内容按行切开；太大时为空。
fn split_lines(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() as u64 > MAX_DIFF_BYTES {
        return None;
    }
    let lines: Vec<String> = String::from_utf8_lossy(bytes).lines().map(str::to_owned).collect();
    (lines.len() <= MAX_VIEW_LINES).then_some(lines)
}

impl Repo {
    /// 读 `path`（相对仓库根）这个文件的整篇 diff，`side` 说和什么比。改名的文件把原来的路径
    /// 放进 `old_path`，不然认不出是改名。已经没有改动时为空。未跟踪的文件整篇都是加的行。
    pub fn file_view(&self, path: &Path, old_path: Option<&Path>, side: &DiffSide) -> Result<Option<DiffView>> {
        let specs: Vec<String> =
            std::iter::once(path).chain(old_path).map(|path| format!(":(literal){}", path.display())).collect();
        let empty_tree = || {
            git(&self.root, &["hash-object", "-t", "tree", "/dev/null"])
                .map(|tree| String::from_utf8_lossy(&tree).trim().to_owned())
        };
        let base = match side {
            DiffSide::Commit { parent, .. } => parent.clone().or_else(empty_tree),
            _ => None,
        };
        let mut args: Vec<&str> = match (side, &base) {
            (DiffSide::Worktree, _) => vec!["-2"],
            // 还没有提交时 `--cached` 自己和空树比。
            (DiffSide::Index, _) => vec!["--cached"],
            (DiffSide::Commit { id, .. }, Some(base)) => vec![base, id],
            _ => return Ok(None),
        };
        args.push("--");
        args.extend(specs.iter().map(String::as_str));
        let mut file = diff(&self.root, &args).into_iter().find(|file| file.path == path);
        if file.is_none() && *side == DiffSide::Worktree && self.untracked(path) {
            let full = self.root.join(path);
            // 和已跟踪的文件一样，超过 `MAX_DIFF_BYTES` 的不读进内存，标成截断。
            let small = fs::symlink_metadata(&full).is_ok_and(|meta| meta.len() <= MAX_DIFF_BYTES);
            file = Some(read_untracked(&full, path.to_path_buf(), small));
        }
        let Some(file) = file else {
            return Ok(None);
        };
        let content = match side {
            _ if file.binary || file.truncated || file.status == FileStatus::Deleted => None,
            _ => self.new_bytes(path, side, ReadOptions { max_bytes: MAX_DIFF_BYTES }).ok().flatten(),
        };
        let mut new_lines = content.as_deref().and_then(split_lines).unwrap_or_default();
        let rows = match merge_rows(&file, &new_lines) {
            Some(rows) if !new_lines.is_empty() || file.status == FileStatus::Deleted => rows,
            _ => {
                new_lines.clear();
                hunk_rows(&file)
            }
        };
        Ok(Some(DiffView { file, new_lines, rows }))
    }

    /// `path` 在 `side` 这一边改完以后的全部字节：工作区的文件、暂存区或提交里的 blob。
    /// 读不到（比如删掉了）时为空。符号链接和 git 一样是它指向的路径。比 `options.max_bytes` 大时
    /// 是 `TooLarge`：工作区的文件先看大小，大的不读进内存；暂存区和提交里的 blob 由 git 读出来再比。
    pub fn new_bytes(
        &self,
        path: &Path,
        side: &DiffSide,
        options: ReadOptions,
    ) -> std::result::Result<Option<Vec<u8>>, TooLarge> {
        let bytes = match side {
            DiffSide::Worktree => {
                let full = self.root.join(path);
                if fs::symlink_metadata(&full).is_ok_and(|meta| meta.len() > options.max_bytes) {
                    return Err(TooLarge);
                }
                worktree_bytes(&full)
            }
            DiffSide::Index => git(&self.root, &["cat-file", "blob", &format!(":{}", path.display())]),
            DiffSide::Commit { id, .. } => git(&self.root, &["cat-file", "blob", &format!("{id}:{}", path.display())]),
        };
        match bytes {
            Some(bytes) if bytes.len() as u64 > options.max_bytes => Err(TooLarge),
            bytes => Ok(bytes),
        }
    }

    /// `path` 在 `side` 这一边改之前的全部字节：工作区比的是暂存区，暂存区比的是 HEAD，提交比的是
    /// 父提交；改了名的按原来的路径 `old_path` 找。新加的文件读不到，为空。
    pub fn old_bytes(&self, path: &Path, old_path: Option<&Path>, side: &DiffSide) -> Option<Vec<u8>> {
        let path = old_path.unwrap_or(path).display();
        let spec = match side {
            DiffSide::Worktree => format!(":{path}"),
            DiffSide::Index => format!("HEAD:{path}"),
            DiffSide::Commit { parent, .. } => format!("{}:{path}", parent.as_ref()?),
        };
        git(&self.root, &["cat-file", "blob", &spec])
    }

    /// `path` 在工作区里、不是目录（符号链接看它自己）、git 没跟踪它。
    fn untracked(&self, path: &Path) -> bool {
        let spec = format!(":(literal){}", path.display());
        fs::symlink_metadata(self.root.join(path)).is_ok_and(|meta| !meta.is_dir())
            && git(&self.root, &["ls-files", "--error-unmatch", "--", &spec]).is_none()
    }
}
