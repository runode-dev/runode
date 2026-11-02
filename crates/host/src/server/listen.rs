//! 监听 socket、接受连接：`Host::listen` 开的 socket 上接受别的进程来的前端，`Host::connect_pair` 给同一个
//! 进程里的桌面开一对 socket；socket 文件、锁文件、对端用户和收发缓冲也在这里设。

#[cfg(unix)]
use std::os::unix::{
    fs::{OpenOptionsExt as _, PermissionsExt as _},
    io::AsRawFd as _,
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
#[cfg(unix)]
use libc::{SO_RCVBUF, SO_SNDBUF};
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::{SO_RCVBUF, SO_SNDBUF};

#[cfg(windows)]
use super::windows::{WakeReader, WakeWriter, set_buffer, socket_pair, wait, wake_pair};
use super::{
    UnixListener, UnixStream,
    connection::{register, start_serving},
};
use crate::{Host, Shared};

/// 绑 socket 时文件名还在（删掉了但别的进程还开着它）最多再试这么久。
const BIND_PATIENCE: Duration = Duration::from_secs(5);
/// 接受连接出错（比如文件描述符用完了）后等一会儿再接，免得空转。
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// 每条连接 socket 的收发缓冲。系统默认的缓冲太小，刷屏的输出一块就塞满，读写两边来回
/// 切换的次数多到吞吐上不去；受内核上限（`kern.ipc.maxsockbuf`）限制。
const SOCKET_BUFFER: i32 = 4 << 20;

/// `Host::listen` 开着的 socket。丢掉时放开锁，socket 文件由退出的一方删。
pub(crate) struct Listening {
    pub(crate) socket: PathBuf,
    /// 监听的 socket，和接受连接的线程共用；交接时把它的描述符交给新宿主。
    #[cfg_attr(not(unix), expect(dead_code, reason = "只在交接时读"))]
    pub(crate) listener: Arc<UnixListener>,
    /// 锁住了的锁文件，交接时连同描述符交给新宿主；拿着它就一直锁着。
    #[cfg_attr(not(unix), expect(dead_code, reason = "只在交接时读"))]
    pub(crate) lock: File,
    /// 让接受连接的线程停下、接着接或者结束。
    pub(crate) control: Arc<ListenControl>,
}

/// 接受连接的线程该做什么，见 `ListenControl`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenState {
    Accepting,
    /// 不接，来的连接排在 backlog 里：交接期间，连接留给接手的新宿主或者回滚后的自己。
    Paused,
    /// 结束线程，之后不再变。只关自己这份描述符，不 `shutdown` 监听的 socket：交接后新宿主
    /// 用的是同一个打开的 socket。
    Stopped,
}

/// 管接受连接的线程：线程 `poll` 监听的 socket 和一根唤醒管道，改状态时往管道里写一个字节
/// 叫醒它。
pub(crate) struct ListenControl {
    /// 线程接连接时一直拿着它，所以 `pause`、`stop` 返回之后线程不会再接。
    state: Mutex<ListenState>,
    wake: WakeWriter,
}

impl ListenControl {
    /// 不再接新连接，返回时已经不会再接，见 `ListenState::Paused`。
    #[cfg(unix)]
    pub(crate) fn pause(&self) {
        self.set(ListenState::Paused);
    }

    /// 接着接新连接，先接 backlog 里排着的。
    #[cfg(unix)]
    pub(crate) fn resume(&self) {
        self.set(ListenState::Accepting);
    }

    /// 结束接受连接的线程，见 `ListenState::Stopped`。
    pub(crate) fn stop(&self) {
        self.set(ListenState::Stopped);
    }

    fn set(&self, to: ListenState) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            // 停过以后线程可能已经结束、唤醒管道的读端跟着关了，再写只会失败（比如交接后
            // `run_until_idle` 退出时又调一次 `stop`）。
            if *state == ListenState::Stopped {
                return;
            }
            *state = to;
        }
        if let Err(err) = (&self.wake).write_all(&[1]) {
            tracing::warn!("failed to wake the listener thread: {err}");
        }
    }
}

/// 接受连接的线程：按 `control` 的状态 `poll` 唤醒管道（停着时只等它）和监听的 socket，有连接
/// 就接下来、登记、起线程服务。
fn accept_loop(shared: &Arc<Shared>, listener: &UnixListener, control: &ListenControl, wake: &WakeReader) {
    loop {
        let state = *control.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state == ListenState::Stopped {
            return;
        }
        let (woken, connecting) = match wait(wake, listener, state == ListenState::Accepting) {
            Ok(ready) => ready,
            Err(err) => {
                if err.kind() != io::ErrorKind::Interrupted {
                    tracing::warn!("failed to wait for connections: {err}");
                    thread::sleep(ACCEPT_BACKOFF);
                }
                continue;
            }
        };
        if woken {
            let mut drained = [0u8; 64];
            let _ = (&*wake).read(&mut drained);
        }
        if !connecting {
            continue;
        }
        let (accepted, failed) = {
            let state = control.state.lock().unwrap_or_else(PoisonError::into_inner);
            if *state != ListenState::Accepting {
                continue;
            }
            accept_ready(listener)
        };
        for stream in accepted {
            // 判断还接不接和登记连接在同一把锁里，空闲退出时不会漏掉刚连上的。
            let Some(id) = register(shared) else {
                drop(stream);
                continue;
            };
            start_serving(shared, id, stream, true);
        }
        if failed {
            thread::sleep(ACCEPT_BACKOFF);
        }
    }
}

#[cfg(unix)]
type WakeReader = io::PipeReader;
#[cfg(unix)]
type WakeWriter = io::PipeWriter;

#[cfg(unix)]
fn wake_pair() -> io::Result<(WakeReader, WakeWriter)> {
    io::pipe()
}

/// 一对互相连着的 socket。
#[cfg(unix)]
pub fn socket_pair() -> io::Result<(UnixStream, UnixStream)> {
    UnixStream::pair()
}

/// 等唤醒管道可读，`accepting` 时也等监听的 socket 来连接；返回两者各自就绪没有。
#[cfg(unix)]
fn wait(wake: &WakeReader, listener: &UnixListener, accepting: bool) -> io::Result<(bool, bool)> {
    let mut fds = [
        libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    let count: libc::nfds_t = if accepting { 2 } else { 1 };
    // SAFETY: `fds` 是本地数组，`count` 不超过它的长度；两个描述符在这次调用期间都开着。
    if unsafe { libc::poll(fds.as_mut_ptr(), count, -1) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0].revents != 0, accepting && fds[1].revents != 0))
}

/// 把 backlog 里排着的连接都接下来；出错（比如文件描述符用完了）时停下，第二项为 true。
fn accept_ready(listener: &UnixListener) -> (Vec<UnixStream>, bool) {
    let mut accepted = Vec::new();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // macOS 上接到的连接沿用监听的 socket 的 `O_NONBLOCK`，连接一律按阻塞的读写。
                match stream.set_nonblocking(false) {
                    Ok(()) => accepted.push(stream),
                    Err(err) => tracing::warn!("failed to set up a connection: {err}"),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return (accepted, false),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                tracing::warn!("failed to accept a connection: {err}");
                return (accepted, true);
            }
        }
    }
}

impl Host {
    /// 在 `socket` 上监听别的进程来的前端，之后在后台线程里一直接受连接，直到宿主退出（见
    /// `Host::run_until_idle`）。
    ///
    /// `lock` 是同一目录里的锁文件：拿到它的进程才监听，另一个宿主已经在监听时返回错误。
    /// 拿到锁以后，`socket` 上已有的文件只能是上次没清掉的，删掉重建。宿主退出时删掉 `socket`
    /// （交接给新宿主时不删）。这个宿主已经在监听时返回错误。
    pub fn listen(&self, socket: &Path, lock: &Path) -> Result<()> {
        // 先看一眼，已经在监听时不去动 `socket` 上的文件；同时调的两次都过了这里的话，`start_listening` 在锁里
        // 拦下后一次，后一次删掉自己刚建的 socket 文件（拿着它的锁，那个路径归它）。
        if self.shared.peers().listening.is_some() {
            return Err(anyhow!("the host is already listening"));
        }
        let lock = lock_exclusively(lock)?;
        // 起不来时要删掉刚建的 socket 文件，删的时候还得锁着：`start_listening` 出错时丢掉交给它的锁，放开以后
        // 别的宿主就能拿到锁、在同一个路径上建好它的 socket，这时再删删掉的是它的。`flock` 锁的是打开的文件，
        // 复制的这份描述符留着就一直锁着。
        let held = lock.try_clone().context("failed to hold the lock")?;
        let listener = bind_fresh(socket).with_context(|| format!("failed to listen on {}", socket.display()))?;
        let started = restrict(socket).and_then(|()| self.start_listening(listener, lock, socket, false));
        if let Err(err) = started {
            let _ = std::fs::remove_file(socket);
            drop(held);
            return Err(err);
        }
        drop(held);
        // 之后开的 shell 里的命令行连这个 socket，开发版和装好的版本同时开着时也不会连错。
        self.set_env(runode_protocol::ENV_SOCKET, socket.as_os_str());
        Ok(())
    }

    /// 在已经开好的 `listener` 上接受别的进程来的前端，同 `Host::listen`，但不设 shell 的环境变量。
    /// `lock` 是锁住了的锁文件（`flock` 锁的是打开的文件，交接时连同描述符一起交过来的仍锁着），
    /// 丢掉时放开；`socket` 是 `listener` 的路径。`paused` 时接受连接的线程先停着（连接排在
    /// backlog 里），等 `ListenControl::resume`。
    pub(crate) fn start_listening(
        &self,
        listener: UnixListener,
        lock: File,
        socket: &Path,
        paused: bool,
    ) -> Result<Arc<ListenControl>> {
        // 看有没有在监听、起线程和登记 `listening` 都在同一把锁里：同时调的两次只有一次起得了线程，另一次
        // 的线程不会被覆盖掉、停不下来。接受连接的线程要拿这把锁时（登记连上来的连接）等这里放开。
        let mut peers = self.shared.peers();
        if peers.listening.is_some() {
            return Err(anyhow!("the host is already listening"));
        }
        // 非阻塞：接受连接的线程先 `poll` 再接，`poll` 之后没接到（别的进程抢先接走了，或者交接
        // 前后两个宿主共用这个打开的 socket）时回去接着等，不卡在 `accept` 里叫不停。
        listener.set_nonblocking(true).context("failed to make the listener non-blocking")?;
        let listener = Arc::new(listener);
        let (wake_rx, wake_tx) = wake_pair().context("failed to create the listener's wake pipe")?;
        let state = if paused { ListenState::Paused } else { ListenState::Accepting };
        let control = Arc::new(ListenControl { state: Mutex::new(state), wake: wake_tx });
        let shared = self.shared.clone();
        let thread_listener = listener.clone();
        let thread_control = control.clone();
        thread::Builder::new()
            .name("host-listener".into())
            .spawn(move || accept_loop(&shared, &thread_listener, &thread_control, &wake_rx))
            .context("failed to start the listener thread")?;
        peers.listening = Some(Listening { socket: socket.into(), listener, lock, control: control.clone() });
        Ok(control)
    }

    /// 在同一个进程里连上宿主：开一对互相连着的 socket，宿主这边起和 socket 上一样的连接，返回
    /// 另一端。之后按 `runode_protocol` 说话，先发 `Hello`。两端的收发缓冲都设好了。宿主在退出时
    /// 返回错误。
    pub fn connect_pair(&self) -> io::Result<UnixStream> {
        let (ours, theirs) = socket_pair()?;
        set_buffers(&theirs);
        let Some(id) = register(&self.shared) else {
            return Err(io::Error::other("the host is shutting down"));
        };
        if !start_serving(&self.shared, id, ours, false) {
            return Err(io::Error::other("failed to start a connection thread"));
        }
        Ok(theirs)
    }
}

/// 删掉 `socket` 上留下的文件再在上面开监听，调用方拿着这个路径的锁。Windows 上删掉的文件在别的进程（比如
/// 来试连的）放手之前还占着名字，绑定报地址已占用，隔一会儿再删再绑，最多 `BIND_PATIENCE`。
pub fn bind_fresh(socket: &Path) -> io::Result<UnixListener> {
    let deadline = Instant::now() + BIND_PATIENCE;
    let mut waited = false;
    loop {
        match std::fs::remove_file(socket) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        match UnixListener::bind(socket) {
            Err(err) if cfg!(windows) && err.kind() == io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                if !waited {
                    tracing::info!("{} is still taken, binding again: {err}", socket.display());
                    waited = true;
                }
                thread::sleep(Duration::from_millis(100));
            }
            bound => return bound,
        }
    }
}

/// 把 socket 文件的权限设成只有自己能读写。Windows 上的 socket 文件继承所在目录的权限，见
/// `Dirs::create_runtime_dir`。
fn restrict(socket: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    #[cfg(not(unix))]
    let _ = socket;
    Ok(())
}

/// 打开并锁住锁文件（Unix 上是 `flock`），拿不到时返回错误。进程退出（或者丢掉返回的文件）时锁自动
/// 放开。
fn lock_exclusively(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).with_context(|| format!("failed to open the lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(anyhow!("another runode already holds {}", path.display())),
        Err(std::fs::TryLockError::Error(err)) => Err(anyhow!("failed to lock {}: {err}", path.display())),
    }
}

/// 连上来的进程是不是和自己同一个用户。
#[cfg(unix)]
pub fn same_user(stream: &UnixStream) -> bool {
    // SAFETY: 没有参数，总是成功。
    peer_uid(stream) == Some(unsafe { libc::geteuid() })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: 描述符来自 `stream`，两个输出参数指向本地变量。
    (unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0).then_some(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    // SAFETY: `ucred` 是纯数据，全零合法。
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: 描述符来自 `stream`；输出参数指向本地变量，长度如实给出。
    let ok = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len)
    } == 0;
    ok.then_some(cred.uid)
}

/// 把 socket 的收发缓冲设成 `SOCKET_BUFFER`；设不了时记日志，照常用。
pub fn set_buffers(stream: &UnixStream) {
    for option in [SO_SNDBUF, SO_RCVBUF] {
        set_buffer(stream, option, SOCKET_BUFFER);
    }
}

/// 把 socket 的一个缓冲（`SO_SNDBUF` 或 `SO_RCVBUF`）设成 `size`；设不了时记日志，照常用。
#[cfg(unix)]
pub(super) fn set_buffer(stream: &UnixStream, option: i32, size: i32) {
    // SAFETY: 描述符来自 `stream`；值指向本地变量，长度是它的大小。
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            (&raw const size).cast(),
            std::mem::size_of_val(&size) as libc::socklen_t,
        )
    };
    if result != 0 {
        tracing::debug!("failed to set a socket buffer: {}", io::Error::last_os_error());
    }
}
