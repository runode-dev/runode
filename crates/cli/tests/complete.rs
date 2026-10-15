//! 补全对着假宿主跑：会话和任务按了 Tab 时现查，宿主没开时也给得出静态的候选。

mod common;

use common::*;
use runode_cli::{Env, exit};
use runode_protocol::{ClientMsg, HostMsg};
use runode_types::{
    agent::{AgentKind, AgentState},
    todo::{Todo, TodoSpec},
};

const A: u128 = 0xabcd_0000_0000_0000_0000_0000_0000_0001;
const B: u128 = 0xabce_0000_0000_0000_0000_0000_0000_0002;

fn todo(id: &str, number: u64, title: &str, done: bool) -> Todo {
    Todo {
        id: id.into(),
        number,
        spec: TodoSpec {
            title: title.into(),
            prompt: String::new(),
            dir: "/tmp/project".into(),
            agent: "claude".into(),
            worktree: false,
        },
        created: 1,
        started: None,
        session: None,
        worktree_dir: None,
        branch: None,
        done_at: done.then_some(2),
        schedule: None,
        rank: 0,
    }
}

fn host(name: &str) -> FakeHost {
    FakeHost::start(name, move |message| match message {
        ClientMsg::Hello { .. } => vec![welcome(true)],
        ClientMsg::ListSessions => {
            // 目录名里的制表符写不进一行一个候选的输出，这个目录不提示。
            let mut odd = session(id(B), meta("zsh", None));
            odd.meta.cwd = Some("/tmp/odd\tname".into());
            vec![HostMsg::SessionList {
                sessions: vec![
                    session(id(A), meta("fixing tests", Some((AgentKind::Claude, AgentState::Working)))),
                    odd,
                ],
            }]
        }
        ClientMsg::ListTodos { req } => vec![HostMsg::Todos {
            req: *req,
            todos: vec![todo("00000001", 1, "write docs", false), todo("00000002", 2, "old one", true)],
            schedules: vec![],
        }],
        _ => vec![],
    })
}

/// 补整条命令行的最后一个词，返回 shell 另外能补什么和候选（带说明）。
fn complete(line: &str, env: &Env) -> (String, Vec<(String, String)>) {
    let args: Vec<String> = line.split(' ').map(str::to_owned).collect();
    let mut out = Vec::new();
    assert_eq!(runode_cli::complete(&args, args.len() - 1, env, &mut out), exit::OK);
    let out = String::from_utf8(out).unwrap();
    let mut lines = out.lines();
    let kind = lines.next().unwrap().to_owned();
    let items = lines
        .map(|line| {
            let (word, doc) = line.split_once('\t').unwrap();
            (word.to_owned(), doc.to_owned())
        })
        .collect();
    (kind, items)
}

#[test]
fn sessions_come_from_the_host_with_their_agent_and_title() {
    let fake = host("sessions");
    let (kind, items) = complete("send ab", &fake.env);
    assert_eq!(kind, "normal");
    assert_eq!(
        items,
        [("abcd0000".to_owned(), "Claude Code working: fixing tests".to_owned()), ("abce0000".into(), "zsh".into())]
    );
    // 在跑的 agent 和工作目录也能按种类、名字指。
    let (_, items) = complete("read ", &fake.env);
    let words: Vec<&str> = items.iter().map(|(word, _)| word.as_str()).collect();
    assert!(words.contains(&"agent:claude"), "{words:?}");
    assert!(words.contains(&"cwd:project"), "{words:?}");
    assert!(!words.iter().any(|word| word.contains("odd")), "{words:?}");
    assert!(!words.contains(&"self"), "outside a runode terminal there is no own session: {words:?}");
    // 在 runode 的终端里先是相对的写法。
    let inside = Env { session: Some(full(A)), ..fake.env.clone() };
    let (_, items) = complete("kill ", &inside);
    assert_eq!(items[0].0, "self");
    // `--near` 的值也是会话。
    let (_, items) = complete("open --near abce", &fake.env);
    assert_eq!(items.len(), 1);
}

#[test]
fn todos_are_offered_by_number_and_done_ones_only_to_show() {
    let fake = host("todos");
    let (_, items) = complete("todo done ", &fake.env);
    assert_eq!(items, [("1".to_owned(), "write docs".to_owned())]);
    let (_, items) = complete("todo show ", &fake.env);
    assert_eq!(items.len(), 2);
}

#[test]
fn without_a_host_the_static_words_remain() {
    let env = Env::default();
    let (kind, items) = complete("todo ", &env);
    assert_eq!(kind, "normal");
    assert_eq!(items[0].0, "add");
    let (_, items) = complete("send ", &env);
    assert!(items.is_empty());
    let (kind, _) = complete("open --cwd ", &env);
    assert_eq!(kind, "dirs");
    // 补的是程序名或者序号不对时什么都不补。
    let (kind, items) = complete("", &env);
    assert_eq!(kind, "normal");
    assert_eq!(items[0].0, "list");
}

#[test]
fn the_scripts_mention_both_names_and_the_env_var() {
    let env = Env::default();
    for shell in ["bash", "fish", "zsh"] {
        let (code, out, _) = run(&format!("completions {shell}"), &env);
        assert_eq!(code, exit::OK);
        assert!(out.contains("RUNODE_GET_COMPLETIONS"), "{shell}");
        assert!(out.contains("runode") && out.contains(" rn"), "{shell}");
    }
    let (code, _, err) = run("completions", &env);
    assert_eq!(code, exit::USAGE);
    assert!(err.contains("needs a shell"));
}

fn full(n: u128) -> String {
    runode_protocol::SessionId(n).to_string()
}

/// 宿主接了连接却不回话时，连握手在内最多等两秒，不卡住 Tab 十秒。
#[test]
fn a_silent_host_does_not_hold_up_completion() {
    let dir = std::env::temp_dir().join(format!("rnc-{}-silent", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("host.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // 接下连接、一直不回话；连接留着不关，命令行看不到连接断了。
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });
    let env = Env { socket: Some(socket), build: "test".into(), ..Env::default() };
    let started = std::time::Instant::now();
    let (kind, items) = complete("send ", &env);
    assert_eq!(kind, "normal");
    assert!(items.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(dir);
}
