//! 远程桌面的采集进程（`runode --screen`）怎么连上：手机经远程访问来的连接由 `remote_access` 接过来
//! （`connect`）；桌面 app 经 ssh 看这台电脑时，远端跑 `runode --ssh-screen`（`run`），把 ssh 的
//! stdin、stdout 原样接到采集进程的 socket 上，两头说的都是 `runode_protocol::screen`。

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{Frame, FrameKind, screen::ScreenHostMsg, write_frame};

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
        return Err(io::Error::new(
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
        Desktop::Nobody => {
            Err(io::Error::new(io::ErrorKind::NotConnected, "nobody is signed in on this computer's screen"))
        }
        Desktop::Other(user) => Err(io::Error::new(
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
fn copy_if_changed(from: &Path, to: &Path) -> io::Result<()> {
    let source = std::fs::metadata(from)?;
    if let Ok(target) = std::fs::metadata(to)
        && target.len() == source.len()
        && target.modified()? >= source.modified()?
    {
        return Ok(());
    }
    let dir = to.parent().ok_or_else(|| io::Error::other("no directory for the screen process"))?;
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!(".runode-{}", std::process::id()));
    std::fs::copy(from, &part)?;
    std::fs::rename(&part, to).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

/// `runode --ssh-screen`：把 stdin、stdout 接到采集进程上，任一头断开就退出，返回退出码。连不上时往
/// stdout 写一条 `ScreenHostMsg::Stopped` 带上原因再退出，观看端照常显示。
pub fn run() -> i32 {
    // stdout 是协议，出了问题只能看日志。
    crate::host_process::init_logging(runode_paths::Dirs::from_env().ssh_screen_log_file());
    let stream = match connect() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("cannot reach the screen process: {err}");
            let reason = format!("cannot start remote desktop on this computer: {err}");
            if let Ok(frame) = Frame::control(&ScreenHostMsg::Stopped {
                reason,
                signed_out: err.kind() == io::ErrorKind::NotConnected,
                no_encoder: false,
            }) {
                let mut out = io::stdout().lock();
                let _ = write_frame(&mut out, FrameKind::Control, 0, &frame.payload);
                let _ = out.flush();
            }
            return 1;
        }
    };
    crate::host_client::bridge::pump_stdio(stream, "screen-bridge-in", 256 * 1024);
    0
}
