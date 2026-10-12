//! 任务板（`ClientMsg::ListTodos` 这几条消息）：开始任务时在会话里跑 agent、提示词原样交到、按需开工作树，
//! 定时补跑，有定时时单独跑的宿主不因空闲退出，任务板变了告诉问过的连接。
//!
//! 宿主开任务会话用用户的 `$SHELL`，这里设成没有 shell 集成的 `/bin/sh`：提示符不报，等满
//! `TYPE_AT_PROMPT_PATIENCE` 才打命令，所以开始任务的测试要几秒。

mod common;

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Once,
    thread,
    time::{Duration, Instant},
};

use common::{Peer, listen, script, temp_dir};
use runode_host::{ClientMsg, Host, HostMsg, SessionId, Stopped};
use runode_protocol::todo::{Column, Todo, TodoSpec, When, column};

/// 宿主开的任务会话跑的 shell。
fn use_plain_sh() {
    static SET: Once = Once::new();
    // SAFETY: 只在这里改，开会话之前改好；这个测试程序里没有别处读写环境变量。
    SET.call_once(|| unsafe { std::env::set_var("SHELL", "/bin/sh") });
}

/// 开一个打开了任务板的宿主，库在 `dir` 里。
fn board(name: &str) -> (Host, PathBuf, Peer) {
    use_plain_sh();
    let dir = temp_dir(name);
    let (host, socket) = listen(&dir);
    host.serve_todos(dir.join("todos.db"));
    let peer = Peer::hello(&socket, false);
    (host, dir, peer)
}

fn spec(title: &str, dir: &Path, agent: &str) -> TodoSpec {
    TodoSpec { title: title.into(), prompt: String::new(), dir: dir.into(), agent: agent.into(), worktree: false }
}

fn add(peer: &mut Peer, spec: TodoSpec) -> String {
    match ask(peer, &ClientMsg::AddTodo { req: 1, spec }) {
        HostMsg::TodoAdded { req: 1, id } => id,
        other => panic!("expected the new todo, got {other:?}"),
    }
}

fn list(peer: &mut Peer) -> Vec<Todo> {
    peer.send(&ClientMsg::ListTodos { req: 2 });
    loop {
        match peer.reply() {
            HostMsg::Todos { req: 2, todos, .. } => return todos,
            HostMsg::TodosChanged => {}
            other => panic!("expected the todos, got {other:?}"),
        }
    }
}

fn start(peer: &mut Peer, id: &str) -> SessionId {
    peer.send(&ClientMsg::StartTodo { req: 3, id: id.into() });
    loop {
        match peer.reply() {
            HostMsg::Opened { req: 3, id } => return id,
            HostMsg::TodosChanged => {}
            other => panic!("expected the session, got {other:?}"),
        }
    }
}

/// 发一条只回 `Done` 的请求，返回回话（跳过其间的 `TodosChanged`）。
fn ask(peer: &mut Peer, message: &ClientMsg) -> HostMsg {
    peer.send(message);
    loop {
        match peer.reply() {
            HostMsg::TodosChanged => {}
            reply => return reply,
        }
    }
}

/// 等到 `path` 有内容，返回它。
fn wait_for_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        assert!(Instant::now() < deadline, "{} never appeared", path.display());
        thread::sleep(Duration::from_millis(50));
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// 提示词经环境变量原样交给 agent：引号、`$`、反引号、换行都不被 shell 解释。
#[test]
fn the_agent_gets_the_prompt_verbatim() {
    let (_host, dir, mut peer) = board("todo-prompt");
    let out = dir.join("prompt.txt");
    let agent = script(&dir, "agent", &format!("printf '%s' \"$1\" > '{}'", out.display()));
    let prompt = "fix \"it\" and 'that'; echo $HOME `whoami`\nsecond line \\ done";
    let id = add(&mut peer, TodoSpec { prompt: prompt.into(), ..spec("Quote test", &dir, &agent) });

    assert_eq!(
        list(&mut peer).iter().map(|todo| (todo.id.as_str(), todo.started)).collect::<Vec<_>>(),
        [(id.as_str(), None)]
    );
    let session = start(&mut peer, &id);
    assert_eq!(wait_for_file(&out), prompt);

    let todos = list(&mut peer);
    assert_eq!((todos[0].session, todos[0].started.is_some()), (Some(session), true));
    // 会话还在时再开始一次回原来的会话。
    assert_eq!(start(&mut peer, &id), session);
}

/// 要工作树的任务在仓库的 `.worktrees/` 下新开一个、拉新分支，主仓库的状态不受影响；删任务或标完成时
/// 会话还在就留着工作树，会话没了就删掉干净的工作树和没有新提交的分支。
#[test]
fn a_worktree_todo_runs_in_its_own_worktree() {
    let (_host, dir, mut peer) = board("todo-worktree");
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("web")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("web").join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);

    let out = dir.join("cwd.txt");
    let agent = script(&dir, "agent", &format!("pwd > '{}'", out.display()));
    let id = add(&mut peer, TodoSpec { worktree: true, ..spec("Fix the login page!", &repo.join("web"), &agent) });
    start(&mut peer, &id);

    let worktree = repo.join(".worktrees").join("fix-the-login-page");
    // 任务的目录在仓库的子目录里，工作树里也进到同一个子目录。
    assert_eq!(
        std::fs::canonicalize(wait_for_file(&out).trim()).unwrap(),
        std::fs::canonicalize(worktree.join("web")).unwrap()
    );
    assert_eq!(git(&worktree, &["branch", "--show-current"]).trim(), "runode/fix-the-login-page");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    let todo = list(&mut peer).remove(0);
    assert_eq!(
        (todo.worktree_dir.as_deref(), todo.branch.as_deref()),
        (Some(worktree.as_path()), Some("runode/fix-the-login-page"))
    );

    // 同名的第二个任务换个名字。
    let second = add(&mut peer, TodoSpec { worktree: true, ..spec("Fix the login page!", &repo, &agent) });
    let second_session = start(&mut peer, &second);
    let second_worktree = repo.join(".worktrees").join(format!("fix-the-login-page-{second}"));
    assert!(second_worktree.is_dir());

    // 会话还在：删任务不删工作树。
    assert_eq!(ask(&mut peer, &ClientMsg::RemoveTodo { req: 4, id }), HostMsg::Done { req: 4 });
    assert!(worktree.is_dir());
    // 会话没了、工作树干净：一起删掉，分支留着。同一条连接上的请求按先后办，`Kill` 办完会话就不在了。
    peer.send(&ClientMsg::Kill { id: second_session });
    assert_eq!(ask(&mut peer, &ClientMsg::RemoveTodo { req: 5, id: second.clone() }), HostMsg::Done { req: 5 });
    assert!(!second_worktree.exists());
    // 分支上没有新提交，跟着删掉。
    let branch = format!("runode/fix-the-login-page-{second}");
    assert!(!git(&repo, &["branch", "--list", &branch]).contains(&branch));

    // 标完成、会话没了以后，定时的线程清掉工作树和分支；有没合并提交的分支留着。
    let third = add(&mut peer, TodoSpec { worktree: true, ..spec("Ship it", &repo, &agent) });
    let third_session = start(&mut peer, &third);
    let third_worktree = repo.join(".worktrees").join("ship-it");
    std::fs::write(third_worktree.join("b.txt"), "b\n").unwrap();
    git(&third_worktree, &["add", "."]);
    git(&third_worktree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "work"]);
    peer.send(&ClientMsg::Kill { id: third_session });
    let done = ClientMsg::UpdateTodo { req: 6, id: third.clone(), spec: None, done: Some(true) };
    assert_eq!(ask(&mut peer, &done), HostMsg::Done { req: 6 });
    let deadline = Instant::now() + Duration::from_secs(10);
    while third_worktree.exists() {
        assert!(Instant::now() < deadline, "the finished todo's worktree was not cleaned up");
        thread::sleep(Duration::from_millis(50));
    }
    assert!(git(&repo, &["branch", "--list", "runode/ship-it"]).contains("runode/ship-it"));
    let todo = list(&mut peer).into_iter().find(|todo| todo.id == third).unwrap();
    assert_eq!((todo.worktree_dir, todo.branch.as_deref()), (None, Some("runode/ship-it")));
}

/// 改、标完成、调先后、删，问过 `ListTodos` 的连接每次都收到 `TodosChanged`；不对的请求回 `Error`。
#[test]
fn edits_are_announced_and_checked() {
    let (_host, dir, mut peer) = board("todo-edits");
    let mut watcher = Peer::hello(&dir.join("host.sock"), false);
    watcher.send(&ClientMsg::ListTodos { req: 1 });
    assert!(matches!(watcher.reply(), HostMsg::Todos { req: 1, .. }));

    let a = add(&mut peer, spec("a", &dir, "claude"));
    assert_eq!(watcher.reply(), HostMsg::TodosChanged);
    let b = add(&mut peer, spec("b", &dir, "claude"));
    assert_eq!(watcher.reply(), HostMsg::TodosChanged);

    // 要标识的地方也认编号：b 是第 2 张，a 是第 1 张。
    match ask(&mut peer, &ClientMsg::GetTodo { req: 4, id: "#2".into() }) {
        HostMsg::Todo { req: 4, todo } => assert_eq!((todo.id.as_str(), todo.number), (b.as_str(), 2)),
        other => panic!("expected todo #2, got {other:?}"),
    }
    let move_b = ClientMsg::MoveTodo { req: 5, id: "#2".into(), before: Some("1".into()) };
    assert_eq!(ask(&mut peer, &move_b), HostMsg::Done { req: 5 });
    assert_eq!(watcher.reply(), HostMsg::TodosChanged);
    assert_eq!(list(&mut peer).iter().map(|todo| todo.id.clone()).collect::<Vec<_>>(), [b.clone(), a.clone()]);

    let done = ClientMsg::UpdateTodo { req: 6, id: a, spec: None, done: Some(true) };
    assert_eq!(ask(&mut peer, &done), HostMsg::Done { req: 6 });
    let todos = list(&mut peer);
    assert_eq!(column(&todos[1], None, todos[1].done_at.unwrap()), Column::Done);

    // 不对的请求：没有这个任务（标识、编号）、标题空着、目录不存在、不在仓库里却要工作树、时间已经过了。
    for (req, message) in [
        (7, ClientMsg::RemoveTodo { req: 7, id: "nothere".into() }),
        (8, ClientMsg::AddTodo { req: 8, spec: spec(" ", &dir, "claude") }),
        (9, ClientMsg::AddTodo { req: 9, spec: spec("t", &dir.join("missing"), "claude") }),
        (10, ClientMsg::AddTodo { req: 10, spec: TodoSpec { worktree: true, ..spec("t", &dir, "claude") } }),
        (11, ClientMsg::AddSchedule { req: 11, spec: spec("t", &dir, "claude"), when: When::Once { at: 1 } }),
        (13, ClientMsg::GetTodo { req: 13, id: "#99".into() }),
    ] {
        assert!(matches!(ask(&mut peer, &message), HostMsg::Error { req: Some(r), .. } if r == req), "{message:?}");
    }

    assert_eq!(ask(&mut peer, &ClientMsg::RemoveTodo { req: 12, id: b }), HostMsg::Done { req: 12 });
    assert_eq!(list(&mut peer).len(), 1);
}

/// 没打开任务板的宿主回 `Error`，连接照旧。
#[test]
fn a_host_without_a_board_says_so() {
    let dir = temp_dir("todo-none");
    let (_host, socket) = listen(&dir);
    let mut peer = Peer::hello(&socket, false);
    peer.send(&ClientMsg::ListTodos { req: 1 });
    assert!(matches!(peer.reply(), HostMsg::Error { req: Some(1), .. }));
    assert!(peer.sessions().is_empty());
}

/// 错过的定时打开任务板时补跑一次：开一张来自它的任务并开始，下一次从现在算起。
#[test]
fn a_missed_schedule_fires_once_when_the_board_opens() {
    use_plain_sh();
    let dir = temp_dir("todo-catch-up");
    let out = dir.join("ran.txt");
    let agent = script(&dir, "agent", &format!("echo ran >> '{}'", out.display()));
    // 先用一个宿主建好库、加上定时，再把它的上一次到点改到很久以前。
    let db = dir.join("todos.db");
    let id = {
        let (host, socket) = listen(&temp_dir("todo-catch-up-1"));
        host.serve_todos(db.clone());
        let mut peer = Peer::hello(&socket, false);
        let when = When::Every { minutes: 60 };
        peer.send(&ClientMsg::AddSchedule { req: 1, spec: spec("Nightly report", &dir, &agent), when });
        let HostMsg::TodoAdded { req: 1, id } = peer.reply() else { panic!("expected the schedule") };
        id
    };
    runode_todo::Store::open(&db).unwrap().set_fired(&id, 1).unwrap();

    let (host, socket) = listen(&temp_dir("todo-catch-up-2"));
    let mut peer = Peer::hello(&socket, false);
    host.serve_todos(db);
    assert_eq!(wait_for_file(&out), "ran\n");
    peer.send(&ClientMsg::ListTodos { req: 2 });
    let HostMsg::Todos { todos, schedules, .. } = peer.reply() else { panic!("expected the todos") };
    assert_eq!(todos.len(), 1);
    assert_eq!((todos[0].spec.title.as_str(), todos[0].schedule.as_ref()), ("Nightly report", Some(&schedules[0].id)));
    assert!(todos[0].started.is_some());
    // 下一次从这次到点算起，一小时以后。
    let fired = schedules[0].last_fired;
    assert!(fired > 1);
    assert_eq!(schedules[0].next_fire, Some(fired + 3600));
}

/// 有定时时单独跑的宿主不因空闲退出，定时删完了照常退出。
#[test]
fn schedules_keep_a_standalone_host_up() {
    let (host, dir, mut peer) = board("todo-idle");
    let when = When::Every { minutes: 60 };
    peer.send(&ClientMsg::AddSchedule { req: 1, spec: spec("Hourly", &dir, "claude"), when });
    let HostMsg::TodoAdded { id, .. } = peer.reply() else { panic!("expected the schedule") };
    drop(peer);

    let stopped = thread::spawn(move || host.run_until_idle(Duration::from_millis(200)));
    thread::sleep(Duration::from_millis(1500));
    assert!(!stopped.is_finished(), "the host went idle with a schedule pending");

    let mut peer = Peer::hello(&dir.join("host.sock"), false);
    assert_eq!(ask(&mut peer, &ClientMsg::RemoveSchedule { req: 2, id }), HostMsg::Done { req: 2 });
    drop(peer);
    assert_eq!(stopped.join().unwrap(), Stopped::Idle);
}

/// 别的程序直接写进库里的任务，开始时宿主照样检查：agent 里带换行的不打进 shell。
#[test]
fn a_bad_todo_written_straight_into_the_database_does_not_start() {
    let (_host, dir, mut peer) = board("todo-bad-db");
    let id = runode_todo::Store::open(&dir.join("todos.db"))
        .unwrap()
        .add_todo(&spec("Bad", &dir, "true\ntouch pwned"), 1, None)
        .unwrap();
    let reply = ask(&mut peer, &ClientMsg::StartTodo { req: 3, id });
    assert!(matches!(reply, HostMsg::Error { req: Some(3), .. }), "{reply:?}");
}

/// 会话还在时再开始任务直接给出那个会话，任务的目录没了也一样：检查任务内容只管开新会话。
#[test]
fn a_running_todo_reopens_after_its_directory_is_gone() {
    let (_host, dir, mut peer) = board("todo-reopen");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let id = add(&mut peer, spec("Reopen", &work, "true"));
    let session = start(&mut peer, &id);
    std::fs::remove_dir_all(&work).unwrap();
    assert_eq!(start(&mut peer, &id), session);
}
