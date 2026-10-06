//! 让 app 开终端、结束终端、切到终端的命令：发给宿主的请求和打印的结果。

mod common;

use common::*;
use runode_cli::exit;
use runode_protocol::{AttachMode, ClientMsg, HostMsg, Placement, SessionId};
use runode_shared_types::session::SessionMeta;

const MINE: u128 = 0x1111_0000_0000_0000_0000_0000_0000_0001;
const NEW: u128 = 0x2222_0000_0000_0000_0000_0000_0000_0002;

/// 有一个会话 `MINE` 的假宿主：开出来的终端是 `NEW`，结束会话时回 `Exited`，切过去回 `Done`。
fn app(name: &str) -> FakeHost {
    FakeHost::start(name, |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(MINE), meta("zsh", None))] }],
        ClientMsg::Open { req, .. } => vec![HostMsg::Opened { req: *req, id: id(NEW) }],
        // 新 shell 连上时还没显示提示符，接着报告它在等输入了。
        ClientMsg::Attach { id, .. } => vec![
            HostMsg::Attached {
                id: *id,
                channel: 5,
                size: SIZE,
                mode: AttachMode::MetaOnly,
                meta: meta("zsh", None),
                settings: None,
            },
            HostMsg::Meta { id: *id, meta: SessionMeta { prompt_cwd: Some("/tmp".into()), ..meta("zsh", None) } },
        ],
        ClientMsg::Kill { id } => vec![HostMsg::Exited { id: *id, status: None }],
        ClientMsg::Reveal { req, .. } => vec![HostMsg::Done { req: *req }],
        _ => vec![],
    })
}

fn controls(fake: &FakeHost) -> Vec<ClientMsg> {
    fake.drain()
        .into_iter()
        .filter_map(|r| match r {
            Received::Control(message) => Some(message),
            Received::Input(..) => None,
        })
        .collect()
}

/// 在自己的终端里 `open` 时开在自己旁边，目录换成绝对路径，打印新会话。
#[test]
fn open_beside_your_own_session() {
    let mut fake = app("open");
    fake.env.session = Some(SessionId(MINE).to_string());
    let (code, out, err) = run("open --down --cwd sub", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, format!("{}\n", SessionId(NEW)));
    let open = controls(&fake).into_iter().find(|m| matches!(m, ClientMsg::Open { .. })).unwrap();
    let here = std::env::current_dir().unwrap();
    assert_eq!(
        open,
        ClientMsg::Open {
            req: 1,
            placement: Placement::Down,
            near: Some(SessionId(MINE)),
            cwd: Some(here.join("sub")),
            focus: false,
        }
    );
}

/// 在 runode 外面 `open` 时不指定旁边是谁，交给 app 挑最前面的窗口；给了命令就打进新终端。
#[test]
fn open_types_the_command_into_the_new_terminal() {
    let fake = app("opencmd");
    let started = std::time::Instant::now();
    let (code, _, err) = run("open --focus -- claude fix it", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "typed once the prompt showed");
    let received = fake.drain();
    assert!(received.contains(&Received::Control(ClientMsg::Open {
        req: 1,
        placement: Placement::Tab,
        near: None,
        cwd: None,
        focus: true,
    })));
    let inputs: Vec<_> = received.into_iter().filter(|r| matches!(r, Received::Input(..))).collect();
    assert_eq!(inputs, [Received::Input(5, b"claude fix it".to_vec()), Received::Input(5, b"\r".to_vec())]);
}

#[test]
fn the_app_can_refuse_to_open() {
    let fake = FakeHost::start("refuse", |message| match message {
        ClientMsg::Open { req, .. } => {
            vec![HostMsg::Error { req: Some(*req), id: None, message: "there is no runode window".into() }]
        }
        _ => vec![],
    });
    let (code, _, err) = run("open", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("no runode window"), "{err}");
}

/// `kill` 等到会话真的结束才返回。
#[test]
fn kill_waits_for_the_session_to_end() {
    let fake = app("kill");
    let (code, _, err) = run("kill 1111", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert!(controls(&fake).contains(&ClientMsg::Kill { id: SessionId(MINE) }));
}

#[test]
fn focus_defaults_to_your_own_session() {
    let mut fake = app("focus");
    fake.env.session = Some(SessionId(MINE).to_string());
    let (code, _, err) = run("focus", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert!(controls(&fake).contains(&ClientMsg::Reveal { req: 1, id: SessionId(MINE) }));
}
