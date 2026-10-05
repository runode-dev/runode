//! 控制消息经帧编码往返、JSON 的样子、对缺字段和新取值的容忍，以及会话标识的写法。

use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, ClientMsg, FinishedCommand, Frame, FrameKind, GoodbyeReason, HostMsg,
    SessionId, SessionInfo, message::InvalidSessionId, read_frame, write_frame,
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    grid::GridSize,
    session::SessionMeta,
    settings::TermSettings,
    shell::{IntegrationMode, Shell},
};
use serde::Serialize;

const ID: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);

fn size() -> GridSize {
    GridSize { cols: 80, rows: 24, cell_width_px: 16, cell_height_px: 32 }
}

fn meta() -> SessionMeta {
    SessionMeta {
        title: Some("修 bug".into()),
        agent: Some(Agent { kind: AgentKind::Claude, state: AgentState::Working }),
        cwd: Some("/tmp".into()),
        foreground_is_shell: false,
        ..SessionMeta::default()
    }
}

/// 经过帧编码、解码再读回来。
fn through_frame<T: Serialize + serde::de::DeserializeOwned>(message: &T) -> T {
    let frame = Frame::control(message).unwrap();
    let mut stream = Vec::new();
    write_frame(&mut stream, frame.kind, frame.channel, &frame.payload).unwrap();
    let read = read_frame(&mut &stream[..]).unwrap().unwrap();
    assert_eq!((read.kind, read.channel), (FrameKind::Control, 0));
    read.message().unwrap()
}

#[test]
fn client_messages_round_trip() {
    let messages = [
        ClientMsg::Hello {
            protocol: runode_protocol::PROTOCOL_VERSION,
            build: BuildId("0.1.0+abc".into()),
            client: ClientKind::Desktop,
            caps: Caps { snapshot: true, vt_replay: true },
        },
        ClientMsg::ListSessions,
        ClientMsg::Spawn {
            req: 3,
            size: size(),
            cwd: Some("/tmp/中文".into()),
            integration: IntegrationMode::Force(Shell::Zsh),
        },
        ClientMsg::Spawn { req: 4, size: size(), cwd: None, integration: IntegrationMode::Detect },
        ClientMsg::Attach { id: ID, size: Some(size()), mode: AttachMode::Snapshot },
        ClientMsg::Attach { id: ID, size: None, mode: AttachMode::MetaOnly },
        ClientMsg::Detach { id: ID },
        ClientMsg::Resize { id: ID, size: size() },
        ClientMsg::Focus { id: ID, focused: true },
        ClientMsg::ClearScreen { id: ID },
        ClientMsg::Kill { id: ID },
        ClientMsg::SetTheme { settings: TermSettings::default() },
        ClientMsg::SetOptions { record_history: false },
        ClientMsg::ReadScreen { id: ID, lines: Some(100) },
        ClientMsg::Handoff,
        ClientMsg::Shutdown { kill_sessions: true },
    ];
    for message in messages {
        assert_eq!(through_frame(&message), message);
    }
}

#[test]
fn host_messages_round_trip() {
    let messages = [
        HostMsg::Welcome { protocol: 1, build: BuildId("b".into()), host_pid: 42, snapshot_format: 1 },
        HostMsg::Incompatible { protocol: 2, build: BuildId("b".into()), reason: "too new".into() },
        HostMsg::SessionList {
            sessions: vec![SessionInfo { id: ID, size: size(), meta: meta(), clients: 0, exited: false }],
        },
        HostMsg::Spawned { req: 3, id: ID },
        HostMsg::Attached { id: ID, channel: 1, size: size(), mode: AttachMode::VtReplay, meta: meta() },
        HostMsg::SnapshotEnd { id: ID },
        HostMsg::Resized { id: ID, size: size() },
        HostMsg::ThemeApplied { id: ID },
        HostMsg::Meta { id: ID, meta: SessionMeta::default() },
        HostMsg::CommandFinished {
            id: ID,
            command: FinishedCommand { cmd: "ls -la".into(), cwd: Some("/".into()), exit: Some(0), ts: 1_790_000_000 },
        },
        HostMsg::Resync { id: ID, reason: "client fell behind".into() },
        HostMsg::Exited { id: ID, status: None },
        HostMsg::ScreenText { id: ID, text: "$ ls\n".into() },
        HostMsg::Error { req: Some(3), id: None, message: "no such directory".into() },
        HostMsg::Goodbye { reason: GoodbyeReason::Handoff },
        HostMsg::Goodbye { reason: GoodbyeReason::Error { message: "boom".into() } },
    ];
    for message in messages {
        assert_eq!(through_frame(&message), message);
    }
}

#[test]
fn the_wire_format_is_readable_json() {
    let json = serde_json::to_string(&ClientMsg::Attach { id: ID, size: None, mode: AttachMode::MetaOnly }).unwrap();
    assert_eq!(json, r#"{"type":"attach","id":"0123456789abcdef0011223344556677","size":null,"mode":"meta_only"}"#);
    let json = serde_json::to_string(&HostMsg::Goodbye { reason: GoodbyeReason::Idle }).unwrap();
    assert_eq!(json, r#"{"type":"goodbye","reason":{"kind":"idle"}}"#);
}

#[test]
fn missing_and_unknown_fields_are_tolerated() {
    let hello: ClientMsg =
        serde_json::from_str(r#"{"type":"hello","protocol":1,"build":"x","client":"cli","future":true}"#).unwrap();
    assert_eq!(
        hello,
        ClientMsg::Hello { protocol: 1, build: BuildId("x".into()), client: ClientKind::Cli, caps: Caps::default() }
    );
    // 必填字段缺了读不了。
    assert!(serde_json::from_str::<ClientMsg>(r#"{"type":"detach"}"#).is_err());
}

#[test]
fn newer_kinds_and_messages_read_as_unknown() {
    // 新的前端种类：旧宿主照样读得懂 Hello，能按协议版本回 Incompatible。
    let hello: ClientMsg =
        serde_json::from_str(r#"{"type":"hello","protocol":9,"build":"x","client":"watch"}"#).unwrap();
    assert!(matches!(hello, ClientMsg::Hello { protocol: 9, client: ClientKind::Unknown, .. }));
    // 新的消息种类，带不带字段都一样。
    assert_eq!(serde_json::from_str::<ClientMsg>(r#"{"type":"teleport","to":"mars"}"#).unwrap(), ClientMsg::Unknown);
    assert_eq!(serde_json::from_str::<HostMsg>(r#"{"type":"confetti"}"#).unwrap(), HostMsg::Unknown);
    // 其他枚举的新取值读不了。
    assert!(
        serde_json::from_str::<ClientMsg>(&format!(r#"{{"type":"attach","id":"{ID}","size":null,"mode":"hologram"}}"#))
            .is_err()
    );
}

#[test]
fn session_ids_are_32_hex_digits() {
    assert_eq!(ID.to_string(), "0123456789abcdef0011223344556677");
    assert_eq!(SessionId(1).to_string(), "00000000000000000000000000000001");
    assert_eq!("0123456789ABCDEF0011223344556677".parse(), Ok(ID));
    for bad in [
        "",
        "123",
        "+123456789abcdef0011223344556677",
        "0123456789abcdef00112233445566778",
        "0123456789abcdef001122334455667g",
    ] {
        assert_eq!(bad.parse::<SessionId>(), Err(InvalidSessionId), "{bad:?}");
    }
    assert!(serde_json::from_str::<SessionId>(r#""xyz""#).is_err());
}

#[cfg(unix)]
#[test]
fn random_session_ids_differ() {
    let a = SessionId::random().unwrap();
    let b = SessionId::random().unwrap();
    assert_ne!(a, b);
    assert_eq!(a.to_string().parse(), Ok(a));
}
