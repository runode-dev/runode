//! 替前端在会话所在的仓库里读写 git（`ClientMsg::Git`）：按会话 shell 当前的目录找到仓库，用
//! `runode_git` 办完，把结果转成 `runode_protocol::git` 的线上类型回话。
//!
//! 每条连接一个工作线程，第一次要时才起，连接断开（`GitWorker` 丢掉）后办完手上的就结束。同一条
//! 连接上的请求按到达的先后一件一件办：先暂存再提交不会颠倒；推送、拉取要等网络，排在后面的跟着等。
//! 会话的目录在读的线程里向会话线程要（排在这条连接之前送去的输入后面），工作线程再等回话。

use std::{path::Path, sync::mpsc, thread};

use runode_git::{Branch, FileDiff, FileStatus, Hunk, Line, LineKind, Operation, Repo, Section, UntrackedCache};
use runode_protocol::{
    HostMsg, SessionId, SessionInfo,
    git::{
        GitBranch, GitFile, GitFileDiff, GitFileStatus, GitHunk, GitLine, GitLineKind, GitOperation, GitRequest,
        GitStatus,
    },
};

use crate::server::{Outbox, REPLY_TIMEOUT, WaitSlot};

/// 一件要办的 git 请求。
pub(crate) struct Job {
    pub(crate) req: u32,
    pub(crate) id: SessionId,
    pub(crate) request: GitRequest,
    /// 会话回的 `SessionInfo`，从里面取 shell 当前的目录。
    pub(crate) info: mpsc::Receiver<SessionInfo>,
    pub(crate) out: Outbox,
    /// 回完话才放开，一条连接上排着的请求不超过 `Waiting` 的上限。
    pub(crate) slot: WaitSlot,
}

/// 一条连接的 git 工作线程。
pub(crate) struct GitWorker {
    jobs: mpsc::Sender<Job>,
}

impl GitWorker {
    pub(crate) fn start() -> std::io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        thread::Builder::new().name("host-git".into()).spawn(move || {
            let mut cache = UntrackedCache::default();
            for job in queue {
                let reply = answer(&job, &mut cache);
                job.out.control(&reply);
                drop(job.slot);
            }
        })?;
        Ok(Self { jobs })
    }

    /// 排进队列；工作线程已经不在了时把这件交回来。
    pub(crate) fn submit(&self, job: Job) -> Result<(), Job> {
        self.jobs.send(job).map_err(|mpsc::SendError(job)| job)
    }
}

fn answer(job: &Job, cache: &mut UntrackedCache) -> HostMsg {
    let (req, id) = (job.req, job.id);
    // 错是这件 git 请求的，不是会话的：不带 `id`，前端看着这个会话的终端页不会当成自己的错。
    let error = |message: String| HostMsg::Error { req: Some(req), id: None, message };
    let dir = match job.info.recv_timeout(REPLY_TIMEOUT) {
        Ok(info) => info.meta.cwd,
        Err(_) => return error(format!("session {id} did not answer")),
    };
    let Some(dir) = dir else {
        return error("the session's directory is unknown".into());
    };
    run(&dir, req, id, job.request.clone(), cache).unwrap_or_else(error)
}

/// 在 `dir` 所在的仓库里办 `request`。
fn run(
    dir: &Path,
    req: u32,
    id: SessionId,
    request: GitRequest,
    cache: &mut UntrackedCache,
) -> Result<HostMsg, String> {
    let repo = || Repo::open(dir).ok_or_else(|| format!("{} is not in a git repository", dir.display()));
    let done = |result: runode_git::Result| result.map_err(|err| err.message);
    match request {
        GitRequest::Status => {}
        GitRequest::Diff { path, staged } => {
            let section = if staged { Section::Staged } else { Section::Unstaged };
            let diff = runode_git::snapshot(dir, cache)
                .and_then(|snapshot| snapshot.files(section).iter().find(|file| file.path == path).map(file_diff));
            return Ok(HostMsg::GitDiff { req, id, diff });
        }
        GitRequest::Branches => {
            let branches = repo()?.branches().into_iter().map(branch).collect();
            return Ok(HostMsg::GitBranches { req, id, branches });
        }
        GitRequest::Stage { paths } => done(repo()?.stage(&paths))?,
        GitRequest::Unstage { paths } => done(repo()?.unstage(&paths))?,
        GitRequest::Discard { paths } => {
            let repo = repo()?;
            // 路径是前端给的：只丢快照里未暂存段确实有的文件，`discard` 还要按状态分开删和恢复。
            let files: Vec<_> = runode_git::snapshot(dir, cache)
                .map(|snapshot| snapshot.unstaged.into_iter().filter(|file| paths.contains(&file.path)).collect())
                .unwrap_or_default();
            done(repo.discard(&files))?;
        }
        GitRequest::StageAll => done(repo()?.stage_all())?,
        GitRequest::UnstageAll => done(repo()?.unstage_all())?,
        GitRequest::Commit { message, stage_all } => {
            let options = runode_git::CommitOptions { stage_all, ..Default::default() };
            done(repo()?.commit(&message, options))?;
        }
        GitRequest::Fetch => done(repo()?.fetch())?,
        GitRequest::Pull => done(repo()?.pull())?,
        GitRequest::Push => done(repo()?.push())?,
        GitRequest::Sync => done(repo()?.sync())?,
        GitRequest::Checkout { branch, remote } => {
            let target = Branch {
                name: branch,
                remote,
                current: false,
                upstream: None,
                subject: String::new(),
                date: String::new(),
            };
            done(repo()?.checkout(&target))?;
        }
        GitRequest::Unknown => return Err("unknown git request".into()),
    }
    Ok(HostMsg::GitStatus { req, id, status: runode_git::snapshot(dir, cache).map(|snapshot| status(&snapshot)) })
}

fn status(snapshot: &runode_git::Snapshot) -> GitStatus {
    let info = &snapshot.info;
    GitStatus {
        root: snapshot.root.clone(),
        branch: info.branch.clone(),
        head: info.head.clone(),
        upstream: info.upstream.clone(),
        ahead: count(info.ahead),
        behind: count(info.behind),
        has_remote: info.has_remote,
        operation: info.operation.map(|operation| match operation {
            Operation::Merge => GitOperation::Merge,
            Operation::Rebase => GitOperation::Rebase,
            Operation::CherryPick => GitOperation::CherryPick,
            Operation::Revert => GitOperation::Revert,
        }),
        staged: snapshot.staged.iter().map(file).collect(),
        unstaged: snapshot.unstaged.iter().map(file).collect(),
    }
}

fn file(diff: &FileDiff) -> GitFile {
    GitFile {
        path: diff.path.clone(),
        old_path: diff.old_path.clone(),
        status: match diff.status {
            FileStatus::Modified => GitFileStatus::Modified,
            FileStatus::Added => GitFileStatus::Added,
            FileStatus::Deleted => GitFileStatus::Deleted,
            FileStatus::Renamed => GitFileStatus::Renamed,
            FileStatus::Untracked => GitFileStatus::Untracked,
            FileStatus::Conflicted => GitFileStatus::Conflicted,
        },
        added: count(diff.added),
        removed: count(diff.removed),
        binary: diff.binary,
        gitlink: diff.gitlink,
    }
}

fn file_diff(diff: &FileDiff) -> GitFileDiff {
    GitFileDiff { file: file(diff), hunks: diff.hunks.iter().map(hunk).collect(), truncated: diff.truncated }
}

fn hunk(hunk: &Hunk) -> GitHunk {
    GitHunk { header: hunk.header.clone(), lines: hunk.lines.iter().map(line).collect() }
}

fn line(line: &Line) -> GitLine {
    GitLine {
        kind: match line.kind {
            LineKind::Context => GitLineKind::Context,
            LineKind::Added => GitLineKind::Added,
            LineKind::Removed => GitLineKind::Removed,
        },
        old: line.old,
        new: line.new,
        text: line.text.clone(),
    }
}

fn branch(branch: Branch) -> GitBranch {
    GitBranch {
        name: branch.name,
        remote: branch.remote,
        current: branch.current,
        upstream: branch.upstream,
        subject: branch.subject,
        date: branch.date,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, process::Command};

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repo_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-host-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        // 不依赖本机的全局配置：身份、签名、钩子都写进仓库自己的配置。
        for (key, value) in
            [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false"), ("core.hooksPath", "no-hooks")]
        {
            git(&dir, &["config", key, value]);
        }
        dir
    }

    fn run_ok(dir: &Path, request: GitRequest, cache: &mut UntrackedCache) -> HostMsg {
        run(dir, 1, SessionId(1), request, cache).unwrap()
    }

    fn status_of(message: HostMsg) -> GitStatus {
        match message {
            HostMsg::GitStatus { status: Some(status), .. } => status,
            other => panic!("expected a status, got {other:?}"),
        }
    }

    /// 一轮常用的操作：看状态、看 diff、暂存、提交、切分支，每步回的都是改完后的状态。
    #[test]
    fn stages_commits_and_switches_branches() {
        let dir = repo_dir("round");
        let mut cache = UntrackedCache::default();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();

        let status = status_of(run_ok(&dir, GitRequest::Status, &mut cache));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.staged.is_empty());
        assert_eq!(status.unstaged.len(), 1);
        assert_eq!(status.unstaged[0].status, GitFileStatus::Untracked);

        let HostMsg::GitDiff { diff: Some(diff), .. } =
            run_ok(&dir, GitRequest::Diff { path: "a.txt".into(), staged: false }, &mut cache)
        else {
            panic!("expected a diff")
        };
        assert_eq!(diff.hunks[0].lines[0].kind, GitLineKind::Added);
        assert_eq!(diff.hunks[0].lines[0].text, "one");
        let HostMsg::GitDiff { diff: None, .. } =
            run_ok(&dir, GitRequest::Diff { path: "a.txt".into(), staged: true }, &mut cache)
        else {
            panic!("an unstaged file has no staged diff")
        };

        let status = status_of(run_ok(&dir, GitRequest::Stage { paths: vec!["a.txt".into()] }, &mut cache));
        assert_eq!(status.staged.len(), 1);
        assert!(status.unstaged.is_empty());

        let status =
            status_of(run_ok(&dir, GitRequest::Commit { message: "first".into(), stage_all: false }, &mut cache));
        assert!(status.staged.is_empty() && status.unstaged.is_empty());
        assert!(status.head.is_some());

        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        let status =
            status_of(run_ok(&dir, GitRequest::Commit { message: "second".into(), stage_all: true }, &mut cache));
        assert!(status.unstaged.is_empty());

        // 丢弃：改了的恢复，未跟踪的删掉；不在改动里的路径不碰。
        std::fs::write(dir.join("a.txt"), "three\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "c.txt\n").unwrap();
        std::fs::write(dir.join("c.txt"), "ignored\n").unwrap();
        let discard = GitRequest::Discard { paths: vec!["a.txt".into(), "b.txt".into(), "c.txt".into()] };
        let status = status_of(run_ok(&dir, discard, &mut cache));
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "two\n");
        assert!(!dir.join("b.txt").exists() && dir.join("c.txt").exists());
        assert_eq!(
            status.unstaged.iter().map(|file| file.path.as_path()).collect::<Vec<_>>(),
            [Path::new(".gitignore")]
        );
        std::fs::remove_file(dir.join(".gitignore")).unwrap();

        git(&dir, &["branch", "side"]);
        let HostMsg::GitBranches { branches, .. } = run_ok(&dir, GitRequest::Branches, &mut cache) else {
            panic!("expected branches")
        };
        let names: Vec<_> = branches.iter().map(|branch| (branch.name.as_str(), branch.current)).collect();
        assert!(names.contains(&("main", true)) && names.contains(&("side", false)), "{names:?}");
        let status = status_of(run_ok(&dir, GitRequest::Checkout { branch: "side".into(), remote: false }, &mut cache));
        assert_eq!(status.branch.as_deref(), Some("side"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 不在仓库里：读状态回空，改仓库的操作报错；git 自己的失败把它的报错原样带回去。
    #[test]
    fn reports_missing_repositories_and_git_errors() {
        let dir = std::env::temp_dir().join(format!("runode-host-git-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cache = UntrackedCache::default();
        if runode_git::in_repo(&dir) {
            // 临时目录落在某个仓库里时这条测不了。
            return;
        }
        assert!(matches!(run_ok(&dir, GitRequest::Status, &mut cache), HostMsg::GitStatus { status: None, .. }));
        assert!(run(&dir, 1, SessionId(1), GitRequest::StageAll, &mut cache).is_err());

        let repo = repo_dir("errors");
        let err =
            run(&repo, 1, SessionId(1), GitRequest::Commit { message: "empty".into(), stage_all: false }, &mut cache)
                .unwrap_err();
        assert!(!err.is_empty());
        let checkout = GitRequest::Checkout { branch: "--orphan=x".into(), remote: false };
        assert!(run(&repo, 1, SessionId(1), checkout, &mut cache).is_err());
        let err = run(&repo, 1, SessionId(1), GitRequest::Stage { paths: vec!["../outside".into()] }, &mut cache)
            .unwrap_err();
        assert!(!err.is_empty());
        std::fs::remove_dir_all(&repo).unwrap();
    }
}
