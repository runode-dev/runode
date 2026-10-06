//! 前端浏览宿主这台电脑上的目录（`ClientMsg::ListDirs`）：只列子目录，跟着符号链接看，按名字不分
//! 大小写排好，太多时截断；列不了时回 `Error`，连接照旧。

mod common;

use std::{
    fs,
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    path::PathBuf,
};

use common::{Peer, listen, temp_dir};
use runode_host::{ClientMsg, HostMsg};
use runode_protocol::message::MAX_DIRS;

fn list(peer: &mut Peer, req: u32, path: Option<PathBuf>) -> HostMsg {
    peer.send(&ClientMsg::ListDirs { req, path });
    peer.reply()
}

/// 文件、指向文件的和指向不在了的东西的符号链接都不列；指向目录的符号链接和隐藏目录列出来。给的路径
/// 不规范时回规范化后的那个。
#[test]
fn only_subdirectories_are_listed_in_name_order() {
    let dir = temp_dir("dirs");
    let (_host, socket) = listen(&dir);
    let tree = dir.join("tree");
    for name in ["b", "A", "c", ".hidden", "中文"] {
        fs::create_dir_all(tree.join(name)).unwrap();
    }
    fs::write(tree.join("file"), "x").unwrap();
    std::os::unix::fs::symlink(tree.join("c"), tree.join("link-dir")).unwrap();
    std::os::unix::fs::symlink(tree.join("file"), tree.join("link-file")).unwrap();
    std::os::unix::fs::symlink(tree.join("gone"), tree.join("dangling")).unwrap();
    // 有的文件系统（比如 APFS）不让建名字不是 UTF-8 的目录，建得了时它不出现在列表里。
    let _ = fs::create_dir(tree.join(std::ffi::OsStr::from_bytes(b"bad-\xff")));

    let mut peer = Peer::hello(&socket, false);
    let reply = list(&mut peer, 1, Some(tree.join("b").join("..")));
    let expected = HostMsg::Dirs {
        req: 1,
        path: fs::canonicalize(&tree).unwrap(),
        dirs: [".hidden", "A", "b", "c", "link-dir", "中文"].map(String::from).to_vec(),
        truncated: false,
    };
    assert_eq!(reply, expected);
    // 空目录。
    let reply = list(&mut peer, 2, Some(tree.join("A")));
    assert!(matches!(reply, HostMsg::Dirs { req: 2, dirs, truncated: false, .. } if dirs.is_empty()));
}

/// 没给路径时列家目录。
#[test]
fn no_path_lists_the_home_directory() {
    let dir = temp_dir("dirshome");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    let home = runode_paths::Dirs::from_env().home.expect("tests run with a home directory");
    match list(&mut peer, 3, None) {
        HostMsg::Dirs { req: 3, path, .. } => assert_eq!(path, fs::canonicalize(home).unwrap()),
        other => panic!("{other:?}"),
    }
}

/// 子目录多于 `MAX_DIRS` 个：给排在前面的那些，标出截断了。
#[test]
fn long_listings_are_truncated() {
    let dir = temp_dir("dirsmany");
    let (_host, socket) = listen(&dir);
    let tree = dir.join("many");
    for i in 0..=MAX_DIRS {
        fs::create_dir_all(tree.join(format!("d{i:05}"))).unwrap();
    }
    let mut peer = Peer::hello(&socket, false);
    match list(&mut peer, 4, Some(tree)) {
        HostMsg::Dirs { req: 4, dirs, truncated: true, .. } => {
            assert_eq!(dirs.len(), MAX_DIRS);
            assert_eq!((dirs[0].as_str(), dirs[MAX_DIRS - 1].as_str()), ("d00000", "d00999"));
        }
        other => panic!("{other:?}"),
    }
}

/// 相对路径、不存在的、不是目录的、没权限读的：回带着请求编号的 `Error`，连接照旧。
#[test]
fn unlistable_paths_are_errors() {
    let dir = temp_dir("dirserr");
    let (_host, socket) = listen(&dir);
    fs::write(dir.join("file"), "x").unwrap();
    let locked = dir.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // 以 root 跑时照样读得了，这一项不测。
    let locked_out = fs::read_dir(&locked).is_err();

    let mut peer = Peer::hello(&socket, false);
    let mut bad = vec![PathBuf::from("relative/dir"), dir.join("missing"), dir.join("file")];
    if locked_out {
        bad.push(locked.clone());
    }
    for (req, path) in (10..).zip(bad) {
        let reply = list(&mut peer, req, Some(path.clone()));
        assert!(
            matches!(&reply, HostMsg::Error { req: Some(r), id: None, message } if *r == req && !message.is_empty()),
            "{path:?}: {reply:?}"
        );
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(list(&mut peer, 20, Some(dir)), HostMsg::Dirs { req: 20, .. }));
}
