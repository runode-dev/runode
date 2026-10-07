//! 宿主怎么跑：看 socket 上有没有单独一个进程的宿主（`runode --host`）在跑、它有没有会话、是不是
//! 这个构建，按配置项 `terminal-host` 决定跑在 app 里、连上它、把它拉起来，还是让新宿主接手它的
//! 会话（`choose_mode`）。

use std::{
    io,
    os::{fd::AsRawFd as _, unix::net::UnixStream},
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
/// 从这个协议版本起，宿主会把会话交给新版本的宿主（`ClientKind::Successor`）；更旧的不会。
pub const HANDOFF_PROTOCOL: u32 = 4;

/// socket 上在跑的宿主的样子，见 `probe`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// 没有宿主在跑。
    Absent,
    /// 有，但协议版本对不上：`protocol` 是它说的版本，比 `HANDOFF_PROTOCOL` 旧的不会交接。
    Incompatible { protocol: u32, reason: String },
    /// 有，但跑在另一个 app 的进程里（`Welcome::standalone` 为假）：那个 app 是它的界面，它的会话
    /// 有没有被认领、有没有会话都说明不了什么（那个 app 可能刚提前拉起 shell 还没连上，或者还没
    /// 开会话）。
    OtherApp,
    /// 有，`sessions` 个会话，其中 `claimed` 个有桌面的界面连着；`build` 是它的构建。
    Running { sessions: usize, claimed: usize, build: BuildId },
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
    /// 在跑的是别的构建：拉起这个构建的新宿主接手它的会话再连上，`end_on_quit` 同 `Keep`。
    HandOver { end_on_quit: bool },
    /// 在跑的宿主太老，不会交接：这次跑在 app 里、不开 socket，问用户留着它还是结束它。
    PreHandoff,
}

/// 按配置项 `terminal-host`、socket 上探到的宿主和这个构建（`build`）决定这次怎么跑：
///
/// - 开着：连上在跑的宿主，没有就拉起一个。在跑的是别的构建时让新宿主接手（`HandOver`），
///   它没有会话时也一样（交接不成再让它退出、拉起新的，见 `host_client::establish`）。
/// - 关着：没有在跑的宿主时跑在 app 里。有（上次开着时留下的）而且带着没有界面连着的会话时，
///   这次接着连它、把会话接回来（别的构建时先让新宿主接手），这次退出时让它连会话一起退出，
///   下次就跑在 app 里；没有会话时让它退出。有会话连着别的桌面时，那是另一个 runode 的宿主，
///   不碰它，跑在 app 里也不开 socket。
/// - 协议对不上：比 `HANDOFF_PROTOCOL` 旧的不会交接，`PreHandoff`；新的会，按开关走 `HandOver`
///   （开关关着时当作留下的会话没有界面连着，交接后这次退出时结束）。
/// - socket 上是另一个 app 进程里的宿主：不管开关，都不连进去当它的界面、也不让它退出，跑在
///   app 里，不开 socket（锁在那个 app 手里）。
pub fn choose_mode(terminal_host: bool, probe: &Probe, build: &BuildId) -> Choice {
    match (terminal_host, probe) {
        (_, Probe::OtherApp) => Choice::InProcess { listen: false },
        (_, Probe::Incompatible { protocol, .. }) if *protocol < HANDOFF_PROTOCOL => Choice::PreHandoff,
        (on, Probe::Incompatible { .. }) => Choice::HandOver { end_on_quit: !on },
        (true, Probe::Running { build: theirs, .. }) if theirs != build => Choice::HandOver { end_on_quit: false },
        (true, Probe::Running { .. }) => Choice::Keep { end_on_quit: false },
        (true, Probe::Absent) => Choice::Launch,
        (false, Probe::Absent) => Choice::InProcess { listen: true },
        (false, Probe::Running { sessions: 0, .. }) => Choice::Retire,
        (false, Probe::Running { claimed: 0, build: theirs, .. }) if theirs != build => {
            Choice::HandOver { end_on_quit: true }
        }
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
    let Ok(mut stream) = connect_cli(socket) else {
        return Ok(Probe::Absent);
    };
    send(&mut stream, &hello(build, ClientKind::Cli))?;
    let build = match receive(&mut stream)? {
        HostMsg::Welcome { standalone: true, build, .. } => build,
        HostMsg::Welcome { standalone: false, .. } => return Ok(Probe::OtherApp),
        HostMsg::Incompatible { protocol, reason, .. } => return Ok(Probe::Incompatible { protocol, reason }),
        other => return Err(io::Error::other(format!("unexpected answer {other:?}"))),
    };
    send(&mut stream, &ClientMsg::ListSessions)?;
    loop {
        if let HostMsg::SessionList { sessions } = receive(&mut stream)? {
            let claimed = sessions.iter().filter(|session| session.claimed).count();
            return Ok(Probe::Running { sessions: sessions.len(), claimed, build });
        }
    }
}

/// 让 `socket` 上在跑的宿主结束所有会话后退出，等它断开，最多 `CONNECT_TIMEOUT`。宿主跑在另一个
/// app 里时不碰它，返回错误。
pub fn retire(socket: &Path, build: &BuildId) -> io::Result<()> {
    let mut stream = connect_cli(socket)?;
    send(&mut stream, &hello(build, ClientKind::Cli))?;
    match receive(&mut stream)? {
        HostMsg::Welcome { standalone: true, .. } => {}
        HostMsg::Welcome { standalone: false, .. } => {
            return Err(io::Error::other("the host runs inside another runode app"));
        }
        _ => return Err(io::Error::other("the host did not say welcome")),
    }
    shut_down(stream)
}

/// 连上 `socket` 上的宿主，读写都最多等 `CONNECT_TIMEOUT`；招呼由调用方打。
fn connect_cli(socket: &Path) -> io::Result<UnixStream> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
    Ok(stream)
}

/// 已经打过招呼的连接上让宿主结束所有会话后退出，读到它断开为止。
fn shut_down(mut stream: UnixStream) -> io::Result<()> {
    send(&mut stream, &ClientMsg::Shutdown { kill_sessions: true })?;
    // 宿主发完 `Goodbye` 就断开；读到断开为止。
    while read_frame(&mut stream).map_err(|err| io::Error::other(err.to_string()))?.is_some() {}
    Ok(())
}

/// `end_old_host`、`terminate` 做成了什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// 旧宿主连同会话结束了。
    Ended,
    /// socket 对面的进程不是以 `--host` 启动的单独宿主（多半是协议 3 及以前的旧 app，宿主跑在
    /// 它的进程里）：没发信号，免得结束一整个 app。
    NotAHost,
}

/// 用户要结束交接没成的旧宿主（连同它的会话）：说得通协议的同 `retire`；协议对不上的（`Probe`
/// 是 `Incompatible`）没法让它自己退出，同 `terminate`。跑在另一个 app 里的不碰，返回错误。
pub fn end_old_host(socket: &Path, build: &BuildId) -> io::Result<Ended> {
    let mut stream = connect_cli(socket)?;
    send(&mut stream, &hello(build, ClientKind::Cli))?;
    match receive(&mut stream)? {
        HostMsg::Welcome { standalone: true, .. } => {}
        HostMsg::Welcome { standalone: false, .. } => {
            return Err(io::Error::other("the host runs inside another runode app"));
        }
        HostMsg::Incompatible { .. } => {
            drop(stream);
            return terminate(socket);
        }
        _ => return Err(io::Error::other("the host did not say welcome")),
    }
    shut_down(stream)?;
    Ok(Ended::Ended)
}

/// 给 `socket` 上的宿主进程发 SIGTERM（连同它的会话一起结束），不管它说什么协议：pid 见
/// `host_pid`，只认同一个用户的进程，而且只认以 `--host` 启动的单独宿主进程（见
/// `launched_as_host`）；别的（旧 app 进程里的宿主）不发，返回 `Ended::NotAHost`。发了之后等到
/// socket 连不上为止，最多 `CONNECT_TIMEOUT`。
pub fn terminate(socket: &Path) -> io::Result<Ended> {
    let pid = host_pid(socket)?;
    if pid <= 0 || pid.unsigned_abs() == std::process::id() {
        return Err(io::Error::other(format!("refusing to end process {pid}")));
    }
    let args = process_args(pid)?;
    if !launched_as_host(&args) {
        tracing::warn!("process {pid} on the host socket is not a standalone host, leaving it alone");
        return Ok(Ended::NotAHost);
    }
    // SAFETY: 只发信号，参数都是值。
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(io::Error::last_os_error());
    }
    tracing::info!("sent SIGTERM to the old host {pid}");
    // 它退出后监听的 socket 跟着关掉，连不上了。
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while UnixStream::connect(socket).is_ok() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("the old host {pid} did not exit")));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(Ended::Ended)
}

/// 启动参数（不含程序名）里有没有 `--host`：有的是单独跑的宿主进程（`runode --host`，以及
/// `--host --take-over`），没有的是 app 本身。
fn launched_as_host(args: &[Vec<u8>]) -> bool {
    args.iter().skip(1).any(|arg| arg == b"--host")
}

/// 进程 `pid` 的启动参数（含程序名），用 `sysctl(KERN_PROCARGS2)` 读，见 `parse_procargs`。
fn process_args(pid: libc::pid_t) -> io::Result<Vec<Vec<u8>>> {
    let mut max: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: mib 和输出参数都指向本地变量，长度是它们的大小；不写入新值。
    let result =
        unsafe { libc::sysctl(mib.as_mut_ptr(), 2, (&raw mut max).cast(), &mut size, std::ptr::null_mut(), 0) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u8; usize::try_from(max).unwrap_or(0).max(4)];
    let mut size = buffer.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: 输出缓冲区长 `size` 字节，内核最多写这么多并把实际长度写回 `size`；不写入新值。
    let result =
        unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buffer.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(size);
    parse_procargs(&buffer).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, format!("cannot read the arguments of process {pid}"))
    })
}

/// 解 `KERN_PROCARGS2` 的结果：本机字节序的 `int argc`，可执行文件的路径（以 NUL 结尾，后面可能
/// 补几个 NUL 对齐），接着 `argc` 个以 NUL 结尾的参数（第一个是程序名），再往后是环境变量，不要。
/// 格式不对时为 `None`。
fn parse_procargs(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    let argc = usize::try_from(i32::from_ne_bytes(data.get(..4)?.try_into().ok()?)).ok()?;
    let rest = &data[4..];
    let path_end = rest.iter().position(|&byte| byte == 0)?;
    let mut rest = &rest[path_end..];
    let start = rest.iter().position(|&byte| byte != 0)?;
    rest = &rest[start..];
    let mut args = Vec::with_capacity(argc.min(64));
    for _ in 0..argc {
        let end = rest.iter().position(|&byte| byte == 0)?;
        args.push(rest[..end].to_vec());
        rest = &rest[end + 1..];
    }
    Some(args)
}

/// `socket` 上宿主进程的 pid：打个招呼，等它回话（`Welcome`，协议对不上的老宿主回
/// `Incompatible`），回过话再读连接对端（`peer_pid`）。监听的 socket 交接过以后，对方 accept
/// 之前读到的是最早建它的那个进程（可能早已退出、号被别的进程占了），回过话就一定 accept 了。
/// `Welcome` 里有宿主自报的 `host_pid` 时以它为准，和对端对不上时记一笔；协议 3 及以前的
/// `Incompatible` 不带 pid，用对端的。不回话的不猜，返回错误。
fn host_pid(socket: &Path) -> io::Result<libc::pid_t> {
    let mut stream = connect_cli(socket)?;
    send(&mut stream, &hello(&super::build(), ClientKind::Cli))?;
    let reported = match receive(&mut stream)? {
        HostMsg::Welcome { host_pid, .. } => libc::pid_t::try_from(host_pid).ok().filter(|pid| *pid > 0),
        HostMsg::Incompatible { .. } => None,
        other => return Err(io::Error::other(format!("unexpected answer {other:?}"))),
    };
    let peer = peer_pid(&stream)?;
    match reported {
        Some(reported) if reported != peer => {
            tracing::warn!("the host says it is process {reported}, the connection says {peer}; going with {reported}");
            Ok(reported)
        }
        Some(reported) => Ok(reported),
        None => Ok(peer),
    }
}

/// 连接对端进程的 pid，只认同一个用户的。要在对方回过话之后读，见 `host_pid`。
fn peer_pid(stream: &UnixStream) -> io::Result<libc::pid_t> {
    let fd = stream.as_raw_fd();
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: 描述符来自 `stream`；两个输出参数指向本地变量。
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: 没有参数，不会失败。
    if uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the host belongs to another user"));
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: 描述符来自 `stream`；值指向本地变量，长度是它的大小。
    let result = unsafe { libc::getsockopt(fd, libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
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
    ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: build.clone(),
        client,
        caps: Caps::default(),
        session: None,
        device: None,
    }
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

    const OURS: &str = "this-build";

    fn running(sessions: usize, claimed: usize, build: &str) -> Probe {
        Probe::Running { sessions, claimed, build: BuildId(build.into()) }
    }

    fn incompatible(protocol: u32) -> Probe {
        Probe::Incompatible { protocol, reason: format!("protocol {protocol}") }
    }

    /// `KERN_PROCARGS2` 的布局：argc、可执行文件路径、补齐的 NUL、参数、环境变量。
    fn procargs(path: &str, padding: usize, args: &[&str], env: &[&str]) -> Vec<u8> {
        let mut data = i32::try_from(args.len()).unwrap().to_ne_bytes().to_vec();
        data.extend_from_slice(path.as_bytes());
        data.extend(std::iter::repeat_n(0, 1 + padding));
        for item in args.iter().chain(env) {
            data.extend_from_slice(item.as_bytes());
            data.push(0);
        }
        data
    }

    fn owned(args: &[&str]) -> Vec<Vec<u8>> {
        args.iter().map(|arg| arg.as_bytes().to_vec()).collect()
    }

    #[test]
    fn process_arguments_are_read_from_the_kernel_layout() {
        let exe = "/Applications/Runode.app/Contents/MacOS/runode";
        let data = procargs(exe, 3, &[exe, "--host"], &["HOME=/Users/me", "TERM=xterm"]);
        assert_eq!(parse_procargs(&data), Some(owned(&[exe, "--host"])));
        // 没有补齐的 NUL，只有程序名。
        assert_eq!(parse_procargs(&procargs(exe, 0, &[exe], &[])), Some(owned(&[exe])));
        // 环境变量不要，没有也行。
        let env = "HOME=/Users/me\0TERM=xterm\0".len();
        assert_eq!(parse_procargs(&data[..data.len() - env]), Some(owned(&[exe, "--host"])));
        // 参数被截断、argc 比实际多、负数、太短：读不出来。
        assert_eq!(parse_procargs(&data[..data.len() - env - 3]), None);
        assert_eq!(parse_procargs(&procargs(exe, 1, &[exe, "--host"], &[])[..60]), None);
        let mut lying = procargs(exe, 1, &[exe], &[]);
        lying[..4].copy_from_slice(&5i32.to_ne_bytes());
        assert_eq!(parse_procargs(&lying), None);
        let mut negative = procargs(exe, 1, &[exe], &[]);
        negative[..4].copy_from_slice(&(-1i32).to_ne_bytes());
        assert_eq!(parse_procargs(&negative), None);
        assert_eq!(parse_procargs(&[1, 0]), None);
    }

    /// 只有以 `--host` 启动的才算单独的宿主；程序名本身不算，app 带的别的参数也不算。
    #[test]
    fn only_processes_started_with_host_are_hosts() {
        assert!(launched_as_host(&owned(&["/x/runode", "--host"])));
        assert!(launched_as_host(&owned(&["/x/runode", "--host", "--take-over"])));
        assert!(!launched_as_host(&owned(&["/x/runode"])));
        assert!(!launched_as_host(&owned(&["/x/runode", "-psn_0_12345"])));
        assert!(!launched_as_host(&owned(&["--host"])));
        assert!(!launched_as_host(&owned(&["/x/runode", "list", "--hosts"])));
        assert!(!launched_as_host(&[]));
    }

    /// 真读一个进程：读这个测试进程自己，和它看到的参数一样，也不是宿主。
    #[test]
    fn this_process_reads_its_own_arguments() {
        let pid = libc::pid_t::try_from(std::process::id()).unwrap();
        let args = process_args(pid).unwrap();
        let expected: Vec<Vec<u8>> = std::env::args_os().map(|arg| arg.into_encoded_bytes()).collect();
        assert_eq!(args, expected);
        assert!(!launched_as_host(&args));
    }

    /// 读另一个进程：启动参数里带 `--host` 的认得出来。
    #[test]
    fn another_process_started_with_host_is_recognised() {
        // 后面的 `:` 让 sh 不直接 exec 成 sleep，参数留在 sh 身上。
        let mut child = std::process::Command::new("/bin/sh").args(["-c", "sleep 5; :", "--host"]).spawn().unwrap();
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        let args = process_args(pid);
        let _ = child.kill();
        let _ = child.wait();
        let args = args.unwrap();
        assert_eq!(args, owned(&["/bin/sh", "-c", "sleep 5; :", "--host"]));
        assert!(launched_as_host(&args));
    }

    /// 决策表：开关 × 探到的宿主 → 怎么跑。
    #[test]
    fn the_decision_table() {
        use Choice::*;
        let table = [
            // 开关开着：连上在跑的，没有就拉起；别的构建交接，没有会话也交接。
            (true, Probe::Absent, Launch),
            (true, running(3, 1, OURS), Keep { end_on_quit: false }),
            (true, running(0, 0, OURS), Keep { end_on_quit: false }),
            (true, running(3, 1, "older"), HandOver { end_on_quit: false }),
            (true, running(3, 0, "older"), HandOver { end_on_quit: false }),
            (true, running(0, 0, "older"), HandOver { end_on_quit: false }),
            // 开关关着：没有就跑在 app 里；留下的会话接回来、这次退出时结束；别的构建先交接。
            (false, Probe::Absent, InProcess { listen: true }),
            (false, running(2, 0, OURS), Keep { end_on_quit: true }),
            (false, running(2, 0, "older"), HandOver { end_on_quit: true }),
            (false, running(0, 0, OURS), Retire),
            (false, running(0, 0, "older"), Retire),
            (false, running(2, 1, OURS), InProcess { listen: false }),
            (false, running(2, 1, "older"), InProcess { listen: false }),
            // 另一个 app 里的宿主：不碰。
            (true, Probe::OtherApp, InProcess { listen: false }),
            (false, Probe::OtherApp, InProcess { listen: false }),
            // 协议对不上：太老的不会交接；比自己新的会，按开关交接。
            (true, incompatible(3), PreHandoff),
            (false, incompatible(3), PreHandoff),
            (true, incompatible(2), PreHandoff),
            (true, incompatible(5), HandOver { end_on_quit: false }),
            (false, incompatible(5), HandOver { end_on_quit: true }),
        ];
        let ours = BuildId(OURS.into());
        for (terminal_host, probe, expected) in table {
            assert_eq!(
                choose_mode(terminal_host, &probe, &ours),
                expected,
                "terminal-host = {terminal_host}, {probe:?}"
            );
        }
    }

    /// 对着真的宿主探：socket 上是另一个 app 进程里的宿主（只 `listen`、没进 `Host::run_until_idle`）。
    mod against_a_host {
        use std::{path::PathBuf, sync::mpsc};

        use runode_host::{Host, Stopped};
        use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

        use super::*;
        use crate::host_client::link::SpawnOptions;

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
                })
                .unwrap();
            let probed = probe(&socket, &build());
            assert_eq!(probed, Probe::OtherApp);
            assert_eq!(choose_mode(false, &probed, &build()), Choice::InProcess { listen: false });
            assert_eq!(choose_mode(true, &probed, &build()), Choice::InProcess { listen: false });
            assert!(retire(&socket, &build()).is_err());
            assert!(matches!(connect(&Link::new(build()), &socket), Err(ConnectError::NotStandalone)));
            let sessions = other_app.list_sessions(WAIT).unwrap();
            assert_eq!(sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [id]);
            other_app.kill(id);
        }

        /// 结束旧宿主时只发给别的进程：socket 对端是自己时不发。
        #[test]
        fn terminate_never_signals_this_process() {
            let (_host, socket) = listening("self");
            let err = terminate(&socket).unwrap_err();
            assert!(err.to_string().contains("refusing"), "{err}");
        }

        /// 另一个 app 还没开会话：同样不碰它。
        #[test]
        fn another_app_without_sessions() {
            let (host, socket) = listening("empty");
            let other_app = Link::new(build());
            other_app.connect(host.connect_pair().unwrap()).unwrap();
            let probed = probe(&socket, &build());
            assert_eq!(probed, Probe::OtherApp);
            assert_eq!(choose_mode(false, &probed, &build()), Choice::InProcess { listen: false });
            assert_eq!(choose_mode(true, &probed, &build()), Choice::InProcess { listen: false });
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
            assert_eq!(probed, Probe::Running { sessions: 0, claimed: 0, build: build() });
            assert_eq!(choose_mode(false, &probed, &build()), Choice::Retire);
            retire(&socket, &build()).unwrap();
            assert_eq!(stopped.recv_timeout(WAIT), Ok(Stopped::Shutdown));
        }
    }
}
