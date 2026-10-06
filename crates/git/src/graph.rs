//! 提交历史和图表：读最近的提交和指向它们的引用，排出每一行左边的分支线（lane），读单个提交
//! 改了什么。界面只管照着 `GraphRow` 画线。

use std::{collections::HashSet, fs, path::Path};

use crate::{FileDiff, GitError, Repo, Result, git, ops::run, parse::parse_diff, snapshot::MAX_DIFF_BYTES};

/// 历史里的一个提交。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    /// 完整的提交号。
    pub id: String,
    /// 父提交的完整提交号，第一个是合并进来之前所在的那条线。
    pub parents: Vec<String>,
    /// 说明的第一行。
    pub subject: String,
    pub author: String,
    /// 提交时间，相对现在，如 `2 days ago`。
    pub date: String,
    /// 指向这个提交的引用，按 git 给的顺序：HEAD 和它所在的分支在前。
    pub refs: Vec<CommitRef>,
}

impl Commit {
    /// 短提交号，界面上显示用。
    pub fn short_id(&self) -> &str {
        self.id.get(..7).unwrap_or(&self.id)
    }
}

/// 指向提交的一个引用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRef {
    /// 分支名、带远端名的远端分支（如 `origin/main`）或者 tag 名；分离的 HEAD 是 `HEAD`。
    pub name: String,
    pub kind: RefKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// 分离头指针时的 HEAD。
    Head,
    /// HEAD 所在的本地分支。
    CurrentBranch,
    Branch,
    Remote,
    Tag,
}

/// 读到的一段历史和排好的图。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    /// 新的在前，父提交总在它的子提交后面，同等条件下按提交时间。
    pub commits: Vec<Commit>,
    /// 和 `commits` 一一对应。
    pub rows: Vec<GraphRow>,
    /// 后面还有更早的提交没读。
    pub more: bool,
}

/// 图里一行：提交的点在第几条 lane，以及这一行里要画的线段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    pub column: usize,
    /// 点的颜色，是第几种颜色（从 0 数，界面按自己的调色板轮换）。
    pub color: usize,
    /// 这一行用到几条 lane，含空着的。
    pub width: usize,
    pub lines: Vec<GraphLine>,
}

/// 一行里的一条线段。上半段从行顶的 `from` 列连到点所在高度的 `to` 列；下半段从点所在高度的
/// `from` 列连到行底的 `to` 列。两列相同是竖线，不同是斜着连过去。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLine {
    pub half: Half,
    pub from: usize,
    pub to: usize,
    pub color: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    Top,
    Bottom,
}

/// 往下走的一条 lane：等着哪个提交出现，画什么颜色。
struct Lane {
    commit: String,
    color: usize,
}

/// 第一个空着的 lane，没有就在最右边加一条。
fn free_lane(lanes: &mut Vec<Option<Lane>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

/// 给按 `History::commits` 的顺序排好的提交排图。每条 lane 等着一个还没出现的提交：
///
/// - 提交出现时落在第一条等着它的 lane 上；还有别的 lane 也等着它（几个分支从这里分出去），
///   在上半段斜着并过来，那几条 lane 空出来。没有 lane 等着它（分支的最新提交）时占第一条空的。
/// - 第一个父提交接着用这条 lane、同一种颜色；合并进来的父提交另占一条空的、换一种颜色。父提交
///   已经有 lane 等着时（别的分支先走到了它）不另占，下半段斜着连过去。
/// - 和这个提交无关的 lane 上下两段都是竖线。lane 空出来以后留在原处，不往左挪，下一个新分支
///   先用它；最右边空着的去掉。
pub fn graph_layout(commits: &[Commit]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut next_color = 0;
    let mut new_color = || {
        next_color += 1;
        next_color - 1
    };
    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let mut lines = Vec::new();
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| lane.as_ref().is_some_and(|lane| lane.commit == commit.id))
            .map(|(ix, _)| ix)
            .collect();
        let (column, color) = match waiting.first() {
            Some(&ix) => (ix, lanes[ix].as_ref().map_or(0, |lane| lane.color)),
            None => (free_lane(&mut lanes), new_color()),
        };
        for (ix, lane) in lanes.iter().enumerate() {
            if let Some(lane) = lane {
                let to = if lane.commit == commit.id { column } else { ix };
                lines.push(GraphLine { half: Half::Top, from: ix, to, color: lane.color });
            }
        }
        for &ix in &waiting {
            lanes[ix] = None;
        }
        for (ix, lane) in lanes.iter().enumerate() {
            if let Some(lane) = lane {
                lines.push(GraphLine { half: Half::Bottom, from: ix, to: ix, color: lane.color });
            }
        }
        for (pi, parent) in commit.parents.iter().enumerate() {
            let existing = lanes.iter().position(|lane| lane.as_ref().is_some_and(|lane| lane.commit == *parent));
            if let Some(ix) = existing {
                let line_color = if pi == 0 { color } else { lanes[ix].as_ref().map_or(color, |lane| lane.color) };
                lines.push(GraphLine { half: Half::Bottom, from: column, to: ix, color: line_color });
                continue;
            }
            let (ix, lane_color) = if pi == 0 { (column, color) } else { (free_lane(&mut lanes), new_color()) };
            lanes[ix] = Some(Lane { commit: parent.clone(), color: lane_color });
            lines.push(GraphLine { half: Half::Bottom, from: column, to: ix, color: lane_color });
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        let width = lines.iter().map(|line| line.from.max(line.to) + 1).max().unwrap_or(0).max(column + 1);
        rows.push(GraphRow { column, color, width, lines });
    }
    rows
}

/// 解析 `git log --decorate=full` 的 `%D`：`HEAD -> refs/heads/main, refs/remotes/origin/main,
/// tag: refs/tags/v1`。引用名里不会有空格和逗号，按 `, ` 切开不会切错。远端的 `HEAD` 这类
/// 指向别的分支的符号引用、stash 和其他引用不要。
fn parse_refs(decoration: &str) -> Vec<CommitRef> {
    decoration
        .split(", ")
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (name, kind) = if let Some(branch) = part.strip_prefix("HEAD -> refs/heads/") {
                (branch, RefKind::CurrentBranch)
            } else if part == "HEAD" {
                ("HEAD", RefKind::Head)
            } else if let Some(tag) = part.strip_prefix("tag: refs/tags/") {
                (tag, RefKind::Tag)
            } else if let Some(branch) = part.strip_prefix("refs/heads/") {
                (branch, RefKind::Branch)
            } else {
                let remote = part.strip_prefix("refs/remotes/")?;
                if remote.ends_with("/HEAD") {
                    return None;
                }
                (remote, RefKind::Remote)
            };
            Some(CommitRef { name: name.to_owned(), kind })
        })
        .collect()
}

/// 解析 `git log -z` 按 `LOG_FORMAT` 给的输出。
fn parse_log(output: &[u8]) -> Vec<Commit> {
    output
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            let mut fields = entry.split('\x1f');
            let (id, parents, author, date, refs) =
                (fields.next()?, fields.next()?, fields.next()?, fields.next()?, fields.next()?);
            let subject = fields.collect::<Vec<_>>().join("\x1f");
            Some(Commit {
                id: id.trim().to_owned(),
                parents: parents.split_whitespace().map(str::to_owned).collect(),
                subject,
                author: author.to_owned(),
                date: date.to_owned(),
                refs: parse_refs(refs),
            })
        })
        .collect()
}

/// 监听到变了的 `path`（绝对路径）是不是 git 目录是 `git_dir` 的那个仓库的引用：`HEAD`、`refs/`
/// 下面的分支和 tag、`packed-refs`。worktree 的分支和 tag 在 `commondir` 指向的共用 git 目录里，
/// 也算。锁文件不算，写完会改名成正式的文件。图表据此重读，工作区里的文件变了不重读。
pub fn refs_changed(git_dir: &Path, path: &Path) -> bool {
    // 先按名字粗筛，大多数事件用不着去碰文件系统。
    let named = |name: &str| path.file_name().is_some_and(|file| file == name);
    let maybe_ref = named("HEAD") || named("packed-refs") || path.components().any(|part| part.as_os_str() == "refs");
    if !maybe_ref || path.extension().is_some_and(|ext| ext == "lock") {
        return false;
    }
    // 事件里的路径解析过符号链接，按原样和真实路径各比一次。
    let under = |dir: &Path, head: bool| {
        let real = fs::canonicalize(dir).ok();
        std::iter::once(dir.to_path_buf()).chain(real).any(|dir| {
            path.strip_prefix(&dir).is_ok_and(|rel| {
                rel.starts_with("refs") || rel == Path::new("packed-refs") || (head && rel == Path::new("HEAD"))
            })
        })
    };
    if under(git_dir, true) {
        return true;
    }
    let common = fs::read_to_string(git_dir.join("commondir")).ok().map(|common| git_dir.join(common.trim()));
    common.is_some_and(|common| under(&common, false))
}

/// 提交号、父提交、作者、相对时间、引用和说明首行，字段之间用 0x1f 隔开。
const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ar%x1f%D%x1f%s";

impl Repo {
    /// 最近的 `limit` 个提交和排好的图：HEAD、所有本地分支，以及这些本地分支还在的上游，合在
    /// 一起按 `--date-order` 排（父提交总在子提交后面，其余按提交时间）。别的远端分支和 tag
    /// 不另外列，落在这些提交上的照样标出来。还没有任何提交时为空。
    pub fn history(&self, limit: usize) -> Result<History> {
        let refs =
            run(&self.root, ["for-each-ref", "--format=%(refname)%00%(upstream)", "refs/heads", "refs/remotes"], None)?;
        let refs = String::from_utf8_lossy(&refs).into_owned();
        let existing: HashSet<&str> = refs.lines().filter_map(|line| line.split('\0').next()).collect();
        let mut upstreams: Vec<&str> = refs
            .lines()
            .filter_map(|line| line.split('\0').nth(1))
            .filter(|upstream| !upstream.is_empty() && existing.contains(upstream))
            .collect();
        upstreams.sort_unstable();
        upstreams.dedup();
        let has_head = git(&self.root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_some();
        let has_branches = existing.iter().any(|name| name.starts_with("refs/heads/"));
        if !has_head && !has_branches {
            return Ok(History::default());
        }
        let count = format!("--max-count={}", limit.saturating_add(1));
        let mut args = vec!["-c", "log.showSignature=false", "log", "--date-order", "-z", "--no-color"];
        args.extend(["--decorate=full", "--decorate-refs-exclude=refs/stash", LOG_FORMAT, &count]);
        if has_head {
            args.push("HEAD");
        }
        args.push("--branches");
        args.extend(upstreams);
        args.push("--");
        let mut commits = parse_log(&run(&self.root, args, None)?);
        let more = commits.len() > limit;
        commits.truncate(limit);
        let rows = graph_layout(&commits);
        Ok(History { commits, rows, more })
    }

    /// `commit` 改了哪些文件、每个文件改了哪几行；合并提交和第一个父提交比，第一个提交和空的
    /// 树比。和工作区的改动一样按 `MAX_DIFF_BYTES` 把大文件当二进制，但不看工作区里的文件。
    pub fn commit_changes(&self, commit: &Commit) -> Result<Vec<FileDiff>> {
        let base = match commit.parents.first() {
            Some(parent) => parent.clone(),
            None => {
                let tree = run(&self.root, ["hash-object", "-t", "tree", "/dev/null"], None)?;
                String::from_utf8_lossy(&tree).trim().to_owned()
            }
        };
        let threshold = format!("core.bigFileThreshold={MAX_DIFF_BYTES}");
        let args = [
            "-c",
            &threshold,
            "diff",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &base,
            &commit.id,
            "--",
        ];
        Ok(parse_diff(&String::from_utf8_lossy(&run(&self.root, args, None)?)))
    }

    /// 切到提交 `id`，成为分离头指针。工作区的改动和它冲突时报 git 的错。
    pub fn checkout_detached(&self, id: &str) -> Result {
        if id.starts_with('-') {
            return Err(GitError::new(format!("不是提交号：{id}")));
        }
        run(&self.root, ["switch", "--detach", id], None).map(drop)
    }

    /// 从提交 `start` 新建分支 `name` 并切过去。
    pub fn create_branch_at(&self, name: &str, start: &str) -> Result {
        if start.starts_with('-') {
            return Err(GitError::new(format!("不是提交号：{start}")));
        }
        run(&self.root, ["switch", "-c", name, start], None).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decorations() {
        let refs = parse_refs(
            "HEAD -> refs/heads/main, refs/remotes/origin/main, refs/remotes/origin/HEAD, tag: refs/tags/v1, refs/heads/feat/x, refs/notes/n",
        );
        let names: Vec<_> = refs.iter().map(|r| (r.name.as_str(), r.kind)).collect();
        assert_eq!(
            names,
            [
                ("main", RefKind::CurrentBranch),
                ("origin/main", RefKind::Remote),
                ("v1", RefKind::Tag),
                ("feat/x", RefKind::Branch),
            ]
        );
        assert_eq!(parse_refs("HEAD, refs/heads/main")[0].kind, RefKind::Head);
        assert!(parse_refs("").is_empty());
    }

    #[test]
    fn parses_log_entries() {
        let output = b"aaa\x1fbbb ccc\x1fT\x1f2 days ago\x1fHEAD -> refs/heads/main\x1fmerge\x1fit\0ddd\x1f\x1fU\x1fnow\x1f\x1froot\0";
        let commits = parse_log(output);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents, ["bbb", "ccc"]);
        assert_eq!(commits[0].subject, "merge\x1fit");
        assert_eq!(commits[0].refs[0].name, "main");
        assert!(commits[1].parents.is_empty() && commits[1].refs.is_empty());
    }
}
