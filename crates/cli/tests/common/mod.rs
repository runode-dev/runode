//! 假宿主：在临时 socket 上接一条连接，自动回 `Hello`，其余控制消息按测试给的脚本回话，收到的
//! 消息和输入都记下来给测试查。

#![allow(dead_code)]

use std::{os::unix::net::UnixListener, sync::mpsc, thread, time::Duration};

use runode_cli::Env;
use runode_protocol::{
    BuildId, ClientMsg, Frame, FrameKind, HostMsg, PROTOCOL_VERSION, SessionId, SessionInfo, read_frame, write_frame,
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
}

impl FakeHost {
    /// 起一个假宿主。`script` 对每条控制消息（`Hello` 除外）给出要回的消息。
    pub fn start(name: &str, mut script: impl FnMut(&ClientMsg) -> Vec<HostMsg> + Send + 'static) -> Self {
        let dir = std::env::temp_dir().join(format!("rnc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, received) = mpsc::channel();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
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
                    }],
                    other => script(other),
                };
                let _ = tx.send(Received::Control(message));
                if !replies.iter().all(&mut reply) {
                    return;
                }
            }
        });
        let env = Env { socket: Some(socket), session: None, build: "test".into() };
        Self { env, received }
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
