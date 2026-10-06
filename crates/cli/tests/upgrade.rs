//! 宿主把会话交给新版本的宿主时（`Goodbye { reason: Handoff }`）：一般的命令报错让用户重跑，
//! `wait` 重新连上新宿主接着等；别的 `Goodbye` 一律当作连接断了。

mod common;

use std::{
    io::Write as _,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use common::*;
use runode_cli::exit;
use runode_protocol::{
    AttachMode, BuildId, ClientMsg, Frame, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION, read_frame, write_frame,
};
use runode_shared_types::{
    agent::{AgentKind, AgentState},
    session::SessionMeta,
};

const S: u128 = 0x5e55_0000_0000_0000_0000_0000_0000_0001;
const UPGRADING: &str = "the runode host is being upgraded; run the command again";
const CLOSED: &str = "the runode app closed the connection";

/// 旧宿主的 socket 路径，假宿主起好以后才填上，给它自己的脚本用。
type Slot = Arc<Mutex<Option<PathBuf>>>;

fn handoff() -> HostMsg {
    HostMsg::Goodbye { reason: GoodbyeReason::Handoff }
}

fn agent(state: AgentState) -> SessionMeta {
    meta("claude", Some((AgentKind::Claude, state)))
}

fn attached(id: runode_protocol::SessionId, meta: SessionMeta) -> HostMsg {
    HostMsg::Attached { id, channel: 7, size: SIZE, mode: AttachMode::MetaOnly, meta, settings: None }
}

fn socket(fake: &FakeHost) -> PathBuf {
    fake.env.socket.clone().unwrap()
}

/// 交接：把新宿主的 socket 挪到旧宿主的路径上，之后连这个路径的是新宿主（监听的 socket 换了手，
/// 路径不变）。
fn hand_over(new: &Path, old: &Path) {
    std::fs::rename(new, old).unwrap();
}

/// 有一个会话 `S` 的假宿主，`attach` 时回 `then`，读屏幕回 `screen`。
fn host(name: &str, meta: SessionMeta, then: Vec<HostMsg>, screen: Option<&'static str>) -> FakeHost {
    let info = meta.clone();
    FakeHost::start(name, move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), info.clone())] }],
        ClientMsg::Attach { id, .. } => {
            let mut replies = vec![attached(*id, meta.clone())];
            replies.extend(then.iter().cloned());
            replies
        }
        ClientMsg::ReadScreen { id, .. } => match screen {
            Some(text) => vec![HostMsg::ScreenText { id: *id, text: text.into(), truncated: false }],
            None => vec![],
        },
        ClientMsg::Layout { req } => vec![HostMsg::Error { req: Some(*req), id: None, message: "no window".into() }],
        _ => vec![],
    })
}

#[test]
fn a_command_cut_off_by_an_upgrade_asks_to_run_it_again() {
    let fake = FakeHost::start("sendup", |message| match message {
        ClientMsg::ListSessions => vec![handoff()],
        _ => vec![],
    });
    let (code, _, err) = run("send 5e55 hello", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains(UPGRADING), "{err}");

    // 已经连上会话、在等回话时被打断也一样。
    let fake = FakeHost::start("keysup", |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), meta("vim", None))] }],
        ClientMsg::Attach { id, .. } => vec![attached(*id, meta("vim", None))],
        ClientMsg::SendKeys { .. } => vec![handoff()],
        _ => vec![],
    });
    let (code, _, err) = run("send 5e55 --key esc", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains(UPGRADING), "{err}");
}

/// 等命令运行完时不重连：新宿主不会重报交接时正在跑的命令。
#[test]
fn waiting_for_a_command_is_not_resumed() {
    let at_prompt = SessionMeta { foreground_is_shell: true, prompt_cwd: Some("/tmp".into()), ..meta("zsh", None) };
    let fake = host("cmdup", at_prompt, vec![handoff()], Some(""));
    let (code, _, err) = run("wait 5e55 --for command --timeout 5", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains(UPGRADING), "{err}");
}

#[test]
fn other_goodbyes_close_the_connection() {
    for (i, reason) in [GoodbyeReason::Shutdown, GoodbyeReason::Idle, GoodbyeReason::Unknown].into_iter().enumerate() {
        let fake = FakeHost::start(&format!("bye{i}"), move |message| match message {
            ClientMsg::ListSessions => vec![HostMsg::Goodbye { reason: reason.clone() }],
            _ => vec![],
        });
        let (code, _, err) = run("list", &fake.env);
        assert_eq!(code, exit::FAILED, "{err}");
        assert!(err.contains(CLOSED), "{err}");
    }
}

/// 比自己新的宿主说了认不出的原因（`GoodbyeReason::Unknown`）：当作连接断了，`wait` 也不重连。
#[test]
fn an_unknown_goodbye_reason_closes_the_connection() {
    let dir = std::env::temp_dir().join(format!("rnc-{}-byenew", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut writer = stream.try_clone().unwrap();
            while let Ok(Some(frame)) = read_frame(&mut stream) {
                let message: ClientMsg = frame.message().unwrap();
                let payload = match message {
                    ClientMsg::Hello { .. } => {
                        let welcome = HostMsg::Welcome {
                            protocol: PROTOCOL_VERSION,
                            build: BuildId("test".into()),
                            host_pid: 1,
                            snapshot_format: 1,
                            standalone: true,
                            handoff: 1,
                        };
                        Frame::control(&welcome).unwrap().payload
                    }
                    _ => br#"{"type":"goodbye","reason":{"kind":"moved_to_the_cloud","when":3}}"#.to_vec(),
                };
                write_frame(&mut writer, FrameKind::Control, 0, &payload).unwrap();
                writer.flush().unwrap();
            }
        }
    });
    let env = runode_cli::Env { socket: Some(path), session: None, build: "test".into(), home: None };
    let (code, _, err) = run("wait 5e55 --for idle --timeout 5", &env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains(CLOSED), "{err}");
}

/// 等 agent 干完时宿主升级了：连上新宿主、重新只看状态地连上会话，接着等到它干完。
#[test]
fn waiting_for_an_agent_resumes_on_the_new_host() {
    let new = host("doneb", agent(AgentState::Working), vec![agent_now(id(S), AgentState::Idle)], None);
    let new_socket = socket(&new);
    let old_socket_slot: Slot = Default::default();
    let slot = old_socket_slot.clone();
    let old = FakeHost::start("donea", move |message| match message {
        ClientMsg::ListSessions => {
            vec![HostMsg::SessionList { sessions: vec![session(id(S), agent(AgentState::Working))] }]
        }
        ClientMsg::Attach { id, .. } => {
            hand_over(&new_socket, slot.lock().unwrap().as_deref().unwrap());
            vec![attached(*id, agent(AgentState::Working)), handoff()]
        }
        _ => vec![],
    });
    *old_socket_slot.lock().unwrap() = Some(socket(&old));
    let (code, out, err) = run("wait 5e55 --for done --timeout 10", &old.env);
    assert_eq!((code, out.as_str()), (exit::OK, "idle\n"), "{err}");
    assert!(
        controls(&new)
            .iter()
            .any(|m| matches!(m, ClientMsg::Attach { id: to, mode: AttachMode::MetaOnly, .. } if *to == id(S))),
        "the new host saw a meta-only attach"
    );
}

/// 等屏幕上出现某行字时宿主升级了：在新宿主上接着读屏幕。
#[test]
fn waiting_for_text_resumes_on_the_new_host() {
    let new = host("textb", meta("zsh", None), vec![], Some("error: boom\n"));
    let new_socket = socket(&new);
    let slot: Slot = Default::default();
    let old_path = slot.clone();
    let old = FakeHost::start("texta", move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), meta("zsh", None))] }],
        ClientMsg::Attach { id, .. } => vec![attached(*id, meta("zsh", None))],
        ClientMsg::ReadScreen { .. } => {
            hand_over(&new_socket, old_path.lock().unwrap().as_deref().unwrap());
            vec![handoff()]
        }
        _ => vec![],
    });
    *slot.lock().unwrap() = Some(socket(&old));
    let (code, out, err) = run("wait 5e55 --for text ^error --timeout 10", &old.env);
    assert_eq!((code, out.as_str()), (exit::OK, "error: boom\n"), "{err}");
}

/// 等屏幕安静时宿主升级了：在新宿主上接着比屏幕。
#[test]
fn waiting_for_quiet_resumes_on_the_new_host() {
    let new = host("quietb", meta("zsh", None), vec![], Some("$\n"));
    let new_socket = socket(&new);
    let slot: Slot = Default::default();
    let old_path = slot.clone();
    let mut reads = 0;
    let old = FakeHost::start("quieta", move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), meta("zsh", None))] }],
        ClientMsg::Attach { id, .. } => vec![attached(*id, meta("zsh", None))],
        ClientMsg::ReadScreen { id, .. } => {
            reads += 1;
            if reads == 1 {
                return vec![HostMsg::ScreenText { id: *id, text: "$\n".into(), truncated: false }];
            }
            hand_over(&new_socket, old_path.lock().unwrap().as_deref().unwrap());
            vec![handoff()]
        }
        _ => vec![],
    });
    *slot.lock().unwrap() = Some(socket(&old));
    let (code, out, err) = run("wait 5e55 --for quiet 0.3 --timeout 10", &old.env);
    assert_eq!((code, out.as_str()), (exit::OK, "quiet\n"), "{err}");
    assert!(controls(&new).iter().any(|m| matches!(m, ClientMsg::ReadScreen { .. })));
}

/// 新宿主里没有这个会话了（交接时它已经结束）：和会话结束一样。
#[test]
fn a_session_missing_after_the_upgrade_has_exited() {
    let new = FakeHost::start("goneb", |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![] }],
        _ => vec![],
    });
    let new_socket = socket(&new);
    let slot: Slot = Default::default();
    let old_path = slot.clone();
    let old = FakeHost::start("gonea", move |message| match message {
        ClientMsg::ListSessions => {
            vec![HostMsg::SessionList { sessions: vec![session(id(S), agent(AgentState::Working))] }]
        }
        ClientMsg::Attach { id, .. } => {
            hand_over(&new_socket, old_path.lock().unwrap().as_deref().unwrap());
            vec![attached(*id, agent(AgentState::Working)), handoff()]
        }
        _ => vec![],
    });
    *slot.lock().unwrap() = Some(socket(&old));
    let (code, _, err) = run("wait 5e55 --for idle --timeout 10", &old.env);
    assert_eq!(code, exit::EXITED, "{err}");
}

/// 新宿主一直不来：最多等到 `--timeout`。
#[test]
fn a_host_that_never_comes_back_times_out() {
    let slot: Slot = Default::default();
    let old_path = slot.clone();
    let old = FakeHost::start("never", move |message| match message {
        ClientMsg::ListSessions => {
            vec![HostMsg::SessionList { sessions: vec![session(id(S), agent(AgentState::Working))] }]
        }
        ClientMsg::Attach { id, .. } => {
            // 旧宿主走了，socket 上没有人在听。
            std::fs::remove_file(old_path.lock().unwrap().as_deref().unwrap()).unwrap();
            vec![attached(*id, agent(AgentState::Working)), handoff()]
        }
        _ => vec![],
    });
    *slot.lock().unwrap() = Some(socket(&old));
    let started = Instant::now();
    let (code, _, err) = run("wait 5e55 --for idle --timeout 0.5", &old.env);
    assert_eq!(code, exit::TIMEOUT, "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}
