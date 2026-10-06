//! 经 socket 请宿主在会话所在的仓库里读写 git（`ClientMsg::Git`）。

mod common;

use std::{path::Path, process::Command};

use common::{Peer, listen, temp_dir};
use runode_host::{ClientMsg, HostMsg, SessionId};
use runode_protocol::git::{GitFileStatus, GitRequest};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(dir).args(args).status().unwrap();
    assert!(status.success(), "git {args:?}");
}

/// 按会话的目录找仓库：状态、暂存都回改完后的状态，同一条连接上的请求按先后办；没有这个会话、
/// 会话不在仓库里时也带着请求的编号回话，错误不带会话的 `id`。
#[test]
fn git_requests_run_in_the_session_directory() {
    let dir = temp_dir("git");
    let (_host, socket) = listen(&dir);
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();

    let mut peer = Peer::hello(&socket, false);
    // 不启动 shell：会话的目录就是开它时给的目录。
    let id = peer.spawn_with("/bin/cat", false, Vec::new(), Some(repo));

    peer.send(&ClientMsg::Git { req: 1, id, request: GitRequest::Status });
    peer.send(&ClientMsg::Git { req: 2, id, request: GitRequest::Stage { paths: vec!["a.txt".into()] } });
    let HostMsg::GitStatus { req: 1, id: answered, status: Some(status) } = peer.reply() else {
        panic!("expected the status")
    };
    assert_eq!(answered, id);
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.unstaged.iter().map(|file| file.status).collect::<Vec<_>>(), [GitFileStatus::Untracked]);
    let HostMsg::GitStatus { req: 2, status: Some(status), .. } = peer.reply() else {
        panic!("expected the status after staging")
    };
    assert_eq!(status.staged.len(), 1);
    assert!(status.unstaged.is_empty());

    peer.send(&ClientMsg::Git { req: 3, id: SessionId(1), request: GitRequest::Status });
    assert!(matches!(peer.reply(), HostMsg::Error { req: Some(3), id: None, .. }));

    let elsewhere = peer.spawn_with("/bin/cat", false, Vec::new(), Some(dir));
    peer.send(&ClientMsg::Git { req: 4, id: elsewhere, request: GitRequest::Status });
    match peer.reply() {
        // 临时目录本身落在某个仓库里时读得到那个仓库，这里只管回的是这条请求。
        HostMsg::GitStatus { req: 4, .. } => {}
        other => panic!("expected a status, got {other:?}"),
    }
    peer.send(&ClientMsg::Kill { id });
    peer.send(&ClientMsg::Kill { id: elsewhere });
}
