//! 解析 git 命令行的输出：`git status --porcelain=v1 -z` 列的文件状态、`git diff` 的统一格式，
//! 以及 git 给特殊路径加的引号。

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use crate::{FileDiff, FileStatus, Hunk, Line, LineKind, snapshot::MAX_FILE_LINES};

/// 解析 `git status --porcelain=v1 -z` 的输出：各个文件的状态，以及被忽略的路径。带
/// `--branch` 时开头的分支那一项跳过，由 `info::parse_branch` 读。
pub(crate) fn parse_status(output: &[u8]) -> (HashMap<PathBuf, FileStatus>, Vec<PathBuf>) {
    let mut statuses = HashMap::new();
    let mut ignored = Vec::new();
    let mut fields = output.split(|b| *b == 0).filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        if field.len() < 4 || field.starts_with(b"## ") {
            continue;
        }
        let (x, y) = (field[0], field[1]);
        // 按原来的字节，不是 UTF-8 的文件名也原样留着，暂存、丢弃时才找得到它。
        let path = PathBuf::from(OsStr::from_bytes(field[3..].strip_suffix(b"/").unwrap_or(&field[3..])));
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
pub(crate) fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    // 当前块里下一行的旧行号和新行号。
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    // 当前块的头有没有记下来；超过行数上限后不再记新块，它的行也不能并进上一个块。
    let mut hunk_kept = false;
    // 不是 `diff --git` 开头的文件（三方合并的格式之类）整个跳过。
    let mut skipping = false;
    for line in text.lines() {
        if line.starts_with("diff ") && !line.starts_with("diff --git ") {
            skipping = true;
            continue;
        }
        if let Some(header) = line.strip_prefix("diff --git ") {
            skipping = false;
            files.push(FileDiff {
                path: header_path(header).unwrap_or_default(),
                old_path: None,
                status: FileStatus::Modified,
                added: 0,
                removed: 0,
                hunks: Vec::new(),
                binary: false,
                truncated: false,
                gitlink: false,
            });
            in_hunk = false;
            continue;
        }
        if skipping {
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if !in_hunk {
            // 文件头里的模式是 160000 的是 gitlink：`index 旧..新 160000`、`new file mode 160000`。
            if (line.starts_with("index ") || line.contains(" mode ")) && line.ends_with(" 160000") {
                file.gitlink = true;
            }
            if line.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = Some(unquote(from));
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.path = unquote(to);
            } else if line.starts_with("Binary files ") {
                file.binary = true;
            } else if let Some(to) = line.strip_prefix("+++ ") {
                // 路径里有空格时 git 在行尾补一个制表符。
                if let Ok(path) = unquote(to.trim_end_matches('\t')).strip_prefix("b") {
                    file.path = path.into();
                }
            } else if let Some(from) = line.strip_prefix("--- ") {
                // 删掉的文件新路径是 /dev/null，用旧路径。
                if let Ok(path) = unquote(from.trim_end_matches('\t')).strip_prefix("a") {
                    file.path = path.into();
                }
            }
        }
        if line.starts_with("@@ ") {
            (old, new) = crate::view::first_lines(line).unwrap_or_default();
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

/// `diff --git a/路径 b/路径` 里的路径。只在新旧路径相同时可靠，改名时由后面的
/// `rename to` 或 `+++` 行纠正。
fn header_path(header: &str) -> Option<PathBuf> {
    if let Some(rest) = header.strip_prefix('"') {
        let end = rest.find("\" ")? + 2;
        return unquote(&header[..end]).strip_prefix("a").ok().map(Path::to_path_buf);
    }
    // 两个路径一样长：`a/P b/P` 共 2P+5 个字节。
    let len = header.len().checked_sub(5)? / 2;
    let (a, b) = (header.get(2..2 + len)?, header.get(header.len() - len..)?);
    (a == b).then(|| PathBuf::from(a))
}

/// 去掉 git 给特殊路径加的双引号和 C 风格转义，按转义前的字节还原成路径。
fn unquote(text: &str) -> PathBuf {
    let Some(inner) = text.strip_prefix('"').and_then(|text| text.strip_suffix('"')) else {
        return PathBuf::from(text);
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
    PathBuf::from(OsString::from_vec(bytes))
}

pub(crate) fn expand_tabs(text: &str) -> String {
    text.replace('\t', "    ")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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
        assert_eq!(
            (deleted.status, deleted.path.clone(), deleted.removed),
            (FileStatus::Deleted, "gone.txt".into(), 1)
        );

        let binary = &files[3];
        assert!(binary.binary);
        assert_eq!((binary.status, binary.path.clone()), (FileStatus::Added, "logo.png".into()));
    }

    #[test]
    fn strips_the_tab_git_adds_after_paths_with_spaces() {
        let files =
            parse_diff("diff --git a/a b.txt b/a b.txt\n--- a/a b.txt\t\n+++ b/a b.txt\t\n@@ -1 +1,2 @@\n a\n+b\n");
        assert_eq!(files[0].path, PathBuf::from("a b.txt"));
    }

    #[test]
    fn parses_porcelain_status() {
        let output = b"## main...origin/main [ahead 1]\0 M src/a.rs\0?? new.txt\0R  b.rs\0a.rs\0!! target/\0UU both.rs\0A  added.rs\0";
        let (statuses, ignored) = parse_status(output);
        assert_eq!(statuses[Path::new("src/a.rs")], FileStatus::Modified);
        assert_eq!(statuses[Path::new("new.txt")], FileStatus::Untracked);
        assert_eq!(statuses[Path::new("b.rs")], FileStatus::Renamed);
        assert_eq!(statuses[Path::new("both.rs")], FileStatus::Conflicted);
        assert_eq!(statuses[Path::new("added.rs")], FileStatus::Added);
        assert!(!statuses.contains_key(Path::new("a.rs")));
        assert_eq!(statuses.len(), 5);
        assert_eq!(ignored, vec![PathBuf::from("target")]);

        // 两个不同的非 UTF-8 名字各占一个键，不都变成 U+FFFD。
        let (statuses, _) = parse_status(b"?? caf\xe9\0?? caf\xe8\0");
        assert_eq!(statuses.len(), 2);
        assert!(statuses.contains_key(Path::new(OsStr::from_bytes(b"caf\xe9"))));
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(unquote("\"a\\\"b\\tc\""), Path::new("a\"b\tc"));
        assert_eq!(unquote("\"\\346\\226\\207.txt\""), Path::new("文.txt"));
        // 不是 UTF-8 的字节原样还原，不换成 U+FFFD。
        assert_eq!(unquote("\"caf\\351.txt\"").as_os_str().as_bytes(), b"caf\xe9.txt");
        assert_eq!(header_path("\"a/\\351\" \"b/\\351\"").unwrap().as_os_str().as_bytes(), b"\xe9");
        assert_eq!(unquote("plain"), Path::new("plain"));
        assert_eq!(header_path("a/x y.txt b/x y.txt"), Some(PathBuf::from("x y.txt")));
        assert_eq!(header_path("\"a/q\\\"x\" \"b/q\\\"x\""), Some(PathBuf::from("q\"x")));
    }
}
