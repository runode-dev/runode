use runode_protocol::todo::{BLOCKED_GIVE_UP, holds_schedule};

use super::*;

#[test]
fn a_todo_is_claimed_by_one_at_a_time() {
    let todos = Todos::default();
    let claim = todos.claim("a").unwrap();
    assert!(todos.claim("a").is_none());
    assert!(todos.claim("b").is_some());
    drop(claim);
    assert!(todos.claim("a").is_some());
}

/// 正在开的任务会话丢掉守卫时减回去；交接开始以后不再记上。
#[cfg(unix)]
#[test]
fn launches_are_counted_only_while_serving() {
    let host = Host::new(runode_protocol::BuildId("test".into()));
    let shared = &host.shared;
    let launching = shared.begin_launch().unwrap();
    assert_eq!(shared.peers().launching, 1);
    drop(launching);
    assert_eq!(shared.peers().launching, 0);
    shared.peers().phase = crate::server::Phase::HandingOver;
    assert!(shared.begin_launch().is_err());
    assert_eq!(shared.peers().launching, 0);
}

/// 标完成、取消完成也要占住任务：清工作树的、正在开始的占着时回一句让前端再试，不改库。
#[test]
fn marking_done_waits_for_a_claimed_todo() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-claim", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.serve_todos(dir.join("todos.db"));
    let spec = TodoSpec {
        title: "t".into(),
        prompt: String::new(),
        dir,
        agent: "claude".into(),
        worktree: false,
        ..TodoSpec::default()
    };
    let shared = &host.shared;
    let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("expected the new todo") };
    let done = |shared: &Shared| shared.todos.with_db(|db| db.todo_by_id(&id)).unwrap().unwrap().done_at;

    let claim = shared.todos.claim(&id).unwrap();
    assert!(shared.update_todo(2, &id, None, Some(true)).is_err());
    assert_eq!(done(shared), None);
    drop(claim);
    assert_eq!(shared.update_todo(3, &id, None, Some(true)), Ok(HostMsg::Done { req: 3 }));
    assert!(done(shared).is_some());
}

/// 标完成后叫醒定时线程时任务已经放开了：它马上来清就占得住，不跳过、等下一轮。不起定时线程，只看
/// 看板变了的那一刻任务还占没占着。
#[test]
fn a_todo_marked_done_is_free_when_the_scheduler_wakes() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-wake", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let spec = TodoSpec {
        title: "t".into(),
        prompt: String::new(),
        dir,
        agent: "claude".into(),
        worktree: false,
        ..TodoSpec::default()
    };
    let shared = &host.shared;
    let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("expected the new todo") };

    let todos = shared.todos.clone();
    let seen = todos.generation();
    let watched = id.clone();
    let busy_on_wake = std::thread::spawn(move || {
        let state = todos.lock();
        let (state, _) =
            todos.changed.wait_timeout_while(state, Duration::from_secs(10), |state| state.generation == seen).unwrap();
        assert_ne!(state.generation, seen, "the board never changed");
        state.busy.contains(&watched)
    });
    // 让它先等上。
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(shared.update_todo(2, &id, None, Some(true)), Ok(HostMsg::Done { req: 2 }));
    assert!(!busy_on_wake.join().unwrap(), "the todo was still claimed when the scheduler woke up");
}

/// 别的进程（交出会话后还在记命令结果的旧宿主）改了库：定时的线程醒来时发现，告诉看着看板的连接。
#[cfg(unix)]
#[test]
fn a_board_changed_by_another_process_is_announced() {
    use runode_protocol::{Caps, ClientKind, ClientMsg, Frame, FrameKind, PROTOCOL_VERSION, read_frame, write_frame};

    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-elsewhere", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.serve_todos(dir.join("todos.db"));

    let mut stream = host.connect_pair().unwrap();
    let mut reader = stream.try_clone().unwrap();
    let (tx, frames) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(Some(frame)) = read_frame(&mut reader) {
            if frame.kind == FrameKind::Control && tx.send(frame.message::<HostMsg>().unwrap()).is_err() {
                break;
            }
        }
    });
    let mut send = |message: &ClientMsg| {
        let frame = Frame::control(message).unwrap();
        write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
    };
    let wait = Duration::from_secs(10);
    send(&ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: runode_protocol::BuildId("test".into()),
        client: ClientKind::Cli,
        caps: Caps::default(),
        session: None,
        device: None,
    });
    assert!(matches!(frames.recv_timeout(wait).unwrap(), HostMsg::Welcome { .. }));
    send(&ClientMsg::ListTodos { req: 1 });
    while frames.recv_timeout(wait).unwrap() == HostMsg::TodosChanged {}

    let spec = TodoSpec {
        title: "t".into(),
        prompt: String::new(),
        dir: dir.clone(),
        agent: "claude".into(),
        worktree: false,
        ..TodoSpec::default()
    };
    Store::open(&dir.join("todos.db")).unwrap().add_todo(&spec, unix_now(), None).unwrap();
    // 叫醒定时的线程，不等它睡满；没叫醒就接着叫。
    let deadline = Instant::now() + wait;
    loop {
        host.shared.todos.nudge();
        match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(HostMsg::TodosChanged) => break,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => assert!(Instant::now() < deadline, "the board change was never announced"),
        }
    }
}

/// 自动完成只认干过活以后的空闲：开始后直接空闲不记，干活以后空闲才记下时刻；标完成、取消完成以后
/// 要再干一轮活。
#[test]
fn auto_done_waits_for_the_agent_to_have_worked() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-auto-done", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let spec = TodoSpec { title: "t".into(), dir, agent: "claude".into(), auto_done: true, ..TodoSpec::default() };
    let shared = &host.shared;
    let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("expected the new todo") };
    let session = SessionId(41);
    shared.todos.with_db(|db| db.set_started(&id, unix_now(), session, None, true)).unwrap();
    shared.todos_changed();
    let since = |shared: &Shared| shared.todos.watched().worked.get(&session).copied();
    // 新读到的会话等定时的线程去问一次；问不到（没有这个会话）就算了。
    assert!(shared.todos.watched().unchecked.contains(&session));
    assert_eq!(shared.finish_idle_todos(), None);
    assert!(shared.todos.watched().unchecked.is_empty());
    assert_eq!(since(shared), None);

    shared.todo_session_changed(session, false, Some(AgentState::Idle));
    assert_eq!(since(shared), None);
    shared.todo_session_changed(session, false, Some(AgentState::Working));
    assert_eq!(since(shared), Some(None));
    shared.todo_session_changed(session, false, Some(AgentState::Idle));
    assert!(matches!(since(shared), Some(Some(_))));
    shared.todo_session_changed(session, false, Some(AgentState::Blocked));
    assert_eq!(since(shared), Some(None));

    shared.todo_session_changed(session, false, Some(AgentState::Working));
    assert_eq!(shared.update_todo(2, &id, None, Some(true)), Ok(HostMsg::Done { req: 2 }));
    assert_eq!(shared.update_todo(3, &id, None, Some(false)), Ok(HostMsg::Done { req: 3 }));
    assert_eq!(since(shared), None);
    shared.todo_session_changed(session, false, Some(AgentState::Idle));
    assert_eq!(since(shared), None);
    // 认不出 agent 了（报完结局退回 shell）也从这时算起。
    shared.todo_session_changed(session, false, Some(AgentState::Working));
    shared.todo_session_changed(session, false, None);
    assert!(matches!(since(shared), Some(Some(_))));
}

/// 报结局：记下结局和截过的结果，按编号也认；读不懂的结局不收。报了没做成的任务不再自动完成。
#[test]
fn a_report_records_the_outcome_and_a_failure_stops_auto_done() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-report", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let spec = TodoSpec { title: "t".into(), dir, agent: "claude".into(), auto_done: true, ..TodoSpec::default() };
    let shared = &host.shared;
    let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("expected the new todo") };
    let read = |shared: &Shared| shared.todos.with_db(|db| db.todo_by_id(&id)).unwrap().unwrap();

    let long = format!("  {}  ", "x".repeat(RESULT_LIMIT + 10));
    assert_eq!(shared.report_todo(2, "#1", Outcome::Ok, &long), Ok(HostMsg::Done { req: 2 }));
    let todo = read(shared);
    assert_eq!(todo.outcome, Some(Outcome::Ok));
    assert_eq!(todo.result.map(|result| result.chars().count()), Some(RESULT_LIMIT));
    assert!(shared.report_todo(3, &id, Outcome::Unknown, "").is_err());
    assert!(shared.report_todo(3, "nothere", Outcome::Ok, "").is_err());

    let session = SessionId(42);
    shared.todos.with_db(|db| db.set_started(&id, unix_now(), session, None, true)).unwrap();
    assert_eq!(shared.report_todo(4, &id, Outcome::Failed, "tests fail").unwrap(), HostMsg::Done { req: 4 });
    // 再报一次不带话：上一次的话不留着冒充这一次的结果。
    assert_eq!(shared.report_todo(5, &id, Outcome::Failed, "  ").unwrap(), HostMsg::Done { req: 5 });
    let todo = read(shared);
    assert_eq!((todo.outcome, todo.result), (Some(Outcome::Failed), None));
    // 干过活、空闲稳定以后，报了没做成的不标完成，也不再看它。
    shared.todo_session_changed(session, false, Some(AgentState::Working));
    if let Some(since) = shared.todos.watched().worked.get_mut(&session) {
        *since = Some(Instant::now() - Duration::from_secs(60));
    }
    shared.finish_idle_todos();
    assert_eq!(read(shared).done_at, None);
    assert!(!shared.todos.watched().worked.contains_key(&session));
}

/// 跑过时限还在干活的、停在询问上太久的任务记成没做成，结果写上原因，没人在看的终端结束掉；没到时限的、
/// 做完了停在 shell 上等人看的不动。
#[test]
fn stuck_todos_are_failed_and_their_terminals_ended() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-stuck", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let shared = &host.shared;
    let add = |title: &str, time_limit, auto_done| {
        let spec = TodoSpec {
            title: title.into(),
            dir: dir.clone(),
            agent: "claude".into(),
            auto_done,
            time_limit,
            ..TodoSpec::default()
        };
        let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("no todo") };
        (id, shared.spawn(SpawnOptions::headless(Some(dir.clone()))).unwrap())
    };
    let (late, late_session) = add("late", Some(1), false);
    let (asking, asking_session) = add("asking", None, true);
    let (fine, fine_session) = add("fine", Some(60), false);
    let (idle, idle_session) = add("idle", Some(1), false);
    let start = |id: &str, session, ago: u64| {
        shared.todos.with_db(|db| db.set_started(id, unix_now() - ago, session, None, true)).unwrap();
    };
    start(&late, late_session, 120);
    start(&asking, asking_session, 120);
    start(&fine, fine_session, 120);
    start(&idle, idle_session, 120);
    shared.todos_changed();
    {
        let mut watched = shared.todos.watched();
        // 会话是这次开始时开的，不是接回的。
        for session in [late_session, idle_session] {
            watched.begun.insert(session, unix_now() - 120);
        }
    }
    // 前台是认不出的程序，当它是 agent 在干活。
    shared.deliver(late_session, Inbox::TypeAtPrompt(b"cat\r".to_vec()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while shared
        .session_info(late_session)
        .is_none_or(|info| info.meta.foreground_is_shell || info.meta.foreground.as_deref() != Some("cat"))
    {
        assert!(Instant::now() < deadline, "cat never came to the front");
        std::thread::sleep(Duration::from_millis(50));
    }
    // 等完再记：shell 起来时报的状态会把它清掉。
    shared.todos.watched().blocked.insert(asking_session, Instant::now() - Duration::from_secs(BLOCKED_GIVE_UP));

    shared.fail_stuck_todos();
    let read = |id: &str| shared.todos.with_db(|db| db.todo_by_id(id)).unwrap().unwrap();
    for (id, session, why) in [(&late, late_session, "time limit"), (&asking, asking_session, "an answer")] {
        let todo = read(id);
        assert_eq!(todo.outcome, Some(Outcome::Failed), "{}: {:?}", todo.spec.title, shared.session_info(session));
        assert!(todo.result.as_deref().is_some_and(|result| result.contains(why)), "{:?}", todo.result);
        assert!(!shared.running(session));
    }
    assert!(!shared.todos.watched().blocked.contains_key(&asking_session));
    for (id, session) in [(&fine, fine_session), (&idle, idle_session)] {
        assert_eq!(read(id).outcome, None);
        assert!(shared.running(session));
        shared.kill(session);
    }
}

/// 排着队的任务按先后、在并发上限内开始；等的任务没做完时等着，没做成时一起记成没做成。
#[test]
fn queued_todos_start_within_the_limit_after_their_dependencies() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-queue", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let shared = &host.shared;
    let add = |title: &str| {
        let spec = TodoSpec { title: title.into(), dir: dir.clone(), agent: "true".into(), ..TodoSpec::default() };
        let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("no todo") };
        id
    };
    let (first, second, third) = (add("first"), add("second"), add("third"));
    let read = |id: &str| shared.todos.with_db(|db| db.todo_by_id(id)).unwrap().unwrap();
    assert_eq!(shared.set_max_running(2, 1), Ok(HostMsg::Done { req: 2 }));
    assert_eq!(shared.queue_todo(3, &first, true, None), Ok(HostMsg::Done { req: 3 }));
    assert_eq!(shared.queue_todo(4, "#2", true, Some("#1")), Ok(HostMsg::Done { req: 4 }));
    assert_eq!(shared.queue_todo(5, &third, true, None), Ok(HostMsg::Done { req: 5 }));
    assert!(shared.queue_todo(6, &first, true, Some(&third)).is_ok());
    assert!(shared.queue_todo(6, &third, true, Some(&first)).is_err(), "a cycle");
    assert!(shared.queue_todo(6, &first, true, None).is_ok());

    assert_eq!(shared.start_queued().0, Some(true));
    // 刚开始的在宽限期里算进行中，占满了上限：等着的第二张不开，排在后面的第三张也等着。
    let started = read(&first);
    assert!(started.started.is_some() && !started.queued);
    assert!(read(&second).started.is_none() && read(&third).started.is_none());

    shared.todos.with_db(|db| db.set_outcome(&first, Some(Outcome::Failed), Some("broke"))).unwrap();
    shared.set_max_running(7, 0).unwrap();
    assert_eq!(shared.start_queued().0, Some(true));
    let failed = read(&second);
    assert_eq!((failed.outcome, failed.queued, failed.started), (Some(Outcome::Failed), false, None));
    assert!(failed.result.is_some_and(|result| result.contains("#1 failed")));
    assert!(read(&third).started.is_some());
    assert_eq!(shared.start_queued(), (Some(false), None));
    for id in [&first, &third] {
        shared.kill(read(id).session.unwrap());
    }
}

/// 已不能改看板时手动触发不消费定时、不起命令：`Host::wait_for_schedule_runs` 拿到 `firing` 以后不再有新起的命令。
#[test]
fn firing_by_hand_while_stopping_starts_nothing() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-fire-stopping", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let shared = &host.shared;
    shared.peers().phase = Phase::Stopping(Stopped::Shutdown);
    let spec = TodoSpec { title: "t".into(), dir: dir.clone(), agent: "claude".into(), ..TodoSpec::default() };
    let when = runode_protocol::todo::When::Every { minutes: 60 };
    let add =
        |run: Option<&str>| shared.todos.with_db(|db| db.add_schedule(&spec, &when, run, None, unix_now())).unwrap();

    let plain = add(None);
    assert!(shared.fire_now(&plain).is_err());
    assert!(shared.todos.with_db(|db| db.list_todos(unix_now())).unwrap().is_empty());
    #[cfg(unix)]
    {
        let running = add(Some("sleep 5"));
        assert!(shared.fire_now(&running).is_err());
        assert!(!shared.todos.runs_pending());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 排着队没开始成的任务留在队里，隔一阵再试，试满次数记成没做成、不再挡着定时；不再排队的不再试。
#[test]
fn failed_starts_are_retried_then_failed() {
    let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-retry", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let host = Host::new(runode_protocol::BuildId("test".into()));
    host.shared.todos.lock().path = Some(dir.join("todos.db"));
    let shared = &host.shared;
    let spec = TodoSpec { title: "t".into(), dir: work.clone(), agent: "claude".into(), ..TodoSpec::default() };
    let HostMsg::TodoAdded { id, .. } = shared.add_todo(1, &spec).unwrap() else { panic!("no todo") };
    let HostMsg::TodoAdded { id: started, .. } = shared.add_todo(2, &spec).unwrap() else { panic!("no todo") };
    shared.todos.with_db(|db| db.set_started(&started, unix_now(), SessionId(43), None, true)).unwrap();
    shared.queue_todo(3, &id, true, None).unwrap();
    // 目录没了，开始不了。
    std::fs::remove_dir_all(&work).unwrap();
    let past = Instant::now() - Duration::from_secs(1);
    shared.todos.watched().retries.insert(started.clone(), (1, past));

    let (queued, next) = shared.start_queued();
    assert_eq!(queued, Some(true));
    assert!(next.expect("a retry is pending") > Instant::now());
    assert_eq!(shared.todos.watched().retries.get(&id).map(|&(tries, _)| tries), Some(1));
    assert!(shared.todos.with_db(|db| db.todo_by_id(&id)).unwrap().unwrap().queued);
    assert!(!shared.todos.watched().retries.contains_key(&started));
    // 没到时候不试。
    assert_eq!(shared.start_queued().1, next);

    shared.todos.watched().retries.insert(id.clone(), (2, past));
    shared.start_queued();
    assert_eq!(shared.start_queued(), (Some(false), None));
    let todo = shared.todos.with_db(|db| db.todo_by_id(&id)).unwrap().unwrap();
    assert_eq!(todo.outcome, Some(Outcome::Failed));
    assert!(todo.result.as_deref().is_some_and(|result| result.starts_with("Runode could not start it")));
    assert!(!holds_schedule(&todo, None, unix_now()));
}
