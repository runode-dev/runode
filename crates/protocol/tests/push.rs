//! 推送的线上格式对着 `tests/fixtures/push` 里的样例：样例是照规格另外写出来的（不由这里的类型生成），
//! 手机端的测试读同一批文件。APNs payload 和中转请求体逐字节比对，测试失败时先看是不是改了线上格式，
//! 要改回来，不要改样例。

use std::path::Path;

use runode_protocol::{
    SessionId,
    push::{
        ApnsEnv, BLOCKED_LINES, LINE_LIMIT, MAX_APNS_PAYLOAD, Notification, NotificationKind, PushError, RELAY_NOTIFY,
        RELAY_NOTIFY_V3, RelayNotify, TEXT_LIMIT, notify_payload, preview_lines,
    },
};
use runode_types::{
    agent::{Agent, AgentKind, AgentState},
    session::SessionMeta,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

const NOW: u64 = 1_760_000_000;
const TOKEN: &str = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011";
const MACHINE: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
const SESSION: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);

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

/// 样例里的几行和手机端的测试共用，不随 `preview_lines` 的取法变。
fn notification() -> Notification {
    let lines = ["Bash command", "cargo test", "Do you want to proceed?"];
    Notification::new(MACHINE.into(), "Ethan 的 MacBook Pro", SESSION, &meta(), &lines.map(String::from))
}

#[test]
fn notifications_match_the_fixtures() {
    matches("notification.json", &notification());
    let payload = notify_payload(&notification()).unwrap();
    assert_eq!(String::from_utf8(payload).unwrap(), fixture("apns_notify.json"));
    assert_eq!(notification().collapse_id(), "0123456789abcdef0011223344556677");
}

/// 不知道是哪个 agent 时标题换一个本地化键、不带参数；没有屏幕文字时不写正文。
#[test]
fn unnamed_agents_and_empty_screens_have_their_own_shape() {
    let meta = SessionMeta { agent: None, ..meta() };
    let bare = Notification::new(MACHINE.into(), "Ethan 的 MacBook Pro", SESSION, &meta, &[]);
    assert_eq!(String::from_utf8(notify_payload(&bare).unwrap()).unwrap(), fixture("apns_notify_unnamed.json"));
}

#[test]
fn relay_requests_match_the_fixtures() {
    let notify = RelayNotify::new(ApnsEnv::Production, TOKEN.into(), &notification(), NOW).unwrap();
    assert_eq!(notify.expiration, NOW + 1800);
    matches("relay_notify.json", &notify);
    matches("push_error.json", &PushError { reason: "BadDeviceToken".into() });
}

/// 任务做完的通知：标题换成任务做完的键、不带参数，副标题同等回答的；没有正文。中转请求体带 `kind`、
/// `v` 是 3，发到 v3 的接口；等回答的仍是 v2，请求体里没有 `kind`。
#[test]
fn todo_done_notifications_match_the_fixtures() {
    let done = Notification::todo_done(MACHINE.into(), "Ethan 的 MacBook Pro", SESSION, "给设置页加暗色模式");
    assert_eq!(String::from_utf8(notify_payload(&done).unwrap()).unwrap(), fixture("apns_todo_done.json"));
    assert_eq!(done.collapse_id(), "0123456789abcdef0011223344556677");
    let notify = RelayNotify::new(ApnsEnv::Production, TOKEN.into(), &done, NOW).unwrap();
    matches("relay_notify_todo_done.json", &notify);
    assert_eq!(done.kind.relay(), (3, RELAY_NOTIFY_V3));
    assert_eq!(NotificationKind::Blocked.relay(), (2, RELAY_NOTIFY));
    assert_eq!((RELAY_NOTIFY, RELAY_NOTIFY_V3), ("/v2/notify", "/v3/notify"));

    let long = Notification::todo_done("m".into(), &"名".repeat(60), SESSION, &"长".repeat(60));
    assert_eq!((long.title.chars().count(), long.machine_name.chars().count()), (TEXT_LIMIT, TEXT_LIMIT));
}

/// 中转请求体里的内容和直连时的 payload 一样照上限删减过：中转服务拼出的 payload 也装得下。
#[test]
fn relay_requests_carry_fitted_content() {
    let mut notification = notification();
    notification.lines = (0..3).map(|i| format!("{i}{}", "x".repeat(1500))).collect();
    let notify = RelayNotify::new(ApnsEnv::Production, TOKEN.into(), &notification, NOW).unwrap();
    let payload: Value = serde_json::from_slice(&notify_payload(&notification).unwrap()).unwrap();
    assert_eq!(notify.notification.lines.join("\n"), payload["aps"]["alert"]["body"].as_str().unwrap());
    assert_eq!(notify.notification.lines.len(), 2);
}

/// 比自己新的一方才有的环境读成 `Unknown`，不至于整条读不了。
#[test]
fn unknown_environments_are_read_as_unknown() {
    assert_eq!(serde_json::from_str::<ApnsEnv>(r#""sandbox_2""#).unwrap(), ApnsEnv::Unknown);
    assert_eq!(serde_json::to_string(&ApnsEnv::Development).unwrap(), r#""development""#);
}

/// 和手机端 `Presentation.previewLines` 一样：去掉行尾空白，跳过空行、只有空白和制表符的行，取最后几行。
#[test]
fn preview_lines_keep_the_question_without_options_and_hints() {
    assert_eq!(preview_lines(screen(), 3), ["│ Bash     │", "Do you want to proceed?"]);
    assert_eq!(preview_lines(screen(), 1), ["Do you want to proceed?"]);
    assert_eq!(preview_lines(screen(), 0), Vec::<String>::new());
    assert_eq!(preview_lines("", 3), Vec::<String>::new());
    // 窄屏下 Claude 的权限确认：选项折到下一行的续行和底下的按键提示都不要。
    let narrow = " Bash command\n╌╌╌╌\n touch hello.txt\n╌╌╌╌\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and \
                  always allow access to\n      /private/tmp/e2e from this project\n   3. No\n\n Esc to cancel · Tab to \
                  amend\n";
    assert_eq!(preview_lines(narrow, 3), [" Bash command", " touch hello.txt", " Do you want to proceed?"]);
    // 提问：选项下面的说明、分隔线下的选项和折成两行的提示都不要。
    let question = " ☐ 午饭\n\n午饭吃什么？\n\n❯ 1. 面条\n     吃面条\n  2. 米饭\n     吃米饭\n────\n  3. Chat about \
                    this\n\nEnter to select · Esc to\ncancel\n";
    assert_eq!(preview_lines(question, 3), [" ☐ 午饭", "午饭吃什么？"]);
    // 没有选项时是最底下的几行。
    assert_eq!(preview_lines("a\n─┼─\n\u{3000}\nb\nContinue? (y/n)", 2), ["b", "Continue? (y/n)"]);
}

/// 标题依次用程序设置的、前台程序名或目录名，空的跳过，都没有时是「终端」；各项截到上限。
#[test]
fn notifications_fall_back_and_clip() {
    let new = |meta: &SessionMeta, lines: &[String]| Notification::new(MACHINE.into(), "Mac", SESSION, meta, lines);
    let mut meta = SessionMeta { title: Some(String::new()), fallback_title: Some("zsh".into()), ..meta() };
    assert_eq!(new(&meta, &[]).title, "zsh");
    meta.fallback_title = None;
    let notification = new(&meta, &[]);
    assert_eq!((notification.title.as_str(), notification.agent.as_str()), ("终端", "Claude Code"));
    meta.agent = None;
    assert_eq!(new(&meta, &[]).agent, "");

    meta.title = Some("长".repeat(TEXT_LIMIT + 1));
    let lines: Vec<String> = (0..5).map(|i| format!("{i}{}", "x".repeat(LINE_LIMIT))).collect();
    let notification = new(&meta, &lines);
    assert_eq!(notification.title, format!("{}…", "长".repeat(TEXT_LIMIT - 1)));
    assert_eq!(notification.lines.len(), BLOCKED_LINES);
    assert!(notification.lines[0].starts_with('2'));
    assert!(notification.lines.iter().all(|line| line.chars().count() == LINE_LIMIT && line.ends_with('…')));
    meta.title = Some("长".repeat(TEXT_LIMIT));
    assert_eq!(new(&meta, &[]).title, "长".repeat(TEXT_LIMIT));

    let long = Notification::new("m".into(), &"名".repeat(60), SessionId(1), &meta, &[]);
    assert_eq!(long.machine_name.chars().count(), TEXT_LIMIT);
}

/// payload 超过上限时先从最上面一行起删，删光了还超才缩短标题，副标题里的标题跟着一起短。
#[test]
fn oversized_payloads_drop_lines_then_shorten_the_title() {
    let mut notification = notification();
    notification.lines = (0..3).map(|i| format!("{i}{}", "x".repeat(1500))).collect();
    let payload: Value = serde_json::from_slice(&notify_payload(&notification).unwrap()).unwrap();
    let body = payload["aps"]["alert"]["body"].as_str().unwrap();
    let lines: Vec<&str> = body.split('\n').collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with('1') && lines[1].starts_with('2'));

    notification.lines = vec!["x".repeat(5000)];
    notification.title = "标".repeat(2000);
    let payload = notify_payload(&notification).unwrap();
    assert!(payload.len() <= MAX_APNS_PAYLOAD, "{}", payload.len());
    let payload: Value = serde_json::from_slice(&payload).unwrap();
    let alert = &payload["aps"]["alert"];
    assert_eq!(alert.get("body"), None);
    let title = alert["subtitle-loc-args"][0].as_str().unwrap();
    assert!(title.ends_with('…') && title.chars().count() < 2000, "{title}");

    // 标题删光了也装不下：电脑名没按 `Notification::new` 截过。
    let huge = Notification { machine_name: "x".repeat(5000), ..notification };
    assert_eq!(notify_payload(&huge), None);
}
