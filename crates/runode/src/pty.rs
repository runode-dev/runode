//! 子 shell 及其伪终端。
//!
//! 读线程通过 channel 转发 PTY 输出，因为接收输出的 VT 状态（`Terminal`）
//! 只能单线程使用，放在 UI 线程上。写入（按键和 VT 查询的回复）可以从任意线程
//! 经共享的 writer 直接写进 PTY。

use std::{
    io::{Read, Write},
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
    child: Box<dyn Child + Send + Sync>,
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
        // 和 Terminal.app、Ghostty 一样用登录 shell，这样会执行用户的 profile。
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "runode");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
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
                child,
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
}

impl Drop for Pty {
    fn drop(&mut self) {
        // 关窗口即结束会话；master 关闭后 shell 本来也会收到 SIGHUP，kill() 只是让它立即退出。
        let _ = self.child.kill();
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
