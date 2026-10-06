//! 各个命令对着假宿主跑：发了什么请求、打印了什么、退出码对不对。

mod common;

use common::*;
use runode_cli::{Env, exit};
use runode_protocol::{AttachMode, ClientMsg, HostMsg, SessionId};
use runode_shared_types::agent::{AgentKind, AgentState};

const A: u128 = 0xabcd_0000_0000_0000_0000_0000_0000_0001;
const B: u128 = 0xabce_0000_0000_0000_0000_0000_0000_0002;

/// 回列会话和连上的假宿主；连上时的状态是 `state`，接着依次发 `then` 里的状态。
fn host(name: &str, state: Option<AgentState>, then: Vec<AgentState>) -> FakeHost {
    FakeHost::start(name, move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList {
            sessions: vec![
                session(id(A), meta("claude here", state.map(|state| (AgentKind::Claude, state)))),
                session(id(B), meta("zsh", None)),
            ],
        }],
        ClientMsg::Attach { id, .. } => {
            let mut replies = vec![HostMsg::Attached {
                id: *id,
                channel: 3,
                size: SIZE,
                mode: AttachMode::MetaOnly,
                meta: meta("claude here", state.map(|state| (AgentKind::Claude, state))),
                settings: None,
            }];
            replies.extend(then.iter().map(|&state| agent_now(*id, state)));
            replies
        }
        ClientMsg::ReadScreen { id, lines, .. } => {
            vec![HostMsg::ScreenText { id: *id, text: format!("screen of {id}, {lines:?} lines\n"), truncated: false }]
        }
        ClientMsg::Layout { req } => no_window(*req),
        _ => vec![],
    })
}

/// app 没开窗口时宿主对 `Layout` 的回话。
fn no_window(req: u32) -> Vec<HostMsg> {
    vec![HostMsg::Error { req: Some(req), id: None, message: "there is no runode window to do this in".into() }]
}

fn full(n: u128) -> String {
    SessionId(n).to_string()
}

#[test]
fn help_and_version_need_no_app() {
    let env = Env { build: "1.2.3".into(), ..Env::default() };
    let (code, out, _) = run("help", &env);
    assert_eq!(code, exit::OK);
    assert!(out.contains("usage: runode"));
    assert_eq!(run("--version", &env).1, "runode 1.2.3\n");
    let (code, _, err) = run("frobnicate", &env);
    assert_eq!(code, exit::USAGE);
    assert!(err.contains("unknown command frobnicate"), "{err}");
}

#[test]
fn list_shows_sessions_and_marks_your_own() {
    let mut fake = host("list", Some(AgentState::Working), vec![]);
    fake.env.session = Some(full(B));
    let (code, out, _) = run("list", &fake.env);
    assert_eq!(code, exit::OK);
    let lines: Vec<&str> = out.lines().collect();
    let words = |line: &str| line.split_whitespace().map(str::to_owned).collect::<Vec<_>>().join(" ");
    // app 没开窗口：没有位置那几列，都是后台会话。
    assert_eq!(words(lines[0]), "ID AGENT STATE FG TITLE DIR VIEW", "{out}");
    assert_eq!(words(lines[1]), "abcd0000 Claude Code working - claude here /tmp/project bg", "{out}");
    assert!(lines[2].starts_with("* abce0000  -"), "{out}");
}

#[test]
fn list_as_json() {
    let fake = host("json", Some(AgentState::Blocked), vec![]);
    let (code, out, _) = run("list --json", &fake.env);
    assert_eq!(code, exit::OK);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["self"], serde_json::Value::Null);
    assert_eq!(json["layout"], serde_json::Value::Null);
    let sessions = &json["sessions"];
    assert_eq!(sessions[0]["id"], full(A));
    assert_eq!(sessions[0]["agent"], "claude");
    assert_eq!(sessions[0]["state"], "blocked");
    assert_eq!(sessions[0]["claimed"], true);
    assert_eq!(sessions[0]["driver"], serde_json::Value::Null);
    assert_eq!(sessions[0]["place"], serde_json::Value::Null);
    assert_eq!(sessions[0]["view"], "bg");
    assert_eq!(sessions[1]["agent"], serde_json::Value::Null);
}

#[test]
fn read_finds_the_session_by_prefix() {
    let fake = host("read", None, vec![]);
    let (code, out, err) = run("read ABCD --lines 5", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, format!("screen of {}, Some(5) lines\n", full(A)));
}

#[test]
fn read_defaults_to_your_own_session() {
    let mut fake = host("own", None, vec![]);
    fake.env.session = Some(full(B));
    let (code, out, _) = run("read", &fake.env);
    assert_eq!(code, exit::OK);
    assert!(out.starts_with(&format!("screen of {}", full(B))), "{out}");

    let (code, _, err) = run("read", &Env::default());
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("outside a runode terminal"), "{err}");
}

#[test]
fn unclear_sessions_are_refused() {
    let fake = host("unclear", None, vec![]);
    let (code, _, err) = run("read abc", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("more than one session"), "{err}");
    let fake = host("missing", None, vec![]);
    let (code, _, err) = run("read 99", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("no session 99"), "{err}");
    // 认不出的写法是参数错误。
    let (code, _, err) = run("read sideways", &fake.env);
    assert_eq!(code, exit::USAGE);
    assert!(err.contains("sideways"), "{err}");
}

#[test]
fn send_types_the_text_then_enter() {
    let fake = host("send", None, vec![]);
    let (code, _, err) = run("send abcd fix the  tests --enter", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let inputs: Vec<_> = fake.drain().into_iter().filter(|r| matches!(r, Received::Input(..))).collect();
    assert_eq!(inputs, [Received::Input(3, b"fix the tests".to_vec()), Received::Input(3, b"\r".to_vec())],);
}

#[test]
fn send_can_wait_until_the_agent_is_done() {
    let fake = host("sendwait", Some(AgentState::Idle), vec![AgentState::Working, AgentState::Idle]);
    let (code, out, err) = run("send abcd go --enter --wait", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "idle\n");
    // 先连上再打字，打字之后 agent 的状态变化才不会漏掉。
    let order: Vec<_> = fake
        .drain()
        .into_iter()
        .filter_map(|r| match r {
            Received::Control(ClientMsg::Attach { .. }) => Some("attach"),
            Received::Input(..) => Some("input"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["attach", "input", "input"]);
}

#[test]
fn wait_returns_when_the_agent_stops() {
    let fake = host("wait", Some(AgentState::Working), vec![AgentState::Working, AgentState::Blocked]);
    let (code, out, _) = run("wait abcd", &fake.env);
    assert_eq!(code, exit::OK);
    assert_eq!(out, "blocked\n");

    // 已经停着的不用等；done 要先看到它干活。
    let fake = host("stopped", Some(AgentState::Idle), vec![]);
    assert_eq!(run("wait abcd", &fake.env).1, "idle\n");
    let fake = host("done", Some(AgentState::Idle), vec![]);
    let (code, _, _) = run("wait abcd --for done --timeout 0.2", &fake.env);
    assert_eq!(code, exit::TIMEOUT);
}

#[test]
fn wait_reports_an_exited_session() {
    let fake = FakeHost::start("exited", |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(A), meta("x", None))] }],
        ClientMsg::Attach { id, .. } => vec![
            HostMsg::Attached {
                id: *id,
                channel: 1,
                size: SIZE,
                mode: AttachMode::MetaOnly,
                meta: meta("x", Some((AgentKind::Codex, AgentState::Working))),
                settings: None,
            },
            HostMsg::Exited { id: *id, status: None },
        ],
        _ => vec![],
    });
    let (code, _, err) = run("wait abcd", &fake.env);
    assert_eq!(code, exit::EXITED, "{err}");
}

#[test]
fn an_absent_app_is_reported() {
    let env = Env { socket: Some(std::env::temp_dir().join("rnc-nobody.sock")), ..Env::default() };
    let (code, _, err) = run("list", &env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("is it running?"), "{err}");
}

#[test]
fn app_launch_arguments_open_the_window() {
    let args = |args: &[&str]| args.iter().map(Into::into).collect::<Vec<std::ffi::OsString>>();
    assert!(!runode_cli::wants_cli(&args(&[])));
    assert!(!runode_cli::wants_cli(&args(&["-psn_0_12345"])));
    assert!(!runode_cli::wants_cli(&args(&["-NSDocumentRevisionsDebugMode", "YES"])));
    assert!(runode_cli::wants_cli(&args(&["list"])));
    assert!(runode_cli::wants_cli(&args(&["--help"])));
}

#[test]
fn sending_to_an_exited_session_fails() {
    let fake = FakeHost::start("sentdead", |message| match message {
        ClientMsg::ListSessions => {
            let mut info = session(id(A), meta("x", None));
            info.exited = true;
            vec![HostMsg::SessionList { sessions: vec![info] }]
        }
        _ => vec![],
    });
    let (code, _, _) = run("send abcd hi --enter --wait", &fake.env);
    assert_eq!(code, exit::EXITED);
    assert!(!fake.drain().iter().any(|r| matches!(r, Received::Input(..))));
}
