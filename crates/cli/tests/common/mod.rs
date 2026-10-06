//! 假宿主：在临时 socket 上接连接，自动回 `Hello`，其余控制消息按测试给的脚本回话，收到的
//! 消息和输入都记下来给测试查。

#![allow(dead_code)]

use std::{
    os::unix::net::{UnixListener, UnixStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use runode_cli::Env;
use runode_protocol::{
    BuildId, ClientMsg, Frame, FrameKind, HostMsg, PROTOCOL_VERSION, PaneLayout, PaneRect, SessionId, SessionInfo,
    TabLayout, WindowLayout, WorkspaceLayout, read_frame, write_frame,
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    grid::GridSize,
    session::SessionMeta,
};

pub const SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };

/// 假宿主收到的东西。
#[derive(Debug, PartialEq)]
pub enum Received {
    Control(ClientMsg),
    Input(u32, Vec<u8>),
}

pub struct FakeHost {
    pub env: Env,
    pub received: mpsc::Receiver<Received>,
    /// 临时目录，`env.home` 指向它下面的 `home`。
    pub dir: std::path::PathBuf,
}

impl FakeHost {
    /// 起一个假宿主。`script` 对每条控制消息（`Hello` 除外）给出要回的消息；连上来的每条连接
    /// 都由它回话，同一个测试里能跑好几条命令。
    pub fn start(name: &str, script: impl FnMut(&ClientMsg) -> Vec<HostMsg> + Send + 'static) -> Self {
        let dir = std::env::temp_dir().join(format!("rnc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, received) = mpsc::channel();
        let script = Arc::new(Mutex::new(script));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (tx, script) = (tx.clone(), script.clone());
                thread::spawn(move || serve(stream, &tx, &script));
            }
        });
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = Env { socket: Some(socket), session: None, build: "test".into(), home: Some(home) };
        Self { env, received, dir }
    }

    /// 收到过的全部东西，等连接上的消息都到了再取。
    pub fn drain(&self) -> Vec<Received> {
        let mut all = Vec::new();
        while let Ok(item) = self.received.recv_timeout(Duration::from_millis(300)) {
            all.push(item);
        }
        all
    }
}

/// 一条连接从头到尾：自动回 `Hello`，别的控制消息交给 `script`。
fn serve(mut stream: UnixStream, tx: &mpsc::Sender<Received>, script: &Mutex<impl FnMut(&ClientMsg) -> Vec<HostMsg>>) {
    let mut writer = stream.try_clone().unwrap();
    let mut reply = |message: &HostMsg| {
        let frame = Frame::control(message).unwrap();
        write_frame(&mut writer, FrameKind::Control, 0, &frame.payload).is_ok()
    };
    while let Ok(Some(frame)) = read_frame(&mut stream) {
        if frame.kind == FrameKind::Input {
            let _ = tx.send(Received::Input(frame.channel, frame.payload));
            continue;
        }
        let message: ClientMsg = frame.message().unwrap();
        let replies = match &message {
            ClientMsg::Hello { .. } => vec![HostMsg::Welcome {
                protocol: PROTOCOL_VERSION,
                build: BuildId("test".into()),
                host_pid: 1,
                snapshot_format: 1,
                standalone: false,
            }],
            other => (script.lock().unwrap())(other),
        };
        let _ = tx.send(Received::Control(message));
        if !replies.iter().all(&mut reply) {
            return;
        }
    }
}

/// 跑一条命令，返回退出码、标准输出和标准错误。
pub fn run(args: &str, env: &Env) -> (i32, String, String) {
    let args: Vec<String> = args.split_whitespace().map(str::to_owned).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = runode_cli::run(&args, env, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

pub fn id(n: u128) -> SessionId {
    SessionId(n)
}

pub fn meta(title: &str, agent: Option<(AgentKind, AgentState)>) -> SessionMeta {
    SessionMeta {
        title: Some(title.into()),
        agent: agent.map(|(kind, state)| Agent { kind, state }),
        cwd: Some("/tmp/project".into()),
        ..SessionMeta::default()
    }
}

pub fn session(id: SessionId, meta: SessionMeta) -> SessionInfo {
    SessionInfo { id, size: SIZE, meta, clients: 1, claimed: true, exited: false }
}

/// 状态变成 `state` 的 `Meta`。
pub fn agent_now(id: SessionId, state: AgentState) -> HostMsg {
    HostMsg::Meta { id, meta: meta("agent", Some((AgentKind::Claude, state))) }
}

/// 收到过的控制消息，等连接上的消息都到了再取。
pub fn controls(fake: &FakeHost) -> Vec<ClientMsg> {
    fake.drain()
        .into_iter()
        .filter_map(|r| match r {
            Received::Control(message) => Some(message),
            Received::Input(..) => None,
        })
        .collect()
}

/// 标签里的一个分屏：序号、会话和在标签区域里的位置（0..1000）。
pub fn pane(index: u32, id: SessionId, (x, y, width, height): (u16, u16, u16, u16), focused: bool) -> PaneLayout {
    PaneLayout { index, id, rect: PaneRect { x, y, width, height }, focused }
}

pub fn tab(index: u32, active: bool, panes: Vec<PaneLayout>) -> TabLayout {
    TabLayout { index, active, panes }
}

pub fn workspace(index: u32, active: bool, tabs: Vec<TabLayout>) -> WorkspaceLayout {
    WorkspaceLayout { index, name: None, active, tabs }
}

pub fn window(index: u32, front: bool, workspaces: Vec<WorkspaceLayout>) -> WindowLayout {
    WindowLayout { index, front, workspaces }
}

/// 整个标签区域。
pub const WHOLE: (u16, u16, u16, u16) = (0, 0, PaneRect::EXTENT, PaneRect::EXTENT);
