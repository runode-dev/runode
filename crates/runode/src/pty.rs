//! 子 shell 及其伪终端。
//!
//! 读线程通过 channel 转发 PTY 输出，因为接收输出的 VT 状态（`Terminal`）
//! 只能单线程使用，放在 UI 线程上。写入（按键和 VT 查询的回复）可以从任意线程
//! 经共享的 writer 直接写进 PTY。

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// 读线程报告给 UI 线程的事件。
pub enum PtyEvent {
    Output(Vec<u8>),
    /// PTY 读到 EOF 或出错：子进程已经退出。
    Exited,
}

#[derive(Clone)]
pub struct PtyWriter(Arc<Mutex<Box<dyn Write + Send>>>);

impl PtyWriter {
    pub fn write(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut writer = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = writer.write_all(data).and_then(|()| writer.flush()) {
            tracing::warn!("pty write failed: {err}");
        }
    }
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    /// `Drop` 里交给回收线程，所以是 `Option`。
    child: Option<Box<dyn Child + Send + Sync>>,
    pub writer: PtyWriter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

impl GridSize {
    fn pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.cols.saturating_mul(self.cell_width_px),
            pixel_height: self.rows.saturating_mul(self.cell_height_px),
        }
    }
}

impl Pty {
    /// 在 `cwd` 下以登录 shell 方式启动 `shell`（为 `None` 时用用户的 `$SHELL`），
    /// 并启动读线程。
    pub fn spawn(
        size: GridSize,
        shell: Option<&str>,
        cwd: Option<&std::path::Path>,
    ) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        let pair = native_pty_system()
            .openpty(size.pty_size())
            .context("openpty failed")?;

        let shell = shell
            .map(str::to_owned)
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/zsh".into());
        let mut cmd = CommandBuilder::new(&shell);
        // 用登录 shell，这样会执行用户的 profile。
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "runode");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // macOS 自带的 BSD ls 只在设置了 CLICOLOR 时才着色；用户已有设置就不覆盖。
        if std::env::var_os("CLICOLOR").is_none() {
            cmd.env("CLICOLOR", "1");
        }
        let cwd = cwd
            .map(Into::into)
            .or_else(|| std::env::var_os("HOME").map(Into::into));
        if let Some(cwd) = cwd {
            cmd.cwd::<std::path::PathBuf>(cwd);
        }

        let child = pair.slave.spawn_command(cmd).context("failed to spawn shell")?;
        // 子进程持有自己的 slave 副本；我们这份必须关掉，子进程退出时才会读到 EOF。
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().context("pty reader")?;
        let writer = pair.master.take_writer().context("pty writer")?;
        let (tx, rx) = unbounded();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || read_loop(reader, tx))
            .context("failed to start pty reader thread")?;

        Ok((
            Self {
                master: pair.master,
                child: Some(child),
                writer: PtyWriter(Arc::new(Mutex::new(writer))),
            },
            rx,
        ))
    }

    pub fn resize(&self, size: GridSize) {
        if let Err(err) = self.master.resize(size.pty_size()) {
            tracing::warn!("pty resize failed: {err}");
        }
    }

    /// 程序没设置标题时显示的名字：前台是 shell 自己时为它当前目录的名字（家目录为 `~`），
    /// 前台在跑别的程序时为该程序的进程名。取不到时为 `None`。
    pub fn foreground_title(&self) -> Option<String> {
        let leader = self.master.process_group_leader()?;
        let shell = self.child.as_ref()?.process_id()?;
        if u32::try_from(leader).ok() == Some(shell) {
            process_cwd(leader).map(|cwd| dir_label(&cwd))
        } else {
            process_name(leader)
        }
    }
}

fn dir_label(path: &Path) -> String {
    if std::env::var_os("HOME").is_some_and(|home| Path::new(&home) == path) {
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

fn read_loop(mut reader: Box<dyn Read + Send>, tx: UnboundedSender<PtyEvent>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.unbounded_send(PtyEvent::Output(buf[..n].to_vec())).is_err() {
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
    let _ = tx.unbounded_send(PtyEvent::Exited);
}
