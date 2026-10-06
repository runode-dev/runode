//! 控制消息经帧编码往返、JSON 的样子、对缺字段和新取值的容忍，以及会话标识的写法。

use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, ClientMsg, FinishedCommand, Frame, FrameKind, GoodbyeReason, HostMsg,
    PaneLayout, PaneRect, Placement, SessionId, SessionInfo, TabLayout, WindowLayout, WorkspaceLayout,
    message::InvalidSessionId, read_frame, write_frame,
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    grid::GridSize,
    session::{DriveAction, Driver, SessionMeta},
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
        foreground: Some("claude".into()),
        driver: Some(Driver { by: Some(ID.to_string()), action: DriveAction::Paste, at_ms: 1_790_000_000_123 }),
        ..SessionMeta::default()
    }
}

/// 一个窗口、两个工作区，当前标签里左右两个分屏。
fn layout() -> Vec<WindowLayout> {
    let pane = |index, id, x, focused| PaneLayout {
        index,
        id: SessionId(id),
        rect: PaneRect { x, y: 0, width: 500, height: PaneRect::EXTENT },
        focused,
    };
    vec![WindowLayout {
        index: 1,
        front: true,
        workspaces: vec![
            WorkspaceLayout {
                index: 1,
                name: Some("runode".into()),
                active: true,
                tabs: vec![
                    TabLayout { index: 1, active: false, panes: vec![pane(1, 1, 0, true)] },
                    TabLayout { index: 2, active: true, panes: vec![pane(1, 2, 0, false), pane(2, 3, 500, true)] },
                ],
            },
            WorkspaceLayout { index: 2, name: None, active: false, tabs: Vec::new() },
        ],
    }]
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
            session: None,
        },
        ClientMsg::Hello {
            protocol: 3,
            build: BuildId("b".into()),
            client: ClientKind::Cli,
            caps: Caps::default(),
            session: Some(ID),
        },
        ClientMsg::ListSessions,
        ClientMsg::Spawn {
            req: 3,
            size: size(),
            cwd: Some("/tmp/中文".into()),
            integration: IntegrationMode::Force(Shell::Zsh),
            start: false,
            shell: Some("/bin/zsh".into()),
            settings: Some(TermSettings { cursor_blink: Some(false), ..TermSettings::default() }),
            env: vec![("RUNODE_BIN".into(), "/Applications/Runode.app/Contents/MacOS/runode".into())],
        },
        ClientMsg::Spawn {
            req: 4,
            size: size(),
            cwd: None,
            integration: IntegrationMode::Detect,
            start: true,
            shell: None,
            settings: None,
            env: Vec::new(),
        },
        ClientMsg::Start { id: ID, integration: IntegrationMode::Off },
        ClientMsg::Attach { id: ID, size: Some(size()), mode: AttachMode::Snapshot },
        ClientMsg::Attach { id: ID, size: None, mode: AttachMode::MetaOnly },
        ClientMsg::Detach { id: ID },
        ClientMsg::Resize { id: ID, size: size() },
        ClientMsg::Focus { id: ID, focused: true },
        ClientMsg::ClearScreen { id: ID },
        ClientMsg::Kill { id: ID },
        ClientMsg::SetTheme { settings: TermSettings::default() },
        ClientMsg::SetOptions { record_history: false },
        ClientMsg::ReadScreen { id: ID, lines: Some(100), command: None },
        ClientMsg::ReadScreen { id: ID, lines: None, command: Some(2) },
        ClientMsg::SendKeys { req: 5, id: ID, keys: vec!["ctrl-c".into(), "down*3".into(), "f5".into()] },
        ClientMsg::Paste { req: 6, id: ID, text: "echo 中文\nls\n".into() },
        ClientMsg::Layout { req: 7 },
        ClientMsg::UiReply { ui: 8, reply: Box::new(HostMsg::Layout { req: 7, windows: layout() }) },
        ClientMsg::UiReply {
            ui: 9,
            reply: Box::new(HostMsg::Error { req: Some(1), id: None, message: "no window".into() }),
        },
        ClientMsg::Open { req: 10, placement: Placement::Down, near: Some(ID), cwd: Some("/tmp".into()), focus: true },
        ClientMsg::Reveal { req: 11, id: ID },
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
        HostMsg::Welcome {
            protocol: 1,
            build: BuildId("b".into()),
            host_pid: 42,
            snapshot_format: 1,
            standalone: false,
        },
        HostMsg::Welcome { protocol: 3, build: BuildId("b".into()), host_pid: 7, snapshot_format: 1, standalone: true },
        HostMsg::Incompatible { protocol: 2, build: BuildId("b".into()), reason: "too new".into() },
        HostMsg::SessionList {
            sessions: vec![
                SessionInfo { id: ID, size: size(), meta: meta(), clients: 0, claimed: false, exited: false },
                SessionInfo {
                    id: SessionId(2),
                    size: size(),
                    meta: SessionMeta::default(),
                    clients: 2,
                    claimed: true,
                    exited: true,
                },
            ],
        },
        HostMsg::Spawned { req: 3, id: ID },
        HostMsg::Attached {
            id: ID,
            channel: 1,
            size: size(),
            mode: AttachMode::VtReplay,
            meta: meta(),
            settings: None,
        },
        HostMsg::Attached {
            id: ID,
            channel: 2,
            size: size(),
            mode: AttachMode::Snapshot,
            meta: SessionMeta::default(),
            settings: Some(TermSettings::default()),
        },
        HostMsg::SnapshotEnd { id: ID },
        HostMsg::Resized { id: ID, size: size() },
        HostMsg::ThemeApplied {
            id: ID,
            settings: TermSettings { scrollback_limit: 4 << 20, ..TermSettings::default() },
        },
        HostMsg::Meta { id: ID, meta: SessionMeta::default() },
        HostMsg::CommandFinished {
            id: ID,
            command: FinishedCommand { cmd: "ls -la".into(), cwd: Some("/".into()), exit: Some(0), ts: 1_790_000_000 },
        },
        HostMsg::Resync { id: ID, reason: "client fell behind".into() },
        HostMsg::Exited { id: ID, status: None },
        HostMsg::Exited { id: ID, status: Some(130) },
        HostMsg::Bell { id: ID },
        HostMsg::ScreenText { id: ID, text: "$ ls\n".into(), truncated: false },
        HostMsg::ScreenText { id: ID, text: "tail\n".into(), truncated: true },
        HostMsg::Layout { req: 7, windows: layout() },
        HostMsg::Layout { req: 8, windows: Vec::new() },
        HostMsg::UiRequest { ui: 1, request: Box::new(ClientMsg::Layout { req: 7 }) },
        HostMsg::UiRequest {
            ui: 2,
            request: Box::new(ClientMsg::Open {
                req: 3,
                placement: Placement::Tab,
                near: None,
                cwd: None,
                focus: false,
            }),
        },
        HostMsg::Opened { req: 3, id: ID },
        HostMsg::Done { req: 4 },
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
        ClientMsg::Hello {
            protocol: 1,
            build: BuildId("x".into()),
            client: ClientKind::Cli,
            caps: Caps::default(),
            session: None,
        }
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

/// `Open` 只写必填的字段时，其余按默认：放在最前面的窗口当前的分屏旁边，沿用它的目录，不切过去。
#[test]
fn open_fills_in_defaults() {
    let open: ClientMsg = serde_json::from_str(r#"{"type":"open","req":4,"placement":"right"}"#).unwrap();
    assert_eq!(open, ClientMsg::Open { req: 4, placement: Placement::Right, near: None, cwd: None, focus: false });
    let opened = HostMsg::Opened { req: 4, id: SessionId(7) };
    assert_eq!(serde_json::from_str::<HostMsg>(&serde_json::to_string(&opened).unwrap()).unwrap(), opened);
}

/// 第 3 版协议新加的字段，旧的一方发来的消息里没有时的读法：`Spawn` 照旧当场启动、用用户的
/// shell；`ReadScreen` 读屏幕而不是命令输出；`Attached`、`ScreenText`、`SessionInfo` 缺的按空、
/// 假读。
#[test]
fn version_3_fields_have_old_defaults() {
    let spawn: ClientMsg = serde_json::from_str(&format!(
        r#"{{"type":"spawn","req":1,"size":{},"cwd":null,"integration":"detect"}}"#,
        serde_json::to_string(&size()).unwrap()
    ))
    .unwrap();
    assert_eq!(
        spawn,
        ClientMsg::Spawn {
            req: 1,
            size: size(),
            cwd: None,
            integration: IntegrationMode::Detect,
            start: true,
            shell: None,
            settings: None,
            env: Vec::new(),
        }
    );
    let read: ClientMsg = serde_json::from_str(&format!(r#"{{"type":"read_screen","id":"{ID}","lines":3}}"#)).unwrap();
    assert_eq!(read, ClientMsg::ReadScreen { id: ID, lines: Some(3), command: None });
    let text: HostMsg = serde_json::from_str(&format!(r#"{{"type":"screen_text","id":"{ID}","text":"x"}}"#)).unwrap();
    assert_eq!(text, HostMsg::ScreenText { id: ID, text: "x".into(), truncated: false });
    let attached: HostMsg = serde_json::from_str(&format!(
        r#"{{"type":"attached","id":"{ID}","channel":1,"size":{},"mode":"meta_only","meta":{{}}}}"#,
        serde_json::to_string(&size()).unwrap()
    ))
    .unwrap();
    assert!(matches!(attached, HostMsg::Attached { settings: None, mode: AttachMode::MetaOnly, .. }));
    let info: SessionInfo = serde_json::from_str(&format!(
        r#"{{"id":"{ID}","size":{},"meta":{{}}}}"#,
        serde_json::to_string(&size()).unwrap()
    ))
    .unwrap();
    assert_eq!((info.clients, info.claimed, info.exited), (0, false, false));
}

/// `Welcome` 后加的 `standalone`：没有它的宿主读成跑在 app 里。第 3 版协议还没发布过，没有哪个
/// 已发布的宿主会少这个字段，所以不升协议版本。
#[test]
fn welcome_without_standalone_reads_as_in_app() {
    let welcome: HostMsg =
        serde_json::from_str(r#"{"type":"welcome","protocol":3,"build":"x","host_pid":9,"snapshot_format":1}"#)
            .unwrap();
    assert!(matches!(welcome, HostMsg::Welcome { standalone: false, host_pid: 9, .. }));
}

/// 转给界面的请求和界面的回话里套着另一种消息，JSON 里就是嵌着的一条消息。
#[test]
fn ui_requests_nest_the_original_message() {
    let json =
        serde_json::to_string(&HostMsg::UiRequest { ui: 4, request: Box::new(ClientMsg::Layout { req: 9 }) }).unwrap();
    assert_eq!(json, r#"{"type":"ui_request","ui":4,"request":{"type":"layout","req":9}}"#);
    let json = serde_json::to_string(&ClientMsg::UiReply { ui: 4, reply: Box::new(HostMsg::Done { req: 9 }) }).unwrap();
    assert_eq!(json, r#"{"type":"ui_reply","ui":4,"reply":{"type":"done","req":9}}"#);
    // 界面不认识的请求读成 `Unknown`，照样能回话。
    let request: HostMsg =
        serde_json::from_str(r#"{"type":"ui_request","ui":1,"request":{"type":"teleport"}}"#).unwrap();
    assert_eq!(request, HostMsg::UiRequest { ui: 1, request: Box::new(ClientMsg::Unknown) });
}

/// 布局的 JSON 样子：分屏的位置按标签区域 0..1000 归一化，缺的标记按假读。
#[test]
fn layouts_read_with_defaults() {
    let window: WindowLayout = serde_json::from_str(&format!(
        r#"{{"index":2,"workspaces":[{{"index":1,"tabs":[{{"index":1,"panes":[
            {{"index":1,"id":"{ID}","rect":{{"x":0,"y":0,"width":1000,"height":1000}}}}]}}]}}]}}"#
    ))
    .unwrap();
    assert_eq!(
        window,
        WindowLayout {
            index: 2,
            front: false,
            workspaces: vec![WorkspaceLayout {
                index: 1,
                name: None,
                active: false,
                tabs: vec![TabLayout {
                    index: 1,
                    active: false,
                    panes: vec![PaneLayout {
                        index: 1,
                        id: ID,
                        rect: PaneRect { x: 0, y: 0, width: PaneRect::EXTENT, height: PaneRect::EXTENT },
                        focused: false,
                    }],
                }],
            }],
        }
    );
}
