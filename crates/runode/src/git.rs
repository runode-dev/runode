//! 读项目目录的 git 状态：工作区相对 HEAD 的逐行改动，以及每个文件的状态，供右侧的改动
//! 面板和文件树使用。只调 `git` 命令行读，不写仓库。

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// 一个文件最多读这么多行改动，再多的只记总数。
const MAX_FILE_LINES: usize = 3000;
/// 未跟踪的文件比这大时不读内容。
const MAX_UNTRACKED_BYTES: u64 = 256 * 1024;
/// 未跟踪的文件最多读这么多个的内容，其余只列出来。
const MAX_UNTRACKED_FILES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl FileStatus {
    /// 改动列表里状态的单字母标记。
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// 在旧文件和新文件里的行号；新增的行没有旧行号，删掉的行没有新行号。
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// 去掉开头的 `+`、`-` 或空格，制表符换成空格。
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// `@@ -a,b +c,d @@` 以及后面的函数名之类。
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// 相对仓库根的路径；删掉的文件是原来的路径。
    pub path: PathBuf,
    /// 改名前的路径。
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<Hunk>,
    pub binary: bool,
    /// 改动超过 `MAX_FILE_LINES` 行，或者是没读内容的未跟踪文件，`hunks` 不全。
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// 仓库根目录。
    pub root: PathBuf,
    /// 有改动的文件，按路径排序。
    pub files: Vec<FileDiff>,
    /// 有改动的文件的状态，键是相对仓库根的路径。
    pub statuses: HashMap<PathBuf, FileStatus>,
    /// 被忽略的文件和目录，相对仓库根；目录被忽略时里面的不再单列。
    pub ignored: Vec<PathBuf>,
}

impl Snapshot {
    pub fn added(&self) -> usize {
        self.files.iter().map(|file| file.added).sum()
    }

    pub fn removed(&self) -> usize {
        self.files.iter().map(|file| file.removed).sum()
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        // 路径里的中文等非 ASCII 字符原样输出，不转成八进制转义。
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// 读 `dir` 所在仓库的状态；`dir` 不在 git 仓库里或者没装 git 时为空。
pub fn snapshot(dir: &Path) -> Option<Snapshot> {
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    let root = local_root(dir, PathBuf::from(String::from_utf8_lossy(&root).trim_end_matches('\n')));
    let status = git(&root, &["status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignored=matching"])?;
    let (statuses, ignored) = parse_status(&status);
    // 还没有提交时和空树比，暂存了的新文件也算进来。
    let base = match git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        Some(_) => "HEAD".to_owned(),
        None => String::from_utf8_lossy(&git(&root, &["hash-object", "-t", "tree", "/dev/null"])?).trim().to_owned(),
    };
    // 前缀写明，免得用户配置了 `diff.noprefix` 之类改掉 `a/`、`b/`。
    let diff = git(
        &root,
        &[
            "diff",
            &base,
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ],
    )
    .unwrap_or_default();
    let mut files = parse_diff(&String::from_utf8_lossy(&diff));
    let mut untracked: Vec<_> = statuses
        .iter()
        .filter(|(_, status)| **status == FileStatus::Untracked)
        .map(|(path, _)| path.clone())
        .collect();
    untracked.sort();
    for (ix, path) in untracked.into_iter().enumerate() {
        files.push(untracked_diff(&root, path, ix < MAX_UNTRACKED_FILES));
    }
    // 冲突等状态以 `git status` 为准，diff 只看得出增删改名。
    for file in &mut files {
        if let Some(status) = statuses.get(&file.path)
            && matches!(status, FileStatus::Conflicted)
        {
            file.status = *status;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(Snapshot { root, files, statuses, ignored })
}

/// git 给的仓库根解析过符号链接；按 `dir` 的写法换回来，界面拿 `dir` 下的路径和它比前缀
/// 才对得上。`dir` 本身不在仓库根下面（仓库内部的符号链接）时保持原样。
fn local_root(dir: &Path, root: PathBuf) -> PathBuf {
    let (Ok(real_dir), Ok(real_root)) = (fs::canonicalize(dir), fs::canonicalize(&root)) else {
        return root;
    };
    let Ok(rel) = real_dir.strip_prefix(&real_root) else {
        return root;
    };
    dir.ancestors().nth(rel.components().count()).map_or(root, Path::to_path_buf)
}

/// 解析 `git status --porcelain=v1 -z` 的输出：各个文件的状态，以及被忽略的路径。
fn parse_status(output: &[u8]) -> (HashMap<PathBuf, FileStatus>, Vec<PathBuf>) {
    let mut statuses = HashMap::new();
    let mut ignored = Vec::new();
    let mut fields = output.split(|b| *b == 0).filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 {
            continue;
        }
        let (x, y) = (field[0], field[1]);
        let path = PathBuf::from(String::from_utf8_lossy(&field[3..]).trim_end_matches('/'));
        // 改名和复制后面跟着原来的路径，单独占一项。
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            fields.next();
        }
        let status = match (x, y) {
            (b'!', b'!') => {
                ignored.push(path);
                continue;
            }
            (b'?', b'?') => FileStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => FileStatus::Conflicted,
            (b'D', _) | (_, b'D') => FileStatus::Deleted,
            (b'R', _) | (_, b'R') => FileStatus::Renamed,
            (b'A', _) => FileStatus::Added,
            _ => FileStatus::Modified,
        };
        statuses.insert(path, status);
    }
    (statuses, ignored)
}

/// 解析 `git diff` 的统一格式输出。
fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    // 当前块里下一行的旧行号和新行号。
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    // 当前块的头有没有记下来；超过行数上限后不再记新块，它的行也不能并进上一个块。
    let mut hunk_kept = false;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            files.push(FileDiff {
                path: header_path(header).unwrap_or_default(),
                old_path: None,
                status: FileStatus::Modified,
                added: 0,
                removed: 0,
                hunks: Vec::new(),
                binary: false,
                truncated: false,
            });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if !in_hunk {
            if line.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = Some(unquote(from).into());
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.path = unquote(to).into();
            } else if line.starts_with("Binary files ") {
                file.binary = true;
            } else if let Some(to) = line.strip_prefix("+++ ") {
                // 路径里有空格时 git 在行尾补一个制表符。
                if let Some(path) = unquote(to.trim_end_matches('\t')).strip_prefix("b/") {
                    file.path = path.into();
                }
            } else if let Some(from) = line.strip_prefix("--- ") {
                // 删掉的文件新路径是 /dev/null，用旧路径。
                if let Some(path) = unquote(from.trim_end_matches('\t')).strip_prefix("a/") {
                    file.path = path.into();
                }
            }
        }
        if let Some(range) = line.strip_prefix("@@ ") {
            let (o, n) = parse_hunk_range(range);
            (old, new) = (o, n);
            in_hunk = true;
            hunk_kept = file.added + file.removed < MAX_FILE_LINES;
            if hunk_kept {
                file.hunks.push(Hunk { header: line.to_owned(), lines: Vec::new() });
            } else {
                file.truncated = true;
            }
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &line[1..]),
            Some(b'-') => (LineKind::Removed, &line[1..]),
            Some(b' ') => (LineKind::Context, &line[1..]),
            // 「\ No newline at end of file」之类的说明。
            _ => continue,
        };
        let numbers = match kind {
            LineKind::Added => (None, Some(new)),
            LineKind::Removed => (Some(old), None),
            LineKind::Context => (Some(old), Some(new)),
        };
        if kind != LineKind::Added {
            old += 1;
        }
        if kind != LineKind::Removed {
            new += 1;
        }
        match kind {
            LineKind::Added => file.added += 1,
            LineKind::Removed => file.removed += 1,
            LineKind::Context => {}
        }
        if file.added + file.removed > MAX_FILE_LINES {
            file.truncated = true;
            continue;
        }
        if !hunk_kept {
            continue;
        }
        if let Some(hunk) = file.hunks.last_mut() {
            hunk.lines.push(Line { kind, old: numbers.0, new: numbers.1, text: expand_tabs(text) });
        }
    }
    files
}

/// `-a,b +c,d @@ ...` 里的起始行号 `a` 和 `c`。
fn parse_hunk_range(range: &str) -> (u32, u32) {
    let mut parts = range.split(' ');
    let start = |part: Option<&str>, sign: char| {
        part.and_then(|part| part.strip_prefix(sign))
            .and_then(|part| part.split(',').next())
            .and_then(|start| start.parse().ok())
            .unwrap_or(0)
    };
    let old = start(parts.next(), '-');
    let new = start(parts.next(), '+');
    (old, new)
}

/// `diff --git a/路径 b/路径` 里的路径。只在新旧路径相同时可靠，改名时由后面的
/// `rename to` 或 `+++` 行纠正。
fn header_path(header: &str) -> Option<PathBuf> {
    if let Some(rest) = header.strip_prefix('"') {
        let end = rest.find("\" ")? + 2;
        return unquote(&header[..end]).strip_prefix("a/").map(PathBuf::from);
    }
    // 两个路径一样长：`a/P b/P` 共 2P+5 个字节。
    let len = header.len().checked_sub(5)? / 2;
    let (a, b) = (header.get(2..2 + len)?, header.get(header.len() - len..)?);
    (a == b).then(|| PathBuf::from(a))
}

/// 去掉 git 给特殊路径加的双引号和 C 风格转义。
fn unquote(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|text| text.strip_suffix('"')) else {
        return text.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    if let Some(next) = chars.next_if(|b| (b'0'..=b'7').contains(b)) {
                        value = value * 8 + u32::from(next - b'0');
                    }
                }
                bytes.push(value as u8);
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn expand_tabs(text: &str) -> String {
    text.replace('\t', "    ")
}

/// 未跟踪的文件当作整个新增；`read` 为假或者文件太大时不读内容。
fn untracked_diff(root: &Path, path: PathBuf, read: bool) -> FileDiff {
    let mut file = FileDiff {
        path,
        old_path: None,
        status: FileStatus::Untracked,
        added: 0,
        removed: 0,
        hunks: Vec::new(),
        binary: false,
        truncated: false,
    };
    let full = root.join(&file.path);
    let small = fs::metadata(&full).is_ok_and(|meta| meta.len() <= MAX_UNTRACKED_BYTES);
    let content = (read && small).then(|| fs::read(&full).ok()).flatten();
    let Some(content) = content else {
        file.truncated = true;
        return file;
    };
    if content.contains(&0) {
        file.binary = true;
        return file;
    }
    let text = String::from_utf8_lossy(&content);
    let lines: Vec<_> = text.lines().collect();
    file.added = lines.len();
    file.truncated = lines.len() > MAX_FILE_LINES;
    if !lines.is_empty() {
        file.hunks.push(Hunk {
            header: format!("@@ -0,0 +1,{} @@", lines.len()),
            lines: lines
                .iter()
                .take(MAX_FILE_LINES)
                .zip(1..)
                .map(|(text, n)| Line { kind: LineKind::Added, old: None, new: Some(n), text: expand_tabs(text) })
                .collect(),
        });
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -2,3 +2,4 @@ fn main() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
 }
\\ No newline at end of file
diff --git a/old name.txt b/new name.txt
similarity index 100%
rename from old name.txt
rename to new name.txt
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..4444444
Binary files /dev/null and b/logo.png differ
";

    #[test]
    fn parses_unified_diff() {
        let files = parse_diff(DIFF);
        assert_eq!(files.len(), 4);

        let main = &files[0];
        assert_eq!(main.path, PathBuf::from("src/main.rs"));
        assert_eq!((main.status, main.added, main.removed), (FileStatus::Modified, 2, 1));
        let lines = &main.hunks[0].lines;
        assert_eq!(main.hunks[0].header, "@@ -2,3 +2,4 @@ fn main() {");
        assert_eq!(lines.len(), 5);
        assert_eq!((lines[0].old, lines[0].new), (Some(2), Some(2)));
        assert_eq!((lines[1].kind, lines[1].old, lines[1].new), (LineKind::Removed, Some(3), None));
        assert_eq!((lines[2].kind, lines[2].old, lines[2].new), (LineKind::Added, None, Some(3)));
        assert_eq!((lines[4].old, lines[4].new), (Some(4), Some(5)));
        assert_eq!(lines[2].text, "let b = 3;");

        let renamed = &files[1];
        assert_eq!(renamed.status, FileStatus::Renamed);
        assert_eq!(renamed.path, PathBuf::from("new name.txt"));
        assert_eq!(renamed.old_path, Some(PathBuf::from("old name.txt")));

        let deleted = &files[2];
        assert_eq!((deleted.status, deleted.path.clone(), deleted.removed), (FileStatus::Deleted, "gone.txt".into(), 1));

        let binary = &files[3];
        assert!(binary.binary);
        assert_eq!((binary.status, binary.path.clone()), (FileStatus::Added, "logo.png".into()));
    }

    #[test]
    fn strips_the_tab_git_adds_after_paths_with_spaces() {
        let files = parse_diff("diff --git a/a b.txt b/a b.txt\n--- a/a b.txt\t\n+++ b/a b.txt\t\n@@ -1 +1,2 @@\n a\n+b\n");
        assert_eq!(files[0].path, PathBuf::from("a b.txt"));
    }

    #[test]
    fn parses_porcelain_status() {
        let output = b" M src/a.rs\0?? new.txt\0R  b.rs\0a.rs\0!! target/\0UU both.rs\0A  added.rs\0";
        let (statuses, ignored) = parse_status(output);
        assert_eq!(statuses[Path::new("src/a.rs")], FileStatus::Modified);
        assert_eq!(statuses[Path::new("new.txt")], FileStatus::Untracked);
        assert_eq!(statuses[Path::new("b.rs")], FileStatus::Renamed);
        assert_eq!(statuses[Path::new("both.rs")], FileStatus::Conflicted);
        assert_eq!(statuses[Path::new("added.rs")], FileStatus::Added);
        assert!(!statuses.contains_key(Path::new("a.rs")));
        assert_eq!(ignored, vec![PathBuf::from("target")]);
    }

    #[test]
    fn keeps_the_symlinked_spelling_of_the_root() {
        let base = std::env::temp_dir().join(format!("runode-git-root-{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(real.join("sub")).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(local_root(&link.join("sub"), real.clone()), link);
        assert_eq!(local_root(&link, real.clone()), link);
        assert_eq!(local_root(&real.join("sub"), real.clone()), real);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(unquote("\"a\\\"b\\tc\""), "a\"b\tc");
        assert_eq!(unquote("\"\\346\\226\\207.txt\""), "文.txt");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(header_path("a/x y.txt b/x y.txt"), Some(PathBuf::from("x y.txt")));
        assert_eq!(header_path("\"a/q\\\"x\" \"b/q\\\"x\""), Some(PathBuf::from("q\"x")));
    }
}
