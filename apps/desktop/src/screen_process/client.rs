//! 连上采集进程（`connect`），没有在跑的就拉起一个；远程访问连不上时交回一条说明原因的连接
//! （`connect_or_explain`）。

use std::{
    io,
    net::Shutdown,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    Frame, FrameKind, read_frame,
    screen::{SCREEN_VERSION, ScreenClientMsg, ScreenDisplay, ScreenHostMsg, StopCause},
    write_frame,
};

use crate::host_client::UnixStream;

/// 拉起采集进程后最多等它这么久开好监听：刚换上的程序第一次跑时系统要先校验签名、扫描一遍。
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// 等采集进程开好监听时隔多久试连一次。
const RETRY: Duration = Duration::from_millis(20);

/// 连上远程桌面的采集进程，没有在跑的就拉起一个。屏幕上没人登录时报 `NotConnected`：Windows 上经 ssh 跑在没有屏幕
/// 的会话里、屏幕上也没人登录，Linux 上屏幕没有本用户的图形会话。
///
/// shortcut: 连上的正好是因空闲正要退出的采集进程时这条连接随即断开，由观看端重连；常见了再在采集进程
/// 里加握手确认
pub fn connect() -> io::Result<UnixStream> {
    if !runode_screen::desktop_available() {
        return Err(runode_screen::caused_error(
            StopCause::NoDesktop,
            io::ErrorKind::NotConnected,
            "nobody is signed in to a desktop on this computer's screen",
        ));
    }
    let socket = runode_paths::Dirs::from_env()
        .screen_socket_file()
        .ok_or_else(|| io::Error::other("no place for the screen socket"))?;
    // 从拉起采集进程时才开始算期限：之前把它复制到固定路径可能要好几秒。
    let mut deadline: Option<Instant> = None;
    // 拉起采集进程时交回的，拿到连上它或者放弃（`launch`）。
    let mut _launched = None;
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if deadline.is_none() => {
                tracing::info!("starting the screen process: {err}");
                _launched = Some(launch(&screen_exe()?)?);
                deadline = Some(Instant::now() + START_TIMEOUT);
            }
            Err(_) if deadline.is_some_and(|deadline| Instant::now() >= deadline) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the screen process did not start in time"));
            }
            Err(_) => {}
        }
        thread::sleep(RETRY);
    }
}

/// 远程访问的 `Connect`：连不上采集进程时交回一条只说一句 `ScreenHostMsg::Stopped`（带原因）就结束的连接，
/// 手机和配对的电脑看得到为什么开不起来。
pub fn connect_or_explain() -> io::Result<UnixStream> {
    connect().or_else(|err| {
        tracing::warn!("cannot reach the screen process: {err}");
        explain(&err)
    })
}

/// 一条照采集进程的样子回过 `ScreenHello` 就说带着 `err` 的 `ScreenHostMsg::Stopped`、随后结束的连接，对面再写来的
/// 都收下扔掉。
fn explain(err: &io::Error) -> io::Result<UnixStream> {
    let stopped = refusal(err);
    let (ours, theirs) = UnixStream::pair()?;
    thread::Builder::new().name("screen-refusal".into()).spawn(move || {
        if let Err(err) = refuse(&theirs, stopped) {
            tracing::debug!("cannot tell the viewer why there is no remote desktop: {err}");
        }
        let _ = theirs.shutdown(Shutdown::Write);
        // 一直收到对面关掉：对面写进来的帧要有人收，写不进时说的原因也转不出去。
        let _ = io::copy(&mut &theirs, &mut io::sink());
    })?;
    Ok(ours)
}

/// 读对面的 `ScreenHello`，版本不同时回 `Incompatible`，否则回 `Ready` 再回 `stopped`。手机要先收到有屏幕的
/// `Ready` 才读后面的消息，这里列不出屏幕，报一块凑的 1920×1080。
fn refuse(stream: &UnixStream, stopped: ScreenHostMsg) -> io::Result<()> {
    let hello = read_frame(&mut &*stream).map_err(io::Error::other)?;
    let version = hello.and_then(|frame| match frame.message::<ScreenClientMsg>() {
        Ok(ScreenClientMsg::ScreenHello { version }) => Some(version),
        _ => None,
    });
    let replies = if version.is_some_and(|version| version != SCREEN_VERSION) {
        vec![ScreenHostMsg::Incompatible { version: SCREEN_VERSION }]
    } else {
        let display = ScreenDisplay {
            id: 0,
            width: 1920,
            height: 1080,
            pixel_width: 1920,
            pixel_height: 1080,
            modes: Vec::new(),
            changed: false,
        };
        let ready =
            ScreenHostMsg::Ready { displays: vec![display], screen_capture: true, post_event: true, datagram: false };
        vec![ready, stopped]
    };
    for reply in replies {
        let frame = Frame::control(&reply).map_err(io::Error::other)?;
        write_frame(&mut &*stream, FrameKind::Control, 0, &frame.payload).map_err(io::Error::other)?;
    }
    Ok(())
}

/// 连不上采集进程（`connect` 的错误）时告诉观看端的。
pub fn refusal(err: &io::Error) -> ScreenHostMsg {
    let reason = format!("cannot start remote desktop on this computer: {err}");
    ScreenHostMsg::stopped(reason, runode_screen::stop_cause(err))
}

/// 拉起采集进程。
#[cfg(not(windows))]
fn launch(exe: &Path) -> io::Result<()> {
    runode_host::launch_screen(exe).map(drop)
}

/// 拉起采集进程。经 ssh 起在没有屏幕的会话里的采集不到屏幕，起到屏幕上的会话里（`desktop_session`），交回的
/// 任务要拿到连上采集进程以后再放；那里登录的不是同一个用户时任务计划不肯以他的身份起，报错。
#[cfg(windows)]
fn launch(exe: &Path) -> io::Result<Option<crate::desktop_session::Scheduled>> {
    use crate::desktop_session::{Desktop, desktop, schedule};
    match desktop() {
        Desktop::Here => runode_host::launch_screen(exe).map(|_| None),
        Desktop::Mine => schedule(exe, "--screen").map(Some),
        Desktop::Nobody => Err(runode_screen::caused_error(
            StopCause::SignedOut,
            io::ErrorKind::NotConnected,
            "nobody is signed in on this computer's screen",
        )),
        Desktop::Other(user) => Err(runode_screen::caused_error(
            StopCause::OtherUser,
            io::ErrorKind::PermissionDenied,
            format!("{user} is signed in on this computer's screen, not this user"),
        )),
    }
}

/// 拉起采集进程用的可执行文件。在 .app 里跑时就是自己，系统按 app 记授权；不在 .app 里时（经 ssh 装在
/// 按构建分开的目录里的那份）用 `Dirs::screen_exe_file`，按需从自己复制过去：系统按路径记授权，路径一换，
/// 屏幕录制和辅助功能就得重新打开，系统设置里还多出一个同名的程序。复制不成时退回用自己。
fn screen_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if !cfg!(target_os = "macos") || exe.components().any(|part| part.as_os_str().to_string_lossy().ends_with(".app")) {
        return Ok(exe);
    }
    let Some(fixed) = runode_paths::Dirs::from_env().screen_exe_file() else { return Ok(exe) };
    match copy_if_changed(&exe, &fixed) {
        Ok(()) => Ok(fixed),
        Err(err) => {
            tracing::warn!("cannot put the screen process at {}: {err}", fixed.display());
            Ok(exe)
        }
    }
}

/// `from` 比 `to` 新或者大小不同时，把它复制成 `to`：先写临时文件再改名换上，在跑的旧采集进程不受影响。
/// 临时文件每次调用一个：同一个进程里同时有几个观看端连进来时各复制各的，不会把别人写了一半的换上去。
fn copy_if_changed(from: &Path, to: &Path) -> io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let source = std::fs::metadata(from)?;
    if let Ok(target) = std::fs::metadata(to)
        && target.len() == source.len()
        && target.modified()? >= source.modified()?
    {
        return Ok(());
    }
    let dir = to.parent().ok_or_else(|| io::Error::other("no directory for the screen process"))?;
    std::fs::create_dir_all(dir)?;
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let part = dir.join(format!(".runode-{}-{nth}", std::process::id()));
    std::fs::copy(from, &part)?;
    std::fs::rename(&part, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn say_hello(stream: &UnixStream, version: u32) {
        let hello = Frame::control(&ScreenClientMsg::ScreenHello { version }).unwrap();
        write_frame(&mut &*stream, FrameKind::Control, 0, &hello.payload).unwrap();
    }

    fn next(stream: &UnixStream) -> Option<ScreenHostMsg> {
        read_frame(&mut &*stream).unwrap().map(|frame| frame.message().unwrap())
    }

    /// 回过 `Ready` 再说带原因的 `Stopped`，然后结束。
    #[test]
    fn explains_why_the_screen_process_is_out_of_reach() {
        let err = runode_screen::caused_error(StopCause::NoDesktop, io::ErrorKind::NotConnected, "no desktop");
        let stream = explain(&err).unwrap();
        say_hello(&stream, SCREEN_VERSION);
        assert!(matches!(next(&stream), Some(ScreenHostMsg::Ready { displays, .. }) if displays.len() == 1));
        let reason = "cannot start remote desktop on this computer: no desktop".into();
        assert_eq!(next(&stream), Some(ScreenHostMsg::stopped(reason, Some(StopCause::NoDesktop))));
        assert_eq!(next(&stream), None);
    }

    /// 同时复制的几次各用各的临时文件，换上去的都是完整的一份，也不留下临时文件。
    #[test]
    fn concurrent_copies_do_not_clobber_each_other() {
        let dir = std::env::temp_dir().join(format!("rnm-screen-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("runode");
        let content = vec![7u8; 4 << 20];
        std::fs::write(&from, &content).unwrap();
        let to = dir.join("screen/runode");
        let copies: Vec<_> = (0..4)
            .map(|_| {
                let (from, to) = (from.clone(), to.clone());
                std::thread::spawn(move || copy_if_changed(&from, &to))
            })
            .collect();
        for copy in copies {
            copy.join().unwrap().unwrap();
        }
        assert_eq!(std::fs::read(&to).unwrap(), content);
        let left: Vec<_> =
            std::fs::read_dir(to.parent().unwrap()).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from("runode")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn other_versions_hear_incompatible() {
        let stream = explain(&io::Error::other("no socket")).unwrap();
        say_hello(&stream, SCREEN_VERSION + 1);
        assert_eq!(next(&stream), Some(ScreenHostMsg::Incompatible { version: SCREEN_VERSION }));
        assert_eq!(next(&stream), None);
    }
}
