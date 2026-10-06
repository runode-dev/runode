//! 读写 git 的消息在线上的样子。手机端测试用的宿主样例（`git_*` 这一组）照着这里写，改了格式两边
//! 一起改。

use runode_protocol::{
    ClientMsg, HostMsg, SessionId,
    git::{
        GitBranch, GitFile, GitFileDiff, GitFileStatus, GitHunk, GitLine, GitLineKind, GitOperation, GitRequest,
        GitStatus,
    },
};
use serde_json::{Value, json};

const ID: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
const ID_TEXT: &str = "0123456789abcdef0011223344556677";

fn same<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T, expected: Value) {
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

#[test]
fn requests() {
    let git = |req, request| ClientMsg::Git { req, id: ID, request };
    same(&git(1, GitRequest::Status), json!({"type": "git", "req": 1, "id": ID_TEXT, "request": {"op": "status"}}));
    same(
        &git(2, GitRequest::Diff { path: "src/a.rs".into(), staged: true }),
        json!({"type": "git", "req": 2, "id": ID_TEXT, "request": {"op": "diff", "path": "src/a.rs", "staged": true}}),
    );
    same(
        &git(3, GitRequest::Unstage { paths: vec!["b.rs".into(), "a.rs".into()] }),
        json!({"type": "git", "req": 3, "id": ID_TEXT, "request": {"op": "unstage", "paths": ["b.rs", "a.rs"]}}),
    );
    same(
        &git(4, GitRequest::Commit { message: "修好了".into(), stage_all: true }),
        json!({"type": "git", "req": 4, "id": ID_TEXT, "request": {"op": "commit", "message": "修好了", "stage_all": true}}),
    );
    same(
        &git(5, GitRequest::Checkout { branch: "origin/feat".into(), remote: true }),
        json!({"type": "git", "req": 5, "id": ID_TEXT, "request": {"op": "checkout", "branch": "origin/feat", "remote": true}}),
    );
    for (request, op) in [
        (GitRequest::StageAll, "stage_all"),
        (GitRequest::UnstageAll, "unstage_all"),
        (GitRequest::Fetch, "fetch"),
        (GitRequest::Pull, "pull"),
        (GitRequest::Push, "push"),
        (GitRequest::Sync, "sync"),
        (GitRequest::Branches, "branches"),
    ] {
        same(&request, json!({"op": op}));
    }
}

/// 旧的一方读得了新的一方多出来的操作和取值，`commit` 缺了 `stage_all` 按假读。
#[test]
fn unknown_values_are_tolerated() {
    let request: GitRequest = serde_json::from_value(json!({"op": "rebase", "onto": "main"})).unwrap();
    assert_eq!(request, GitRequest::Unknown);
    let request: GitRequest = serde_json::from_value(json!({"op": "commit", "message": "m"})).unwrap();
    assert_eq!(request, GitRequest::Commit { message: "m".into(), stage_all: false });
    let status: GitFileStatus = serde_json::from_value(json!("typechange")).unwrap();
    assert_eq!(status, GitFileStatus::Unknown);
    let operation: GitOperation = serde_json::from_value(json!("bisect")).unwrap();
    assert_eq!(operation, GitOperation::Unknown);
}

#[test]
fn replies() {
    let status = GitStatus {
        root: "/Users/me/app".into(),
        branch: Some("main".into()),
        head: Some("1a2b3c4".into()),
        upstream: Some("origin/main".into()),
        ahead: 2,
        behind: 1,
        has_remote: true,
        operation: Some(GitOperation::CherryPick),
        staged: vec![GitFile {
            path: "src/new.rs".into(),
            old_path: Some("src/old.rs".into()),
            status: GitFileStatus::Renamed,
            added: 3,
            removed: 1,
            binary: false,
            gitlink: false,
        }],
        unstaged: vec![GitFile {
            path: "notes.txt".into(),
            old_path: None,
            status: GitFileStatus::Untracked,
            added: 1,
            removed: 0,
            binary: false,
            gitlink: false,
        }],
    };
    same(
        &HostMsg::GitStatus { req: 1, id: ID, status: Some(status) },
        json!({
            "type": "git_status", "req": 1, "id": ID_TEXT,
            "status": {
                "root": "/Users/me/app", "branch": "main", "head": "1a2b3c4", "upstream": "origin/main",
                "ahead": 2, "behind": 1, "has_remote": true, "operation": "cherry_pick",
                "staged": [{
                    "path": "src/new.rs", "old_path": "src/old.rs", "status": "renamed",
                    "added": 3, "removed": 1, "binary": false, "gitlink": false
                }],
                "unstaged": [{
                    "path": "notes.txt", "old_path": null, "status": "untracked",
                    "added": 1, "removed": 0, "binary": false, "gitlink": false
                }]
            }
        }),
    );
    same(
        &HostMsg::GitStatus { req: 2, id: ID, status: None },
        json!({"type": "git_status", "req": 2, "id": ID_TEXT, "status": null}),
    );

    let diff = GitFileDiff {
        file: GitFile {
            path: "a.txt".into(),
            old_path: None,
            status: GitFileStatus::Modified,
            added: 1,
            removed: 1,
            binary: false,
            gitlink: false,
        },
        hunks: vec![GitHunk {
            header: "@@ -1,2 +1,2 @@ fn main".into(),
            lines: vec![
                GitLine { kind: GitLineKind::Context, old: Some(1), new: Some(1), text: "keep".into() },
                GitLine { kind: GitLineKind::Removed, old: Some(2), new: None, text: "old".into() },
                GitLine { kind: GitLineKind::Added, old: None, new: Some(2), text: "new".into() },
            ],
        }],
        truncated: false,
    };
    same(
        &HostMsg::GitDiff { req: 3, id: ID, diff: Some(diff) },
        json!({
            "type": "git_diff", "req": 3, "id": ID_TEXT,
            "diff": {
                "file": {
                    "path": "a.txt", "old_path": null, "status": "modified",
                    "added": 1, "removed": 1, "binary": false, "gitlink": false
                },
                "hunks": [{
                    "header": "@@ -1,2 +1,2 @@ fn main",
                    "lines": [
                        {"kind": "context", "old": 1, "new": 1, "text": "keep"},
                        {"kind": "removed", "old": 2, "new": null, "text": "old"},
                        {"kind": "added", "old": null, "new": 2, "text": "new"}
                    ]
                }],
                "truncated": false
            }
        }),
    );

    let branches = vec![
        GitBranch {
            name: "main".into(),
            remote: false,
            current: true,
            upstream: Some("origin/main".into()),
            subject: "init".into(),
            date: "2 days ago".into(),
        },
        GitBranch {
            name: "origin/feat".into(),
            remote: true,
            current: false,
            upstream: None,
            subject: "wip".into(),
            date: "1 hour ago".into(),
        },
    ];
    same(
        &HostMsg::GitBranches { req: 4, id: ID, branches },
        json!({
            "type": "git_branches", "req": 4, "id": ID_TEXT,
            "branches": [
                {"name": "main", "remote": false, "current": true, "upstream": "origin/main", "subject": "init", "date": "2 days ago"},
                {"name": "origin/feat", "remote": true, "current": false, "upstream": null, "subject": "wip", "date": "1 hour ago"}
            ]
        }),
    );
}
