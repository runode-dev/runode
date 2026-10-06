//! 宿主怎么跑：看 socket 上有没有单独一个进程的宿主（`runode --host`）在跑、它有没有会话，按配置项
//! `terminal-host` 决定跑在 app 里、连上它，还是把它拉起来（`choose_mode`）。

use std::{
    io,
    os::unix::net::UnixStream,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    BuildId, Caps, ClientKind, ClientMsg, Frame, FrameKind, HostMsg, PROTOCOL_VERSION, read_frame, write_frame,
};

use super::link::{ConnectError, Link};

/// 试连之间隔这么久。
const RETRY_INTERVAL: Duration = Duration::from_millis(5);
/// 拉起宿主后最多等这么久连上它；探一探在跑的宿主也最多这么久。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// 拉起后这么久还连不上（比如它没抢到锁就退出了），再拉一次；多拉起的那个抢不到锁，自己退出。
const RELAUNCH_AFTER: Duration = Duration::from_millis(500);

/// socket 上在跑的宿主的样子，见 `probe`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// 没有宿主在跑。
    Absent,
    /// 有，但协议版本对不上，多半是旧版本的宿主还活着。
    Incompatible(String),
    /// 有，但跑在另一个 app 的进程里（`Welcome::standalone` 为假）：那个 app 是它的界面，它的会话
    /// 有没有被认领、有没有会话都说明不了什么（那个 app 可能刚提前拉起 shell 还没连上，或者还没
    /// 开会话）。
    OtherApp,
    /// 有，`sessions` 个会话，其中 `claimed` 个有桌面的界面连着。
    Running { sessions: usize, claimed: usize },
}

/// 这次启动宿主怎么跑，见 `choose_mode`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// 跑在 app 里；`listen` 时开 socket 让命令行连上来（另一个宿主拿着锁时开不了，不开）。
    InProcess { listen: bool },
    /// 连上在跑的那个；`end_on_quit` 时这次退出时让它连会话一起退出。
    Keep { end_on_quit: bool },
    /// 让在跑的那个（没有会话）退出，再跑在 app 里。
    Retire,
    /// 拉起单独一个进程的宿主再连上；已经有在跑的就直接连。
    Launch,
}

/// 按配置项 `terminal-host` 和 socket 上探到的宿主决定这次怎么跑：
///
/// - 开着：连上在跑的宿主，没有就拉起一个。
/// - 关着：没有在跑的宿主时跑在 app 里。有（上次开着时留下的）而且带着没有界面连着的会话时，
///   这次接着连它、把会话接回来，这次退出时让它连会话一起退出，下次就跑在 app 里；没有会话时
///   让它退出。有会话连着别的桌面时，那是另一个 runode 的宿主，不碰它，跑在 app 里也不开 socket。
/// - 协议对不上：跑在 app 里，不开 socket（锁在它手里）。
/// - socket 上是另一个 app 进程里的宿主：不管开关，都不连进去当它的界面、也不让它退出，跑在
///   app 里，不开 socket（锁在那个 app 手里）。
pub fn choose_mode(terminal_host: bool, probe: &Probe) -> Choice {
    match (terminal_host, probe) {
        (_, Probe::Incompatible(_) | Probe::OtherApp) => Choice::InProcess { listen: false },
        (true, Probe::Running { .. }) => Choice::Keep { end_on_quit: false },
        (true, Probe::Absent) => Choice::Launch,
        (false, Probe::Absent) => Choice::InProcess { listen: true },
        (false, Probe::Running { sessions: 0, .. }) => Choice::Retire,
        (false, Probe::Running { claimed: 0, .. }) => Choice::Keep { end_on_quit: true },
        (false, Probe::Running { .. }) => Choice::InProcess { listen: false },
    }
}

/// 以命令行的身份连 `socket` 看看有没有宿主、有几个会话：不登记成界面，也不算认领了会话。连上
/// 却在 `Welcome` 前断开（它正因空闲退出）时过一会儿再试，最多 `CONNECT_TIMEOUT`。
pub fn probe(socket: &Path, build: &BuildId) -> Probe {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match probe_once(socket, build) {
            Ok(probe) => return probe,
            Err(err) => {
                if Instant::now() >= deadline {
                    tracing::warn!("cannot tell whether a host is running on {}: {err}", socket.display());
                    return Probe::Absent;
                }
                thread::sleep(RETRY_INTERVAL);
            }
        }
    }
}

/// 探一次；连不上 socket 时是 `Absent`，握手或列会话没成时返回错误，由调用方再试。
fn probe_once(socket: &Path, build: &BuildId) -> io::Result<Probe> {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return Ok(Probe::Absent);
    };
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    send(&mut stream, &hello(build, ClientKind::Cli))?;
    match receive(&mut stream)? {
        HostMsg::Welcome { standalone: true, .. } => {}
        HostMsg::Welcome { standalone: false, .. } => return Ok(Probe::OtherApp),
        HostMsg::Incompatible { reason, .. } => return Ok(Probe::Incompatible(reason)),
        other => return Err(io::Error::other(format!("unexpected answer {other:?}"))),
    }
    send(&mut stream, &ClientMsg::ListSessions)?;
    loop {
        if let HostMsg::SessionList { sessions } = receive(&mut stream)? {
            let claimed = sessions.iter().filter(|session| session.claimed).count();
            return Ok(Probe::Running { sessions: sessions.len(), claimed });
        }
    }
}

/// 让 `socket` 上在跑的宿主结束所有会话后退出，等它断开，最多 `CONNECT_TIMEOUT`。宿主跑在另一个
/// app 里时不碰它，返回错误。
pub fn retire(socket: &Path, build: &BuildId) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    send(&mut stream, &hello(build, ClientKind::Cli))?;
    match receive(&mut stream)? {
        HostMsg::Welcome { standalone: true, .. } => {}
        HostMsg::Welcome { standalone: false, .. } => {
            return Err(io::Error::other("the host runs inside another runode app"));
        }
        _ => return Err(io::Error::other("the host did not say welcome")),
    }
    send(&mut stream, &ClientMsg::Shutdown { kill_sessions: true })?;
    // 宿主发完 `Goodbye` 就断开；读到断开为止。
    while read_frame(&mut stream).map_err(|err| io::Error::other(err.to_string()))?.is_some() {}
    Ok(())
}

/// `link` 连上 `socket` 上单独一个进程的宿主（见 `Link::connect_standalone`）。连上却在 `Welcome` 前
/// 断开时过一会儿再试，最多 `CONNECT_TIMEOUT`。
pub fn connect(link: &Link, socket: &Path) -> Result<(), ConnectError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        let result =
            UnixStream::connect(socket).map_err(ConnectError::Io).and_then(|stream| link.connect_standalone(stream));
        match result {
            Err(ConnectError::Closed) if Instant::now() < deadline => thread::sleep(RETRY_INTERVAL),
            result => return result,
        }
    }
}

/// `link` 连上 `socket` 上单独一个进程的宿主，没有就用 `exe --host` 拉起一个：每 `RETRY_INTERVAL`
/// 试连一次，最多 `CONNECT_TIMEOUT`。连上却在 `Welcome` 前断开（撞上它正因空闲退出）时接着试，
/// 它退出后再拉起新的。socket 上是另一个 app 里的宿主时不拉起，返回 `ConnectError::NotStandalone`。
pub fn connect_or_launch(link: &Link, socket: &Path, exe: &Path) -> Result<(), ConnectError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut launched_at: Option<Instant> = None;
    loop {
        let result =
            UnixStream::connect(socket).map_err(ConnectError::Io).and_then(|stream| link.connect_standalone(stream));
        match result {
            Ok(()) => return Ok(()),
            Err(err @ (ConnectError::Incompatible(_) | ConnectError::NotStandalone)) => return Err(err),
            Err(ConnectError::Closed) => {}
            Err(ConnectError::Io(err)) => {
                if launched_at.is_none_or(|at| at.elapsed() >= RELAUNCH_AFTER) {
                    tracing::info!("starting a host process: {err}");
                    runode_host::launch(exe).map_err(ConnectError::Io)?;
                    launched_at = Some(Instant::now());
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(ConnectError::Io(io::Error::new(io::ErrorKind::TimedOut, "the host did not come up in time")));
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

fn hello(build: &BuildId, client: ClientKind) -> ClientMsg {
    ClientMsg::Hello { protocol: PROTOCOL_VERSION, build: build.clone(), client, caps: Caps::default(), session: None }
}

fn send(stream: &mut UnixStream, message: &ClientMsg) -> io::Result<()> {
    let frame = Frame::control(message).map_err(io::Error::other)?;
    write_frame(stream, frame.kind, 0, &frame.payload).map_err(|err| io::Error::other(err.to_string()))
}

/// 下一条控制消息；连接断了时返回错误。
fn receive(stream: &mut UnixStream) -> io::Result<HostMsg> {
    loop {
        let frame = read_frame(stream)
            .map_err(|err| io::Error::other(err.to_string()))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "the host closed the connection"))?;
        if frame.kind == FrameKind::Control {
            return frame.message().map_err(io::Error::other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_host_on_uses_or_starts_a_host_process() {
        assert_eq!(choose_mode(true, &Probe::Absent), Choice::Launch);
        let running = Probe::Running { sessions: 3, claimed: 1 };
        assert_eq!(choose_mode(true, &running), Choice::Keep { end_on_quit: false });
        assert_eq!(choose_mode(true, &Probe::Running { sessions: 0, claimed: 0 }), Choice::Keep { end_on_quit: false });
    }

    #[test]
    fn terminal_host_off_runs_in_the_app() {
        assert_eq!(choose_mode(false, &Probe::Absent), Choice::InProcess { listen: true });
    }

    #[test]
    fn a_leftover_host_with_sessions_is_kept_until_this_quit() {
        let leftover = Probe::Running { sessions: 2, claimed: 0 };
        assert_eq!(choose_mode(false, &leftover), Choice::Keep { end_on_quit: true });
    }

    #[test]
    fn a_leftover_host_without_sessions_is_retired() {
        assert_eq!(choose_mode(false, &Probe::Running { sessions: 0, claimed: 0 }), Choice::Retire);
    }

    #[test]
    fn a_host_another_window_uses_is_left_alone() {
        let busy = Probe::Running { sessions: 2, claimed: 1 };
        assert_eq!(choose_mode(false, &busy), Choice::InProcess { listen: false });
    }

    #[test]
    fn a_host_inside_another_app_is_left_alone() {
        assert_eq!(choose_mode(true, &Probe::OtherApp), Choice::InProcess { listen: false });
        assert_eq!(choose_mode(false, &Probe::OtherApp), Choice::InProcess { listen: false });
    }

    #[test]
    fn an_incompatible_host_falls_back_to_the_app_without_a_socket() {
        let old = Probe::Incompatible("protocol 2".into());
        assert_eq!(choose_mode(true, &old), Choice::InProcess { listen: false });
        assert_eq!(choose_mode(false, &old), Choice::InProcess { listen: false });
    }

    /// 对着真的宿主探：socket 上是另一个 app 进程里的宿主（只 `listen`、没进 `Host::run_until_idle`）。
    mod against_a_host {
        use std::{path::PathBuf, sync::mpsc};

        use runode_host::{Host, Stopped};
        use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

        use super::*;
        use crate::session_host::link::SpawnOptions;

        const BUILD: &str = "launch-test";
        const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
        const WAIT: Duration = Duration::from_secs(10);

        fn build() -> BuildId {
            BuildId(BUILD.into())
        }

        /// 一个宿主开着 `name` 目录里的 socket，返回它和 socket 的路径。
        fn listening(name: &str) -> (Host, PathBuf) {
            let dir = std::env::temp_dir().join(format!("rnm-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let host = Host::new(build());
            let socket = dir.join("host.sock");
            host.listen(&socket, &dir.join("host.lock")).unwrap();
            (host, socket)
        }

        /// 另一个 app 刚提前拉起了 shell（`start: false`，还没有视图连上它，没被认领）：探到的是
        /// 别的 app 的宿主，开关开着关着都不连进去，也不让它退出，它的会话还在。
        #[test]
        fn another_app_that_just_prespawned() {
            let (host, socket) = listening("prespawn");
            let other_app = Link::new(build());
            other_app.connect(host.connect_pair().unwrap()).unwrap();
            let id = other_app
                .spawn(SpawnOptions {
                    size: SIZE,
                    cwd: None,
                    integration: IntegrationMode::Off,
                    start: false,
                    shell: Some("/bin/cat".into()),
                    settings: None,
                    env: Vec::new(),
                })
                .unwrap();
            let probed = probe(&socket, &build());
            assert_eq!(probed, Probe::OtherApp);
            assert_eq!(choose_mode(false, &probed), Choice::InProcess { listen: false });
            assert_eq!(choose_mode(true, &probed), Choice::InProcess { listen: false });
            assert!(retire(&socket, &build()).is_err());
            assert!(matches!(connect(&Link::new(build()), &socket), Err(ConnectError::NotStandalone)));
            let sessions = other_app.list_sessions(WAIT).unwrap();
            assert_eq!(sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [id]);
            other_app.kill(id);
        }

        /// 另一个 app 还没开会话：同样不碰它。
        #[test]
        fn another_app_without_sessions() {
            let (host, socket) = listening("empty");
            let other_app = Link::new(build());
            other_app.connect(host.connect_pair().unwrap()).unwrap();
            let probed = probe(&socket, &build());
            assert_eq!(probed, Probe::OtherApp);
            assert_eq!(choose_mode(false, &probed), Choice::InProcess { listen: false });
            assert_eq!(choose_mode(true, &probed), Choice::InProcess { listen: false });
            assert!(retire(&socket, &build()).is_err());
            let exe = PathBuf::from("/nonexistent/runode");
            assert!(matches!(connect_or_launch(&Link::new(build()), &socket, &exe), Err(ConnectError::NotStandalone)));
            assert!(other_app.list_sessions(WAIT).is_ok(), "the other app's host is still there");
        }

        /// 单独一个进程的宿主没有会话：探得出来，开关关着时让它退出。
        #[test]
        fn a_standalone_host_without_sessions_is_retired() {
            let (host, socket) = listening("retire");
            let stopped = {
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || tx.send(host.run_until_idle(Duration::from_secs(60))));
                rx
            };
            // `run_until_idle` 起来之前宿主还说自己跑在 app 里。
            let deadline = Instant::now() + WAIT;
            let probed = loop {
                match probe(&socket, &build()) {
                    Probe::OtherApp if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                    probed => break probed,
                }
            };
            assert_eq!(probed, Probe::Running { sessions: 0, claimed: 0 });
            assert_eq!(choose_mode(false, &probed), Choice::Retire);
            retire(&socket, &build()).unwrap();
            assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Shutdown));
        }
    }
}
