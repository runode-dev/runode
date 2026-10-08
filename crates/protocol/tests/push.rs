//! 推送的线上格式对着 `tests/fixtures/push` 里的样例：样例是照规格另外写出来的（不由这里的类型生成），
//! 手机端的测试读同一批文件。APNs payload 和中转请求体逐字节比对，测试失败时先看是不是改了线上格式，
//! 要改回来，不要改样例。

use std::path::Path;

use runode_protocol::{
    SessionId,
    push::{
        ActivityAttributes, ActivityContent, ActivityEvent, ApnsEnv, BLOCKED_LINES, LINE_LIMIT, MAX_APNS_PAYLOAD,
        PushError, RELAY_VERSION, RelayBroadcast, RelayChannel, RelayCreateChannel, RelayDeleteChannel, RelayStart,
        TEXT_LIMIT, end_payload, preview_lines, start_payload, update_payload,
    },
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    session::SessionMeta,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

const NOW: u64 = 1_760_000_000;
const TOKEN: &str = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011";
const CHANNEL: &str = "mLiGCQf1Ee+wAAAAAAAAAA==";

/// 样例文件的内容，去掉结尾的换行。
fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/push").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    text.trim_end_matches('\n').to_owned()
}

/// 样例读成 `expected`，`expected` 写出来和样例逐字节一样。
fn matches<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, expected: &T) {
    let text = fixture(name);
    let read: T = serde_json::from_str(&text).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert_eq!(&read, expected, "{name}");
    assert_eq!(serde_json::to_string(expected).unwrap(), text, "{name}");
}

fn attributes() -> ActivityAttributes {
    ActivityAttributes::new(
        "6F9619FF-8B86-D011-B42D-00C04FC964FF".into(),
        "Ethan 的 MacBook Pro",
        SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677),
    )
}

fn meta() -> SessionMeta {
    SessionMeta {
        title: Some("修 bug".into()),
        fallback_title: Some("claude".into()),
        agent: Some(Agent { kind: AgentKind::Claude, state: AgentState::Blocked }),
        ..SessionMeta::default()
    }
}

fn screen() -> &'static str {
    "╭──────────╮\n│ Bash     │\n╰──────────╯\n\nDo you want to proceed?   \n❯ 1. Yes\n  \
     2. No, and tell Claude what to do differently (esc)\n\n\t \n"
}

fn content() -> ActivityContent {
    ActivityContent::new(&meta(), &preview_lines(screen(), BLOCKED_LINES))
}

fn ended() -> ActivityContent {
    ActivityContent::new(&meta(), &[])
}

#[test]
fn attributes_and_content_match_the_fixtures() {
    matches("attributes.json", &attributes());
    matches("content_state.json", &content());
}

#[test]
fn apns_payloads_match_the_fixtures() {
    let start = start_payload(&attributes(), &content(), CHANNEL, NOW).unwrap();
    assert_eq!(String::from_utf8(start).unwrap(), fixture("apns_start.json"));
    let update = update_payload(&content(), NOW + 60).unwrap();
    assert_eq!(String::from_utf8(update).unwrap(), fixture("apns_update.json"));
    let end = end_payload(&ended(), NOW + 120).unwrap();
    assert_eq!(String::from_utf8(end).unwrap(), fixture("apns_end.json"));
}

#[test]
fn relay_requests_match_the_fixtures() {
    matches("relay_create_channel.json", &RelayCreateChannel { v: RELAY_VERSION, env: ApnsEnv::Development });
    matches("relay_channel.json", &RelayChannel { channel: CHANNEL.into() });
    matches(
        "relay_delete_channel.json",
        &RelayDeleteChannel { v: RELAY_VERSION, env: ApnsEnv::Production, channel: CHANNEL.into() },
    );
    let start = RelayStart::new(ApnsEnv::Production, TOKEN.into(), CHANNEL.into(), attributes(), &content(), NOW);
    matches("relay_start.json", &start.unwrap());
    let update = RelayBroadcast::update(ApnsEnv::Production, CHANNEL.into(), &content(), NOW + 60).unwrap();
    assert_eq!((update.event, update.stale_date), (ActivityEvent::Update, Some(NOW + 60 + 1800)));
    matches("relay_update.json", &update);
    let end = RelayBroadcast::end(ApnsEnv::Development, CHANNEL.into(), &ended(), NOW + 120).unwrap();
    matches("relay_end.json", &end);
    matches("push_error.json", &PushError { reason: "BadDeviceToken".into() });
}

/// 中转请求体里的内容和直连时的 payload 一样照上限删减过：中转服务拼出的 payload 也装得下。
#[test]
fn relay_requests_carry_fitted_content() {
    let mut content = content();
    content.lines = (0..3).map(|i| format!("{i}{}", "x".repeat(1500))).collect();
    let start =
        RelayStart::new(ApnsEnv::Production, TOKEN.into(), CHANNEL.into(), attributes(), &content, NOW).unwrap();
    let payload: Value =
        serde_json::from_slice(&start_payload(&attributes(), &content, CHANNEL, NOW).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&start.content).unwrap(), payload["aps"]["content-state"]);
    assert_eq!(start.content.lines.len(), 2);
    let update = RelayBroadcast::update(ApnsEnv::Production, CHANNEL.into(), &content, NOW).unwrap();
    assert_eq!(update.content.lines.len(), 2);
}

/// 比自己新的一方才有的环境读成 `Unknown`，不至于整条读不了。
#[test]
fn unknown_environments_are_read_as_unknown() {
    assert_eq!(serde_json::from_str::<ApnsEnv>(r#""sandbox_2""#).unwrap(), ApnsEnv::Unknown);
    assert_eq!(serde_json::to_string(&ApnsEnv::Development).unwrap(), r#""development""#);
}

/// 和手机端 `Presentation.previewLines` 一样：去掉行尾空白，跳过空行、只有空白和制表符的行，取最后几行。
#[test]
fn preview_lines_keep_the_last_meaningful_lines() {
    assert_eq!(
        preview_lines(screen(), 3),
        ["Do you want to proceed?", "❯ 1. Yes", "  2. No, and tell Claude what to do differently (esc)"]
    );
    assert_eq!(preview_lines(screen(), 10)[0], "│ Bash     │");
    assert_eq!(preview_lines(screen(), 0), Vec::<String>::new());
    assert_eq!(preview_lines("", 3), Vec::<String>::new());
    assert_eq!(preview_lines("a\n─┼─\n\u{3000}\nb", 3), ["a", "b"]);
}

/// 标题依次用程序设置的、前台程序名或目录名，空的跳过，都没有时是「终端」；各项截到上限。
#[test]
fn content_falls_back_and_clips() {
    let mut meta = SessionMeta { title: Some(String::new()), fallback_title: Some("zsh".into()), ..meta() };
    assert_eq!(ActivityContent::new(&meta, &[]).title, "zsh");
    meta.fallback_title = None;
    let content = ActivityContent::new(&meta, &[]);
    assert_eq!((content.title.as_str(), content.agent.as_str()), ("终端", "Claude Code"));
    meta.agent = None;
    assert_eq!(ActivityContent::new(&meta, &[]).agent, "");

    meta.title = Some("长".repeat(TEXT_LIMIT + 1));
    let lines: Vec<String> = (0..5).map(|i| format!("{i}{}", "x".repeat(LINE_LIMIT))).collect();
    let content = ActivityContent::new(&meta, &lines);
    assert_eq!(content.title, format!("{}…", "长".repeat(TEXT_LIMIT - 1)));
    assert_eq!(content.lines.len(), BLOCKED_LINES);
    assert!(content.lines[0].starts_with('2'));
    assert!(content.lines.iter().all(|line| line.chars().count() == LINE_LIMIT && line.ends_with('…')));
    meta.title = Some("长".repeat(TEXT_LIMIT));
    assert_eq!(ActivityContent::new(&meta, &[]).title, "长".repeat(TEXT_LIMIT));

    let long = ActivityAttributes::new("m".into(), &"名".repeat(60), SessionId(1));
    assert_eq!(long.machine_name.chars().count(), TEXT_LIMIT);
}

/// payload 超过上限时先从最上面一行起删，删光了还超才缩短标题，提醒里的标题跟着一起短。
#[test]
fn oversized_payloads_drop_lines_then_shorten_the_title() {
    let attributes = attributes();
    let mut content = content();
    content.lines = (0..3).map(|i| format!("{i}{}", "x".repeat(1500))).collect();
    let payload: Value = serde_json::from_slice(&start_payload(&attributes, &content, CHANNEL, NOW).unwrap()).unwrap();
    let lines: Vec<&str> = payload["aps"]["content-state"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with('1') && lines[1].starts_with('2'));

    content.lines = vec!["x".repeat(5000)];
    content.title = "标".repeat(2000);
    for payload in [
        start_payload(&attributes, &content, CHANNEL, NOW).unwrap(),
        update_payload(&content, NOW).unwrap(),
        end_payload(&content, NOW).unwrap(),
    ] {
        assert!(payload.len() <= MAX_APNS_PAYLOAD, "{}", payload.len());
        let payload: Value = serde_json::from_slice(&payload).unwrap();
        let state = &payload["aps"]["content-state"];
        assert_eq!(state["lines"], Value::Array(vec![]));
        let title = state["title"].as_str().unwrap();
        assert!(title.ends_with('…') && title.chars().count() < 2000, "{title}");
        if let Some(alert) = payload["aps"].get("alert") {
            assert_eq!(alert["body"]["loc-args"][0].as_str(), Some(title));
        }
    }

    // 标题删光了也装不下：attributes 没按 `ActivityAttributes::new` 截过。
    let huge = ActivityAttributes { machine_name: "x".repeat(5000), ..attributes };
    assert_eq!(start_payload(&huge, &content, CHANNEL, NOW), None);
}
