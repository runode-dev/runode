//! `runode --host`：终端宿主单独一个进程跑，由 app 拉起（见 `runode_host::launch`），app 退出后
//! 会话照常跑着。只开宿主的 socket 等前端连上来，没有会话也没有连接、持续 `IDLE_EXIT` 后自己退出，
//! 收到 `Shutdown` 时结束所有会话后退出。`runode --host --stay` 是登录自启的服务（`runode_autostart`）
//! 拉起的宿主，不因空闲退出：服务管理器不会把正常退出的它拉回来，手机和命令行随时要连它。
//! Unix 上收到 SIGTERM、SIGINT 时和收到 `Shutdown` 一样结束所有会话后退出；SIGHUP 不理，从 ssh 里
//! 放到后台跑的宿主断开连接后照常跑着。不碰 GPUI、提前拉起 shell 和通知：主题、剪贴板的规矩
//! 由连上来的桌面告诉它。配置只读远程访问的两项，自己跟着配置文件开关监听（见
//! `remote_access::follow_config`），app 关着时手机也连得上；开着远程访问、又有手机可能连上来
//! （配对过的设备，或者正在配对）时不因空闲退出。
//!
//! `runode --host --take-over`（`take_over`，只在 Unix 上）是升级时新版本的 app 或 `runode --host --upgrade`
//! （`host_client::handoff::upgrade`）拉起的新宿主：先接手 socket 上旧宿主的会话和 socket，再照常跑。交出
//! 会话的旧宿主不马上退出，等它拉起的 shell 都结束了才退出（`wait_for_children`）。systemd 的服务重载时
//! 拉起的新宿主向服务管理器报告自己是新的主进程（`systemd::Notifier`），接手后和服务的宿主一样不因空闲退出。

#[cfg(unix)]
use std::{
    fs::File,
    io::Write as _,
    os::fd::FromRawFd as _,
    sync::{
        Arc, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use std::{fs::OpenOptions, sync::Mutex, time::Duration};

use runode_host::{BuildId, Host};
#[cfg(unix)]
use runode_host::{STATUS_FD, Stopped, TakeOverError, TakeOverOptions};

#[cfg(unix)]
use crate::host_client::{HandoffFailure, HandoffStatus, READY_BY};

/// 没有会话也没有连接，持续这么久就退出。
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// 日志超过这么大时，启动时清空重写。
const LOG_LIMIT: u64 = 8 << 20;

/// 跑宿主，返回进程的退出码。`stay` 时不因空闲退出。
pub fn run(stay: bool) -> i32 {
    // SAFETY: `main` 直接交到这里，还没开别的线程。
    let disclaimed = unsafe { runode_host::disclaim_responsibility() };
    // 服务管理器给主进程的通知 socket 不传给会话里的 shell：服务的 `NotifyAccess=all` 让 cgroup 里谁都能报告，
    // shell 里的程序照它报 `MAINPID=`、`STOPPING=1` 时，服务管理器会连宿主带会话一起结束。
    // SAFETY: 同上，还没开别的线程。
    #[cfg(unix)]
    unsafe {
        std::env::remove_var("NOTIFY_SOCKET");
    }
    let dirs = runode_paths::Dirs::from_env();
    init_logging(dirs.host_log_file());
    warn_if_not_disclaimed(disclaimed);
    let host = prepare(&dirs);
    match crate::host_client::listen(&host, &dirs) {
        Ok(socket) => tracing::info!("host {} listening on {}", std::process::id(), socket.display()),
        Err(err) => {
            // 多半是另一个宿主已经在跑（两个 app 同时拉起时由锁决出一个）。
            tracing::info!("the host does not start: {err:#}");
            return 1;
        }
    }
    serve(&host, if stay { Duration::MAX } else { IDLE_EXIT });
    0
}

/// 跟着配置开关远程访问，跑到空闲 `idle` 或收到 `Shutdown` 为止。
fn serve(host: &Host, idle: Duration) {
    let remote = crate::remote_access::follow_config(host);
    let stopped = host.run_until_idle(idle);
    // 先停远程访问再退出：交接时新宿主在等这个进程放开端口。
    drop(remote);
    #[cfg(unix)]
    if stopped == Stopped::Handoff {
        tracing::info!("host {} handed off; waiting for its shells to exit", std::process::id());
        wait_for_children();
    }
    tracing::info!("host {} exits: {stopped:?}", std::process::id());
}

/// 等这个进程的子进程（交出去的会话里的 shell）都结束。macOS 按责任进程认本地网络权限，shell 的
/// 责任进程是拉起它的宿主：宿主一退出，shell 里的程序连局域网就被拦（No route to host），新宿主
/// 接手了也一样，所以交出会话后还得留着。交接时每个 shell 已经有一个线程在等它（`pty-reaper`），
/// 这里谁先收到都行。
#[cfg(unix)]
fn wait_for_children() {
    loop {
        // SAFETY: 不要退出状态，只等任意一个子进程结束。
        if unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
        {
            // ECHILD：子进程都结束了。
            return;
        }
    }
}

/// `runode --host --take-over`：接手 socket 上旧宿主的会话（见 `Host::take_over`），把结果写成一行
/// `HandoffStatus` 交给拉起它的 app（状态管道，`STATUS_FD`），成了就照 `run` 跑下去；没成时会话
/// 还在旧宿主手里，退出。要回 `HandoffReady` 之前先写一行 `HandoffStatus::ready`，从启动起过了
/// `READY_BY` 就不再回，见 `host_client::handoff` 的模块文档。返回进程的退出码。
#[cfg(unix)]
pub fn take_over() -> i32 {
    // 先于认领状态管道：认领后它带着 close-on-exec，重新执行就丢了。
    // SAFETY: `main` 直接交到这里，还没开别的线程。
    let disclaimed = unsafe { runode_host::disclaim_responsibility() };
    // SAFETY: 同上，还没开别的线程。
    let service = unsafe { systemd::Notifier::take() }.map(Arc::new);
    let ready_by = Instant::now() + READY_BY;
    // 先于打开任何文件：没有状态管道时 `STATUS_FD` 这个号会被日志文件这类占去。
    let mut status = StatusPipe::take();
    let dirs = runode_paths::Dirs::from_env();
    init_logging(dirs.host_log_file());
    warn_if_not_disclaimed(disclaimed);
    if status.0.is_none() {
        tracing::warn!("no status pipe on fd {STATUS_FD}; nobody will hear how the takeover went");
    }
    let host = prepare(&dirs);
    let Some(socket) = dirs.host_socket_file() else {
        tracing::warn!("the socket path is too long, nothing to take over");
        status.report(&HandoffStatus::failed(HandoffFailure::Failed { message: "the socket path is too long".into() }));
        return 1;
    };
    let started = Instant::now();
    tracing::info!("host {} taking over the sessions on {}", std::process::id(), socket.display());
    let status = Arc::new(Mutex::new(status));
    // 报告过自己是服务的主进程：没接手时要把主进程报回旧宿主，不然服务管理器等这个进程退出就当服务停了。
    let claimed = Arc::new(AtomicBool::new(false));
    let on_ready = {
        let (status, service, claimed) = (status.clone(), service.clone(), claimed.clone());
        Arc::new(move || {
            // 先于回 `HandoffReady`：旧宿主收到它就交出会话，之后它退出时服务管理器得已经认这个进程了，不然
            // 服务管理器当服务停了，连这个进程带会话一起结束。报告不了就不接手。
            if let Some(service) = &service {
                claimed.store(true, Ordering::SeqCst);
                service
                    .main_pid(std::process::id())
                    .map_err(|err| format!("cannot tell the service manager about the new main process: {err}"))?;
            }
            status.lock().unwrap_or_else(PoisonError::into_inner).note(&HandoffStatus::ready());
            Ok(())
        })
    };
    let options = TakeOverOptions { ready_by: Some(ready_by), on_ready: Some(on_ready), ..TakeOverOptions::default() };
    let result = host.take_over(&socket, options);
    let mut status = std::mem::replace(&mut *status.lock().unwrap_or_else(PoisonError::into_inner), StatusPipe(None));
    match result {
        Ok(report) => {
            tracing::info!(
                "host {} took over {} sessions in {:?}; {} of them without scrollback: {:?}",
                std::process::id(),
                report.sessions,
                started.elapsed(),
                report.replayed.len(),
                report.replayed,
            );
            // 旧宿主交出会话以后就不再改看板、不再触发定时，这边接着办。先于报告：app 看到报告就连上来
            // 问 `ListTodos`，这时还没打开看板的话回错，看板一直停在错误上。
            if let Some(db) = dirs.todos_db() {
                host.serve_todos(db);
            }
            status.report(&HandoffStatus::took_over(report.sessions, report.replayed));
            // 关掉管道，app 不必等这个进程退出。
            drop(status);
            // 旧宿主退出前还占着远程访问的端口和锁，这边开不了时隔一会儿再试，见 `runode_remote::Service`。
            // 服务里的宿主、装了宿主的登录自启时（旧宿主是自启拉起的 `--stay`）不因空闲退出。
            let stay = service.is_some() || runode_autostart::is_installed(runode_autostart::Kind::Host, &dirs);
            serve(&host, if stay { Duration::MAX } else { IDLE_EXIT });
            0
        }
        Err(err) => {
            tracing::warn!("host {} did not take over after {:?}: {err}", std::process::id(), started.elapsed());
            if let Some(service) = service.filter(|_| claimed.load(Ordering::SeqCst)) {
                service.restore();
            }
            let failure = match err {
                TakeOverError::Refused(reason) => HandoffFailure::Refused { reason },
                TakeOverError::Failed(message) => HandoffFailure::Failed { message },
            };
            status.report(&HandoffStatus::failed(failure));
            1
        }
    }
}

/// 两种跑法共用的准备：放宽描述符上限、换到家目录，建好标成单独跑的宿主，接上信号。
fn prepare(dirs: &runode_paths::Dirs) -> Host {
    #[cfg(unix)]
    if let Err(err) = runode_terminal::pty::raise_fd_limit() {
        tracing::warn!("failed to raise the open file limit: {err}");
    }
    // 不占着拉起它的地方（比如某个仓库里的目录）；shell 各自从自己的目录开始。
    if let Some(home) = &dirs.home
        && let Err(err) = std::env::set_current_dir(home)
    {
        tracing::warn!("failed to change to {}: {err}", home.display());
    }
    let host = Host::new(BuildId(env!("RUNODE_BUILD").into()));
    // 开 socket（或接手 socket）之前就标上：第一个连上来的前端也认得出这是单独跑的宿主。
    host.mark_standalone();
    crate::host_client::expose_cli(&host);
    #[cfg(unix)]
    if let Err(err) = signals::follow(&host) {
        tracing::warn!("failed to handle signals: {err}");
    }
    host
}

/// 拉起这个进程的 app 读结果的管道（`STATUS_FD`）；没有（比如手动跑的）时为空，报告什么都不做。
#[cfg(unix)]
struct StatusPipe(Option<File>);

#[cfg(unix)]
impl StatusPipe {
    /// 认领 `STATUS_FD`：它开着而且是管道时才用，设上 close-on-exec，之后启动的 shell 不会带走它
    /// （带走了 app 就等不到结尾）。
    fn take() -> Self {
        // SAFETY: 输出参数指向本地变量；只查这个号上是什么。
        let pipe = unsafe {
            let mut stat: libc::stat = std::mem::zeroed();
            libc::fstat(STATUS_FD, &mut stat) == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFIFO
        };
        if !pipe {
            return Self(None);
        }
        // SAFETY: 只改这个描述符的标志。
        unsafe { libc::fcntl(STATUS_FD, libc::F_SETFD, libc::FD_CLOEXEC) };
        // SAFETY: 这个号开着，是拉起时接上的管道写端，进程里别处不用它；交给 `File` 后由它关。
        Self(Some(unsafe { File::from_raw_fd(STATUS_FD) }))
    }

    /// 写一行结果后关掉；写过一次后再调什么都不做。写不了时记日志。
    fn report(&mut self, status: &HandoffStatus) {
        self.note(status);
        self.0 = None;
    }

    /// 写一行 JSON，管道接着开着。写不了时记日志。
    fn note(&mut self, status: &HandoffStatus) {
        let Some(file) = &mut self.0 else { return };
        let written = serde_json::to_vec(status).map_err(std::io::Error::other).and_then(|mut line| {
            line.push(b'\n');
            file.write_all(&line)
        });
        if let Err(err) = written {
            tracing::warn!("failed to report the takeover to the app: {err}");
        }
    }
}

/// `runode_host::disclaim_responsibility` 没成时记一笔：这个宿主开的终端可能连不上局域网。
fn warn_if_not_disclaimed(result: std::io::Result<()>) {
    if let Err(err) = result {
        tracing::warn!("the host stays under its launcher's responsible process: {err}");
    }
}

/// 日志追加写到 `path`，超过 `LOG_LIMIT` 时清空重写；写不了时不记日志。
pub(crate) fn init_logging(path: Option<std::path::PathBuf>) {
    let Some(path) = path else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT);
    let file = OpenOptions::new().create(true).append(!too_big).write(true).truncate(too_big).open(&path);
    let Ok(file) = file else { return };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
}

/// 宿主进程的信号：SIGTERM、SIGINT 让宿主结束所有会话后退出（`Host::shut_down`），SIGHUP 不理。都装成
/// 处理函数而不是忽略：exec 时处理函数复位成默认，宿主拉起的 shell 照常收到这些信号；忽略会被 shell 继承。
/// 在 systemd 的服务里接手时向服务管理器报告主进程（`sd_notify` 的 `MAINPID=`）。
#[cfg(unix)]
mod systemd {
    use std::{ffi::OsString, io, os::unix::net::UnixDatagram};

    /// 服务管理器的通知 socket，和交接前服务的主进程（旧宿主）。
    pub(super) struct Notifier {
        socket: OsString,
        old_main: Option<u32>,
    }

    impl Notifier {
        /// 从环境里取走 `NOTIFY_SOCKET` 和 `MAINPID`（服务管理器给重载命令设的，经它传到这里），之后起的
        /// shell 不带着它们。不在服务里时为 `None`。
        ///
        /// # Safety
        ///
        /// 要改环境变量：调用时进程里不能有别的线程。
        pub(super) unsafe fn take() -> Option<Self> {
            let socket = std::env::var_os("NOTIFY_SOCKET").filter(|socket| !socket.is_empty());
            let old_main = std::env::var("MAINPID").ok().and_then(|pid| pid.parse().ok());
            // SAFETY: 调用方保证还没有别的线程。
            unsafe {
                std::env::remove_var("NOTIFY_SOCKET");
                std::env::remove_var("MAINPID");
            }
            Some(Self { socket: socket?, old_main })
        }

        /// 报告服务的主进程换成了 `pid`，等服务管理器处理完这条报告才返回（`barrier`）。
        pub(super) fn main_pid(&self, pid: u32) -> io::Result<()> {
            let socket = self.connect()?;
            socket.send(format!("MAINPID={pid}").as_bytes())?;
            barrier(&socket)
        }

        /// 没接手：把主进程报回旧宿主。
        pub(super) fn restore(&self) {
            let Some(pid) = self.old_main else {
                tracing::warn!("the service manager now waits on this host, which is about to exit");
                return;
            };
            if let Err(err) = self.main_pid(pid) {
                tracing::warn!("cannot give the main process back to the old host: {err}");
            }
        }

        fn connect(&self) -> io::Result<UnixDatagram> {
            let socket = UnixDatagram::unbound()?;
            // `@` 开头的是 Linux 的抽象地址。
            #[cfg(target_os = "linux")]
            if let Some(name) = self.socket.as_encoded_bytes().strip_prefix(b"@") {
                use std::os::linux::net::SocketAddrExt as _;
                socket.connect_addr(&std::os::unix::net::SocketAddr::from_abstract_name(name)?)?;
                return Ok(socket);
            }
            socket.connect(&self.socket)?;
            Ok(socket)
        }
    }

    /// 服务管理器处理完这之前发的报告时才返回，最多等 `BARRIER_TIMEOUT`（sd_notify 的 `BARRIER=1`：随报告交过去
    /// 一个管道的写端，它处理到这条时关掉，这边读端就读到了结尾）。
    #[cfg(target_os = "linux")]
    fn barrier(socket: &UnixDatagram) -> io::Result<()> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

        let mut fds = [0; 2];
        // SAFETY: 只往 `fds` 里写两个描述符。
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 两个描述符是刚建的，归这里所有。
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        send_with_fd(socket, b"BARRIER=1", write.as_raw_fd())?;
        drop(write);
        let mut poll = libc::pollfd { fd: read.as_raw_fd(), events: 0, revents: 0 };
        let timeout = i32::try_from(BARRIER_TIMEOUT.as_millis()).unwrap_or(i32::MAX);
        loop {
            // SAFETY: `poll` 指着一个有效的 pollfd；对端关掉时报 POLLHUP，不用登记事件。
            match unsafe { libc::poll(&raw mut poll, 1, timeout) } {
                0 => return Err(io::Error::new(io::ErrorKind::TimedOut, "the service manager did not answer")),
                n if n > 0 => return Ok(()),
                _ if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                _ => return Err(io::Error::last_os_error()),
            }
        }
    }

    /// 只有 Linux 上有 systemd，别处的服务管理器不认 `BARRIER=1`。
    #[cfg(not(target_os = "linux"))]
    fn barrier(_: &UnixDatagram) -> io::Result<()> {
        Ok(())
    }

    #[cfg(target_os = "linux")]
    const BARRIER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    /// 发一条带着描述符 `fd` 的报告（SCM_RIGHTS）。
    #[cfg(target_os = "linux")]
    fn send_with_fd(socket: &UnixDatagram, message: &[u8], fd: std::os::fd::RawFd) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;

        let fd_len = size_of::<std::os::fd::RawFd>() as u32;
        // u64 的缓冲区保证 cmsghdr 要的对齐。
        let mut control = [0u64; 4];
        // SAFETY: CMSG_SPACE 只做算术。
        let space = unsafe { libc::CMSG_SPACE(fd_len) };
        debug_assert!(usize::try_from(space).is_ok_and(|space| space <= size_of_val(&control)));
        let mut iov = libc::iovec { iov_base: message.as_ptr().cast_mut().cast(), iov_len: message.len() };
        // SAFETY: 全零是 msghdr 合法的初值。
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = &raw mut iov;
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        // glibc 和 musl 上这个字段的类型不一样。
        header.msg_controllen = space.try_into().map_err(io::Error::other)?;
        // SAFETY: `header` 的控制缓冲区够放一个带描述符的 cmsghdr，下面只写在它里面。
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&raw const header);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(fd_len) as _;
            std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<std::os::fd::RawFd>(), fd);
        }
        // SAFETY: `header` 指着的缓冲区在调用期间都活着。
        if unsafe { libc::sendmsg(socket.as_raw_fd(), &raw const header, libc::MSG_NOSIGNAL) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(all(test, target_os = "linux"))]
    mod tests {
        use super::*;

        #[test]
        fn main_pid_waits_for_the_service_manager() {
            let dir = std::env::temp_dir().join(format!("runode-notify-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("notify");
            let manager = UnixDatagram::bind(&path).unwrap();
            let notifier = Notifier { socket: path.into_os_string(), old_main: None };
            let reader = std::thread::spawn(move || {
                let mut buf = [0; 64];
                let n = manager.recv(&mut buf).unwrap();
                let first = String::from_utf8_lossy(&buf[..n]).into_owned();
                // 不收控制信息时内核把随报告来的描述符关掉，和服务管理器处理完关掉一样。
                let n = manager.recv(&mut buf).unwrap();
                (first, String::from_utf8_lossy(&buf[..n]).into_owned())
            });
            notifier.main_pid(42).unwrap();
            assert_eq!(reader.join().unwrap(), ("MAINPID=42".to_owned(), "BARRIER=1".to_owned()));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(unix)]
mod signals {
    use std::{
        io,
        os::fd::RawFd,
        sync::atomic::{AtomicI32, Ordering},
    };

    use runode_host::Host;

    /// 自管道的写端，处理函数往里写一个字节；没装上时是 -1。
    static PIPE: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn on_stop(_: libc::c_int) {
        let fd = PIPE.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write 是异步信号安全的；管道满了（已经有字节没读）时写不进也无妨。
            unsafe { libc::write(fd, [0u8].as_ptr().cast(), 1) };
        }
    }

    extern "C" fn on_hangup(_: libc::c_int) {}

    /// 装上处理函数，另起一个线程等自管道，读到了就 `Host::shut_down`。
    pub(super) fn follow(host: &Host) -> io::Result<()> {
        let mut fds: [RawFd; 2] = [-1; 2];
        // SAFETY: `fds` 是两个元素的数组。
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let [read, write] = fds;
        for fd in fds {
            // SAFETY: 只改刚建的描述符的标志：不让 shell 带走，写端不阻塞。
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        // SAFETY: 同上。
        unsafe { libc::fcntl(write, libc::F_SETFL, libc::fcntl(write, libc::F_GETFL) | libc::O_NONBLOCK) };
        PIPE.store(write, Ordering::Relaxed);
        install(libc::SIGTERM, on_stop)?;
        // 拉起时就忽略 SIGINT（没有作业控制的脚本里 `runode --host &`）的照旧忽略，不然那个终端里的
        // Ctrl+C 会结束全部会话；shell 由 PTY 拉起时信号复位成默认，不受这里影响。
        if !ignored(libc::SIGINT)? {
            install(libc::SIGINT, on_stop)?;
        }
        install(libc::SIGHUP, on_hangup)?;
        let host = host.clone();
        std::thread::Builder::new().name("host-signals".into()).spawn(move || {
            let mut byte = 0u8;
            loop {
                // SAFETY: 读一个字节进本地变量。
                let n = unsafe { libc::read(read, (&raw mut byte).cast(), 1) };
                if n == 1 {
                    break;
                }
                if n == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    return;
                }
            }
            tracing::info!("host {} got a signal to stop; ending all sessions", std::process::id());
            host.shut_down();
        })?;
        Ok(())
    }

    fn ignored(signal: libc::c_int) -> io::Result<bool> {
        // SAFETY: 只读出当前的处理方式，结构体先清零。
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut action) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(action.sa_sigaction == libc::SIG_IGN)
        }
    }

    fn install(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) -> io::Result<()> {
        // SAFETY: 处理函数只做异步信号安全的事；结构体先清零再填。
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
