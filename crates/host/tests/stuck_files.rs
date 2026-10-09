//! 要读文件系统的请求（列项目命令、列目录）卡住时不挡着同一条连接上的别的请求。断掉的网络挂载上
//! stat 会一直卡住，这里用命名管道代替：runode 根目录里自己加的命令文件 `tasks.json` 是个没人写的
//! 命名管道时，打开它一直等。
//!
//! 要把家目录指到临时目录，改的是整个进程的环境变量，所以这个文件只放这一个测试。

mod common;

use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    os::unix::{ffi::OsStrExt as _, fs::OpenOptionsExt as _},
    time::{Duration, Instant},
};

use common::{Peer, WAIT, listen, temp_dir};
use runode_host::{ClientMsg, HostMsg};
use runode_protocol::FrameKind;

/// 每条连接最多同时有这么多读文件系统的请求在办，和宿主里的上限一样。
const MAX_WAITING: u32 = 16;

/// 列项目命令卡在打开 `tasks.json` 上时，同一条连接上列会话、列目录照常回话；卡住的占满名额后，
/// 再来的读文件系统的请求当场回 `Error`；管道那头有人写了以后，卡住的都办完。
#[test]
fn a_stuck_file_read_does_not_hold_up_the_connection() {
    let dir = temp_dir("stuckfiles");
    let home = dir.join("home");
    fs::create_dir_all(home.join(".runode")).unwrap();
    let fifo = home.join(".runode").join("tasks.json");
    let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` 是以 NUL 结尾的路径，调用期间一直有效。
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    // SAFETY: 这个测试程序只有这一个测试，这时还没有起别的线程读写环境变量。
    unsafe { std::env::set_var("HOME", &home) };
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);

    peer.send(&ClientMsg::ListProjectTasks { req: 1, dir: dir.clone() });
    peer.send(&ClientMsg::ListSessions);
    assert!(matches!(peer.reply(), HostMsg::SessionList { .. }));
    peer.send(&ClientMsg::ListDirs { req: 2, path: Some(dir.clone()) });
    assert!(matches!(peer.reply(), HostMsg::Dirs { req: 2, .. }));

    for req in 3..=MAX_WAITING + 1 {
        peer.send(&ClientMsg::ListProjectTasks { req, dir: dir.clone() });
    }
    peer.send(&ClientMsg::ListDirs { req: 100, path: Some(dir.clone()) });
    match peer.reply() {
        HostMsg::Error { req: Some(100), message, .. } => assert!(message.contains("too many"), "{message}"),
        other => panic!("expected the extra request to be refused: {other:?}"),
    }

    // 写的一端开一下再关上：卡在打开上的读的一端都放开，读到空的接着办完；还没走到打开的下一轮再放。
    let mut answered = Vec::new();
    let deadline = Instant::now() + WAIT;
    while answered.len() < MAX_WAITING as usize {
        assert!(Instant::now() < deadline, "only {answered:?} were answered");
        if let Ok(writer) = OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&fifo) {
            drop(writer);
        }
        let Ok(frame) = peer.frames.recv_timeout(Duration::from_millis(50)) else { continue };
        if frame.kind == FrameKind::Control
            && let HostMsg::ProjectTasks { req, .. } = frame.message().unwrap()
        {
            answered.push(req);
        }
    }
    answered.sort_unstable();
    assert_eq!(answered, [1].into_iter().chain(3..=MAX_WAITING + 1).collect::<Vec<_>>());
    peer.send(&ClientMsg::ListDirs { req: 101, path: Some(dir) });
    assert!(matches!(peer.reply(), HostMsg::Dirs { req: 101, .. }));
}
