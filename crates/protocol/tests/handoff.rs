//! 交接消息的编解码：往返、各种损坏的数据、`pending_command` 的三种状态、`Commit` 的 `at_prompt` 和旧版本
//! 读得了的样子，以及口令不进 `Debug`。

use runode_protocol::{
    AtPrompt, BuildId, HANDOFF_FORMAT, HandoffPart, HandoffPartError, OLDEST_READABLE_HANDOFF_FORMAT, RedactorState,
    ReportToken, RunningCommand, SessionId, WindowLayout, decode_part, encode_part,
};
use runode_types::{
    agent::{Agent, AgentKind, AgentState},
    clipboard::{ClipboardAccess, ClipboardRead, ClipboardWrite},
    grid::GridSize,
    session::SessionMeta,
    settings::TermSettings,
};

const ID: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);

fn host() -> HandoffPart {
    HandoffPart::Host {
        format: HANDOFF_FORMAT,
        build: BuildId("0.1.0+abc".into()),
        snapshot_format: 1,
        sessions: 2,
        theme: Some(TermSettings { cursor_blink: Some(false), ..TermSettings::default() }),
        record_history: false,
        socket: "/tmp/runode/host.sock".into(),
        clipboard: ClipboardAccess { write: ClipboardWrite::Deny, read: ClipboardRead::Allow },
        layout: vec![WindowLayout { index: 1, front: true, workspaces: Vec::new() }],
    }
}

fn session(pending_command: Option<Option<String>>) -> HandoffPart {
    HandoffPart::Session {
        id: ID,
        started: true,
        pid: Some(4242),
        size: GridSize { cols: 120, rows: 40, cell_width_px: 16, cell_height_px: 32 },
        report_token: Some(ReportToken("secret-token".into())),
        settings: TermSettings::default(),
        shell: Some("/bin/zsh".into()),
        start_dir: Some("/tmp/中文".into()),
        meta: Box::new(SessionMeta {
            title: Some("修 bug".into()),
            agent: Some(Agent { kind: AgentKind::Claude, state: AgentState::Working }),
            cwd: Some("/tmp".into()),
            shell_path: Some("/bin:/usr/bin".into()),
            ..SessionMeta::default()
        }),
        prompt_reported: true,
        running: Some(RunningCommand { cmd: "cargo test".into(), cwd: Some("/tmp".into()), ts: 1_790_000_000 }),
        pending_shell_cwd: None,
        pending_command,
        redactor: RedactorState { matched: 2, inside: true },
    }
}

fn commit() -> HandoffPart {
    HandoffPart::Commit { pending_input: vec![ID, SessionId(2)], at_prompt: vec![AtPrompt { id: ID, len: 1 }] }
}

/// 编好的消息的头（JSON）。
fn header_of(data: &[u8]) -> String {
    let len = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    String::from_utf8(data[4..4 + len].to_vec()).unwrap()
}

#[test]
fn formats_are_in_order() {
    const { assert!(OLDEST_READABLE_HANDOFF_FORMAT >= 1 && OLDEST_READABLE_HANDOFF_FORMAT <= HANDOFF_FORMAT) };
}

#[test]
fn parts_round_trip() {
    let snapshot: Vec<u8> = (0..=255).cycle().take(70_000).collect();
    let cases: [(HandoffPart, Vec<&[u8]>); 6] = [
        (host(), vec![]),
        (session(None), vec![&snapshot, b"\x1b[2J\x1b[Hhello"]),
        (session(Some(None)), vec![b"", b""]),
        (session(Some(Some("ls -la".into()))), vec![b"", b"x"]),
        (commit(), vec![b"\x03", b"ls\r"]),
        (HandoffPart::Commit { pending_input: Vec::new(), at_prompt: Vec::new() }, vec![]),
    ];
    for (part, blocks) in cases {
        let data = encode_part(&part, &blocks).unwrap();
        let (decoded, decoded_blocks) = decode_part(&data).unwrap();
        assert_eq!(decoded, part);
        assert_eq!(decoded_blocks, blocks);
    }
}

/// 路径不是 UTF-8 的会话照样交得出去：交接自己的路径原样交给新宿主，`meta` 里的有损地写。
#[test]
fn non_utf8_paths_are_handed_over() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _, path::PathBuf};

    let odd = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff"));
    let HandoffPart::Session {
        id,
        started,
        pid,
        size,
        report_token,
        settings,
        shell,
        mut meta,
        prompt_reported,
        running,
        pending_command,
        redactor,
        ..
    } = session(None)
    else {
        unreachable!()
    };
    meta.prompt_cwd = Some(odd.clone());
    let part = HandoffPart::Session {
        id,
        started,
        pid,
        size,
        report_token,
        settings,
        shell,
        start_dir: Some(odd.clone()),
        meta,
        prompt_reported,
        running: running.map(|running| RunningCommand { cwd: Some(odd.clone()), ..running }),
        pending_shell_cwd: Some(odd.clone()),
        pending_command,
        redactor,
    };
    let (decoded, _) = decode_part(&encode_part(&part, &[b"", b""]).unwrap()).unwrap();
    let HandoffPart::Session { start_dir, running, pending_shell_cwd, meta, .. } = decoded else { unreachable!() };
    assert_eq!((start_dir.as_ref(), pending_shell_cwd.as_ref()), (Some(&odd), Some(&odd)));
    assert_eq!(running.unwrap().cwd.as_ref(), Some(&odd));
    assert_eq!(meta.prompt_cwd, Some(PathBuf::from("/tmp/\u{fffd}")));

    let HandoffPart::Host {
        format, build, snapshot_format, sessions, theme, record_history, clipboard, layout, ..
    } = host()
    else {
        unreachable!()
    };
    let part = HandoffPart::Host {
        format,
        build,
        snapshot_format,
        sessions,
        theme,
        record_history,
        socket: odd.join("host.sock"),
        clipboard,
        layout,
    };
    assert_eq!(decode_part(&encode_part(&part, &[]).unwrap()).unwrap().0, part);
}

/// `pending_command` 的三种状态在 JSON 里各不相同：没有字段、`null`、字符串。
#[test]
fn pending_command_keeps_all_three_states() {
    let header = |part: &HandoffPart| header_of(&encode_part(part, &[b"", b""]).unwrap());
    assert!(!header(&session(None)).contains("pending_command"));
    assert!(header(&session(Some(None))).contains(r#""pending_command":null"#));
    assert!(header(&session(Some(Some("ls".into())))).contains(r#""pending_command":"ls""#));
}

/// 没有等提示符的输入时 `Commit` 不写 `at_prompt`，和不认识它的版本写的一样。
#[test]
fn an_empty_at_prompt_is_left_out() {
    let part = HandoffPart::Commit { pending_input: vec![ID], at_prompt: Vec::new() };
    assert!(!header_of(&encode_part(&part, &[b"x"]).unwrap()).contains("at_prompt"));
}

/// 不认识 `at_prompt` 的版本（这里照它冻结的样子写一份 `Commit`）照样读得了带着它的 `Commit`，块也一块不少：
/// 等提示符的字节在块末尾，它整块当场写，不丢。
#[test]
fn a_commit_with_at_prompt_reads_in_an_older_version() {
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum OldPart {
        Commit { pending_input: Vec<SessionId> },
    }
    #[derive(serde::Deserialize)]
    struct OldHeader {
        part: OldPart,
        blocks: Vec<u64>,
    }
    let data = encode_part(&commit(), &[b"ls\ragent\r", b"q"]).unwrap();
    let header: OldHeader = serde_json::from_str(&header_of(&data)).unwrap();
    let OldPart::Commit { pending_input } = header.part;
    assert_eq!(pending_input, [ID, SessionId(2)]);
    assert_eq!(header.blocks, [9, 1]);
}

/// 以后的版本在已有的块后面加的块，读的一方不认识就不管。
#[test]
fn extra_blocks_are_kept() {
    let data = encode_part(&session(None), &[b"a", b"b", b"c"]).unwrap();
    let (_, blocks) = decode_part(&data).unwrap();
    assert_eq!(blocks, [&b"a"[..], &b"b"[..], &b"c"[..]]);
}

#[test]
fn too_few_blocks_are_rejected() {
    let data = encode_part(&session(None), &[b"snapshot"]).unwrap();
    assert!(matches!(decode_part(&data), Err(HandoffPartError::MissingBlocks { needed: 2, got: 1 })));
    let data = encode_part(&commit(), &[b"x"]).unwrap();
    assert!(matches!(decode_part(&data), Err(HandoffPartError::MissingBlocks { needed: 2, got: 1 })));
}

#[test]
fn truncated_data_is_rejected() {
    let data = encode_part(&session(None), &[b"snapshot", b"replay"]).unwrap();
    // 每一个比完整数据短的前缀都读不了，也不会越界。
    for len in 0..data.len() {
        let result = decode_part(&data[..len]);
        assert!(matches!(result, Err(HandoffPartError::Truncated)), "prefix of {len} bytes: {result:?}");
    }
    assert!(matches!(decode_part(&[]), Err(HandoffPartError::Truncated)));
    assert!(matches!(decode_part(&[1, 0, 0]), Err(HandoffPartError::Truncated)));
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut data = encode_part(&host(), &[]).unwrap();
    data.extend_from_slice(b"junk");
    assert!(matches!(decode_part(&data), Err(HandoffPartError::Trailing(4))));
}

/// 头里写的长度不像样（比数据长得多）时报错，不照着它分配内存。
#[test]
fn absurd_lengths_are_rejected() {
    let mut data = u32::MAX.to_le_bytes().to_vec();
    data.extend_from_slice(b"{}");
    assert!(matches!(decode_part(&data), Err(HandoffPartError::Truncated)));

    let header = format!(r#"{{"part":{{"type":"commit","pending_input":[]}},"blocks":[{}]}}"#, u64::MAX);
    let mut data = u32::try_from(header.len()).unwrap().to_le_bytes().to_vec();
    data.extend_from_slice(header.as_bytes());
    data.extend_from_slice(b"short");
    assert!(matches!(decode_part(&data), Err(HandoffPartError::Truncated)));

    let blocks = vec!["1"; 100_000].join(",");
    let header = format!(r#"{{"part":{{"type":"commit","pending_input":[]}},"blocks":[{blocks}]}}"#);
    let mut data = u32::try_from(header.len()).unwrap().to_le_bytes().to_vec();
    data.extend_from_slice(header.as_bytes());
    assert!(matches!(decode_part(&data), Err(HandoffPartError::Truncated)));
}

#[test]
fn unreadable_headers_are_rejected() {
    for header in [
        "not json",
        r#"{"part":{"type":"teleport"},"blocks":[]}"#,
        r#"{"part":{"type":"commit","pending_input":[]}}"#,
        r#"{"blocks":[]}"#,
        r#"{"part":{"type":"commit","pending_input":["xyz"]},"blocks":[]}"#,
    ] {
        let mut data = u32::try_from(header.len()).unwrap().to_le_bytes().to_vec();
        data.extend_from_slice(header.as_bytes());
        assert!(matches!(decode_part(&data), Err(HandoffPartError::Json(_))), "{header}");
    }
}

/// 读不懂的布局（比如后来的版本给它加了必填的字段）当空的读，整条 `Host` 照样读得了。
#[test]
fn an_unreadable_layout_reads_as_none() {
    let mut header = serde_json::json!({ "part": host(), "blocks": [] });
    header["part"]["layout"] = serde_json::json!([{ "index": 1, "workspaces": [{ "future": true }] }]);
    let header = header.to_string();
    let mut data = u32::try_from(header.len()).unwrap().to_le_bytes().to_vec();
    data.extend_from_slice(header.as_bytes());
    let (part, _) = decode_part(&data).unwrap();
    let mut expected = host();
    if let HandoffPart::Host { layout, .. } = &mut expected {
        layout.clear();
    }
    assert_eq!(part, expected);
}

/// 交接的消息可能进日志，`Debug` 不打出报告口令。
#[test]
fn debug_hides_the_report_token() {
    let printed = format!("{:?}", session(None));
    assert!(!printed.contains("secret-token"), "{printed}");
    assert!(printed.contains("<redacted>"), "{printed}");
}
