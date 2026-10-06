//! 仓库的分支、上游、进行中的合并之类和 stash，随 `snapshot` 一起读进 `Snapshot::info`。

use std::path::Path;

use crate::git;

/// 仓库此刻在哪个分支上、和上游差几个提交、有没有做到一半的合并，以及 stash 列表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoInfo {
    /// 当前分支名；分离头指针时为空。
    pub branch: Option<String>,
    /// HEAD 的短哈希；还没有提交时为空。
    pub head: Option<String>,
    /// 上游分支，如 `origin/main`；没设上游，或者设了但远端已经删了这个分支时为空。
    pub upstream: Option<String>,
    /// 本地比上游多、少的提交数。
    pub ahead: usize,
    pub behind: usize,
    /// 配了至少一个远端。
    pub has_remote: bool,
    /// 做到一半、等着继续或放弃的操作。
    pub operation: Option<Operation>,
    /// `stash@{0}` 在前。
    pub stashes: Vec<Stash>,
}

/// 做到一半的操作，通常是停在冲突上等用户解决。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

/// stash 列表里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// `stash@{index}` 里的序号，给 `Repo::stash_apply` 这些用。
    pub index: usize,
    /// `git stash list` 的标题，如 `WIP on main: 1a2b3c4 改了什么` 或 `On main: 留着的说明`。
    pub message: String,
}

/// 和 `git status` 同时跑的另外几个进程读到的：stash 列表，有没有远端。
pub(crate) struct Extra {
    stashes: Vec<Stash>,
    has_remote: bool,
}

pub(crate) fn read_extra(repo: &Path) -> Extra {
    let (stashes, remotes) = std::thread::scope(|scope| {
        let stashes = scope.spawn(|| git(repo, &["stash", "list", "-z", "--format=%s"]));
        let remotes = git(repo, &["remote"]);
        (stashes.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)), remotes)
    });
    Extra {
        stashes: parse_stashes(&stashes.unwrap_or_default()),
        has_remote: remotes.is_some_and(|remotes| remotes.iter().any(|b| !b.is_ascii_whitespace())),
    }
}

/// 拼出 `RepoInfo`：`status` 是带 `--branch` 的 `git status --porcelain=v1 -z` 的输出，
/// `head` 是 HEAD 的短哈希，`git_dir` 用来看有没有做到一半的操作。
pub(crate) fn read(status: &[u8], head: Option<String>, git_dir: &Path, extra: Extra) -> RepoInfo {
    let line = status.split(|b| *b == 0).next().unwrap_or_default();
    let mut info = parse_branch(&String::from_utf8_lossy(line));
    info.head = head;
    info.has_remote = extra.has_remote;
    info.operation = operation(git_dir);
    info.stashes = extra.stashes;
    info
}

/// 解析 `git status --porcelain=v1 --branch` 开头那一项，只填分支、上游和 ahead/behind。
/// 写法有 `## main`、`## main...origin/main [ahead 1, behind 2]`、`## main...origin/main [gone]`、
/// `## No commits yet on main`（老版本是 `Initial commit on main`）和 `## HEAD (no branch)`。
/// 分支名里不会有空格、`[` 和 `..`，按这几个切开不会切错。
pub(crate) fn parse_branch(line: &str) -> RepoInfo {
    let mut info = RepoInfo::default();
    let Some(rest) = line.strip_prefix("## ") else {
        return info;
    };
    let rest = ["No commits yet on ", "Initial commit on "]
        .iter()
        .find_map(|prefix| rest.strip_prefix(prefix))
        .unwrap_or(rest);
    if rest.starts_with("HEAD (no branch)") {
        return info;
    }
    let (names, tracking) = match rest.split_once(" [") {
        Some((names, tracking)) => (names, tracking.trim_end_matches(']')),
        None => (rest, ""),
    };
    let (branch, upstream) = match names.split_once("...") {
        Some((branch, upstream)) => (branch, Some(upstream)),
        None => (names, None),
    };
    info.branch = Some(branch.to_owned());
    if tracking == "gone" {
        return info;
    }
    info.upstream = upstream.map(str::to_owned);
    for part in tracking.split(", ") {
        let count = |prefix| part.strip_prefix(prefix).and_then(|n: &str| n.parse().ok());
        if let Some(n) = count("ahead ") {
            info.ahead = n;
        } else if let Some(n) = count("behind ") {
            info.behind = n;
        }
    }
    info
}

/// 按 git 目录里留下的标记文件认做到一半的操作。worktree 的这些文件在它自己的 git 目录里。
/// 变基停下来时也可能留着 `CHERRY_PICK_HEAD`，所以先认变基；`rebase-apply` 也是 `git am`
/// 用的目录，`git am` 做到一半时不算任何一种。
fn operation(git_dir: &Path) -> Option<Operation> {
    let has = |name: &str| git_dir.join(name).exists();
    if has("rebase-merge") {
        Some(Operation::Rebase)
    } else if has("rebase-apply") {
        (!has("rebase-apply/applying")).then_some(Operation::Rebase)
    } else if has("MERGE_HEAD") {
        Some(Operation::Merge)
    } else if has("CHERRY_PICK_HEAD") {
        Some(Operation::CherryPick)
    } else if has("REVERT_HEAD") {
        Some(Operation::Revert)
    } else {
        None
    }
}

/// `git stash list -z --format=%s` 的输出：每个 stash 一项，`stash@{0}` 在前。
fn parse_stashes(output: &[u8]) -> Vec<Stash> {
    let text = String::from_utf8_lossy(output);
    let text = text.strip_suffix('\0').unwrap_or(&text);
    if text.trim().is_empty() {
        return Vec::new();
    }
    text.split('\0').enumerate().map(|(index, message)| Stash { index, message: message.trim().to_owned() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_lines() {
        let info = parse_branch("## main...origin/main [ahead 1, behind 22]");
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.upstream.as_deref(), Some("origin/main"));
        assert_eq!((info.ahead, info.behind), (1, 22));

        let info = parse_branch("## feat/x...origin/feat/x [behind 3]");
        assert_eq!((info.branch.as_deref(), info.ahead, info.behind), (Some("feat/x"), 0, 3));

        let info = parse_branch("## main");
        assert_eq!((info.branch.as_deref(), info.upstream), (Some("main"), None));

        let info = parse_branch("## main...origin/main [gone]");
        assert_eq!((info.branch.as_deref(), info.upstream), (Some("main"), None));

        let info = parse_branch("## No commits yet on dev");
        assert_eq!(info.branch.as_deref(), Some("dev"));
        assert_eq!(parse_branch("## Initial commit on dev").branch.as_deref(), Some("dev"));

        assert_eq!(parse_branch("## HEAD (no branch)"), RepoInfo::default());
    }

    #[test]
    fn parses_stash_list() {
        assert!(parse_stashes(b"").is_empty());
        let stashes = parse_stashes(b"On main: two\0WIP on main: 1a2b3c4 one\n");
        assert_eq!(stashes.len(), 2);
        assert_eq!(stashes[0], Stash { index: 0, message: "On main: two".into() });
        assert_eq!(stashes[1].message, "WIP on main: 1a2b3c4 one");
    }
}
