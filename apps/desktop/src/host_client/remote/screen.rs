//! 看一台 SSH 主机的远程桌面：经主连接起那边的 `runode --ssh-screen`，ssh 的 stdin、stdout 上说
//! `runode_protocol::screen`，和手机经远程访问说的一样。主机连上以后才起：远端已经装好这个构建；没有
//! 主连接（Windows）时这条 ssh 自己认证，要问用户时经 askpass 问。

use std::{
    io::{BufReader, BufWriter, Write as _},
    process::{Child, Stdio},
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread,
};

use runode_protocol::{
    Frame, FrameKind, read_frame,
    screen::{SCREEN_VERSION, ScreenClientMsg, ScreenHostMsg, decode_video},
    write_frame,
};
use runode_types::screen::VideoFrame;

use super::{RemoteHost, Status, deploy, ssh};

/// 远端交来的东西，在读线程上交出。
pub enum ScreenEvent {
    Message(ScreenHostMsg),
    Video(VideoFrame),
    /// 连接断了（ssh 退出、远端断开或者发来读不懂的帧），之后不会再有事件。
    Closed,
}

/// 往远端发消息的一头，可以复制给别的线程用；连接断了以后发的消息丢掉。
#[derive(Clone)]
pub struct ScreenSender(mpsc::Sender<ScreenClientMsg>);

impl ScreenSender {
    pub fn send(&self, message: ScreenClientMsg) {
        let _ = self.0.send(message);
    }
}

/// 一条远程桌面的连接。丢掉时结束远端的桥接和 ssh。
pub struct ScreenLink {
    sender: ScreenSender,
    child: Arc<Mutex<Option<Child>>>,
}

impl ScreenLink {
    pub fn sender(&self) -> ScreenSender {
        self.sender.clone()
    }
}

impl Drop for ScreenLink {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// 连上 `host` 的远程桌面，先发 `ScreenHello`；远端交来的消息和画面经 `on_event` 在读线程上交出，最后
/// 一个是 `ScreenEvent::Closed`，同时交给它往远端发消息的一头（解不了的画面当场要关键帧）。会阻塞着起
/// ssh，在后台线程上调。
pub fn open(
    host: &RemoteHost,
    mut on_event: impl FnMut(ScreenEvent, &ScreenSender) + Send + 'static,
) -> Result<ScreenLink, String> {
    if !matches!(host.status(), Status::Connected(_)) {
        return Err(format!("not connected to {}", host.name));
    }
    let script = match host.family() {
        runode_ssh::bootstrap::Family::Unix => runode_ssh::bootstrap::screen_command(&deploy::install_dir()),
        runode_ssh::bootstrap::Family::Windows => {
            runode_ssh::bootstrap::windows::screen_command(&deploy::install_dir())
        }
    };
    let mut command = host.exec(&ssh(), &script, true, false);
    command.stderr(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    let pipes = runode_ssh::process::Pipes { stdin: true, stdout: true, stderr: false };
    let piped = runode_ssh::process::spawn(&mut command, pipes).map_err(|err| format!("cannot run ssh: {err}"))?;
    let (Some(input), Some(output)) = (piped.stdin, piped.stdout) else {
        return Err("ssh gave no pipes".into());
    };
    let child = Arc::new(Mutex::new(Some(piped.child)));

    let (tx, rx) = mpsc::channel::<ScreenClientMsg>();
    let _ = tx.send(ScreenClientMsg::ScreenHello { version: SCREEN_VERSION });
    thread::Builder::new()
        .name("screen-writer".into())
        .spawn(move || {
            let mut input = BufWriter::new(input);
            for message in rx {
                let Ok(frame) = Frame::control(&message) else { continue };
                if write_frame(&mut input, FrameKind::Control, 0, &frame.payload).is_err() || input.flush().is_err() {
                    return;
                }
            }
        })
        .map_err(|err| format!("cannot start a thread: {err}"))?;

    let sender = ScreenSender(tx);
    let reader_sender = sender.clone();
    let name = host.name.clone();
    thread::Builder::new()
        .name("screen-reader".into())
        .spawn(move || {
            let mut output = BufReader::with_capacity(256 * 1024, output);
            loop {
                let frame = match read_frame(&mut output) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(err) => {
                        tracing::info!("the remote desktop of {name} closed: {err}");
                        break;
                    }
                };
                let event = match frame.kind {
                    FrameKind::Control => match frame.message::<ScreenHostMsg>() {
                        Ok(message) => ScreenEvent::Message(message),
                        // 更新的远端多出来的消息不认得，跳过。
                        Err(err) => {
                            tracing::debug!("unknown remote desktop message from {name}: {err}");
                            continue;
                        }
                    },
                    FrameKind::Video => match decode_video(&frame.payload) {
                        Some(video) => ScreenEvent::Video(video),
                        None => {
                            tracing::warn!("bad video frame from {name}");
                            break;
                        }
                    },
                    _ => continue,
                };
                on_event(event, &reader_sender);
            }
            on_event(ScreenEvent::Closed, &reader_sender);
        })
        .map_err(|err| format!("cannot start a thread: {err}"))?;

    Ok(ScreenLink { sender, child })
}
