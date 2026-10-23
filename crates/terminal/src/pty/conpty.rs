//! Windows 上的伪终端：ConPTY，由 portable-pty 打开和启动 shell。读写各一个线程，都阻塞着读写
//! 管道；不能交接，宿主升级时会话跟着重启。
//!
//! shell 退出后 ConPTY 不会自己关掉输出管道，所以另起一个线程等 shell 退出，退出后关掉伪控制台
//! （`ClosePseudoConsole`），读线程读完剩下的输出读到 EOF，报告 `PtyEvent::Exited`。关伪控制台
//! 要等输出管道里的读完，读线程一直在读，所以不会卡住。

mod process;

use std::{
    cell::Cell,
    ffi::OsString,
    io::{ErrorKind, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use anyhow::{Context as _, Result, bail};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, SlavePty, native_pty_system};
use runode_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};

pub use process::process_args;
pub(crate) use process::{group_members, process_name};

use super::{PtyEvent, PtySink, PtyWriter, WriterMsg, pty_size, set_current_thread_interactive};
use crate::shell_integration;

/// 伪控制台，shell 退出后由等它的线程取走关掉；之后改尺寸什么都不做。
type SharedMaster = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;

/// 没指定 shell 时 `Pty::start` 起的那个：PATH 里有 PowerShell 7 时用 `pwsh.exe`，否则用系统自带的
/// `powershell.exe`。
pub fn default_shell() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|dir| dir.join("pwsh.exe").is_file()) {
        "pwsh.exe".into()
    } else {
        "powershell.exe".into()
    }
}

pub struct Pty {
    master: SharedMaster,
    /// 还没启动 shell 时的从设备和读线程要交给的 `PtySink`，`start` 时交出去。
    pending: Option<(Box<dyn SlavePty + Send>, PtySink)>,
    /// shell 的进程号和结束它用的句柄；还没启动时为 `None`。
    shell: Option<(u32, Box<dyn ChildKiller + Send + Sync>)>,
    pub writer: PtyWriter,
    /// 启动 shell 时交给集成脚本的报告口令，见 `shell_integration::prepare`。
    report_token: Option<String>,
    /// 交口令给集成脚本的文件，脚本读完自己删；shell 没读就退出时由 `Drop` 删。
    report_token_file: Option<PathBuf>,
    /// 启动 shell 时另外设的环境变量，见 `set_env`。
    env: Vec<(OsString, OsString)>,
    /// 启动 shell 时告诉集成脚本开哪些功能，见 `set_shell_features`。
    shell_features: String,
    /// 最近一次设给 PTY 的尺寸。
    size: Cell<GridSize>,
}

impl PtyWriter {
    /// 起写线程，把排进队列的数据按顺序写进 `pipe`。所有发送端都没了、写出错或者收到
    /// `WriterMsg::Finish` 时结束；伪控制台关掉以后写会出错，卡在写上的线程也随之结束。
    fn start(mut pipe: Box<dyn Write + Send>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<WriterMsg>();
        let queued = Arc::new(AtomicUsize::new(0));
        thread::Builder::new()
            .name("pty-writer".into())
            .spawn({
                let queued = queued.clone();
                move || {
                    set_current_thread_interactive();
                    while let Ok(message) = rx.recv() {
                        let data = match message {
                            WriterMsg::Data(data) => data,
                            // 不交接，没有要交回去的输入。
                            WriterMsg::Finish(done) => {
                                let _ = done.send(Vec::new());
                                return;
                            }
                        };
                        let written = pipe.write_all(&data).and_then(|()| pipe.flush());
                        queued.fetch_sub(data.len(), Ordering::Relaxed);
                        if let Err(err) = written {
                            tracing::warn!("pty write failed: {err}");
                            return;
                        }
                    }
                }
            })
            .context("failed to start pty writer thread")?;
        Ok(Self(tx, queued))
    }

    /// 叫写线程结束、不等它，排在后面的都丢掉。
    fn close(&self) {
        let (done, _) = mpsc::channel();
        let _ = self.0.send(WriterMsg::Finish(done));
    }
}

impl Pty {
    /// 在 `cwd` 下启动 `shell`（为 `None` 时用 `default_shell`），按 `integration` 注入 shell 集成，
    /// 并启动读线程，输出交给 `sink`。
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
        let pair = native_pty_system().openpty(pty_size(size)).context("openpty failed")?;
        let writer = PtyWriter::start(pair.master.take_writer().context("failed to take the pty writer")?)?;
        Ok(Self {
            master: Arc::new(Mutex::new(Some(pair.master))),
            pending: Some((pair.slave, sink)),
            shell: None,
            writer,
            report_token: None,
            report_token_file: None,
            env: Vec::new(),
            shell_features: shell_integration::features(&TermSettings::default()),
            size: Cell::new(size),
        })
    }

    /// 启动 shell 时给它设这个环境变量，盖过从 app 继承来的同名变量；已经启动了的不受影响。
    pub fn set_env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.env.push((key.into(), value.into()));
    }

    /// 启动 shell 时按 `settings` 告诉集成脚本开哪些功能，见 `shell_integration::features`。
    pub fn set_shell_features(&mut self, settings: &TermSettings) {
        self.shell_features = shell_integration::features(settings);
    }

    pub fn started(&self) -> bool {
        self.pending.is_none()
    }

    /// shell 报告 PATH 等信息时要带的口令，见 `shell_integration::prepare`。
    pub fn report_token(&self) -> Option<&str> {
        self.report_token.as_deref()
    }

    /// 在 `cwd` 下启动 shell 和读线程，参数同 `spawn`。已经启动过时什么都不做。shell 起不来时
    /// 什么都没动，还算没启动，可以换个参数再来。
    pub fn start(
        &mut self,
        shell: Option<&str>,
        cwd: Option<&std::path::Path>,
        integration: IntegrationMode,
    ) -> Result<()> {
        let Some((slave, _)) = &self.pending else {
            return Ok(());
        };
        let reader = {
            let master = self.master.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(master) = master.as_ref() else {
                bail!("the pseudo console has been closed");
            };
            master.try_clone_reader().context("failed to clone the pty reader")?
        };
        let shell = shell.map_or_else(default_shell, str::to_owned);
        let mut cmd = CommandBuilder::new(&shell);
        let dirs = runode_paths::Dirs::from_env();
        let report = shell_integration::prepare(&dirs, integration, &shell, &self.shell_features, &mut cmd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "runode");
        cmd.env("TERM_PROGRAM_VERSION", crate::VERSION);
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        if let Some(cwd) = cwd.map(Into::into).or(dirs.home) {
            cmd.cwd::<PathBuf>(cwd);
        }
        handle_ctrl_c();
        let mut child = match slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(err) => {
                if let Some((_, file)) = &report {
                    let _ = std::fs::remove_file(file);
                }
                return Err(err.context("failed to spawn shell"));
            }
        };
        (self.report_token, self.report_token_file) = report.unzip();
        let (slave, sink) = self.pending.take().expect("the pending slave was checked above");
        // 从设备也拿着伪控制台，丢掉它，shell 退出时关掉主设备那份才真的关掉伪控制台。
        drop(slave);
        let mut killer = child.clone_killer();
        let pid = child.process_id().unwrap_or_default();
        let master = self.master.clone();
        let waited = thread::Builder::new().name("pty-waiter".into()).spawn(move || {
            let _ = child.wait();
            drop(master.lock().unwrap_or_else(PoisonError::into_inner).take());
        });
        if let Err(err) = waited {
            let _ = killer.kill();
            return Err(anyhow::Error::new(err).context("failed to start the pty waiter thread"));
        }
        self.shell = Some((pid, killer));
        // 读线程起不来时没人读 PTY，会话随即报告结束；shell 当场结束。
        if let Err(err) = thread::Builder::new().name("pty-reader".into()).spawn(move || read_loop(reader, sink)) {
            if let Some((_, mut killer)) = self.shell.take() {
                let _ = killer.kill();
            }
            return Err(anyhow::Error::new(err).context("failed to start pty reader thread"));
        }
        Ok(())
    }

    /// 改 PTY 的尺寸；伪控制台关掉以后什么都不做。
    pub fn resize(&self, size: GridSize) {
        let master = self.master.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(master) = master.as_ref() else {
            return;
        };
        match master.resize(pty_size(size)) {
            Ok(()) => self.size.set(size),
            Err(err) => tracing::warn!("pty resize failed: {err}"),
        }
    }

    /// shell 自己当前所在的目录（不管前台在跑什么）。
    pub fn shell_cwd(&self) -> Option<PathBuf> {
        process::process_cwd(self.shell_pid()?)
    }

    /// 前台是不是 shell 自己，也就是没有在跑别的程序。取不到时当作不是。
    pub fn foreground_is_shell(&self) -> bool {
        self.foreground().is_some_and(|(_, is_shell)| is_shell)
    }

    /// 「前台」的进程和它是不是 shell 自己。Windows 没有终端的前台进程组，取 shell 最新起的那个
    /// 子进程；shell 没有子进程时是 shell 自己。
    pub(crate) fn foreground(&self) -> Option<(u32, bool)> {
        let shell = self.shell_pid()?;
        Some(process::newest_child(shell).map_or((shell, true), |child| (child, false)))
    }

    /// shell 的进程号；还没启动时为 `None`。
    pub(crate) fn shell_pid(&self) -> Option<u32> {
        self.shell.as_ref().map(|(pid, _)| *pid).filter(|&pid| pid != 0)
    }
}

/// 让之后起的 shell 照常处理 Ctrl+C。「忽略 Ctrl+C」随进程遗传：拉起本进程的一方用了
/// `CREATE_NEW_PROCESS_GROUP`（比如 sshd）时本进程带着它，起的 shell 也带着，在终端里按 Ctrl+C
/// 停不下程序。
fn handle_ctrl_c() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: 不登记处理函数，只改本进程处理 Ctrl+C 的标志。
        if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) } == 0 {
            tracing::warn!("cannot let shells handle Ctrl+C: {}", std::io::Error::last_os_error());
        }
    });
}

fn read_loop(mut pipe: Box<dyn Read + Send>, mut sink: PtySink) {
    set_current_thread_interactive();
    let mut buf = vec![0u8; 64 * 1024];
    // 收的一方不要了也读到 EOF：没人读输出管道时，关伪控制台会一直等下去。
    let mut wanted = true;
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if wanted {
                    wanted = sink(PtyEvent::Output(buf[..n].into()));
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => {
                // 伪控制台关掉后读输出管道得到 `ERROR_BROKEN_PIPE`，和 EOF 一样。
                tracing::debug!("pty read ended: {err}");
                break;
            }
        }
    }
    if wanted {
        sink(PtyEvent::Exited);
    }
}

impl Drop for Pty {
    /// 关窗口即结束会话：结束 shell，等它的线程随即关掉伪控制台，还连着它的程序收到关闭事件。
    /// 还没启动的在另一个线程里关伪控制台，关的时候要等它的输出读完，不能卡住调用方。
    fn drop(&mut self) {
        if let Some(file) = self.report_token_file.take() {
            let _ = std::fs::remove_file(file);
        }
        match self.shell.take() {
            Some((_, mut killer)) => {
                let _ = killer.kill();
            }
            None => {
                let master = self.master.lock().unwrap_or_else(PoisonError::into_inner).take();
                let pending = self.pending.take();
                let _ = thread::Builder::new().name("pty-closer".into()).spawn(move || drop((master, pending)));
            }
        }
        self.writer.close();
    }
}
