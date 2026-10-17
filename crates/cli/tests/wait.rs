//! 等命令运行完、等屏幕上出现某行字、等屏幕安静，以及 `send --wait` 按会话的样子挑等什么。

mod common;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use common::*;
use runode_cli::exit;
use runode_protocol::{AttachMode, ClientMsg, FinishedCommand, HostMsg, SessionId};
use runode_types::{
    agent::{AgentKind, AgentState},
    session::SessionMeta,
};

const S: u128 = 0x5e55_0000_0000_0000_0000_0000_0000_0001;

/// 前台是开了 shell 集成的 shell。
fn at_prompt() -> SessionMeta {
    SessionMeta { foreground_is_shell: true, prompt_cwd: Some("/tmp".into()), ..meta("zsh", None) }
}

fn finished(exit: Option<i32>) -> HostMsg {
    HostMsg::CommandFinished { id: id(S), command: FinishedCommand { cmd: "make".into(), cwd: None, exit, ts: 0 } }
}

/// 有一个会话 `S` 的假宿主：连上时的状态是 `meta`，接着发 `then`；读屏幕依次回 `screens` 里的文字，
/// 读完了一直回最后一份。
fn host(name: &str, meta: SessionMeta, then: Vec<HostMsg>, screens: Vec<&'static str>) -> FakeHost {
    let screens = Arc::new(Mutex::new(screens.into_iter().map(String::from).collect::<Vec<_>>()));
    let info_meta = meta.clone();
    FakeHost::start(name, move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), info_meta.clone())] }],
        ClientMsg::Attach { id, .. } => {
            let mut replies = vec![HostMsg::Attached {
                id: *id,
                channel: 7,
                size: SIZE,
                mode: AttachMode::MetaOnly,
                meta: meta.clone(),
                settings: None,
                shell: None,
            }];
            replies.extend(then.iter().cloned());
            replies
        }
        ClientMsg::ReadScreen { id, .. } => {
            let mut screens = screens.lock().unwrap();
            let text = if screens.len() > 1 { screens.remove(0) } else { screens[0].clone() };
            vec![HostMsg::ScreenText { id: *id, text, truncated: false }]
        }
        ClientMsg::SendKeys { req, .. } | ClientMsg::Paste { req, .. } => vec![HostMsg::Done { req: *req }],
        ClientMsg::Layout { req } => vec![HostMsg::Error { req: Some(*req), id: None, message: "no window".into() }],
        _ => vec![],
    })
}

#[test]
fn wait_for_a_command_reports_its_exit_status() {
    let fake = host("cmdok", at_prompt(), vec![finished(Some(0))], vec![""]);
    assert_eq!(run("wait 5e55 --for command", &fake.env), (exit::OK, "exit 0\n".into(), String::new()));

    // 失败的命令：真正的退出码打在标准输出上，命令行以 4 退出。
    let fake = host("cmdfail", at_prompt(), vec![finished(Some(2))], vec![""]);
    let (code, out, _) = run("wait 5e55 --for command", &fake.env);
    assert_eq!((code, out.as_str()), (exit::COMMAND_FAILED, "exit 2\n"));

    // 等的是下一条：别的会话的命令不算，超时照常。
    let other = HostMsg::CommandFinished {
        id: SessionId(1),
        command: FinishedCommand { cmd: "x".into(), cwd: None, exit: Some(0), ts: 0 },
    };
    let fake = host("cmdother", at_prompt(), vec![other], vec![""]);
    assert_eq!(run("wait 5e55 --for command --timeout 0.2", &fake.env).0, exit::TIMEOUT);
}

#[test]
fn waiting_for_a_command_needs_shell_integration() {
    let fake = host("cmdnoint", meta("zsh", None), vec![], vec![""]);
    let (code, _, err) = run("wait 5e55 --for command", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("--for quiet"), "{err}");
}

#[test]
fn wait_for_text_reads_the_screen_until_a_line_matches() {
    let fake = host("text", at_prompt(), vec![], vec!["$ make\n", "$ make\ncompiling\n", "compiling\nerror: boom\n"]);
    let (code, out, err) = run("wait 5e55 --for text ^error --timeout 5", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "error: boom\n");
    let reads = controls(&fake).iter().filter(|m| matches!(m, ClientMsg::ReadScreen { .. })).count();
    assert_eq!(reads, 3);
}

/// 会话里的程序要改窗口尺寸（CSI 8 t）时宿主随时插进来的 `ResizeWindow` 不是回话。
#[test]
fn a_window_resize_request_is_not_an_answer() {
    let meta = at_prompt();
    let fake = FakeHost::start("resize-window", move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), meta.clone())] }],
        ClientMsg::Attach { id, .. } => vec![HostMsg::Attached {
            id: *id,
            channel: 7,
            size: SIZE,
            mode: AttachMode::MetaOnly,
            meta: meta.clone(),
            settings: None,
            shell: None,
        }],
        ClientMsg::ReadScreen { id, .. } => vec![
            HostMsg::ResizeWindow { id: *id, rows: 50, columns: 120 },
            HostMsg::ScreenText { id: *id, text: "error: boom\n".into(), truncated: false },
        ],
        _ => vec![],
    });
    let (code, out, err) = run("wait 5e55 --for text ^error --timeout 5", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "error: boom\n");
}

/// 大到算截止时间会溢出的超时当作不设，不 panic。
#[test]
fn a_huge_timeout_means_no_timeout() {
    let fake = host("huge", at_prompt(), vec![], vec!["error: boom\n"]);
    let (code, out, err) = run("wait 5e55 --for text ^error --timeout 1.8e19", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "error: boom\n");
}

#[test]
fn wait_for_new_text_skips_what_is_already_there() {
    let screens = vec!["error: old\n", "error: old\nok\n", "error: old\nok\nerror: new\n"];
    let fake = host("newtext", at_prompt(), vec![], screens);
    let (code, out, err) = run("wait 5e55 --for text error --new --lines 100 --timeout 5", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "error: new\n");
    assert!(controls(&fake).iter().any(|m| matches!(m, ClientMsg::ReadScreen { lines: Some(100), .. })));

    // 不带 --new 时已经在的那行就算。
    let fake = host("oldtext", at_prompt(), vec![], vec!["error: old\n"]);
    assert_eq!(run("wait 5e55 --for text error", &fake.env).1, "error: old\n");
    let fake = host("notext", at_prompt(), vec![], vec!["fine\n"]);
    assert_eq!(run("wait 5e55 --for text error --timeout 0.3", &fake.env).0, exit::TIMEOUT);
}

#[test]
fn wait_for_quiet_needs_the_screen_to_stay_the_same() {
    let fake = host("quiet", at_prompt(), vec![], vec!["1\n", "2\n", "3\n", "4\n", "4\n"]);
    let started = Instant::now();
    let (code, out, err) = run("wait 5e55 --for quiet 0.3 --timeout 5", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "quiet\n");
    // 前几次读到的都在变，从最后一次变化起再静 0.3 秒。
    assert!(started.elapsed() >= Duration::from_millis(600), "{:?}", started.elapsed());
    // 一直在变的屏幕等不到。安静的间隔比超时短，忽略了屏幕变化的话不到超时就报 quiet；
    // 交替的快照按 0.1 秒读一次够读二十秒，整个超时里屏幕都在变。
    let flapping: Vec<&str> = (0..200).map(|i| if i % 2 == 0 { "a\n" } else { "b\n" }).collect();
    let fake = host("noisy", at_prompt(), vec![], flapping);
    let (code, out, err) = run("wait 5e55 --for quiet 0.2 --timeout 1", &fake.env);
    assert_eq!((code, out.as_str()), (exit::TIMEOUT, ""), "{err}");
}

#[test]
fn an_exited_session_ends_a_text_wait() {
    let fake = host("textexit", at_prompt(), vec![HostMsg::Exited { id: id(S), status: Some(0) }], vec!["$\n"]);
    let (code, _, _) = run("wait 5e55 --for text never --timeout 5", &fake.env);
    assert_eq!(code, exit::EXITED);
}

/// `send --wait` 挑等什么：有 agent 等它干完，开了集成的 shell 上按了回车等命令，别的等屏幕安静。
#[test]
fn send_wait_picks_what_to_wait_for() {
    let agent = |state| meta("claude", Some((AgentKind::Claude, state)));
    let working = HostMsg::Meta { id: id(S), meta: agent(AgentState::Working), shell: None };
    let idle = HostMsg::Meta { id: id(S), meta: agent(AgentState::Idle), shell: None };
    let cases = [
        // 有 agent：等它干完。
        ("send 5e55 go --enter --wait", agent(AgentState::Idle), vec![working, idle], "--for done", exit::OK, "idle\n"),
        // shell 提示符、按了回车：等命令。
        ("send 5e55 make --enter --wait", at_prompt(), vec![finished(Some(1))], "--for command", 4, "exit 1\n"),
        ("send 5e55 make --key enter --wait", at_prompt(), vec![finished(Some(0))], "--for command", 0, "exit 0\n"),
        // 刚开的 shell 还没出提示符：先等它出来，再照样等命令。
        (
            "send 5e55 make --enter --wait",
            SessionMeta { prompt_cwd: None, ..at_prompt() },
            vec![HostMsg::Meta { id: id(S), meta: at_prompt(), shell: None }, finished(Some(1))],
            "--for command",
            4,
            "exit 1\n",
        ),
        // 没按回车、没有 shell 集成、前台不是 shell：等屏幕安静。
        ("send 5e55 make --wait", at_prompt(), vec![], "--for quiet", exit::OK, "quiet\n"),
        ("send 5e55 make --enter --wait", meta("zsh", None), vec![], "--for quiet", exit::OK, "quiet\n"),
        (
            "send 5e55 q --enter --wait",
            SessionMeta { foreground_is_shell: false, ..at_prompt() },
            vec![],
            "--for quiet",
            exit::OK,
            "quiet\n",
        ),
    ];
    for (i, (command, meta, then, waits, code, out)) in cases.into_iter().enumerate() {
        let fake = host(&format!("sendwait{i}"), meta, then, vec!["$\n"]);
        let (got_code, got_out, err) = run(command, &fake.env);
        assert!(err.contains(waits), "{command}: {err}");
        assert_eq!((got_code, got_out.as_str()), (code, out), "{command}: {err}");
    }
}

/// 等命令时，打进去的多行文字会被 shell 当成几条命令，等到第一条就返回，所以一个字都不打就拒绝；
/// 粘贴的照常等。
#[test]
fn send_wait_for_a_command_refuses_typed_line_breaks() {
    let send = |name: &str, paste: bool| {
        let fake = host(name, at_prompt(), vec![finished(Some(0))], vec!["$\n"]);
        let mut args = vec!["send", "5e55", "cd app\nmake", "--enter", "--wait"];
        if paste {
            args.push("--paste");
        }
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = runode_cli::run(&args, &fake.env, &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap(), fake.drain())
    };

    let (code, _, err, received) = send("multiline", false);
    assert_ne!(code, exit::OK, "{err}");
    assert!(err.contains("--paste"), "{err}");
    let typed = received.iter().any(|r| {
        matches!(r, Received::Input(..) | Received::Control(ClientMsg::Paste { .. } | ClientMsg::SendKeys { .. }))
    });
    assert!(!typed, "nothing is typed");

    let (code, out, err, _) = send("multilinepaste", true);
    assert_eq!((code, out.as_str()), (exit::OK, "exit 0\n"), "{err}");
    assert!(err.contains("--for command"), "{err}");
}

/// 发给 agent 的输入没让它干活（回车没提交上）时，`send --wait` 过一会儿就失败，不等到 `--timeout`。
#[test]
fn send_wait_gives_up_when_the_agent_never_starts() {
    let fake = host("stall", meta("claude", Some((AgentKind::Claude, AgentState::Idle))), vec![], vec!["$\n"]);
    let started = Instant::now();
    let (code, _, err) = run("send 5e55 go --enter --wait --timeout 60", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains("no activity"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn send_wait_gives_up_when_a_blocked_agent_goes_idle_without_working() {
    let idle =
        HostMsg::Meta { id: id(S), meta: meta("claude", Some((AgentKind::Claude, AgentState::Idle))), shell: None };
    let fake =
        host("stallblocked", meta("claude", Some((AgentKind::Claude, AgentState::Blocked))), vec![idle], vec!["$\n"]);
    let started = Instant::now();
    let (code, _, err) = run("send 5e55 --key esc --wait --timeout 60", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains("no activity"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(30));
}

/// 答完一个提问紧接着问下一个、中间的干活没认出来时状态一直是 Blocked，宿主不发状态：到点时屏幕上换了
/// 提问就停在 `blocked`，还是同一个提问才当成没提交上。
#[test]
fn send_wait_to_a_blocked_agent_stops_at_the_next_question() {
    let blocked = || meta("claude", Some((AgentKind::Claude, AgentState::Blocked)));
    let fake = host("nextquestion", blocked(), vec![], vec!["Allow ls?\n", "Allow rm?\n"]);
    let (code, out, err) = run("send 5e55 1 --wait --timeout 60", &fake.env);
    assert_eq!((code, out.as_str()), (exit::OK, "blocked\n"), "{err}");

    let fake = host("samequestion", blocked(), vec![], vec!["Allow ls?\n"]);
    let (code, _, err) = run("send 5e55 1 --wait --timeout 60", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains("no activity"), "{err}");
}

#[test]
fn send_types_or_pastes_then_presses_keys_then_enter() {
    let fake = host("keys", meta("vim", None), vec![], vec![""]);
    let (code, _, err) = run("send 5e55 hello --key esc --key down*2 --enter", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let received = fake.drain();
    let order: Vec<String> = received
        .iter()
        .filter_map(|r| match r {
            Received::Input(7, data) => Some(format!("input {}", String::from_utf8_lossy(data))),
            Received::Control(ClientMsg::SendKeys { id: to, keys, .. }) if *to == id(S) => {
                Some(format!("keys {}", keys.join(" ")))
            }
            _ => None,
        })
        .collect();
    assert_eq!(order, ["input hello", "keys esc down*2", "input \r"]);

    let fake = host("paste", meta("vim", None), vec![], vec![""]);
    let (code, _, err) = run("send 5e55 --paste two words", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let received = fake.drain();
    assert!(received.iter().any(|r| matches!(
        r,
        Received::Control(ClientMsg::Paste { id: to, text, .. }) if *to == id(S) && text == "two words"
    )));
    assert!(!received.iter().any(|r| matches!(r, Received::Input(..))));
}

#[test]
fn bad_keys_are_refused_before_anything_is_sent() {
    let fake = host("badkey", meta("vim", None), vec![], vec![""]);
    let (code, _, err) = run("send 5e55 hi --key hyper-x", &fake.env);
    assert_eq!(code, exit::USAGE);
    assert!(err.contains("hyper-x"), "{err}");
    assert!(fake.drain().is_empty());
}

#[test]
fn read_a_command_and_say_when_it_was_cut_off() {
    let fake = FakeHost::start("readcmd", |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), at_prompt())] }],
        ClientMsg::ReadScreen { id, command: Some(n), .. } => {
            vec![HostMsg::ScreenText { id: *id, text: format!("output of {n}\n"), truncated: *n > 1 }]
        }
        _ => vec![],
    });
    assert_eq!(run("read 5e55 --command", &fake.env), (exit::OK, "output of 1\n".into(), String::new()));
    let (code, out, err) = run("read 5e55 --command 2", &fake.env);
    assert_eq!((code, out.as_str()), (exit::OK, "output of 2\n"));
    assert!(err.contains("gone from the scrollback"), "{err}");
}

/// 有一个会话 `S`、读屏幕时回 `screen` 给的消息的假宿主。
fn screen_host(name: &str, screen: impl Fn(SessionId) -> Vec<HostMsg> + Send + 'static) -> FakeHost {
    let meta = at_prompt();
    FakeHost::start(name, move |message| match message {
        ClientMsg::ListSessions => vec![HostMsg::SessionList { sessions: vec![session(id(S), meta.clone())] }],
        ClientMsg::Attach { id, .. } => vec![HostMsg::Attached {
            id: *id,
            channel: 7,
            size: SIZE,
            mode: AttachMode::MetaOnly,
            meta: meta.clone(),
            settings: None,
            shell: None,
        }],
        ClientMsg::ReadScreen { id, .. } => screen(*id),
        _ => vec![],
    })
}

/// 宿主不回读屏幕的请求时，到了 `--timeout` 就超时，不等满回话的十秒。
#[test]
fn an_unanswered_screen_read_times_out_on_time() {
    let fake = screen_host("screen-silent", |_| vec![]);
    let started = Instant::now();
    let (code, _, err) = run("wait 5e55 --for text never --timeout 0.3", &fake.env);
    assert_eq!(code, exit::TIMEOUT, "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

/// 宿主摘掉了订阅（`Resync`）以后会话结束了也收不到，读屏幕等着的不再干等到超时。
#[test]
fn a_lost_subscription_ends_a_text_wait() {
    let fake = screen_host("screen-resync", |id| {
        vec![
            HostMsg::Resync { id, reason: "the client reads too slowly".into() },
            HostMsg::ScreenText { id, text: "$\n".into(), truncated: false },
        ]
    });
    let started = Instant::now();
    let (code, _, err) = run("wait 5e55 --for text never --timeout 5", &fake.env);
    assert_eq!(code, exit::FAILED, "{err}");
    assert!(err.contains("lost track"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
}
