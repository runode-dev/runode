//! 按写法找会话：按位置（要 app 回答布局）、按标题、agent 和目录，对上几个时列出候选；app 没开
//! 窗口时按位置的写法报错，别的照常。

mod common;

use common::*;
use runode_cli::exit;
use runode_protocol::{ClientMsg, HostMsg, SessionId, WindowLayout};
use runode_shared_types::{
    agent::{AgentKind, AgentState},
    session::{DriveAction, Driver, SessionMeta},
};

// 窗口 1（最前面）的工作区 1：标签 1 里左边 A，右边上 B、下 C（C 高一些，中线离 A 的中线更近），
// 焦点在 B；标签 2 里只有 D。
// 窗口 2 的工作区 1 标签 1 里是 E。F 不在任何窗口里。
const A: u128 = 0xa000_0000_0000_0000_0000_0000_0000_0001;
const B: u128 = 0xb000_0000_0000_0000_0000_0000_0000_0002;
const C: u128 = 0xc000_0000_0000_0000_0000_0000_0000_0003;
const D: u128 = 0xd000_0000_0000_0000_0000_0000_0000_0004;
const E: u128 = 0xe000_0000_0000_0000_0000_0000_0000_0005;
const F: u128 = 0xf000_0000_0000_0000_0000_0000_0000_0006;

fn layout() -> Vec<WindowLayout> {
    vec![
        window(
            1,
            true,
            vec![workspace(
                1,
                true,
                vec![
                    tab(
                        1,
                        true,
                        vec![
                            pane(1, id(A), (0, 0, 500, 1000), false),
                            pane(2, id(B), (500, 0, 500, 400), true),
                            pane(3, id(C), (500, 400, 500, 600), false),
                        ],
                    ),
                    tab(2, false, vec![pane(1, id(D), WHOLE, true)]),
                ],
            )],
        ),
        window(2, false, vec![workspace(1, true, vec![tab(1, true, vec![pane(1, id(E), WHOLE, true)])])]),
    ]
}

fn sessions() -> Vec<runode_protocol::SessionInfo> {
    let in_dir = |title: &str, dir: &str, agent| SessionMeta { cwd: Some(dir.into()), ..meta(title, agent) };
    let mut driven = session(id(C), in_dir("zsh", "/tmp/web", None));
    driven.meta.driver = Some(Driver { by: Some(SessionId(A).to_string()), action: DriveAction::Keys, at_ms: 7 });
    driven.meta.foreground = Some("zsh".into());
    let mut background = session(id(F), in_dir("server", "/tmp/api", Some((AgentKind::Codex, AgentState::Idle))));
    background.claimed = false;
    vec![
        session(id(A), in_dir("claude", "/tmp/project", Some((AgentKind::Claude, AgentState::Working)))),
        session(id(B), in_dir("zsh", "/tmp/project", None)),
        driven,
        session(id(D), in_dir("logs", "/tmp/project", None)),
        session(id(E), in_dir("vim", "/tmp/project", None)),
        background,
    ]
}

/// 回列会话、布局（`windows` 为 `None` 时回 app 没开窗口）和读屏幕的假宿主；读屏幕回的是读的哪个会话。
fn app(name: &str, windows: Option<Vec<WindowLayout>>, own: Option<u128>) -> FakeHost {
    let mut fake = FakeHost::start(name, move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: sessions() }],
        ClientMsg::Layout { req } => match &windows {
            Some(windows) => vec![HostMsg::Layout { req: *req, windows: windows.clone() }],
            None => vec![HostMsg::Error { req: Some(*req), id: None, message: "there is no runode window".into() }],
        },
        ClientMsg::ReadScreen { id, .. } => {
            vec![HostMsg::ScreenText { id: *id, text: format!("{id}\n"), truncated: false }]
        }
        _ => vec![],
    });
    fake.env.session = own.map(|own| SessionId(own).to_string());
    fake
}

/// `read SELECTOR` 读到的是哪个会话。
fn read(fake: &FakeHost, selector: &str) -> Result<u128, (i32, String)> {
    let (code, out, err) = run(&format!("read {selector}"), &fake.env);
    if code != exit::OK {
        return Err((code, err));
    }
    Ok(out.trim().parse::<SessionId>().unwrap().0)
}

#[test]
fn positions_are_found_from_your_own_pane() {
    let fake = app("positions", Some(layout()), Some(A));
    for (selector, expected) in [
        ("self", A),
        (".", A),
        ("right", C),
        ("next", B),
        ("prev", C),
        ("pane:3", C),
        ("tab:2", D),
        ("tab:1.3", C),
        ("tab:1", B),
        ("win:2", E),
        ("win:2/tab:1.1", E),
        ("win:1/ws:1/pane:2", B),
        ("title:LOG", D),
        ("agent:claude", A),
        ("agent:codex:idle", F),
        ("cwd:web", C),
        ("cwd:/tmp/web/", C),
        ("f000", F),
    ] {
        assert_eq!(read(&fake, selector), Ok(expected), "{selector}");
    }
}

#[test]
fn positions_outside_runode_start_at_the_front_window() {
    let fake = app("front", Some(layout()), None);
    assert_eq!(read(&fake, "pane:1"), Ok(A));
    assert_eq!(read(&fake, "win:2/pane:1"), Ok(E));
    let (code, err) = read(&fake, "left").unwrap_err();
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("inside a runode terminal"), "{err}");
}

#[test]
fn missing_places_are_reported() {
    let fake = app("missing", Some(layout()), Some(A));
    for (selector, message) in [
        ("left", "no pane to the left"),
        ("pane:9", "no pane 9"),
        ("tab:5", "no tab 5"),
        ("win:3", "no window 3"),
        ("ws:2/tab:1", "no workspace 2"),
        ("agent:gemini", "no session matches agent:gemini"),
    ] {
        let (code, err) = read(&fake, selector).unwrap_err();
        assert_eq!(code, exit::FAILED, "{selector}");
        assert!(err.contains(message), "{selector}: {err}");
    }
    // 后台会话不在窗口里，按位置找不到它自己旁边的。
    let fake = app("bgself", Some(layout()), Some(F));
    let (_, err) = read(&fake, "down").unwrap_err();
    assert!(err.contains("in a runode window"), "{err}");
}

#[test]
fn ambiguous_selectors_list_the_candidates() {
    let fake = app("ambiguous", Some(layout()), Some(A));
    let (code, err) = read(&fake, "title:zsh").unwrap_err();
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("title:zsh matches more than one session"), "{err}");
    // 每个候选一行：短标识、标题和目录。
    assert!(err.contains("\n  b0000000  zsh  /tmp/project\n"), "{err}");
    assert!(err.contains("\n  c0000000  zsh  /tmp/web\n"), "{err}");
    let (code, err) = read(&fake, "cwd:project").unwrap_err();
    assert_eq!(code, exit::FAILED);
    assert_eq!(err.matches("\n  ").count(), 4, "{err}");
}

/// app 没开窗口：按位置的写法报错，别的照常；列会话时没有位置那几列，都是后台会话。
#[test]
fn without_a_window_only_positions_fail() {
    let fake = app("nowindow", None, Some(A));
    let (code, err) = read(&fake, "right").unwrap_err();
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("needs a runode window"), "{err}");
    assert!(read(&fake, "tab:1").unwrap_err().1.contains("needs a runode window"));
    assert_eq!(read(&fake, "self"), Ok(A));
    assert_eq!(read(&fake, "title:logs"), Ok(D));

    let (code, out, _) = run("list", &fake.env);
    assert_eq!(code, exit::OK);
    assert!(!out.contains("WIN") && !out.contains("REL"), "{out}");
    assert!(out.lines().skip(1).all(|line| line.ends_with("bg")), "{out}");
    let (_, out, _) = run("list --json", &fake.env);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["layout"], serde_json::Value::Null);
    assert!(json["sessions"].as_array().unwrap().iter().all(|s| s["view"] == "bg"));
}

#[test]
fn list_shows_where_each_terminal_is() {
    let fake = app("listing", Some(layout()), Some(A));
    let (code, out, err) = run("list", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let rows: Vec<Vec<&str>> = out.lines().map(|line| line.split_whitespace().collect()).collect();
    assert_eq!(
        rows[0],
        ["ID", "WIN", "WS", "TAB", "PANE", "REL", "AGENT", "STATE", "FG", "TITLE", "DIR", "VIEW"],
        "{out}"
    );
    // 按位置排，后台会话在最后。
    assert_eq!(rows[1][..7], ["*", "a0000000", "1", "1", "1", "1", "self"], "{out}");
    assert_eq!(rows[2][..7], ["b0000000", "1", "1", "1", "2", "-", "-"], "{out}");
    assert_eq!(rows[3][..6], ["c0000000", "1", "1", "1", "3", "right"], "{out}");
    assert_eq!(rows[3][8..], ["zsh", "zsh", "/tmp/web", "shown"], "{out}");
    assert_eq!(*rows[4].last().unwrap(), "hidden", "{out}");
    assert_eq!(rows[6][..6], ["f0000000", "-", "-", "-", "-", "-"], "{out}");
    assert_eq!(*rows[6].last().unwrap(), "bg", "{out}");
}

#[test]
fn list_json_has_the_layout_and_each_place() {
    let fake = app("listjson", Some(layout()), Some(A));
    let (code, out, _) = run("list --json", &fake.env);
    assert_eq!(code, exit::OK);
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["self"], SessionId(A).to_string());
    assert_eq!(json["layout"], serde_json::to_value(layout()).unwrap());
    let sessions = json["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 6);
    let c = &sessions[2];
    assert_eq!(c["id"], SessionId(C).to_string());
    assert_eq!(c["foreground"], "zsh");
    assert_eq!(c["driver"]["by"], SessionId(A).to_string());
    assert_eq!(c["driver"]["action"], "keys");
    assert_eq!(c["driver"]["at_ms"], 7);
    assert_eq!(
        c["place"],
        serde_json::json!({
            "window": 1, "workspace": 1, "tab": 1, "pane": 3, "focused": false, "selector": "win:1/ws:1/tab:1.3"
        })
    );
    assert_eq!(c["view"], "shown");
    assert_eq!(sessions[0]["rel"], serde_json::json!(["self"]));
    assert_eq!(sessions[1]["rel"], serde_json::json!([]));
    assert_eq!(c["rel"], serde_json::json!(["right"]));
    let f = &sessions[5];
    assert_eq!((f["claimed"].clone(), f["place"].clone(), f["view"].clone()), (false.into(), ().into(), "bg".into()));
}

/// 在 runode 的终端里跑时，握手带上自己所在的会话，宿主据此记下谁在操作。
#[test]
fn hello_names_your_own_session() {
    let fake = app("hello", None, Some(A));
    run("read self", &fake.env);
    let hello = controls(&fake).into_iter().next().unwrap();
    assert!(matches!(hello, ClientMsg::Hello { session: Some(own), .. } if own == SessionId(A)), "{hello:?}");
}
