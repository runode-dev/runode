//! 子 shell 及其伪终端。
//!
//! 读线程把 PTY 输出交给创建时给的 `PtySink`，由宿主那边的会话线程接着处理。写入（按键、
//! 粘贴和 VT 查询的回复）经 `PtyWriter` 排进这个伪终端自己的写队列，由写线程按顺序写出，
//! 所以写入方从不阻塞：给一个不读 stdin 的程序粘贴一大段时，等着的只有写线程。
//!
//! 宿主升级时会话要交给新宿主，shell 不中断：`Pty::release` 交出 PTY master 的描述符、shell 的
//! pid 和重建要的其余状态（`PtyHandoff`），不结束 shell；对面用 `Pty::adopt` 接上。所以 master
//! 的描述符由这里自己持有：伪终端和 shell 仍由 portable-pty 打开和启动，打开后就复制一份 master
//! 的描述符，portable-pty 的 master 随即关掉；读线程、写线程和改尺寸、读前台进程组共用这一份。
//! 每个会话占两个描述符：master 和 `Notifier`（macOS 上是一个 kqueue）。
//!
//! master 设成非阻塞的，读写线程都先 `poll` 再读写：读线程要能被叫停，写线程卡住时要能把没写出
//! 的输入交出来。非阻塞记在打开的文件上，交出去的描述符也是非阻塞的，接手方用的同样是这里的
//! 读写线程。
//!
//! 交接分两步走，交出方提交之前接手方不能碰 PTY：`Pty::adopt_paused` 把读写线程先起好、停在
//! 闸门（`Gate`）上，一个字节都不读不写；`Pty::resume_reading` 打开闸门，不接手了就
//! `Pty::release` 原样交回去。线程事先起好，开闸这一步不会失败。从没开过闸的 `Pty` 直接丢掉
//! （比如接手方的会话线程 panic 了）也不碰 shell：只放读写线程过去、等它们结束，关掉自己的
//! 描述符，不发信号：那时 shell 还归交出方管，它回滚后照常用。开过闸以后丢掉照常结束 shell，
//! 见 `Drop`。

mod notify;
mod reader;

use std::{
    cell::Cell,
    ffi::OsString,
    fmt,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, PtySize, SlavePty, native_pty_system};
use runode_agent_detect::{ForegroundJob, ForegroundProcess};
use runode_shared_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};

use crate::shell_integration;
use notify::Notifier;
use reader::Reader;

/// 写线程写不进去（程序不读输入）时，隔这么久看一眼是不是要交接了。
const WRITER_STUCK_POLL: Duration = Duration::from_millis(50);
/// `raise_fd_limit` 最多把描述符的软上限提到这么高：macOS 的 `OPEN_MAX`，`setrlimit` 不接受
/// 比它大的软上限。
const FD_LIMIT_TARGET: libc::rlim_t = 10240;

/// 读线程交给 `PtySink` 的事件。
pub enum PtyEvent {
    Output(Arc<[u8]>),
    /// PTY 读到 EOF 或出错：子进程已经退出。接手来的会话里 shell 退出也算，见 `Pty::adopt`。
    Exited,
}

/// 收 PTY 输出的一方，在读线程里调用。返回 false 表示不再要了，读线程随之结束。可以阻塞，
/// 用来给读线程限流：收的一方积压太多时让它先别读。
pub type PtySink = Box<dyn FnMut(PtyEvent) -> bool + Send>;

/// 写队列里的一项。
enum WriterMsg {
    Data(Vec<u8>),
    /// 交接：能写的写完，写不进去的连同后面排着的交回来，然后写线程结束，见 `Pty::release`。
    Finish(mpsc::Sender<Vec<u8>>),
}

/// 往 PTY 写的一端：只把数据排进写队列，不等它写出去。
#[derive(Clone)]
pub struct PtyWriter(mpsc::Sender<WriterMsg>);

/// 闸门的状态，见 `Gate`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GateState {
    Closed,
    Open,
    /// 不接手了：读线程不读就交回 `PtySink`，写线程一个字节都不写，排着的输入原样交回去。
    Abandoned,
}

/// `Pty::adopt_paused` 接手来的 PTY 上，读写线程起好后先等着的闸门：交出方提交之前，接手方
/// 不能从 PTY 读走输出，也不能往里写输入。只开（或放弃）一次。
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

impl Gate {
    fn closed() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(GateState::Closed), changed: Condvar::new() })
    }

    /// 等闸门打开，返回 true；放弃了（`GateState::Abandoned`）时返回 false。
    fn wait(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while *state == GateState::Closed {
            state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        *state == GateState::Open
    }

    fn set(&self, to: GateState) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = to;
        self.changed.notify_all();
    }
}

/// 写线程，交接时要叫停它、等它结束。
struct WriterThread {
    /// 置上后写线程写不进去时不再等，把没写出的交回来。
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

/// 一次写到底的结果。
enum Written {
    All,
    /// 要交接了，从这个位置起没写出去。
    Stopped(usize),
    Failed(std::io::Error),
}

impl PtyWriter {
    pub fn write(&self, data: &[u8]) {
        if !data.is_empty() {
            self.send(data.to_vec());
        }
    }

    /// 同 `write`，数据已经在自己的缓冲里时不必再复制一份。
    pub fn send(&self, data: Vec<u8>) {
        // 写线程已经结束（PTY 出错、关了或者交出去了）时没有可写的地方，丢掉。
        if !data.is_empty() && self.0.send(WriterMsg::Data(data)).is_err() {
            tracing::debug!("pty writer is gone, input dropped");
        }
    }

    /// 起写线程，把排进队列的数据按顺序写进 `master`。所有发送端都没了、写出错或者收到
    /// `WriterMsg::Finish` 时结束。给了 `gate` 时先等它打开再写，放弃了就一个字节都不写，排着的
    /// 输入等 `WriterMsg::Finish` 原样交回去。
    fn start(master: Arc<OwnedFd>, gate: Option<Arc<Gate>>) -> Result<(Self, WriterThread)> {
        let (tx, rx) = mpsc::channel::<WriterMsg>();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = thread::Builder::new()
            .name("pty-writer".into())
            .spawn({
                let stop = stop.clone();
                move || {
                    if let Some(gate) = gate
                        && !gate.wait()
                    {
                        hand_back(&rx, Vec::new());
                        return;
                    }
                    write_loop(&master, &rx, &stop);
                }
            })
            .context("failed to start pty writer thread")?;
        Ok((Self(tx), WriterThread { stop, handle }))
    }

    /// 交接时叫停写线程并等它结束，返回没写出去的输入：程序不读输入、写不进去的那些，和排在
    /// 后面的。写线程不会卡住超过 `WRITER_STUCK_POLL`。之后再写的都丢掉。
    fn finish(&self, thread: WriterThread) -> Vec<u8> {
        let (done, unwritten) = mpsc::channel();
        let sent = self.0.send(WriterMsg::Finish(done)).is_ok();
        // 先排上 `Finish` 再置标记：写线程看到标记后收队列，一定收得到它。
        thread.stop.store(true, Ordering::Release);
        // 写线程出错先结束了的话没有交回来的。
        let pending = if sent { unwritten.recv().unwrap_or_default() } else { Vec::new() };
        if thread.handle.join().is_err() {
            tracing::warn!("the pty writer thread panicked");
        }
        pending
    }
}

fn write_loop(master: &OwnedFd, rx: &mpsc::Receiver<WriterMsg>, stop: &AtomicBool) {
    set_current_thread_interactive();
    while let Ok(message) = rx.recv() {
        match message {
            WriterMsg::Data(data) => match write_all(master, &data, stop) {
                Written::All => {}
                Written::Stopped(at) => {
                    hand_back(rx, data[at..].to_vec());
                    return;
                }
                Written::Failed(err) => {
                    tracing::warn!("pty write failed: {err}");
                    return;
                }
            },
            WriterMsg::Finish(done) => {
                let _ = done.send(Vec::new());
                return;
            }
        }
    }
}

/// 把 `pending` 和队列里排在 `Finish` 前面的输入一起交回去。发送端都没了也没等到 `Finish` 时
/// 丢掉。
fn hand_back(rx: &mpsc::Receiver<WriterMsg>, mut pending: Vec<u8>) {
    for message in rx {
        match message {
            WriterMsg::Data(data) => pending.extend_from_slice(&data),
            WriterMsg::Finish(done) => {
                let _ = done.send(pending);
                return;
            }
        }
    }
}

/// 把 `data` 都写进非阻塞的 `master`；写不进去时等它能写，要交接了（`stop`）就不等了。
fn write_all(master: &OwnedFd, data: &[u8], stop: &AtomicBool) -> Written {
    let mut at = 0;
    while at < data.len() {
        let rest = &data[at..];
        // SAFETY: `master` 开着，`rest` 是可读的缓冲，长度如实传入。
        let n = unsafe { libc::write(master.as_raw_fd(), rest.as_ptr().cast(), rest.len()) };
        if let Ok(n) = usize::try_from(n) {
            at += n;
            continue;
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            std::io::ErrorKind::Interrupted => {}
            std::io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Acquire) {
                    return Written::Stopped(at);
                }
                let mut poll = libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
                let timeout = libc::c_int::try_from(WRITER_STUCK_POLL.as_millis()).unwrap_or(libc::c_int::MAX);
                // SAFETY: 只传了一个指向本地变量的 pollfd。
                unsafe { libc::poll(&raw mut poll, 1, timeout) };
            }
            _ => return Written::Failed(err),
        }
    }
    Written::All
}

/// 把当前线程设成交互用的服务质量（macOS 的 `QOS_CLASS_USER_INTERACTIVE`），按键到回显路上的
/// 线程不被调度到能效核上排队。别的系统上什么都不做。
pub fn set_current_thread_interactive() {
    #[cfg(target_os = "macos")]
    {
        let result = unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
        if result != 0 {
            tracing::debug!("failed to raise the thread's QoS: {result}");
        }
    }
}

/// 把本进程能同时打开的描述符数（`RLIMIT_NOFILE` 的软上限）提到硬上限，最多 `FD_LIMIT_TARGET`；
/// 已经够高时不动。返回提完之后的软上限。每个会话要占两个描述符，从 Finder 启动的 app 软上限
/// 只有 256，开得多了会不够。
pub fn raise_fd_limit() -> std::io::Result<u64> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: 只往传进去的本地结构里写。
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let target = limit.rlim_max.min(FD_LIMIT_TARGET);
    if limit.rlim_cur >= target {
        return Ok(limit.rlim_cur);
    }
    limit.rlim_cur = target;
    // SAFETY: 只读传进去的本地结构。
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(target)
}

/// 交出去的会话：在另一个进程（或者同一个进程的别处）用 `Pty::adopt` 重建。`master` 要经
/// Unix socket 的 `SCM_RIGHTS` 传过去，其余字段由调用方自己编码。丢掉它会关掉这份描述符；
/// 交出方和接手方都关了以后，shell 收到 SIGHUP。
pub struct PtyHandoff {
    /// PTY master 的描述符，带 `FD_CLOEXEC`，是非阻塞的。
    pub master: OwnedFd,
    /// shell 的进程号。它是交出方的子进程，接手方不是它的父进程。
    pub pid: u32,
    /// PTY 现在的尺寸。内核里的尺寸随描述符一起留着，这里只是给接手方记账用。
    pub size: GridSize,
    /// 启动 shell 时交给集成脚本的报告口令，见 `Pty::report_token`。
    pub report_token: Option<String>,
    /// 交出时还没写进 PTY 的输入（程序没在读输入），接手方在别的输入之前先写，见 `Pty::adopt`。
    pub pending_input: Vec<u8>,
}

impl fmt::Debug for PtyHandoff {
    /// 口令不打出来，只说有没有。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyHandoff")
            .field("master", &self.master)
            .field("pid", &self.pid)
            .field("size", &self.size)
            .field("report_token", &self.report_token.as_ref().map(|_| "<redacted>"))
            .field("pending_input", &self.pending_input.len())
            .finish()
    }
}

/// `Pty::adopt` 失败：交来的东西原样还给调用方，没有关掉描述符，shell 不受影响。
#[derive(Debug)]
pub struct AdoptError {
    pub handoff: PtyHandoff,
    pub error: anyhow::Error,
}

impl fmt::Display for AdoptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to adopt the pty: {:#}", self.error)
    }
}

impl std::error::Error for AdoptError {}

/// PTY 上跑着的 shell。
enum Shell {
    /// 自己启动的子进程，退出时自己回收。
    Child(Box<dyn Child + Send + Sync>),
    /// 接手来的，进程号是这个：不是自己的子进程，只能经 `Notifier` 看着它退出。
    Adopted(libc::pid_t),
}

pub struct Pty {
    /// PTY master 的描述符，和读线程、写线程共用；交出去以后为 `None`，改尺寸、读前台进程都不再
    /// 碰 PTY。
    master: Option<Arc<OwnedFd>>,
    /// `adopt_paused` 接手来、还没 `resume_reading` 时读写线程等着的闸门，开了或者放弃了以后为
    /// `None`。丢掉时它还在就说明从没开过闸，不结束 shell，见 `Drop`。
    gate: Option<Arc<Gate>>,
    /// 叫醒读线程；接手来的会话里还看着 shell 退出。
    notifier: Arc<Notifier>,
    /// 还没启动 shell 时的从设备和读线程要交给的 `PtySink`，`start` 时交出去。
    pending: Option<(Box<dyn SlavePty + Send>, PtySink)>,
    /// `Drop` 和 `release` 里取走，所以是 `Option`。
    shell: Option<Shell>,
    /// 读线程；还没启动 shell 时为 `None`。
    reader: Option<Reader>,
    pub writer: PtyWriter,
    /// 写线程，`release` 时取走。
    writer_thread: Option<WriterThread>,
    /// 启动 shell 时交给集成脚本的报告口令，见 `shell_integration::prepare`；没注入集成或者
    /// 还没启动时为 `None`。
    report_token: Option<String>,
    /// 启动 shell 时另外设的环境变量，见 `set_env`。
    env: Vec<(OsString, OsString)>,
    /// 启动 shell 时告诉集成脚本开哪些功能，见 `set_shell_features`。
    shell_features: String,
    /// 最近一次设给 PTY 的尺寸。
    size: Cell<GridSize>,
}

fn pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.cols.saturating_mul(size.cell_width_px),
        pixel_height: size.rows.saturating_mul(size.cell_height_px),
    }
}

/// 改 PTY 的尺寸（`TIOCSWINSZ`），前台程序随之收到 SIGWINCH。
fn set_winsize(master: BorrowedFd<'_>, size: GridSize) -> std::io::Result<()> {
    let size = pty_size(size);
    let winsize = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    };
    // SAFETY: `master` 在这次调用期间一直开着；`TIOCSWINSZ` 只读传进去的结构。
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const winsize) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// 给描述符设上 `O_NONBLOCK`。
fn set_nonblocking(fd: BorrowedFd<'_>) -> std::io::Result<()> {
    // SAFETY: `fd` 开着；F_GETFL/F_SETFL 只读写文件状态标志。
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 终端的前台进程组号；没有前台进程组或者读不到时为 `None`。
fn foreground_group(master: BorrowedFd<'_>) -> Option<libc::pid_t> {
    // SAFETY: `master` 在这次调用期间一直开着。
    let group = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    (group > 0).then_some(group)
}

/// 终端的前台进程组，要它属于以 `shell` 为首的会话：终端还是 shell 的控制终端，或者这个组的
/// 组长还在 shell 的会话里。当场从终端读出，所以不会是进程号被重用了的别的进程组。
fn shell_foreground(master: BorrowedFd<'_>, shell: libc::pid_t) -> Option<libc::pid_t> {
    let group = foreground_group(master)?;
    // SAFETY: 两个调用都只查询，失败时返回 -1。
    let (terminal_session, group_session) = unsafe { (libc::tcgetsid(master.as_raw_fd()), libc::getsid(group)) };
    (terminal_session == shell || group_session == shell).then_some(group)
}

/// 共用的 master 在读写线程都结束后只剩这一份，取出来；万一还有别的引用，复制一份。
fn take_master(master: Arc<OwnedFd>) -> std::io::Result<OwnedFd> {
    Arc::try_unwrap(master).or_else(|shared| shared.try_clone())
}

impl Pty {
    /// 在 `cwd` 下以登录 shell 方式启动 `shell`（为 `None` 时用用户的 `$SHELL`），按 `integration`
    /// 注入 shell 集成，并启动读线程，输出交给 `sink`。
    pub fn spawn(
        size: GridSize,
        shell: Option<&str>,
        cwd: Option<&std::path::Path>,
        integration: IntegrationMode,
        sink: PtySink,
    ) -> Result<Self> {
        let mut pty = Self::open(size, sink)?;
        pty.start(shell, cwd, integration)?;
        Ok(pty)
    }

    /// 只打开伪终端，shell 等 `start` 时再启动；在那之前没有子进程，也没有读线程。写线程现在
    /// 就起，写进去的内容等 shell 启动后读。
    pub fn open(size: GridSize, sink: PtySink) -> Result<Self> {
        // 系统的 openpty 内部用了不可重入的 ptsname，多个线程同时开伪终端会互相踩，
        // 拿到错的从设备名而失败。
        static OPENPTY: Mutex<()> = Mutex::new(());
        let pair = {
            let _guard = OPENPTY.lock().unwrap_or_else(|e| e.into_inner());
            native_pty_system().openpty(pty_size(size))
        }
        .context("openpty failed")?;
        let raw = pair.master.as_raw_fd().ok_or_else(|| anyhow!("the pty master has no file descriptor"))?;
        // SAFETY: `raw` 是 `pair.master` 持有的描述符，复制完之前它一直开着。
        let master =
            unsafe { BorrowedFd::borrow_raw(raw) }.try_clone_to_owned().context("failed to dup the pty master")?;
        // 自己这份复制好了，portable-pty 的 master 不再需要；它的写端从来没取过，关掉时也不会
        // 往 PTY 里写东西。
        drop(pair.master);
        set_nonblocking(master.as_fd()).context("failed to make the pty master non-blocking")?;
        let notifier = Arc::new(Notifier::new(None).context("failed to create the pty reader's notifier")?);
        let master = Arc::new(master);
        let (writer, writer_thread) = PtyWriter::start(master.clone(), None)?;
        Ok(Self {
            master: Some(master),
            gate: None,
            notifier,
            pending: Some((pair.slave, sink)),
            shell: None,
            reader: None,
            writer,
            writer_thread: Some(writer_thread),
            report_token: None,
            env: Vec::new(),
            shell_features: shell_integration::features(&TermSettings::default()),
            size: Cell::new(size),
        })
    }

    /// 启动 shell 时给它设这个环境变量，盖过从 app 继承来的同名变量；已经启动了的不受影响。
    pub fn set_env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.env.push((key.into(), value.into()));
    }

    /// 启动 shell 时按 `settings` 告诉集成脚本开哪些功能，见 `shell_integration::features`；不调时
    /// 按默认的设置开。已经启动了的不受影响。
    pub fn set_shell_features(&mut self, settings: &TermSettings) {
        self.shell_features = shell_integration::features(settings);
    }

    /// 接手 `Pty::release` 交出来的会话：用交来的 master 起读写线程，输出交给 `sink`，交出时没写
    /// 出去的输入先写。shell 不是本进程的子进程，不回收它也拿不到退出码；它退出时即使 PTY 还没
    /// 读到 EOF（比如后台程序还开着终端），读完剩下的输出也报告 `PtyEvent::Exited`。丢掉返回的
    /// `Pty` 会结束 shell 的进程组，见 `Drop`。失败时交来的东西原样放在 `AdoptError` 里还回去。
    pub fn adopt(handoff: PtyHandoff, sink: PtySink) -> Result<Self, AdoptError> {
        Self::adopt_with(handoff, sink, None)
    }

    /// 同 `adopt`，但读写线程起好后先停在闸门上：不从 PTY 读，`handoff.pending_input` 和之后经
    /// `writer` 写的都排着不写，直到 `resume_reading` 打开闸门，排着的按先后写出去（交出时没写
    /// 出去的在最前面）。交出方还没提交、可能回滚时用它：在这之前 PTY 原封未动。
    ///
    /// 停着时改尺寸、读前台进程照常（它们不读写 PTY）；不接手了用 `release` 原样交回去，
    /// `pending_input` 和排着的输入都在交回的 `PtyHandoff::pending_input` 里。
    ///
    /// 还没 `resume_reading` 就丢掉（包括持有它的线程 panic、栈展开时丢掉）不结束 shell：交出方
    /// 还没提交，shell 仍归它管，丢掉只是放弃接手。读写线程放过去并等它们结束，排着的输入丢掉，
    /// 关掉这边的 master 和 `Notifier`，不给 shell 和前台进程组发任何信号。这时交出方要是也已经
    /// 关了它那份 master，这里关的就是最后一份，shell 照终端的规矩收到内核发的 SIGHUP。开过闸
    /// 以后丢掉和 `adopt` 的一样结束 shell。
    pub fn adopt_paused(handoff: PtyHandoff, sink: PtySink) -> Result<Self, AdoptError> {
        Self::adopt_with(handoff, sink, Some(Gate::closed()))
    }

    /// `adopt` 和 `adopt_paused`：给了 `gate` 时读写线程先等它。
    fn adopt_with(handoff: PtyHandoff, sink: PtySink, gate: Option<Arc<Gate>>) -> Result<Self, AdoptError> {
        let Some(pid) = libc::pid_t::try_from(handoff.pid).ok().filter(|&pid| pid > 0) else {
            let error = anyhow!("invalid shell pid {}", handoff.pid);
            return Err(AdoptError { handoff, error });
        };
        if let Err(err) = set_nonblocking(handoff.master.as_fd()) {
            let error = anyhow::Error::new(err).context("failed to make the pty master non-blocking");
            return Err(AdoptError { handoff, error });
        }
        let notifier = match Notifier::new(Some(pid)) {
            Ok(notifier) => Arc::new(notifier),
            Err(err) => {
                let error = anyhow::Error::new(err).context("failed to watch the shell");
                return Err(AdoptError { handoff, error });
            }
        };
        let PtyHandoff { master, pid: raw_pid, size, report_token, pending_input } = handoff;
        let master = Arc::new(master);
        // 起线程失败时线程拿走的那份引用随之丢掉，master 又只剩这一份，原样还回去。
        let give_back = |master: Arc<OwnedFd>, report_token, pending_input, error| {
            let master = Arc::into_inner(master).expect("the threads holding the master have ended");
            AdoptError { handoff: PtyHandoff { master, pid: raw_pid, size, report_token, pending_input }, error }
        };
        let (writer, writer_thread) = match PtyWriter::start(master.clone(), gate.clone()) {
            Ok(started) => started,
            Err(error) => return Err(give_back(master, report_token, pending_input, error)),
        };
        let reader = match Reader::start(master.clone(), notifier.clone(), sink, gate.clone()) {
            Ok(reader) => reader,
            Err(error) => {
                // 写线程收不到东西了就结束，等它放开 master；停在闸门上的先放它过去。
                if let Some(gate) = &gate {
                    gate.set(GateState::Abandoned);
                }
                drop(writer);
                if writer_thread.handle.join().is_err() {
                    tracing::warn!("the pty writer thread panicked");
                }
                return Err(give_back(master, report_token, pending_input, error));
            }
        };
        writer.send(pending_input);
        Ok(Self {
            master: Some(master),
            gate,
            notifier,
            pending: None,
            shell: Some(Shell::Adopted(pid)),
            reader: Some(reader),
            writer,
            writer_thread: Some(writer_thread),
            report_token,
            // 接手来的 shell 早就启动了，没有要设的环境变量。
            env: Vec::new(),
            shell_features: String::new(),
            size: Cell::new(size),
        })
    }

    /// 让读线程停下，之后不再从 PTY 读，没读的输出留在 PTY 里。读线程已经读出来的那块照样交给
    /// `PtySink`，交完就结束，不报告 `PtyEvent::Exited`；`reader_finished` 为 true 后，`PtySink`
    /// 不会再收到东西。不等读线程结束。交接用它先停下来，交接不成或者要等下一批输出（比如快照
    /// 编不出来，见 `SnapshotError::Unfinished`）时用 `resume_reading` 接着读。
    pub fn stop_reading(&mut self) {
        if let Some(reader) = &mut self.reader {
            reader.stop();
        }
    }

    /// `stop_reading` 之后接着读，输出还交给原来的 `PtySink`。读线程还没停下时撤回叫停，已经
    /// 停下时重新起读线程。没有叫停过、或者已经读到 EOF 时什么都不做。
    ///
    /// `adopt_paused` 接手来的在这里打开闸门：读线程开始读，排着的输入开始写。这一步不会失败。
    pub fn resume_reading(&mut self) -> Result<()> {
        if let Some(gate) = self.gate.take() {
            gate.set(GateState::Open);
        }
        match &mut self.reader {
            Some(reader) if self.master.is_some() => reader.resume(),
            _ => Ok(()),
        }
    }

    /// 读线程已经结束（或者还没启动 shell，根本没有读线程）。
    pub fn reader_finished(&self) -> bool {
        self.reader.as_ref().is_none_or(Reader::finished)
    }

    /// 交出会话，不结束 shell：停下读线程并等它结束（同 `stop_reading`），叫停写线程，然后交出
    /// master 描述符、shell 的 pid、尺寸、报告口令和写不进去的输入（程序没在读输入时，写线程
    /// 不等它读，见 `PtyHandoff::pending_input`）。交出后这个 `Pty` 不再管这个会话：写进去的都
    /// 丢掉，改尺寸、读前台进程都不碰 PTY，丢掉它也不结束 shell。出错时（还没启动 shell、已经
    /// 交出去过）什么都没动，`Pty` 照常可用。
    ///
    /// 等读线程结束时，它要是正卡在 `PtySink` 里等调用方腾地方（限流），就会一直等下去；这种
    /// `PtySink` 要先 `stop_reading`，一边处理收到的输出一边等 `reader_finished`，再来交出。
    ///
    /// shell 是自己的子进程时，交出后还在这边起一个线程等它退出、回收它，免得留下僵尸；不影响
    /// 接手方发现它退出。
    pub fn release(&mut self) -> Result<PtyHandoff> {
        if !self.started() {
            bail!("the shell has not been started");
        }
        if self.master.is_none() {
            bail!("the pty has already been handed off");
        }
        let pid = self.shell_pid().context("the shell's pid is unknown")?;
        let pid = u32::try_from(pid).context("invalid shell pid")?;
        // 上面是会失败的检查，失败时什么都没动；下面不再失败，只剩取回 master 那一步理论上的退路。
        // `adopt_paused` 接手来、还停着的：读写线程不读不写就结束，排着的输入原样交回来。
        if let Some(gate) = self.gate.take() {
            gate.set(GateState::Abandoned);
        }
        if let Some(reader) = &mut self.reader {
            reader.stop();
            reader.join();
        }
        let pending_input = self.writer_thread.take().map(|thread| self.writer.finish(thread)).unwrap_or_default();
        if let Some(Shell::Child(mut child)) = self.shell.take() {
            let reaped = thread::Builder::new().name("pty-reaper".into()).spawn(move || {
                let _ = child.wait();
            });
            if let Err(err) = reaped {
                tracing::warn!("failed to start the pty reaper thread: {err}");
            }
        }
        let master = self.master.take().context("the pty has already been handed off")?;
        let master = take_master(master).context("failed to take back the pty master")?;
        Ok(PtyHandoff { master, pid, size: self.size.get(), report_token: self.report_token.clone(), pending_input })
    }

    /// 丢掉还停在闸门上、从没开过闸的 `Pty`：放读写线程过去并等它们结束（它们都还停在闸门上，
    /// 放过去就结束，不会卡住），排着的输入丢掉；shell 不碰，见 `adopt_paused`。读写线程手里的
    /// master 和 `Notifier` 随它们结束放开，这个 `Pty` 自己的那份随后在字段丢掉时关掉。
    fn drop_paused(&mut self, gate: &Gate) {
        gate.set(GateState::Abandoned);
        if let Some(reader) = &mut self.reader {
            // 交回来的 `PtySink` 不要了。
            drop(reader.join());
        }
        if let Some(thread) = self.writer_thread.take() {
            // 停在闸门上的写线程放过去后收到 `WriterMsg::Finish` 就交回排着的输入结束，不管
            // 别处还有没有 `PtyWriter` 的克隆。
            drop(self.writer.finish(thread));
        }
        self.shell = None;
    }

    /// 已经启动了 shell。
    pub fn started(&self) -> bool {
        self.pending.is_none()
    }

    /// shell 报告 PATH 等信息时要带的口令，见 `shell_integration::prepare`。
    pub fn report_token(&self) -> Option<&str> {
        self.report_token.as_deref()
    }

    /// 在 `cwd` 下启动 shell 和读线程，参数同 `spawn`。已经启动过时什么都不做。
    pub fn start(
        &mut self,
        shell: Option<&str>,
        cwd: Option<&std::path::Path>,
        integration: IntegrationMode,
    ) -> Result<()> {
        if self.pending.is_none() {
            return Ok(());
        }
        // 交出去的一定已经启动过，上面就返回了；这里只是取共用的 master。
        let Some(master) = self.master.clone() else {
            bail!("the pty has been handed off");
        };
        let Some((slave, sink)) = self.pending.take() else {
            return Ok(());
        };
        let shell =
            shell.map(str::to_owned).or_else(|| std::env::var("SHELL").ok()).unwrap_or_else(|| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(&shell);
        // 用登录 shell，这样会执行用户的 profile。
        let report_token = shell_integration::prepare(integration, &shell, &self.shell_features, &mut cmd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "runode");
        cmd.env("TERM_PROGRAM_VERSION", crate::VERSION);
        // macOS 自带的 BSD ls 只在设置了 CLICOLOR 时才着色；用户已有设置就不覆盖。
        if std::env::var_os("CLICOLOR").is_none() {
            cmd.env("CLICOLOR", "1");
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        let cwd = cwd.map(Into::into).or_else(|| runode_paths::Dirs::from_env().home);
        if let Some(cwd) = cwd {
            cmd.cwd::<std::path::PathBuf>(cwd);
        }

        let child = slave.spawn_command(cmd).context("failed to spawn shell")?;
        self.shell = Some(Shell::Child(child));
        self.report_token = report_token;
        // 子进程持有自己的 slave 副本；我们这份必须关掉，子进程退出时才会读到 EOF。
        drop(slave);

        self.reader = Some(Reader::start(master, self.notifier.clone(), sink, None)?);
        Ok(())
    }

    /// 复制一份 PTY master 的描述符（带 `FD_CLOEXEC`，和这份共用同一个打开的文件，也是非阻塞
    /// 的）。交接时交出方先把复制的一份传过去，自己这份照常用，提交时才 `release`；交出去以后
    /// 出错。
    pub fn dup_master(&self) -> std::io::Result<OwnedFd> {
        match &self.master {
            Some(master) => master.try_clone(),
            None => Err(std::io::Error::other("the pty has been handed off")),
        }
    }

    /// 改 PTY 的尺寸；交出去以后什么都不做。
    pub fn resize(&self, size: GridSize) {
        let Some(master) = &self.master else {
            return;
        };
        match set_winsize(master.as_fd(), size) {
            Ok(()) => self.size.set(size),
            Err(err) => tracing::warn!("pty resize failed: {err}"),
        }
    }

    /// shell 自己当前所在的目录（不管前台在跑什么）。
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        process_cwd(self.shell_pid()?)
    }

    /// 前台是不是 shell 自己，也就是没有在跑别的程序。取不到时当作不是。
    pub fn foreground_is_shell(&self) -> bool {
        self.foreground().is_some_and(|(_, is_shell)| is_shell)
    }

    /// 终端前台进程组的组长，以及它是不是 shell 自己。交出去以后为 `None`。
    pub(crate) fn foreground(&self) -> Option<(libc::pid_t, bool)> {
        let leader = foreground_group(self.master.as_ref()?.as_fd())?;
        let shell = self.shell_pid()?;
        Some((leader, leader == shell))
    }

    /// shell 的进程号；还没启动或者交出去以后为 `None`。
    pub(crate) fn shell_pid(&self) -> Option<libc::pid_t> {
        match self.shell.as_ref()? {
            Shell::Child(child) => libc::pid_t::try_from(child.process_id()?).ok(),
            Shell::Adopted(pid) => Some(*pid),
        }
    }
}

/// 以 `leader` 为组长的进程组里的全部进程，带着进程名和参数，认 agent 用。一个都读不到时
/// 为 `None`。
pub(crate) fn process_group(leader: libc::pid_t) -> Option<ForegroundJob> {
    let processes: Vec<ForegroundProcess> = group_members(leader)
        .into_iter()
        .filter_map(|pid| {
            let argv = process_argv(pid);
            Some(ForegroundProcess {
                pid: u32::try_from(pid).ok()?,
                name: process_name(pid)?,
                argv0: argv.as_ref().and_then(|argv| argv.first()).and_then(|first| {
                    let name = first.rsplit('/').next().unwrap_or(first);
                    let name = name.strip_prefix('-').unwrap_or(name);
                    (!name.is_empty()).then(|| name.to_owned())
                }),
                argv,
            })
        })
        .collect();
    let leader = u32::try_from(leader).ok()?;
    (!processes.is_empty()).then_some(ForegroundJob { leader, processes })
}

pub fn dir_label(path: &Path) -> String {
    if runode_paths::Dirs::from_env().is_home(path) {
        return "~".into();
    }
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        // 根目录没有文件名。
        None => path.display().to_string(),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn process_name(pid: libc::pid_t) -> Option<String> {
    // 内核里的进程名最长 2 * MAXCOMLEN 字节。
    let mut buf = [0u8; 64];
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

#[cfg(target_os = "macos")]
fn process_cwd(pid: libc::pid_t) -> Option<PathBuf> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};

    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), size) };
    if written != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    // `vip_path` 是按 MAXPATHLEN 连续排布的 C 字符串，内核保证以 NUL 结尾。
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    Some(std::ffi::OsStr::from_bytes(path.to_bytes()).into())
}

/// 进程组里的进程号；组长总在里面，读不到组员时只有组长。
#[cfg(target_os = "macos")]
fn group_members(leader: libc::pid_t) -> Vec<libc::pid_t> {
    // `proc_listpids` 按进程组列进程的类型，libc 里没有这个常量。
    const PROC_PGRP_ONLY: u32 = 2;
    let Ok(group) = u32::try_from(leader) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; 16];
    // 进程多得放不下时加倍再读，最多试几次。
    for _ in 0..6 {
        let capacity = std::mem::size_of_val(pids.as_slice());
        let Ok(capacity_c) = libc::c_int::try_from(capacity) else {
            break;
        };
        let written = unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group, pids.as_mut_ptr().cast(), capacity_c) };
        let Ok(written) = usize::try_from(written) else {
            break;
        };
        if written < capacity {
            pids.truncate(written / std::mem::size_of::<libc::pid_t>());
            pids.retain(|&pid| pid > 0);
            if !pids.contains(&leader) {
                pids.insert(0, leader);
            }
            return pids;
        }
        pids.resize(pids.len() * 2, 0);
    }
    vec![leader]
}

/// 进程的全部参数，用 `sysctl(KERN_PROCARGS2)` 读：开头是参数个数，接着是可执行文件路径和
/// 补齐用的 NUL，然后是各个参数，每个以 NUL 结尾。程序运行中改了 argv[0]（比如 node 的
/// `process.title`）时读到的是改过的。
#[cfg(target_os = "macos")]
fn process_argv(pid: libc::pid_t) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    let ok = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    if ok != 0 || size < 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let ok = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    if ok != 0 {
        return None;
    }
    buf.truncate(size);
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let start = exec_end + rest[exec_end..].iter().position(|&b| b != 0)?;
    let argv: Vec<String> =
        rest[start..].split(|&b| b == 0).take(argc).map(|arg| String::from_utf8_lossy(arg).into_owned()).collect();
    (!argv.is_empty()).then_some(argv)
}

#[cfg(not(target_os = "macos"))]
fn group_members(leader: libc::pid_t) -> Vec<libc::pid_t> {
    vec![leader]
}

#[cfg(not(target_os = "macos"))]
fn process_argv(_pid: libc::pid_t) -> Option<Vec<String>> {
    None
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn process_name(_pid: libc::pid_t) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn process_cwd(_pid: libc::pid_t) -> Option<PathBuf> {
    None
}

impl Drop for Pty {
    fn drop(&mut self) {
        // `adopt_paused` 接手来、从没开过闸：交出方还没提交，shell 归它管，这里只关自己的描述符，
        // 不结束 shell。读写线程还停在闸门上，不放过去的话它们一直等着、master 也一直关不掉。
        if let Some(gate) = self.gate.take() {
            self.drop_paused(&gate);
            return;
        }
        match self.shell.take() {
            // 关窗口即结束会话；master 关闭后 shell 本来也会收到 SIGHUP，kill() 只是让它立即退出。
            // 子进程退出后还得 wait 才会被系统回收，否则每关一个标签就留一个僵尸进程；
            // shell 可能忽略 SIGHUP，所以放到单独的线程里等，不阻塞界面。
            Some(Shell::Child(mut child)) => {
                let _ = child.kill();
                let _ = thread::Builder::new().name("pty-reaper".into()).spawn(move || {
                    let _ = child.wait();
                });
            }
            // 接手来的 shell 不是自己的子进程，不能 wait，也不归这边回收；给前台进程组和它的
            // 进程组发 SIGHUP，不退出再 SIGKILL。shell 已经退出、前台程序还在时，前台进程组照样
            // 收到 SIGHUP。
            Some(Shell::Adopted(pid)) => {
                let foreground = self.master.as_ref().and_then(|master| shell_foreground(master.as_fd(), pid));
                self.notifier.clone().terminate(foreground);
            }
            // 还没启动，或者已经交出去了。
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn winsize(master: BorrowedFd<'_>) -> libc::winsize {
        let mut size = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: `master` 开着；`TIOCGWINSZ` 只往传进去的结构里写。
        let result = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        size
    }

    #[test]
    fn resize_sets_the_window_size() {
        let size = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
        let pty = Pty::open(size, Box::new(|_| true)).unwrap();
        let master = pty.master.clone().unwrap();
        let got = winsize(master.as_fd());
        assert_eq!((got.ws_col, got.ws_row), (20, 4));
        pty.resize(GridSize { cols: 100, rows: 40, ..size });
        let got = winsize(master.as_fd());
        assert_eq!((got.ws_col, got.ws_row, got.ws_xpixel, got.ws_ypixel), (100, 40, 800, 640));
    }

    #[test]
    fn raise_fd_limit_never_lowers_it() {
        let mut before = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: 只往本地结构里写。
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut before) }, 0);
        let raised = raise_fd_limit().unwrap();
        assert!(raised >= before.rlim_cur);
        assert!(raised <= before.rlim_max);
        assert_eq!(raise_fd_limit().unwrap(), raised);
    }

    fn alive(pid: libc::pid_t) -> bool {
        // SAFETY: 信号 0 不发信号，只检查进程在不在。
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// 从没开过闸的 `Pty` 丢掉时读写线程都已结束、放开了 master 和 `Notifier`（丢完就没有别的
    /// 引用，描述符随之关掉），shell 不受影响。
    #[test]
    fn dropping_a_paused_pty_closes_its_descriptors_and_spares_the_shell() {
        let size = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
        let old = Pty::spawn(size, Some("/bin/sh"), None, IntegrationMode::Off, Box::new(|_| true)).unwrap();
        let pid = old.shell_pid().unwrap();
        let handoff = PtyHandoff {
            master: old.dup_master().unwrap(),
            pid: u32::try_from(pid).unwrap(),
            size,
            report_token: None,
            pending_input: b"exit\n".to_vec(),
        };
        let paused = Pty::adopt_paused(handoff, Box::new(|_| true)).unwrap();
        // 别处还留着一个 `PtyWriter` 的克隆，写线程照样结束。
        let writer = paused.writer.clone();
        let master = Arc::downgrade(paused.master.as_ref().unwrap());
        let notifier = Arc::downgrade(&paused.notifier);
        drop(paused);
        assert!(master.upgrade().is_none(), "the paused pty's master must be closed");
        assert!(notifier.upgrade().is_none(), "the paused pty's notifier must be closed");
        writer.write(b"exit\n");
        // 结束接手来的 shell 时等 SIGHUP 的时限过了也还活着。
        thread::sleep(Duration::from_millis(500));
        assert!(alive(pid), "dropping a paused pty must not end the shell");
        drop(old);
    }

    /// `PtyHandoff` 的 `Debug` 不打出口令。
    #[test]
    fn handoff_debug_hides_the_report_token() {
        let (rx, _tx) = std::io::pipe().unwrap();
        let handoff = PtyHandoff {
            master: rx.into(),
            pid: 1,
            size: GridSize { cols: 1, rows: 1, cell_width_px: 1, cell_height_px: 1 },
            report_token: Some("secret-token".into()),
            pending_input: Vec::new(),
        };
        let text = format!("{handoff:?}");
        assert!(!text.contains("secret-token"), "{text}");
        assert!(text.contains("redacted"), "{text}");
    }
}
