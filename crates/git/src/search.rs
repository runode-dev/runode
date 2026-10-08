//! 在项目里找文件：按名字找时列出仓库里的文件（`list_files`），按内容找时调 `git grep`（`grep`）。

use std::{
    ffi::OsStr,
    io::{BufRead, BufReader},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{git, in_repo};

/// 按内容找到的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepMatch {
    /// 相对搜索的目录。
    pub path: PathBuf,
    /// 从 1 数。
    pub line: u32,
    pub text: String,
}

/// `root` 下已跟踪和未跟踪、没被忽略的文件，相对 `root`；`root` 不在仓库里时为空。子模块
/// 里的文件不列。
pub fn list_files(root: &Path) -> Option<Vec<PathBuf>> {
    let out = git(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--deduplicate"])?;
    Some(out.split(|&b| b == 0).filter(|path| !path.is_empty()).map(|path| OsStr::from_bytes(path).into()).collect())
}

/// 按内容找什么、在哪些文件里找。
#[derive(Clone, Copy, Debug, Default)]
pub struct GrepQuery<'a> {
    pub pattern: &'a str,
    pub ignore_case: bool,
    /// 只算前后不挨着字母、数字和下划线的。
    pub whole_word: bool,
    /// `pattern` 是扩展正则（ERE），否则按字面找。
    pub regex: bool,
    /// 只找这些路径：git 的 pathspec，可以带 `:(glob)`、`:(exclude)` 这类前缀；为空时找全部。
    pub pathspecs: &'a [String],
}

/// 在 `root` 下按内容找 `query`。在仓库里时连未跟踪的文件一起找、跳过被忽略的，不在仓库里时
/// 找整个目录；跳过二进制文件。找够 `limit` 行就停下，不等 git 把整个目录找完；`cancel` 置位
/// 时杀掉 git、返回已经找到的，用来在搜索词变了时停下在大目录里跑着的上一次。
pub fn grep(root: &Path, query: &GrepQuery, limit: usize, cancel: &AtomicBool) -> Vec<GrepMatch> {
    if query.pattern.is_empty() {
        return Vec::new();
    }
    let scope = if in_repo(root) { "--untracked" } else { "--no-index" };
    let child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=false", "grep", "-z", "-n", "-I", "--no-color", "--exclude-standard"])
        .arg(scope)
        .arg(if query.regex { "-E" } else { "-F" })
        .args(query.ignore_case.then_some("-i"))
        .args(query.whole_word.then_some("-w"))
        .args(["-e", query.pattern, "--"])
        .args(query.pathspecs)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    let stdout = child.stdout.take();
    let child = Mutex::new(child);
    let done = AtomicBool::new(false);
    let mut matches = Vec::new();
    thread::scope(|scope| {
        // git 一直没有输出时读会一直等着，取消得由另一个线程杀掉它。
        scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.lock().map(|mut child| child.kill());
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        for line in stdout.into_iter().flat_map(|stdout| BufReader::new(stdout).split(b'\n')) {
            let Ok(line) = line else {
                break;
            };
            // `-z` 时每行是「路径 NUL 行号 NUL 内容」。
            let mut parts = line.splitn(3, |&b| b == 0);
            let (Some(path), Some(number), Some(text)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let Some(number) = std::str::from_utf8(number).ok().and_then(|number| number.parse().ok()) else {
                continue;
            };
            let text = String::from_utf8_lossy(text);
            matches.push(GrepMatch {
                path: OsStr::from_bytes(path).into(),
                line: number,
                text: text.trim_end_matches('\r').to_owned(),
            });
            if matches.len() >= limit {
                break;
            }
        }
        done.store(true, Ordering::Relaxed);
    });
    // 找够了时 git 可能还在跑。
    if let Ok(mut child) = child.into_inner() {
        let _ = child.kill();
        let _ = child.wait();
    }
    matches
}
