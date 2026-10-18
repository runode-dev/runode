//! `todo` 和 `schedule` 对着假宿主跑：发了什么请求、打印了什么。

mod common;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use common::*;
use runode_cli::exit;
use runode_protocol::{
    ClientMsg, HostMsg, SessionId,
    todo::{DEFAULT_AGENT, Schedule, Todo, TodoSpec, When},
};
use runode_types::agent::{AgentKind, AgentState};
use unicode_width::UnicodeWidthStr as _;

const S: u128 = 0x5e55_0000_0000_0000_0000_0000_0000_0001;

/// 在 `dir` 下新建一个 git 仓库，返回它的目录：要工作树的测试不靠源码目录恰好在仓库里。
fn git_repo(dir: &Path) -> String {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git").arg("-C").arg(&repo).args(["init", "-q"]).status().unwrap();
    assert!(status.success());
    repo.display().to_string()
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn spec(title: &str, dir: &str) -> TodoSpec {
    TodoSpec {
        title: title.into(),
        prompt: String::new(),
        dir: dir.into(),
        agent: "claude".into(),
        worktree: false,
        ..TodoSpec::default()
    }
}

/// 编号取标识的末位，`00000003` 是 #3。
fn todo(id: &str, title: &str) -> Todo {
    Todo {
        id: id.into(),
        number: id[7..].parse().unwrap(),
        spec: spec(title, "/tmp/project"),
        created: 1,
        started: None,
        session: None,
        worktree_dir: None,
        branch: None,
        done_at: None,
        schedule: None,
        rank: 0,
    }
}

/// 认得看板的假宿主：会话 `S` 里 claude 在干活；任务和定时照给的回，别的请求照常回话。
fn host(name: &str, todos: Vec<Todo>, schedules: Vec<Schedule>) -> FakeHost {
    FakeHost::start(name, move |message| match message {
        ClientMsg::Hello { .. } => vec![welcome(true)],
        ClientMsg::ListSessions => vec![HostMsg::SessionList {
            sessions: vec![session(id(S), meta("agent", Some((AgentKind::Claude, AgentState::Working))))],
        }],
        ClientMsg::ListTodos { req } => {
            vec![
                HostMsg::Todos { req: *req, todos: todos.clone(), schedules: schedules.clone() },
                HostMsg::TodosChanged,
            ]
        }
        ClientMsg::AddTodo { req, .. } => vec![HostMsg::TodoAdded { req: *req, id: "0000abcd".into() }],
        ClientMsg::AddSchedule { req, .. } => vec![HostMsg::TodoAdded { req: *req, id: "0000beef".into() }],
        ClientMsg::StartTodo { req, .. } => vec![HostMsg::Opened { req: *req, id: SessionId(S) }],
        ClientMsg::GetTodo { req, id } => {
            match todos.iter().find(|todo| todo.id == *id || format!("#{}", todo.number) == *id) {
                Some(todo) => vec![HostMsg::Todo { req: *req, todo: todo.clone() }],
                None => vec![HostMsg::Error { req: Some(*req), id: None, message: format!("no todo {id}") }],
            }
        }
        ClientMsg::UpdateTodo { req, .. }
        | ClientMsg::RemoveTodo { req, .. }
        | ClientMsg::RemoveSchedule { req, .. } => {
            vec![HostMsg::Done { req: *req }]
        }
        _ => vec![],
    })
}

/// 收到的控制消息，去掉握手。
fn requests(fake: &FakeHost) -> Vec<ClientMsg> {
    controls(fake).into_iter().filter(|message| !matches!(message, ClientMsg::Hello { .. })).collect()
}

/// 带空格的参数（`--at "2026-10-13 14:00"`）用这个跑。
fn run_args(args: &[&str], fake: &FakeHost) -> (i32, String, String) {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = runode_cli::run(&args, &fake.env, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

#[test]
fn add_sends_an_absolute_dir_and_the_default_agent() {
    let fake = host("todoadd", vec![], vec![]);
    let (code, out, err) = run("todo add fix the login bug", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "0000abcd\n");
    let here = std::env::current_dir().unwrap();
    let sent = requests(&fake);
    let [ClientMsg::AddTodo { spec, .. }] = sent.as_slice() else { panic!("{sent:?}") };
    assert_eq!(
        (spec.title.as_str(), spec.prompt.as_str(), spec.agent.as_str()),
        ("fix the login bug", "", DEFAULT_AGENT)
    );
    // 按字节比：`PathBuf` 相等不看尾斜杠。
    assert_eq!(spec.dir.as_os_str(), here.as_os_str());

    // 在 git 仓库里时工作树默认开着。
    let fake = host("todoaddrepo", vec![], vec![]);
    let repo = git_repo(&fake.dir);
    assert_eq!(run(&format!("todo add fix it --cwd {repo}"), &fake.env).0, exit::OK);
    assert!(matches!(requests(&fake).as_slice(), [ClientMsg::AddTodo { spec, .. }] if spec.worktree));
}

#[test]
fn add_outside_a_repository_needs_no_worktree_and_start_follows() {
    let fake = host("todostart", vec![], vec![]);
    let dir = fake.dir.display().to_string();
    let (code, out, err) =
        run(&format!("todo add docs --cwd {dir} --agent codex --prompt write-the-docs --start"), &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, format!("0000abcd\n{}\n", SessionId(S)));
    let sent = requests(&fake);
    let [ClientMsg::AddTodo { spec, .. }, ClientMsg::StartTodo { id, .. }] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(id, "0000abcd");
    assert_eq!(
        *spec,
        TodoSpec {
            title: "docs".into(),
            prompt: "write-the-docs".into(),
            dir: PathBuf::from(&dir),
            agent: "codex".into(),
            worktree: false,
            ..TodoSpec::default()
        }
    );
    // `--no-worktree` 在仓库里也不要工作树。
    let fake = host("todonowt", vec![], vec![]);
    let repo = git_repo(&fake.dir);
    assert_eq!(run(&format!("todo add docs --cwd {repo} --no-worktree"), &fake.env).0, exit::OK);
    assert!(matches!(requests(&fake).as_slice(), [ClientMsg::AddTodo { spec, .. }] if !spec.worktree));
}

/// 标题、提示词里的控制字符（转义序列、换行）不原样打到终端上：不清屏、不把表格的一行拆成几行。
#[test]
fn control_characters_are_not_printed() {
    let mut nasty = todo("00000001", "boom\x1b[2J\nsecond\tline");
    nasty.spec.prompt = "fix it\x1b]0;pwned\x07\nthen\ttest".into();
    let fake = host("todoctl", vec![nasty], vec![]);
    let (code, out, err) = run("todo list", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert!(!out.contains('\x1b') && !out.contains('\t'), "{out:?}");
    assert_eq!(out.lines().count(), 2, "{out:?}");
    assert!(out.contains("boom?[2J?second line"), "{out:?}");

    let (code, out, err) = run("todo show #1", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert!(!out.contains('\x1b') && !out.contains('\x07'), "{out:?}");
    // 提示词照样按行分开显示。
    assert!(out.contains("  fix it?]0;pwned?\n  then test\n"), "{out:?}");
}

#[test]
fn list_shows_each_task_in_its_column() {
    let started = Todo { started: Some(now() - 100), session: Some(SessionId(S)), ..todo("00000002", "working one") };
    let finished = Todo { done_at: Some(now()), ..todo("00000003", "finished") };
    let mut gone = Todo { started: Some(now() - 100), session: Some(SessionId(S + 1)), ..todo("00000004", "gone") };
    gone.spec.dir = "/elsewhere".into();
    gone.spec.agent = "codex --full-auto".into();
    let fake = host("todolist", vec![finished, todo("00000001", "还没开始"), gone, started], vec![]);
    let (code, out, err) = run("todo list", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let rows: Vec<Vec<&str>> =
        out.lines().map(|line| line.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect()).collect();
    assert_eq!(
        rows,
        [
            vec!["#", "ID", "COLUMN", "AGENT", "TITLE", "DIR"],
            vec!["#1", "00000001", "todo", "claude", "还没开始", "/tmp/project"],
            vec!["#2", "00000002", "in_progress", "claude", "working one", "/tmp/project"],
            vec!["#4", "00000004", "needs_you", "codex --full-auto", "gone", "/elsewhere"],
            vec!["#3", "00000003", "done", "claude", "finished", "/tmp/project"],
        ],
        "{out}"
    );
    // 中文标题占两格，后面的列照样对齐。
    let dir_starts: Vec<usize> = out.lines().map(|line| line[..line.len() - last_cell(line).len()].width()).collect();
    assert!(dir_starts.iter().all(|&start| start == dir_starts[0]), "{out}");
    // 先问会话、再问任务；之后宿主发的 `TodosChanged` 不当回话。
    assert!(matches!(requests(&fake).as_slice(), [ClientMsg::ListSessions, ClientMsg::ListTodos { .. }]));

    let (code, out, _) = run("todo list --json", &fake.env);
    assert_eq!(code, exit::OK);
    let listed: serde_json::Value = serde_json::from_str(&out).unwrap();
    let columns: Vec<(&str, &str)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|todo| (todo["id"].as_str().unwrap(), todo["column"].as_str().unwrap()))
        .collect();
    assert_eq!(
        columns,
        [("00000001", "todo"), ("00000002", "in_progress"), ("00000004", "needs_you"), ("00000003", "done")]
    );
    assert_eq!(listed[0]["spec"]["title"], "还没开始");
}

/// 表格一行里最后一格。
fn last_cell(line: &str) -> &str {
    line.rsplit("  ").next().unwrap().trim_start()
}

#[test]
fn show_prints_everything_about_one_task() {
    let mut gone = Todo {
        started: Some(now() - 100),
        session: Some(SessionId(S + 1)),
        worktree_dir: Some("/tmp/project/.worktrees/gone".into()),
        branch: Some("runode/gone".into()),
        schedule: Some("0000cafe".into()),
        ..todo("00000004", "gone")
    };
    gone.spec.prompt = "first line\nsecond line".into();
    let nightly = Schedule {
        id: "0000cafe".into(),
        spec: spec("nightly", "/tmp/project"),
        when: When::Every { minutes: 60 },
        last_fired: 0,
        next_fire: None,
    };
    let fake = host("todoshow", vec![gone], vec![nightly]);
    let (code, out, err) = run("todo show #4", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "#4  gone");
    for wanted in [
        "id        00000004",
        "column    needs_you",
        "worktree  /tmp/project/.worktrees/gone",
        "branch    runode/gone",
        "schedule  nightly (0000cafe)",
        "prompt",
        "  first line",
        "  second line",
    ] {
        assert!(lines.contains(&wanted), "{wanted:?} missing from\n{out}");
    }
    assert!(lines.iter().any(|line| line.starts_with("session") && line.ends_with("(gone)")), "{out}");
    assert!(!lines.iter().any(|line| line.starts_with("done")), "{out}");
    assert!(matches!(requests(&fake).first(), Some(ClientMsg::GetTodo { id, .. }) if id == "#4"));

    let (code, out, _) = run("todo show 00000004 --json", &fake.env);
    assert_eq!(code, exit::OK);
    let shown: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!((shown["number"].as_u64(), shown["column"].as_str()), (Some(4), Some("needs_you")));
    assert_eq!(shown["session_alive"], false);

    let (code, _, err) = run("todo show #9", &fake.env);
    assert_ne!(code, exit::OK);
    assert!(err.contains("no todo #9"), "{err}");
}

#[test]
fn done_start_and_rm_send_one_request_each() {
    let fake = host("tododone", vec![], vec![]);
    assert_eq!(run("todo done 00000001", &fake.env).0, exit::OK);
    assert_eq!(
        requests(&fake),
        [ClientMsg::UpdateTodo { req: 1, id: "00000001".into(), spec: None, done: Some(true) }]
    );
    let (code, out, _) = run("todo start 00000001", &fake.env);
    assert_eq!((code, out), (exit::OK, format!("{}\n", SessionId(S))));
    assert_eq!(run("todo rm 00000001", &fake.env).0, exit::OK);
    assert_eq!(run("schedule rm 0000beef", &fake.env).0, exit::OK);
    assert_eq!(
        requests(&fake),
        [
            ClientMsg::StartTodo { req: 1, id: "00000001".into() },
            ClientMsg::RemoveTodo { req: 1, id: "00000001".into() },
            ClientMsg::RemoveSchedule { req: 1, id: "0000beef".into() },
        ]
    );
}

#[test]
fn errors_from_the_host_are_reported() {
    let fake = FakeHost::start("todoerr", |message| match message {
        ClientMsg::Hello { .. } => vec![welcome(true)],
        ClientMsg::AddTodo { req, .. } => vec![HostMsg::TodoAdded { req: *req, id: "0000abcd".into() }],
        ClientMsg::StartTodo { req, .. } | ClientMsg::UpdateTodo { req, .. } => {
            vec![HostMsg::Error { req: Some(*req), id: None, message: "no todo 0000abcd".into() }]
        }
        _ => vec![],
    });
    let (code, _, err) = run("todo done 0000abcd", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("no todo 0000abcd"), "{err}");
    // 加上了、没开始成：标识照样打印，错误里说清加上了。
    let (code, out, err) = run("todo add docs --start", &fake.env);
    assert_eq!((code, out.as_str()), (exit::FAILED, "0000abcd\n"));
    assert!(err.contains("added todo 0000abcd") && err.contains("no todo 0000abcd"), "{err}");
}

#[test]
fn an_old_host_is_not_sent_task_board_messages() {
    // 不回 `Hello` 时假宿主回的 `Welcome` 里 `todos` 为假。
    let fake = FakeHost::start("todoold", |_| vec![]);
    for command in ["todo list", "todo add x", "todo done 00000001", "schedule list", "schedule add x --every 5"] {
        let (code, _, err) = run(command, &fake.env);
        assert_eq!(code, exit::FAILED, "{command}");
        assert!(err.contains("too old for the task board"), "{command}: {err}");
    }
    assert_eq!(requests(&fake), []);
}

#[test]
fn schedule_add_sends_when_and_no_worktree_by_default() {
    let fake = host("schedadd", vec![], vec![]);
    let (code, out, err) = run("schedule add morning report --daily 09:30 --days mon-fri", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, "0000beef\n");
    let repo = git_repo(&fake.dir);
    let (code, _, err) =
        run_args(&["schedule", "add", "once", "--at", "2099-01-02 03:04", "--cwd", &repo, "--worktree"], &fake);
    assert_eq!(code, exit::OK, "{err}");
    let sent = requests(&fake);
    let [ClientMsg::AddSchedule { spec, when, .. }, ClientMsg::AddSchedule { spec: once, when: at, .. }] =
        sent.as_slice()
    else {
        panic!("{sent:?}");
    };
    assert_eq!((spec.title.as_str(), spec.worktree, spec.agent.as_str()), ("morning report", false, DEFAULT_AGENT));
    assert_eq!(spec.dir, std::env::current_dir().unwrap());
    assert_eq!(*when, When::Daily { hour: 9, minute: 30, days: 0b011_1110 });
    assert!(once.worktree);
    assert!(matches!(at, When::Once { at } if *at > now()), "{at:?}");

    let (code, _, err) = run_args(&["schedule", "add", "late", "--at", "2000-01-01 00:00"], &fake);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("has passed"), "{err}");
    let dir = fake.dir.display().to_string();
    let (code, _, err) = run(&format!("schedule add r --every 5 --cwd {dir} --worktree"), &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("not in a git repository"), "{err}");
}

#[test]
fn schedule_list_writes_when_and_next_in_local_time() {
    // 先加一个一次性的，拿到它换算出的 Unix 时间，再让宿主照它列出来：钟点写回去得是同一个。
    let fake = host("schedonce", vec![], vec![]);
    run_args(&["schedule", "add", "x", "--at", "2099-01-02 03:04"], &fake);
    let sent = requests(&fake);
    let [ClientMsg::AddSchedule { when: When::Once { at }, .. }] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    let at = *at;
    let schedule = |id: &str, when, next_fire| Schedule {
        id: id.into(),
        spec: spec("report", "/tmp/project"),
        when,
        last_fired: 0,
        next_fire,
    };
    let fake = host(
        "schedlist",
        vec![],
        vec![
            schedule("0000000a", When::Daily { hour: 9, minute: 30, days: 0b011_1110 }, Some(at)),
            schedule("0000000b", When::Every { minutes: 15 }, None),
            schedule("0000000c", When::Once { at }, Some(at)),
        ],
    );
    let (code, out, err) = run("schedule list", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let rows: Vec<Vec<&str>> =
        out.lines().map(|line| line.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect()).collect();
    assert_eq!(
        rows,
        [
            vec!["ID", "WHEN", "NEXT", "AGENT", "TITLE", "DIR"],
            vec!["0000000a", "daily 09:30 mon-fri", "2099-01-02 03:04", "claude", "report", "/tmp/project"],
            vec!["0000000b", "every 15m", "-", "claude", "report", "/tmp/project"],
            vec!["0000000c", "once 2099-01-02 03:04", "2099-01-02 03:04", "claude", "report", "/tmp/project"],
        ],
        "{out}"
    );
    let (code, out, _) = run("schedule list --json", &fake.env);
    assert_eq!(code, exit::OK);
    let listed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(listed[1]["when"], serde_json::json!({ "kind": "every", "minutes": 15 }));
    assert_eq!(listed[2]["next_fire"], at);
}
