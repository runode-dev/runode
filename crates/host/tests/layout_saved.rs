//! 单独跑的宿主在没有界面时自己改的布局（`Host::keep_layout`）：重启后还在，界面报来布局后文件删掉。

mod common;

use std::path::{Path, PathBuf};

use common::{Peer, host, temp_dir};
use runode_host::{ClientMsg, Host, HostMsg};
use runode_protocol::ClientKind;

/// 宿主记着 `file` 里的布局、监听 `dir` 里的 socket。
fn keep_and_listen(dir: &Path, file: &Path) -> (Host, PathBuf) {
    let host = host();
    host.keep_layout(file.to_owned());
    let socket = dir.join("host.sock");
    host.listen(&socket, &dir.join("host.lock")).unwrap();
    (host, socket)
}

/// 手机新建、改名的工作区在宿主重启后还在；界面报来布局（接过去了）后文件删掉。
#[test]
fn a_restarted_host_keeps_the_workspaces_it_changed() {
    let (first, second, work) = (temp_dir("layout-a"), temp_dir("layout-b"), temp_dir("layout-ws"));
    let file = first.join("layout.json");
    let canonical = std::fs::canonicalize(&work).unwrap();
    #[cfg(windows)]
    let canonical = PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap());

    let (_before, socket) = keep_and_listen(&first, &file);
    let mut phone = Peer::connect(&socket).greet(ClientKind::Mobile, false);
    phone.send(&ClientMsg::OpenWorkspace { req: 1, dir: work, focus: false, name: Some("app".into()) });
    let HostMsg::Opened { req: 1, id } = phone.reply() else { panic!("expected opened") };
    phone.send(&ClientMsg::RenameWorkspace { req: 2, window: 1, workspace: 1, name: "renamed".into() });
    assert_eq!(phone.reply(), HostMsg::Done { req: 2 });
    phone.send(&ClientMsg::Kill { id });
    drop(phone);

    // 另一个宿主当重启后的：会话不在了，工作区和名字照文件恢复。
    let (_after, socket) = keep_and_listen(&second, &file);
    let mut phone = Peer::connect(&socket).greet(ClientKind::Mobile, false);
    phone.send(&ClientMsg::Layout { req: 3 });
    let HostMsg::Layout { windows, .. } = phone.reply() else { panic!("expected the layout") };
    let workspaces: Vec<_> = windows
        .iter()
        .flat_map(|window| &window.workspaces)
        .map(|workspace| (workspace.dir.clone(), workspace.name.clone(), workspace.tabs.len()))
        .collect();
    assert_eq!(workspaces, [(Some(canonical), Some("renamed".into()), 0)]);
    assert!(file.exists());

    let mut desktop = Peer::desktop(&socket);
    desktop.send(&ClientMsg::LayoutChanged { windows: Some(windows) });
    // 同一条连接按顺序办：回了这个请求，`LayoutChanged` 就办完了。
    desktop.sessions();
    assert!(!file.exists(), "the desktop took the layout over");
}
