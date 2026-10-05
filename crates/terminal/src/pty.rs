//! 子 shell 及其伪终端。
//!
//! 读线程把 PTY 输出交给创建时给的 `PtySink`，由宿主那边的会话线程接着处理。写入（按键、
//! 粘贴和 VT 查询的回复）经 `PtyWriter` 排进这个伪终端自己的写队列，由写线程按顺序写出，
//! 所以写入方从不阻塞：给一个不读 stdin 的程序粘贴一大段时，等着的只有写线程。

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::{Context as _, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};
use runode_agent_detect::{ForegroundJob, ForegroundProcess};
use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

use crate::shell_integration;

/// 读线程交给 `PtySink` 的事件。
pub enum PtyEvent {
    Output(Arc<[u8]>),
    /// PTY 读到 EOF 或出错：子进程已经退出。
    Exited,
}

/// 收 PTY 输出的一方，在读线程里调用。返回 false 表示不再要了，读线程随之结束。可以阻塞，
/// 用来给读线程限流：收的一方积压太多时让它先别读。
pub type PtySink = Box<dyn FnMut(PtyEvent) -> bool + Send>;

/// 往 PTY 写的一端：只把数据排进写队列，不等它写出去。
#[derive(Clone)]
pub struct PtyWriter(mpsc::Sender<Vec<u8>>);

impl PtyWriter {
    pub fn write(&self, data: &[u8]) {
        if !data.is_empty() {
            self.send(data.to_vec());
        }
    }

    /// 同 `write`，数据已经在自己的缓冲里时不必再复制一份。
    pub fn send(&self, data: Vec<u8>) {
        // 写线程已经结束（PTY 出错或关了）时没有可写的地方，丢掉。
        if !data.is_empty() && self.0.send(data).is_err() {
            tracing::debug!("pty writer is gone, input dropped");
        }
    }

    /// 起写线程，把排进队列的数据按顺序写进 `writer`。所有发送端都没了、或者写出错时结束。
    fn start(mut writer: Box<dyn Write + Send>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                set_current_thread_interactive();
                for data in rx {
                    if let Err(err) = writer.write_all(&data).and_then(|()| writer.flush()) {
                        tracing::warn!("pty write failed: {err}");
                        return;
                    }
                }
            })
            .context("failed to start pty writer thread")?;
        Ok(Self(tx))
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

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    /// 还没启动 shell 时的从设备和读线程要交给的 `PtySink`，`start` 时交出去。
    pending: Option<(Box<dyn SlavePty + Send>, PtySink)>,
    /// `Drop` 里交给回收线程，所以是 `Option`。
    child: Option<Box<dyn Child + Send + Sync>>,
    pub writer: PtyWriter,
    /// 启动 shell 时交给集成脚本的报告口令，见 `shell_integration::prepare`；没注入集成或者
    /// 还没启动时为 `None`。
    report_token: Option<String>,
}

fn pty_size(size: GridSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: size.cols.saturating_mul(size.cell_width_px),
        pixel_height: size.rows.saturating_mul(size.cell_height_px),
    }
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
        let writer = PtyWriter::start(pair.master.take_writer().context("pty writer")?)?;
        Ok(Self { master: pair.master, pending: Some((pair.slave, sink)), child: None, writer, report_token: None })
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
        let shell = shell
            .map(str::to_owned)
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/zsh".into());
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
        let cwd = cwd.map(Into::into).or_else(|| runode_paths::Dirs::from_env().home);
        if let Some(cwd) = cwd {
            cmd.cwd::<std::path::PathBuf>(cwd);
        }

        let child = slave.spawn_command(cmd).context("failed to spawn shell")?;
        self.child = Some(child);
        self.report_token = report_token;
        // 子进程持有自己的 slave 副本；我们这份必须关掉，子进程退出时才会读到 EOF。
        drop(slave);

        let reader = self.master.try_clone_reader().context("pty reader")?;
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || read_loop(reader, sink))
            .context("failed to start pty reader thread")?;
        Ok(())
    }

    pub fn resize(&self, size: GridSize) {
        if let Err(err) = self.master.resize(pty_size(size)) {
            tracing::warn!("pty resize failed: {err}");
        }
    }

    /// 程序没设置标题时显示的名字：前台是 shell 自己时为 `shell_dir` 给出的目录的名字（家目录为 `~`），
    /// 前台在跑别的程序时为该程序的进程名。取不到时为 `None`。
    pub fn foreground_title(&self, shell_dir: impl FnOnce() -> Option<PathBuf>) -> Option<String> {
        let (leader, is_shell) = self.foreground()?;
        if is_shell {
            shell_dir().map(|cwd| dir_label(&cwd))
        } else {
            process_name(leader)
        }
    }

    /// shell 自己当前所在的目录（不管前台在跑什么）。
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        let pid = self.child.as_ref()?.process_id()?;
        process_cwd(libc::pid_t::try_from(pid).ok()?)
    }

    /// 前台是不是 shell 自己，也就是没有在跑别的程序。取不到时当作不是。
    pub fn foreground_is_shell(&self) -> bool {
        self.foreground().is_some_and(|(_, is_shell)| is_shell)
    }

    /// 终端前台进程组的组长，以及它是不是 shell 自己。
    pub(crate) fn foreground(&self) -> Option<(libc::pid_t, bool)> {
        let leader = self.master.process_group_leader()?;
        let shell = self.child.as_ref()?.process_id()?;
        Some((leader, u32::try_from(leader).ok() == Some(shell)))
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
    let written = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), size)
    };
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
        // 关窗口即结束会话；master 关闭后 shell 本来也会收到 SIGHUP，kill() 只是让它立即退出。
        // 子进程退出后还得 wait 才会被系统回收，否则每关一个标签就留一个僵尸进程；
        // shell 可能忽略 SIGHUP，所以放到单独的线程里等，不阻塞界面。
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = thread::Builder::new()
                .name("pty-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, mut sink: PtySink) {
    set_current_thread_interactive();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if !sink(PtyEvent::Output(buf[..n].into())) {
                    return;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::debug!("pty read ended: {err}");
                break;
            }
        }
    }
    sink(PtyEvent::Exited);
}
