//! The child shell and its pseudo-terminal.
//!
//! A reader thread forwards PTY output over a channel, because the VT state it
//! feeds (`Terminal`) is single-threaded and lives on the UI thread. Writes
//! (keystrokes and VT query replies) go straight to the PTY from any thread
//! through a shared writer.

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// What the reader thread reports to the UI thread.
pub enum PtyEvent {
    Output(Vec<u8>),
    /// The PTY reached EOF or failed: the child is gone.
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
    /// Spawns `shell` (the user's `$SHELL` when `None`) as a login shell in
    /// `cwd` and starts the reader thread.
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
        // A login shell, like Terminal.app and Ghostty, so the user's profile runs.
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
        // The child holds its own copy of the slave; ours must close so EOF arrives on exit.
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
        // Closing the window ends the session; the shell gets SIGHUP from the
        // closed master anyway, kill() just makes it prompt.
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
