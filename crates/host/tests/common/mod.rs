//! 几个测试文件共用的：按协议说话的前端（经 socket 或者 `Host::connect_pair`）、临时目录、当
//! shell 用的脚本。

#![allow(dead_code)]

use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, Host, HostMsg, SessionId};
use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, Frame, FrameKind, PROTOCOL_VERSION, read_frame, write_frame,
};
use runode_shared_types::{grid::GridSize, shell::IntegrationMode};

pub const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
pub const WAIT: Duration = Duration::from_secs(10);
pub const BUILD: &str = "test-build";

pub fn host() -> Host {
    Host::new(BuildId(BUILD.into()))
}

/// 一个空的临时目录，socket 和锁放在里面。路径要短，放得进 `sun_path`。
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rnh-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 开一个宿主监听 `dir` 里的 socket。
pub fn listen(dir: &Path) -> (Host, PathBuf) {
    let host = host();
    let socket = dir.join("host.sock");
    host.listen(&socket, &dir.join("host.lock")).unwrap();
    (host, socket)
}

/// 在 `dir` 里写一个叫 `name` 的可执行脚本当 shell 用，返回它的路径。登录 shell 会多带一个 `-l`
/// 参数，脚本不看参数。
pub fn script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// 按协议说话的一个前端：读到的帧由后台线程交过来，测试里按超时等。丢掉时断开连接。
pub struct Peer {
    pub stream: UnixStream,
    pub frames: mpsc::Receiver<Frame>,
    next_req: u32,
}

impl Drop for Peer {
    fn drop(&mut self) {
        // 读的线程拿着同一个 socket 的另一份描述符，只关自己这份宿主看不到断开。
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Peer {
    pub fn over(stream: UnixStream) -> Self {
        let mut reader = stream.try_clone().unwrap();
        let (tx, frames) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        Self { stream, frames, next_req: 1 }
    }

    pub fn connect(socket: &Path) -> Self {
        Self::over(UnixStream::connect(socket).unwrap())
    }

    /// 握手，`snapshot` 时说自己能解同一个构建的快照。
    pub fn greet(self, client: ClientKind, snapshot: bool) -> Self {
        self.greet_from(client, snapshot, None)
    }

    /// 握手，`session` 是前端自己所在的会话（在 runode 的终端里跑的命令行带着它）。
    pub fn greet_from(mut self, client: ClientKind, snapshot: bool, session: Option<SessionId>) -> Self {
        self.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            client,
            caps: Caps { snapshot, vt_replay: true },
            session,
            device: None,
        });
        assert!(matches!(self.message(), HostMsg::Welcome { protocol: PROTOCOL_VERSION, .. }));
        self
    }

    /// 经 socket 连上、以命令行的身份握手。
    pub fn hello(socket: &Path, snapshot: bool) -> Self {
        Self::connect(socket).greet(ClientKind::Cli, snapshot)
    }

    /// 经 socket 连上、以桌面界面的身份握手。
    pub fn desktop(socket: &Path) -> Self {
        Self::connect(socket).greet(ClientKind::Desktop, true)
    }

    /// 经 `Host::connect_pair` 连上、以桌面界面的身份握手。
    pub fn pair(host: &Host) -> Self {
        Self::over(host.connect_pair().unwrap()).greet(ClientKind::Desktop, true)
    }

    pub fn send(&mut self, message: &ClientMsg) {
        let frame = Frame::control(message).unwrap();
        write_frame(&mut self.stream, frame.kind, 0, &frame.payload).unwrap();
    }

    pub fn input(&mut self, channel: u32, data: &[u8]) {
        write_frame(&mut self.stream, FrameKind::Input, channel, data).unwrap();
    }

    pub fn frame(&self) -> Frame {
        self.frames.recv_timeout(WAIT).expect("timed out waiting for a frame")
    }

    /// 下一条控制消息，跳过中间的输出帧。
    pub fn message(&self) -> HostMsg {
        loop {
            let frame = self.frame();
            if frame.kind == FrameKind::Control {
                return frame.message().unwrap();
            }
        }
    }

    /// 回话：下一条不是 `Meta`、`CommandFinished`、`Bell` 这类随时会插进来的状态的控制消息。
    pub fn reply(&self) -> HostMsg {
        loop {
            match self.message() {
                HostMsg::Meta { .. } | HostMsg::CommandFinished { .. } | HostMsg::Bell { .. } => {}
                message => return message,
            }
        }
    }

    /// 一直读，直到 `found` 认出某件事（控制消息为 `Some`，输出帧为 `None`），返回在那之前（含）
    /// 收到的这个通道的全部输出。
    pub fn wait(&self, channel: u32, mut found: impl FnMut(Option<&HostMsg>, &[u8]) -> bool) -> Vec<u8> {
        let deadline = Instant::now() + WAIT;
        let mut output = Vec::new();
        loop {
            let frame =
                self.frames.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("timed out");
            let message = match frame.kind {
                FrameKind::Output if frame.channel == channel => {
                    output.extend_from_slice(&frame.payload);
                    None
                }
                FrameKind::Control => Some(frame.message::<HostMsg>().unwrap()),
                _ => continue,
            };
            if found(message.as_ref(), &output) {
                return output;
            }
        }
    }

    /// 等到输出里出现 `needle`。
    pub fn wait_for_output(&self, channel: u32, needle: &[u8]) -> Vec<u8> {
        self.wait(channel, |_, output| contains(output, needle))
    }

    /// 连上会话，返回它的通道和快照（或 VT 重放）的字节。
    pub fn attach(&mut self, id: SessionId, mode: AttachMode) -> (u32, Vec<u8>) {
        self.attach_sized(id, Some(SIZE), mode)
    }

    pub fn attach_sized(&mut self, id: SessionId, size: Option<GridSize>, mode: AttachMode) -> (u32, Vec<u8>) {
        self.send(&ClientMsg::Attach { id, size, mode });
        let HostMsg::Attached { id: attached, channel, mode: given, .. } = self.reply() else {
            panic!("expected attached");
        };
        assert_eq!(attached, id);
        (channel, self.screen(id, channel, given))
    }

    /// `Attached` 之后的快照帧，读到 `SnapshotEnd`；`MetaOnly` 时没有。
    pub fn screen(&self, id: SessionId, channel: u32, mode: AttachMode) -> Vec<u8> {
        let mut screen = Vec::new();
        if mode != AttachMode::MetaOnly {
            loop {
                let frame = self.frame();
                match frame.kind {
                    FrameKind::Snapshot => {
                        assert_eq!(frame.channel, channel);
                        screen.extend_from_slice(&frame.payload);
                    }
                    FrameKind::Control => {
                        assert!(matches!(frame.message().unwrap(), HostMsg::SnapshotEnd { id: end } if end == id));
                        break;
                    }
                    kind => panic!("unexpected {kind:?} before the snapshot ended"),
                }
            }
        }
        screen
    }

    /// 开一个跑 `shell` 的会话（不开 shell 集成），返回它的标识。
    pub fn spawn(&mut self, shell: &str) -> SessionId {
        self.spawn_with(shell, true, Vec::new(), None)
    }

    pub fn spawn_with(
        &mut self,
        shell: &str,
        start: bool,
        env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
    ) -> SessionId {
        let req = self.next_req;
        self.next_req += 1;
        self.send(&ClientMsg::Spawn {
            req,
            size: SIZE,
            cwd,
            integration: IntegrationMode::Off,
            start,
            shell: Some(shell.into()),
            settings: None,
            env,
        });
        match self.reply() {
            HostMsg::Spawned { req: answered, id } if answered == req => id,
            other => panic!("expected spawned, got {other:?}"),
        }
    }

    /// 列会话。
    pub fn sessions(&mut self) -> Vec<runode_protocol::SessionInfo> {
        self.send(&ClientMsg::ListSessions);
        match self.reply() {
            HostMsg::SessionList { sessions } => sessions,
            other => panic!("expected a session list, got {other:?}"),
        }
    }

    /// 连接断开（读到头）前收到的全部控制消息。
    pub fn until_closed(&self) -> Vec<HostMsg> {
        let deadline = Instant::now() + WAIT;
        let mut messages = Vec::new();
        loop {
            match self.frames.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(frame) if frame.kind == FrameKind::Control => messages.push(frame.message().unwrap()),
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return messages,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the connection stayed open: {messages:?}"),
            }
        }
    }
}
