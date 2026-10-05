//! 分支：列出本地和远端分支，切换、新建分支。只对顶层仓库。

use std::process::{Command, Stdio};

use crate::{GitError, Repo, Result, git, ops::run};

/// 分支列表里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// 本地分支是 `main` 这样的名字，远端分支带上远端名，如 `origin/main`。
    pub name: String,
    pub remote: bool,
    /// 当前所在的分支；远端分支总是假。
    pub current: bool,
    /// 本地分支的上游，如 `origin/main`。
    pub upstream: Option<String>,
    /// 最近一次提交的标题。
    pub subject: String,
    /// 最近一次提交的相对时间，如 `2 days ago`。
    pub date: String,
}

/// `name` 能不能用作新分支的名字，按 `git check-ref-format --branch` 的规矩。
pub fn valid_branch_name(name: &str) -> bool {
    // `@{-1}` 这类写法 git 会当成「上一个分支」去解析，`HEAD` 新版 git 也不让用。
    if name.is_empty() || name.starts_with('-') || name.contains("@{") || name == "HEAD" {
        return false;
    }
    Command::new("git")
        .args(["check-ref-format", "--branch", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 解析 `branches` 里 `git for-each-ref` 的输出，一行一个引用，字段间用 NUL 隔开。
fn parse_branches(output: &str) -> Vec<Branch> {
    let mut branches: Vec<Branch> = output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let (refname, head, upstream, symref, date) =
                (fields.next()?, fields.next()?, fields.next()?, fields.next()?, fields.next()?);
            let subject = fields.collect::<Vec<_>>().join("\0");
            // `origin/HEAD` 这类指向别的分支的符号引用不算分支。
            if !symref.is_empty() {
                return None;
            }
            let (name, remote) = match refname.strip_prefix("refs/heads/") {
                Some(name) => (name, false),
                None => (refname.strip_prefix("refs/remotes/")?, true),
            };
            if remote && name.ends_with("/HEAD") {
                return None;
            }
            Some(Branch {
                name: name.to_owned(),
                remote,
                current: head == "*",
                upstream: (!upstream.is_empty()).then(|| upstream.to_owned()),
                subject,
                date: date.to_owned(),
            })
        })
        .collect();
    // 输出已经按提交时间倒序，稳定排序只把本地分支挪到前面。
    branches.sort_by_key(|branch| branch.remote);
    branches
}

impl Repo {
    /// 顶层仓库的本地分支和远端分支，本地的在前、远端的在后，各按最近一次提交的时间
    /// 倒序。读不出来时为空。
    pub fn branches(&self) -> Vec<Branch> {
        let format = "--format=%(refname)%00%(HEAD)%00%(upstream:strip=2)%00%(symref)%00%(committerdate:relative)%00%(contents:subject)";
        let output = git(&self.root, &["for-each-ref", "--sort=-committerdate", format, "refs/heads", "refs/remotes"]);
        parse_branches(&String::from_utf8_lossy(&output.unwrap_or_default()))
    }

    /// 切到 `branch`。远端分支：已经有同名的本地分支就切过去，否则新建一个跟踪它的本地分支。
    /// 工作区的改动和目标分支冲突时报 git 的错。
    pub fn checkout(&self, branch: &Branch) -> Result {
        if !branch.remote {
            return run(&self.root, ["switch", &branch.name], None).map(drop);
        }
        // 远端名本身可以带 `/`，按最长的那个远端名切开。
        let remotes = String::from_utf8_lossy(&run(&self.root, ["remote"], None)?).into_owned();
        let local = remotes
            .lines()
            .filter_map(|remote| branch.name.strip_prefix(remote.trim())?.strip_prefix('/'))
            .min_by_key(|local| local.len())
            .ok_or_else(|| GitError::new(format!("找不到 {} 所属的远端", branch.name)))?;
        let local_ref = format!("refs/heads/{local}");
        if git(&self.root, &["rev-parse", "--verify", "--quiet", &local_ref]).is_some() {
            return run(&self.root, ["switch", local], None).map(drop);
        }
        run(&self.root, ["switch", "-c", local, "--track", &branch.name], None).map(drop)
    }

    /// 从当前提交新建分支 `name` 并切过去。
    pub fn create_branch(&self, name: &str) -> Result {
        run(&self.root, ["switch", "-c", name], None).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_for_each_ref_output() {
        let rows = [
            ["refs/heads/feat", " ", "", "", "1 hour ago", "add a\0b"],
            ["refs/remotes/origin/HEAD", " ", "", "refs/remotes/origin/main", "2 days ago", "init"],
            ["refs/remotes/origin/main", " ", "", "", "2 days ago", "init"],
            ["refs/heads/main", "*", "origin/main", "", "2 days ago", "init"],
        ];
        let output: String = rows.iter().map(|row| row.join("\0") + "\n").collect();
        let branches = parse_branches(&output);
        let names: Vec<_> = branches.iter().map(|branch| (branch.name.as_str(), branch.remote)).collect();
        assert_eq!(names, [("feat", false), ("main", false), ("origin/main", true)]);
        assert_eq!(branches[0].subject, "add a\0b");
        assert_eq!(branches[0].date, "1 hour ago");
        assert!(branches[1].current && !branches[0].current);
        assert_eq!(branches[1].upstream.as_deref(), Some("origin/main"));
    }
}
