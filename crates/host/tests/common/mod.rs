//! 几个测试文件共用的：按协议说话的前端（经 socket 或者 `Host::connect_pair`）、临时目录、当
//! shell 用的脚本。

#![allow(dead_code)]

use std::{
    collections::VecDeque,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use runode_host::{ClientMsg, Host, HostMsg, SessionId};
use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, Frame, FrameKind, PROTOCOL_VERSION, read_frame, write_frame,
};
use runode_types::{grid::GridSize, shell::IntegrationMode};

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

/// 按协议说话的一个前端：读到的帧由后台线程交过来，测试里按超时等（`next_frame`）。丢掉时断开连接。
pub struct Peer {
    pub stream: UnixStream,
    frames: mpsc::Receiver<Frame>,
    /// 读快照时先收下的别的帧（别的会话的输出和消息），按先后排在之后读到的前面，见 `screen`。
    held: Mutex<VecDeque<Frame>>,
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
        Self { stream, frames, held: Mutex::default(), next_req: 1 }
    }

    /// 下一帧，最多等 `timeout`；读快照时先收下的排在前面。
    pub fn next_frame(&self, timeout: Duration) -> Result<Frame, mpsc::RecvTimeoutError> {
        if let Some(frame) = self.held.lock().unwrap().pop_front() {
            return Ok(frame);
        }
        self.frames.recv_timeout(timeout)
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
            caps: Caps { snapshot, vt_replay: true, shell_report: false },
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
        self.next_frame(WAIT).expect("timed out waiting for a frame")
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

    /// 回话：下一条不是 `Meta`、`CommandFinished`、`Bell`、`SizeOwner` 这类随时会插进来的状态的
    /// 控制消息。
    pub fn reply(&self) -> HostMsg {
        loop {
            match self.message() {
                HostMsg::Meta { .. }
                | HostMsg::CommandFinished { .. }
                | HostMsg::Bell { .. }
                | HostMsg::SizeOwner { .. } => {}
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
            let frame = self.next_frame(deadline.saturating_duration_since(Instant::now())).expect("timed out");
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
        self.send(&ClientMsg::Attach { id, size: Some(SIZE), mode });
        let HostMsg::Attached { id: attached, channel, mode: given, .. } = self.reply() else {
            panic!("expected attached");
        };
        assert_eq!(attached, id);
        (channel, self.screen(id, channel, given))
    }

    /// `Attached` 之后的快照帧，读到这个会话的 `SnapshotEnd`；`MetaOnly` 时没有。同一条连接上别的会话在
    /// 各自的线程里发事件，可能夹在快照帧中间：先收下，之后照先后读到。之前读别的会话的快照时可能已经
    /// 收下了这个会话的，先在收下的里面找。
    pub fn screen(&self, id: SessionId, channel: u32, mode: AttachMode) -> Vec<u8> {
        let mut screen = Vec::new();
        if mode == AttachMode::MetaOnly {
            return screen;
        }
        // 快照帧并进 `screen`，读到这个会话的 `SnapshotEnd` 时为 true；别的帧原样还回来。
        let mut take = |frame: Frame| match frame.kind {
            FrameKind::Snapshot if frame.channel == channel => {
                screen.extend_from_slice(&frame.payload);
                Ok(false)
            }
            FrameKind::Control if matches!(frame.message().unwrap(), HostMsg::SnapshotEnd { id: end } if end == id) => {
                Ok(true)
            }
            _ => Err(frame),
        };
        let mut ended = false;
        {
            let mut held = self.held.lock().unwrap();
            let mut rest = VecDeque::with_capacity(held.len());
            for frame in held.drain(..) {
                if ended {
                    rest.push_back(frame);
                    continue;
                }
                match take(frame) {
                    Ok(end) => ended = end,
                    Err(frame) => rest.push_back(frame),
                }
            }
            *held = rest;
        }
        let deadline = Instant::now() + WAIT;
        while !ended {
            let frame = self
                .frames
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("timed out waiting for the snapshot");
            match take(frame) {
                Ok(end) => ended = end,
                Err(frame) => self.held.lock().unwrap().push_back(frame),
            }
        }
        screen
    }

    /// 开一个跑 `shell` 的会话（不开 shell 集成），返回它的标识。
    pub fn spawn(&mut self, shell: &str) -> SessionId {
        self.spawn_with(shell, true, None)
    }

    pub fn spawn_with(&mut self, shell: &str, start: bool, cwd: Option<PathBuf>) -> SessionId {
        self.spawn_in(shell, SIZE, start, cwd, None)
    }

    /// 同 `spawn_with`，当场启动，`cwd` 不是目录时从 `fallback` 开。
    pub fn spawn_falling_back(&mut self, shell: &str, cwd: PathBuf, fallback: PathBuf) -> SessionId {
        self.spawn_in(shell, SIZE, true, Some(cwd), Some(fallback))
    }

    /// 开一个按 `size` 跑 `shell` 的会话（不开 shell 集成），`start` 为假时等 `Start`。
    pub fn spawn_sized(&mut self, shell: &str, size: GridSize, start: bool) -> SessionId {
        self.spawn_in(shell, size, start, None, None)
    }

    fn spawn_in(
        &mut self,
        shell: &str,
        size: GridSize,
        start: bool,
        cwd: Option<PathBuf>,
        fallback_cwd: Option<PathBuf>,
    ) -> SessionId {
        let req = self.next_req;
        self.next_req += 1;
        self.send(&ClientMsg::Spawn {
            req,
            size,
            cwd,
            fallback_cwd,
            integration: IntegrationMode::Off,
            start,
            shell: Some(shell.into()),
            settings: None,
        });
        match self.reply() {
            HostMsg::Spawned { req: answered, id } if answered == req => id,
            other => panic!("expected spawned, got {other:?}"),
        }
    }

    /// 读会话的屏幕：`lines` 为 `None` 时是当前一屏，否则是含回滚历史的最底下这么多行。
    pub fn screen_text(&mut self, id: SessionId, lines: Option<u32>) -> String {
        self.send(&ClientMsg::ReadScreen { id, lines, command: None });
        match self.reply() {
            HostMsg::ScreenText { id: read, text, .. } if read == id => text,
            other => panic!("expected the screen, got {other:?}"),
        }
    }

    /// 反复读屏幕，直到 `done` 认可，返回那一次读到的。
    pub fn wait_screen(&mut self, id: SessionId, lines: Option<u32>, mut done: impl FnMut(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = self.screen_text(id, lines);
            if done(&text) {
                return text;
            }
            assert!(Instant::now() < deadline, "timed out waiting for the screen of {id}: {text:?}");
            thread::sleep(Duration::from_millis(20));
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
            match self.next_frame(deadline.saturating_duration_since(Instant::now())) {
                Ok(frame) if frame.kind == FrameKind::Control => messages.push(frame.message().unwrap()),
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return messages,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("the connection stayed open: {messages:?}"),
            }
        }
    }
}

/// macOS 按哪个进程的授权判断 `pid` 的隐私权限（本地网络等）。
#[cfg(target_os = "macos")]
pub fn responsible_pid(pid: libc::pid_t) -> libc::pid_t {
    unsafe extern "C" {
        fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
    }
    // SAFETY: 只按 pid 查，不碰内存。
    unsafe { responsibility_get_pid_responsible_for_pid(pid) }
}

/// `pid` 所在的资源 coalition（macOS 给进程分的组，launchd 每个任务一个，子进程继承）。
#[cfg(target_os = "macos")]
pub fn coalition(pid: libc::pid_t) -> u64 {
    // `sys/proc_info.h` 的 `PROC_PIDCOALITIONINFO`，答的是 `struct proc_pidcoalitioninfo`：头一项是资源
    // coalition 的号。
    const PROC_PIDCOALITIONINFO: libc::c_int = 20;
    let mut info = [0u64; 5];
    let size = libc::c_int::try_from(std::mem::size_of_val(&info)).unwrap();
    // SAFETY: 缓冲是本地数组，大小如实给出。
    let read = unsafe { libc::proc_pidinfo(pid, PROC_PIDCOALITIONINFO, 0, info.as_mut_ptr().cast(), size) };
    assert_eq!(read, size, "cannot read the coalition of {pid}");
    info[0]
}
