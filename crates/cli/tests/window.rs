//! 让 app 开终端、结束终端、切到终端、挪动和对调分屏的命令：发给宿主的请求和打印的结果。

mod common;

use common::*;
use runode_cli::exit;
use runode_protocol::{AttachMode, ClientMsg, HostMsg, Placement, SessionId};
use runode_types::session::SessionMeta;

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
                shell: None,
            },
            HostMsg::Meta {
                id: *id,
                meta: SessionMeta { prompt_cwd: Some("/tmp".into()), ..meta("zsh", None) },
                shell: None,
            },
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
            fold: None,
            from: Some(SessionId(MINE)),
        }
    );
}

/// 自己的会话已经不在了（tmux 里留着旧终端的 `RUNODE_SESSION`）时照在 runode 外面办，交给 app 挑
/// 最前面的窗口；明说了 `--near` 那个旧会话的照旧报错。
#[test]
fn open_with_a_stale_own_session_uses_the_front_window() {
    const GONE: u128 = 0x3333_0000_0000_0000_0000_0000_0000_0003;
    let mut fake = app("openstale");
    fake.env.session = Some(SessionId(GONE).to_string());
    let (code, _, err) = run("open --right", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let open = controls(&fake).into_iter().find(|m| matches!(m, ClientMsg::Open { .. })).unwrap();
    let ClientMsg::Open { placement, near, from, .. } = open else { unreachable!() };
    assert_eq!((placement, near, from), (Placement::Right, None, Some(SessionId(GONE))));

    let (code, _, _) = run("open --near 3333", &fake.env);
    assert_eq!(code, exit::FAILED);
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
        fold: None,
        from: None,
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

/// `move` 不给会话和 `--near` 时都是自己的会话：挪成自己后面的新标签。`swap` 原样带上两个会话。
#[test]
fn move_and_swap_name_the_sessions_to_the_app() {
    let mut fake = FakeHost::start("move", |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList {
            sessions: vec![session(id(MINE), meta("zsh", None)), session(id(NEW), meta("zsh", None))],
        }],
        ClientMsg::MovePane { req, .. } | ClientMsg::SwapPanes { req, .. } => vec![HostMsg::Done { req: *req }],
        _ => vec![],
    });
    fake.env.session = Some(SessionId(MINE).to_string());
    let (code, _, err) = run("move", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let (code, _, err) = run("swap self 2222 --focus", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let sent: Vec<_> = controls(&fake)
        .into_iter()
        .filter(|message| matches!(message, ClientMsg::MovePane { .. } | ClientMsg::SwapPanes { .. }))
        .collect();
    assert_eq!(
        sent,
        [
            ClientMsg::MovePane {
                req: 1,
                id: SessionId(MINE),
                placement: Placement::Tab,
                near: Some(SessionId(MINE)),
                focus: false,
            },
            ClientMsg::SwapPanes { req: 1, a: SessionId(MINE), b: SessionId(NEW), focus: true },
        ]
    );
}
