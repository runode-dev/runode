//! 连上宿主的 socket：握手，发控制消息和输入，按超时等宿主的消息。
//!
//! 读放在单独的线程里，读到的控制消息经 channel 交过来，等消息时能按截止时刻放弃，不会把一帧
//! 读到一半。命令行只看状态，输出帧和快照帧直接丢掉。

use std::{
    os::unix::net::UnixStream,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use runode_protocol::{
    BuildId, Caps, ClientKind, ClientMsg, Frame, FrameKind, HostMsg, PROTOCOL_VERSION, read_frame, write_frame,
};

use crate::Env;

/// 等宿主回话的最长时间；`wait` 等 agent 不受它限制。
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Connection {
    stream: UnixStream,
    messages: mpsc::Receiver<HostMsg>,
}

impl Connection {
    /// 连上宿主并握手。
    pub(crate) fn open(env: &Env) -> Result<Self> {
        let socket = env.socket.as_deref().ok_or_else(|| anyhow!("no place to find the runode app's socket"))?;
        let stream = UnixStream::connect(socket)
            .with_context(|| format!("cannot reach the runode app at {}; is it running?", socket.display()))?;
        let mut reader = stream.try_clone()?;
        let (tx, messages) = mpsc::channel();
        thread::Builder::new().name("runode-reader".into()).spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut reader) {
                if frame.kind != FrameKind::Control {
                    continue;
                }
                let Ok(message) = frame.message::<HostMsg>() else { continue };
                if tx.send(message).is_err() {
                    break;
                }
            }
        })?;
        let mut connection = Self { stream, messages };
        connection.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(env.build.clone()),
            client: ClientKind::Cli,
            caps: Caps::default(),
        })?;
        match connection.reply()? {
            HostMsg::Welcome { .. } => Ok(connection),
            HostMsg::Incompatible { reason, .. } => bail!("the runode app speaks another protocol: {reason}"),
            other => bail!("unexpected answer to hello: {}", kind(&other)),
        }
    }

    pub(crate) fn send(&mut self, message: &ClientMsg) -> Result<()> {
        let frame = Frame::control(message)?;
        write_frame(&mut self.stream, frame.kind, frame.channel, &frame.payload)
            .map_err(|err| anyhow!("lost the runode app: {err}"))
    }

    pub(crate) fn input(&mut self, channel: u32, data: &[u8]) -> Result<()> {
        write_frame(&mut self.stream, FrameKind::Input, channel, data)
            .map_err(|err| anyhow!("lost the runode app: {err}"))
    }

    /// 下一条消息，到 `deadline` 还没有时为 `None`；为 `None` 的 `deadline` 一直等。
    pub(crate) fn next(&self, deadline: Option<Instant>) -> Result<Option<HostMsg>> {
        let received = match deadline {
            Some(at) => match self.messages.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(message) => Ok(message),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
            },
            None => self.messages.recv().map_err(|_| ()),
        };
        received.map(Some).map_err(|()| anyhow!("the runode app closed the connection"))
    }

    /// 回话：跳过 `Meta` 这类随时会插进来的状态，宿主报错时返回错误。
    pub(crate) fn reply(&self) -> Result<HostMsg> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            match self.next(Some(deadline))? {
                None => bail!("the runode app did not answer"),
                Some(HostMsg::Meta { .. } | HostMsg::CommandFinished { .. }) => {}
                Some(HostMsg::Error { message, .. }) => bail!("{message}"),
                Some(message) => return Ok(message),
            }
        }
    }
}

/// 消息的种类，报错时用。
pub(crate) fn kind(message: &HostMsg) -> String {
    serde_json::to_value(message)
        .ok()
        .and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "message".into())
}
