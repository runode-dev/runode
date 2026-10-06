//! 交接格式 1 的样例，永远要读得了。
//!
//! 升级时新宿主要和任何一个已经发布过的旧宿主谈交接，所以这里的 JSON 是照着第一个会交接的
//! 版本发出的样子手写的，不由现在的类型生成：这些测试失败说明改了冻结的结构（改了字段名、
//! 类型，或者加了不带 `#[serde(default)]` 的字段，包括嵌在里面的 `TermSettings`、`GridSize`、
//! `SessionMeta`），要改回来，不要改样例。见 `message` 和 `handoff` 的模块文档。

use runode_protocol::{
    BuildId, ClientKind, ClientMsg, FrameKind, GoodbyeReason, HandoffPart, HandoffRefusal, HostMsg, RedactorState,
    ReportToken, RunningCommand, SessionId, decode_part, read_frame,
};
use runode_shared_types::{
    clipboard::ClipboardAccess,
    color::{Rgb, TerminalColor},
    grid::GridSize,
    settings::TermSettings,
};

const ID: &str = "0123456789abcdef0011223344556677";
const OTHER: &str = "00000000000000000000000000000002";

/// 照格式拼一条描述符消息的数据：`u32 LE 头的字节数 | 头 | 块`。
fn message(header: &str, blocks: &[&[u8]]) -> Vec<u8> {
    let mut data = u32::try_from(header.len()).unwrap().to_le_bytes().to_vec();
    data.extend_from_slice(header.as_bytes());
    for block in blocks {
        data.extend_from_slice(block);
    }
    data
}

/// 样例里的主题：调色板只改一项，光标颜色是 RGB。
const SETTINGS: &str = r#"{"background":[23,22,24],"foreground":[230,225,216],"palette":[[0,[57,58,61]]],
    "cursor_style":"block","cursor_blink":null,"cursor_color":{"rgb":[1,2,3]},"cursor_text":null,
    "selection_background":null,"selection_foreground":null,"search_background":{"rgb":[255,231,149]},
    "search_foreground":{"rgb":[0,0,0]},"search_selected_background":{"rgb":[242,165,126]},
    "search_selected_foreground":{"rgb":[0,0,0]},"option_as_alt":"false","scrollback_limit":10485760}"#;

fn settings() -> TermSettings {
    TermSettings {
        palette: vec![(0, Rgb(57, 58, 61))],
        cursor_color: Some(TerminalColor::Rgb(Rgb(1, 2, 3))),
        ..TermSettings::default()
    }
}

#[test]
fn successor_hello_is_readable() {
    let hello: ClientMsg = serde_json::from_str(
        r#"{"type":"hello","protocol":4,"build":"0.2.0+new","client":"successor","caps":{"snapshot":false,"vt_replay":false},"session":null,"device":null}"#,
    )
    .unwrap();
    assert!(matches!(
        hello,
        ClientMsg::Hello { protocol: 4, client: ClientKind::Successor, ref build, session: None, device: None, .. }
            if *build == BuildId("0.2.0+new".into())
    ));
}

/// 帧头也冻结着：协议版本对不上的两个宿主之间，`Hello` 和 `Welcome` 照样按这个帧头收发。
#[test]
fn hello_frame_is_readable() {
    let payload = br#"{"type":"hello","protocol":9,"build":"future","client":"successor"}"#;
    let mut stream = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
    stream.push(2);
    stream.extend_from_slice(&0u32.to_le_bytes());
    stream.extend_from_slice(payload);
    let frame = read_frame(&mut &stream[..]).unwrap().unwrap();
    assert_eq!((frame.kind, frame.channel), (FrameKind::Control, 0));
    assert!(matches!(frame.message::<ClientMsg>().unwrap(), ClientMsg::Hello { protocol: 9, .. }));
}

#[test]
fn welcome_with_handoff_is_readable() {
    let welcome: HostMsg = serde_json::from_str(
        r#"{"type":"welcome","protocol":4,"build":"0.1.0+old","host_pid":4242,"snapshot_format":1,"standalone":true,"handoff":1}"#,
    )
    .unwrap();
    assert_eq!(
        welcome,
        HostMsg::Welcome {
            protocol: 4,
            build: BuildId("0.1.0+old".into()),
            host_pid: 4242,
            snapshot_format: 1,
            standalone: true,
            handoff: 1,
        }
    );
    let incompatible: HostMsg =
        serde_json::from_str(r#"{"type":"incompatible","protocol":4,"build":"0.1.0+old","reason":"too new"}"#).unwrap();
    assert!(matches!(incompatible, HostMsg::Incompatible { protocol: 4, .. }));
}

#[test]
fn handoff_requests_are_readable() {
    let cases = [
        (r#"{"type":"handoff","min_format":1,"max_format":1}"#, ClientMsg::Handoff { min_format: 1, max_format: 1 }),
        (r#"{"type":"handoff_ready"}"#, ClientMsg::HandoffReady),
        (r#"{"type":"handoff_abort","reason":"boom"}"#, ClientMsg::HandoffAbort { reason: "boom".into() }),
        (r#"{"type":"handoff_done"}"#, ClientMsg::HandoffDone),
    ];
    for (json, expected) in cases {
        assert_eq!(serde_json::from_str::<ClientMsg>(json).unwrap(), expected, "{json}");
    }
}

/// `Goodbye` 的各种原因，包括以后的版本才有的：认不出的原因读成 `Unknown`，不至于整条读不了。
#[test]
fn goodbye_reasons_are_readable() {
    let cases = [
        (r#"{"kind":"shutdown"}"#, GoodbyeReason::Shutdown),
        (r#"{"kind":"handoff"}"#, GoodbyeReason::Handoff),
        (r#"{"kind":"idle"}"#, GoodbyeReason::Idle),
        (r#"{"kind":"error","message":"boom"}"#, GoodbyeReason::Error { message: "boom".into() }),
        (r#"{"kind":"maintenance","until":1790000000}"#, GoodbyeReason::Unknown),
    ];
    for (reason, expected) in cases {
        let json = format!(r#"{{"type":"goodbye","reason":{reason}}}"#);
        assert_eq!(serde_json::from_str::<HostMsg>(&json).unwrap(), HostMsg::Goodbye { reason: expected }, "{json}");
    }
}

#[test]
fn handoff_answers_are_readable() {
    let cases = [
        (r#"{"type":"handoff_begin","format":1,"sessions":3}"#, HostMsg::HandoffBegin { format: 1, sessions: 3 }),
        (
            r#"{"type":"handoff_refused","reason":{"kind":"desktop_connected"}}"#,
            HostMsg::HandoffRefused { reason: HandoffRefusal::DesktopConnected },
        ),
        (
            r#"{"type":"handoff_refused","reason":{"kind":"busy"}}"#,
            HostMsg::HandoffRefused { reason: HandoffRefusal::Busy },
        ),
        (
            r#"{"type":"handoff_refused","reason":{"kind":"not_standalone"}}"#,
            HostMsg::HandoffRefused { reason: HandoffRefusal::NotStandalone },
        ),
        (
            r#"{"type":"handoff_refused","reason":{"kind":"unsupported_format","writes":1}}"#,
            HostMsg::HandoffRefused { reason: HandoffRefusal::UnsupportedFormat { writes: 1 } },
        ),
        (r#"{"type":"goodbye","reason":{"kind":"handoff"}}"#, HostMsg::Goodbye { reason: GoodbyeReason::Handoff }),
    ];
    for (json, expected) in cases {
        assert_eq!(serde_json::from_str::<HostMsg>(json).unwrap(), expected, "{json}");
    }
}

#[test]
fn host_part_is_readable() {
    let header = format!(
        r#"{{"part":{{"type":"host","format":1,"build":"0.1.0+old","snapshot_format":1,"sessions":2,
            "theme":{SETTINGS},"record_history":true,"socket":"/tmp/runode/host.sock"}},"blocks":[]}}"#
    );
    let data = message(&header, &[]);
    let (part, blocks) = decode_part(&data).unwrap();
    assert_eq!(
        part,
        HandoffPart::Host {
            format: 1,
            build: BuildId("0.1.0+old".into()),
            snapshot_format: 1,
            sessions: 2,
            theme: Some(settings()),
            record_history: true,
            socket: "/tmp/runode/host.sock".into(),
            // 格式 1 的旧宿主没有这一项，按默认的规矩读。
            clipboard: ClipboardAccess::default(),
        }
    );
    assert!(blocks.is_empty());
}

#[test]
fn session_part_is_readable() {
    let header = format!(
        r#"{{"part":{{"type":"session","id":"{ID}","started":true,"pid":777,
            "size":{{"cols":80,"rows":24,"cell_width_px":16,"cell_height_px":32}},
            "report_token":"0f1e2d3c","settings":{SETTINGS},"shell":"/bin/zsh","start_dir":"/tmp",
            "env":[["FOO","bar"]],
            "meta":{{"title":"vim","fallback_title":null,"agent":null,"cwd":"/tmp","prompt_cwd":null,
                "foreground_is_shell":false,"shell_path":{{"Unix":[47,98,105,110]}},
                "shell_names":{{"aliases":[],"alias_values":[],"functions":[],"builtins":[],"keywords":[]}},
                "foreground":"vim","driver":null}},
            "prompt_reported":true,"running":{{"cmd":"vim a.txt","cwd":"/tmp","ts":1790000000}},
            "pending_shell_cwd":"/tmp","pending_command":null,"redactor":{{"matched":3,"inside":false}}}},
          "blocks":[4,2]}}"#
    );
    let data = message(&header, &[b"snap", b"rp"]);
    let (part, blocks) = decode_part(&data).unwrap();
    let HandoffPart::Session {
        id,
        started,
        pid,
        size,
        report_token,
        settings: theme,
        shell,
        start_dir,
        env,
        meta,
        prompt_reported,
        running,
        pending_shell_cwd,
        pending_command,
        redactor,
    } = part
    else {
        panic!("not a session: {part:?}");
    };
    assert_eq!(id, ID.parse::<SessionId>().unwrap());
    assert!(started && prompt_reported);
    assert_eq!(pid, Some(777));
    assert_eq!(size, GridSize { cols: 80, rows: 24, cell_width_px: 16, cell_height_px: 32 });
    assert_eq!(report_token, Some(ReportToken("0f1e2d3c".into())));
    assert_eq!(theme, settings());
    assert_eq!(shell.as_deref(), Some("/bin/zsh"));
    assert_eq!(start_dir, Some("/tmp".into()));
    assert_eq!(env, vec![("FOO".to_owned(), "bar".to_owned())]);
    assert_eq!((meta.title.as_deref(), meta.foreground.as_deref()), (Some("vim"), Some("vim")));
    assert_eq!(meta.shell_path, Some("/bin".into()));
    assert_eq!(running, Some(RunningCommand { cmd: "vim a.txt".into(), cwd: Some("/tmp".into()), ts: 1_790_000_000 }));
    assert_eq!(pending_shell_cwd, Some("/tmp".into()));
    // `null` 是收到了不带原文的 `command` 报告，和没有这个字段不一样。
    assert_eq!(pending_command, Some(None));
    assert_eq!(redactor, RedactorState { matched: 3, inside: false });
    assert_eq!(blocks, [&b"snap"[..], &b"rp"[..]]);
}

/// 没启动的会话：可以缺省的字段都不写。
#[test]
fn minimal_session_part_is_readable() {
    let header = format!(
        r#"{{"part":{{"type":"session","id":"{ID}","started":false,
            "size":{{"cols":80,"rows":24,"cell_width_px":16,"cell_height_px":32}},"settings":{SETTINGS}}},
          "blocks":[0,0]}}"#
    );
    let data = message(&header, &[b"", b""]);
    let (part, blocks) = decode_part(&data).unwrap();
    assert!(matches!(
        part,
        HandoffPart::Session {
            started: false,
            pid: None,
            report_token: None,
            shell: None,
            start_dir: None,
            prompt_reported: false,
            running: None,
            pending_shell_cwd: None,
            pending_command: None,
            redactor: RedactorState { matched: 0, inside: false },
            ..
        }
    ));
    assert_eq!(blocks, [&b""[..], &b""[..]]);
}

#[test]
fn commit_part_is_readable() {
    let header = format!(r#"{{"part":{{"type":"commit","pending_input":["{ID}","{OTHER}"]}},"blocks":[3,1]}}"#);
    let data = message(&header, &[b"ls\r", b"q"]);
    let (part, blocks) = decode_part(&data).unwrap();
    assert_eq!(part, HandoffPart::Commit { pending_input: vec![ID.parse().unwrap(), OTHER.parse().unwrap()] });
    assert_eq!(blocks, [&b"ls\r"[..], &b"q"[..]]);
}
