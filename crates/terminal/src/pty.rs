//! 子 shell 及其伪终端。
//!
//! 读线程把 PTY 输出交给创建时给的 `PtySink`，由宿主那边的会话线程接着处理。写入（按键、
//! 粘贴和 VT 查询的回复）经 `PtyWriter` 排进这个伪终端自己的写队列，由写线程按顺序写出，
//! 所以写入方从不阻塞：给一个不读 stdin 的程序粘贴一大段时，等着的只有写线程。
//!
//! 宿主升级时会话要交给新宿主，shell 不中断：`Pty::release` 交出 PTY master 的描述符、shell 的
//! pid 和重建要的其余状态（`PtyHandoff`），不结束 shell；对面用 `Pty::adopt` 接上。所以 master
//! 的描述符由这里自己持有：伪终端和 shell 仍由 portable-pty 打开和启动，打开后就复制一份 master
//! 的描述符，读、写、改尺寸、读前台进程组都用自己这份，portable-pty 的 master 随即关掉。

mod exit_watch;
mod reader;

use std::{
    cell::Cell,
    ffi::OsString,
    fs::File,
    io::Write,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use portable_pty::{Child, CommandBuilder, PtySize, SlavePty, native_pty_system};
use runode_agent_detect::{ForegroundJob, ForegroundProcess};
use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

use crate::shell_integration;
use exit_watch::ExitWatch;
use reader::Reader;

/// `Pty::release` 等写线程把排着的输入写完的最长时间。程序不读 stdin、写线程卡住时不再等，
/// 卡住的那些等它读了照样写出去，可能和接手一方写的交错。
const WRITER_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// 前面的都写完后回个话，然后写线程结束，见 `Pty::release`。
    Finish(mpsc::Sender<()>),
}

/// 往 PTY 写的一端：只把数据排进写队列，不等它写出去。
#[derive(Clone)]
pub struct PtyWriter(mpsc::Sender<WriterMsg>);

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
    /// `WriterMsg::Finish` 时结束。
    fn start(master: OwnedFd) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<WriterMsg>();
        let mut master = File::from(master);
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                set_current_thread_interactive();
                for message in rx {
                    match message {
                        WriterMsg::Data(data) => {
                            if let Err(err) = master.write_all(&data) {
                                tracing::warn!("pty write failed: {err}");
                                return;
                            }
                        }
                        WriterMsg::Finish(done) => {
                            drop(master);
                            let _ = done.send(());
                            return;
                        }
                    }
                }
            })
            .context("failed to start pty writer thread")?;
        Ok(Self(tx))
    }

    /// 让写线程写完已经排着的输入后结束，最多等 `timeout`；之后再写的都丢掉。返回写完了没有。
    fn finish(&self, timeout: Duration) -> bool {
        let (done, finished) = mpsc::channel();
        if self.0.send(WriterMsg::Finish(done)).is_err() {
            // 写线程已经结束了。
            return true;
        }
        match finished.recv_timeout(timeout) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => true,
            Err(mpsc::RecvTimeoutError::Timeout) => false,
        }
    }
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

/// 交出去的会话：在另一个进程（或者同一个进程的别处）用 `Pty::adopt` 重建。`master` 要经
/// Unix socket 的 `SCM_RIGHTS` 传过去，其余字段由调用方自己编码。丢掉它会关掉这份描述符；
/// 交出方和接手方都关了以后，shell 收到 SIGHUP。
#[derive(Debug)]
pub struct PtyHandoff {
    /// PTY master 的描述符，带 `FD_CLOEXEC`。
    pub master: OwnedFd,
    /// shell 的进程号。它是交出方的子进程，接手方不是它的父进程。
    pub pid: u32,
    /// PTY 现在的尺寸。内核里的尺寸随描述符一起留着，这里只是给接手方记账用。
    pub size: GridSize,
    /// 启动 shell 时交给集成脚本的报告口令，见 `Pty::report_token`。
    pub report_token: Option<String>,
}

/// PTY 上跑着的 shell。
enum Shell {
    /// 自己启动的子进程，退出时自己回收。
    Child(Box<dyn Child + Send + Sync>),
    /// 接手来的：不是自己的子进程，只能看着它退出，见 `ExitWatch`。
    Adopted(Arc<ExitWatch>),
}

pub struct Pty {
    /// PTY master 的描述符。读线程和写线程各用一份复制的。
    master: OwnedFd,
    /// 还没启动 shell 时的从设备和读线程要交给的 `PtySink`，`start` 时交出去。
    pending: Option<(Box<dyn SlavePty + Send>, PtySink)>,
    /// `Drop` 和 `release` 里取走，所以是 `Option`。
    shell: Option<Shell>,
    /// 读线程；还没启动 shell 时为 `None`。
    reader: Option<Reader>,
    pub writer: PtyWriter,
    /// 启动 shell 时交给集成脚本的报告口令，见 `shell_integration::prepare`；没注入集成或者
    /// 还没启动时为 `None`。
    report_token: Option<String>,
    /// 启动 shell 时另外设的环境变量，见 `set_env`。
    env: Vec<(OsString, OsString)>,
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

/// 终端的前台进程组号；没有前台进程组或者读不到时为 `None`。
fn foreground_group(master: BorrowedFd<'_>) -> Option<libc::pid_t> {
    // SAFETY: `master` 在这次调用期间一直开着。
    let group = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    (group > 0).then_some(group)
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
        let writer = PtyWriter::start(master.try_clone().context("pty writer")?)?;
        Ok(Self {
            master,
            pending: Some((pair.slave, sink)),
            shell: None,
            reader: None,
            writer,
            report_token: None,
            env: Vec::new(),
            size: Cell::new(size),
        })
    }

    /// 启动 shell 时给它设这个环境变量，盖过从 app 继承来的同名变量；已经启动了的不受影响。
    pub fn set_env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.env.push((key.into(), value.into()));
    }

    /// 接手 `Pty::release` 交出来的会话：用交来的 master 起读写线程，输出交给 `sink`。shell 不是
    /// 本进程的子进程，不回收它也拿不到退出码；它退出时即使 PTY 还没读到 EOF（比如后台程序还
    /// 开着终端），读完剩下的输出也报告 `PtyEvent::Exited`。丢掉返回的 `Pty` 会结束 shell 的
    /// 进程组，见 `Drop`。
    pub fn adopt(handoff: PtyHandoff, sink: PtySink) -> Result<Self> {
        let PtyHandoff { master, pid, size, report_token } = handoff;
        let pid = libc::pid_t::try_from(pid).ok().filter(|&pid| pid > 0).context("invalid shell pid")?;
        let exit = Arc::new(ExitWatch::new(pid));
        let writer = PtyWriter::start(master.try_clone().context("pty writer")?)?;
        let reader = Reader::start(master.try_clone().context("pty reader")?, Some(exit.clone()), sink)?;
        Ok(Self {
            master,
            pending: None,
            shell: Some(Shell::Adopted(exit)),
            reader: Some(reader),
            writer,
            report_token,
            // 接手来的 shell 早就启动了，没有要设的环境变量。
            env: Vec::new(),
            size: Cell::new(size),
        })
    }

    /// 交接的第一步（可选）：让读线程停下，之后不再从 PTY 读，没读的输出留给接手方。读线程已经
    /// 读出来的那块照样交给 `PtySink`，交完就结束，不报告 `PtyEvent::Exited`；`reader_finished`
    /// 为 true 后，`PtySink` 不会再收到东西。不等读线程结束。
    pub fn stop_reading(&mut self) {
        if let Some(reader) = &mut self.reader {
            reader.stop();
        }
    }

    /// 读线程已经结束（或者还没启动 shell，根本没有读线程）。
    pub fn reader_finished(&self) -> bool {
        self.reader.as_ref().is_none_or(Reader::finished)
    }

    /// 交出会话，不结束 shell：停下读线程并等它结束（同 `stop_reading`），让写线程写完排着的输入
    /// （最多等 `WRITER_FLUSH_TIMEOUT`），然后交出复制的一份 master 描述符、shell 的 pid、尺寸和
    /// 报告口令。交出后这个 `Pty` 不再管这个会话：经它的 `writer` 写的都丢掉，读不到前台进程，
    /// 丢掉它也不结束 shell，只关掉自己那份描述符。出错时（还没启动 shell、复制描述符失败）什么
    /// 都没动，`Pty` 照常可用。
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
        let pid = self.shell_pid().context("the shell's pid is unknown")?;
        let pid = u32::try_from(pid).context("invalid shell pid")?;
        // 先做会失败的事，失败时什么都没动。
        let master = self.master.try_clone().context("failed to dup the pty master")?;
        if let Some(reader) = &mut self.reader {
            reader.stop();
            reader.join();
        }
        if !self.writer.finish(WRITER_FLUSH_TIMEOUT) {
            tracing::warn!("the pty writer is stuck, handing off with input still queued");
        }
        if let Some(Shell::Child(mut child)) = self.shell.take() {
            let reaped = thread::Builder::new().name("pty-reaper".into()).spawn(move || {
                let _ = child.wait();
            });
            if let Err(err) = reaped {
                tracing::warn!("failed to start the pty reaper thread: {err}");
            }
        }
        Ok(PtyHandoff { master, pid, size: self.size.get(), report_token: self.report_token.clone() })
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
        let Some((slave, sink)) = self.pending.take() else {
            return Ok(());
        };
        let shell =
            shell.map(str::to_owned).or_else(|| std::env::var("SHELL").ok()).unwrap_or_else(|| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(&shell);
        // 用登录 shell，这样会执行用户的 profile。
        let report_token = shell_integration::prepare(integration, &shell, &mut cmd);
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

        let master = self.master.try_clone().context("pty reader")?;
        self.reader = Some(Reader::start(master, None, sink)?);
        Ok(())
    }

    pub fn resize(&self, size: GridSize) {
        match set_winsize(self.master.as_fd(), size) {
            Ok(()) => self.size.set(size),
            Err(err) => tracing::warn!("pty resize failed: {err}"),
        }
    }

    /// 程序没设置标题时显示的名字：前台是 shell 自己时为 `shell_dir` 给出的目录的名字（家目录为 `~`），
    /// 前台在跑别的程序时为该程序的进程名。取不到时为 `None`。
    pub fn foreground_title(&self, shell_dir: impl FnOnce() -> Option<PathBuf>) -> Option<String> {
        let (leader, is_shell) = self.foreground()?;
        if is_shell { shell_dir().map(|cwd| dir_label(&cwd)) } else { process_name(leader) }
    }

    /// shell 自己当前所在的目录（不管前台在跑什么）。
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        process_cwd(self.shell_pid()?)
    }

    /// 前台是不是 shell 自己，也就是没有在跑别的程序。取不到时当作不是。
    pub fn foreground_is_shell(&self) -> bool {
        self.foreground().is_some_and(|(_, is_shell)| is_shell)
    }

    /// 终端前台进程组的组长，以及它是不是 shell 自己。
    pub(crate) fn foreground(&self) -> Option<(libc::pid_t, bool)> {
        let leader = foreground_group(self.master.as_fd())?;
        let shell = self.shell_pid()?;
        Some((leader, leader == shell))
    }

    /// shell 的进程号；还没启动时为 `None`。
    fn shell_pid(&self) -> Option<libc::pid_t> {
        match self.shell.as_ref()? {
            Shell::Child(child) => libc::pid_t::try_from(child.process_id()?).ok(),
            Shell::Adopted(exit) => Some(exit.pid()),
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
fn process_name(pid: libc::pid_t) -> Option<String> {
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
fn process_name(_pid: libc::pid_t) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn process_cwd(_pid: libc::pid_t) -> Option<PathBuf> {
    None
}

impl Drop for Pty {
    fn drop(&mut self) {
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
            // 接手来的 shell 不是自己的子进程，不能 wait，也不归这边回收；给它的进程组和前台
            // 进程组发 SIGHUP，不退出再 SIGKILL。
            Some(Shell::Adopted(exit)) => exit.terminate(foreground_group(self.master.as_fd())),
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
        let got = winsize(pty.master.as_fd());
        assert_eq!((got.ws_col, got.ws_row), (20, 4));
        pty.resize(GridSize { cols: 100, rows: 40, ..size });
        let got = winsize(pty.master.as_fd());
        assert_eq!((got.ws_col, got.ws_row, got.ws_xpixel, got.ws_ypixel), (100, 40, 800, 640));
    }
}
