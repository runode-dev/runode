//! 任务库：任务和定时读写的来回、排序和编号，不碰别的版本的库和不是库的文件。

use std::path::{Path, PathBuf};

use runode_todo::{DONE_KEPT, Store};
use runode_types::{
    session::SessionId,
    todo::{Outcome, TodoSpec, When},
};
use rusqlite::Connection;
fn spec(title: &str) -> TodoSpec {
    TodoSpec {
        title: title.into(),
        prompt: String::new(),
        dir: "/src/app".into(),
        agent: "claude".into(),
        worktree: true,
        ..TodoSpec::default()
    }
}

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-todos-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("todos.db")
}

fn ids(store: &Store, now: u64) -> Vec<String> {
    store.list_todos(now).unwrap().into_iter().map(|todo| todo.id).collect()
}

#[test]
fn todos_round_trip_and_keep_their_order() {
    let path = temp_db("round-trip");
    let mut store = Store::open(&path).unwrap();
    let a = store.add_todo(&spec("a"), 100, None).unwrap();
    let b = store.add_todo(&spec("b"), 101, Some("sched01")).unwrap();
    let c = store.add_todo(&spec("c"), 102, None).unwrap();
    assert_eq!(a.len(), 8);
    assert_eq!(ids(&store, 200), [a.clone(), b.clone(), c.clone()]);

    let session = SessionId(0xabc);
    store.set_started(&b, 150, session, Some((Path::new("/src/app/.worktrees/b"), "runode/b")), true).unwrap();
    assert!(store.set_started("nothere", 150, session, None, true).is_err());
    let read = store.todo_by_id(&b).unwrap().unwrap();
    assert_eq!(
        (read.started, read.session, read.worktree_dir.as_deref(), read.branch.as_deref(), read.schedule.as_deref()),
        (Some(150), Some(session), Some(Path::new("/src/app/.worktrees/b")), Some("runode/b"), Some("sched01"))
    );
    assert_eq!(store.latest_open_todo_of("sched01").unwrap().map(|todo| todo.id), Some(b.clone()));

    // 挪到前面、挪到最后、挪到不存在的任务前面。
    store.move_todo(&c, Some(&a)).unwrap();
    assert_eq!(ids(&store, 200), [c.clone(), a.clone(), b.clone()]);
    store.move_todo(&c, None).unwrap();
    assert_eq!(ids(&store, 200), [a.clone(), b.clone(), c.clone()]);
    assert!(store.move_todo(&a, Some("nothere")).is_err());
    assert!(store.move_todo("nothere", None).is_err());

    // 完成了的只给 `DONE_KEPT` 内的，取消完成后又回来。
    assert!(store.update_todo(&a, None, Some(Some(200))).unwrap());
    assert_eq!(ids(&store, 200 + DONE_KEPT), [a.clone(), b.clone(), c.clone()]);
    assert_eq!(ids(&store, 201 + DONE_KEPT), [b.clone(), c.clone()]);
    assert!(store.update_todo(&a, Some(&spec("a2")), Some(None)).unwrap());
    assert_eq!(store.todo_by_id(&a).unwrap().unwrap().spec.title, "a2");
    assert!(!store.update_todo("nothere", None, Some(None)).unwrap());

    // 完成了、有工作树的待清理；清过以后不再列，取消完成后重新算。
    assert!(store.update_todo(&b, None, Some(Some(300))).unwrap());
    assert_eq!(store.unswept_done().unwrap().iter().map(|todo| todo.id.as_str()).collect::<Vec<_>>(), [b.as_str()]);
    store.set_swept(&b, true, false).unwrap();
    assert!(store.unswept_done().unwrap().is_empty());
    let swept = store.todo_by_id(&b).unwrap().unwrap();
    assert_eq!((swept.worktree_dir, swept.branch.as_deref()), (None, Some("runode/b")));
    assert!(store.update_todo(&b, None, Some(None)).unwrap());

    assert_eq!(store.remove_todo(&b).unwrap().map(|todo| todo.id), Some(b));
    assert_eq!(store.remove_todo("nothere").unwrap(), None);

    // 重新打开，数据还在。
    drop(store);
    assert_eq!(ids(&Store::open(&path).unwrap(), 300), [a, c]);
}

#[test]
fn schedules_round_trip() {
    let mut store = Store::open(&temp_db("schedules")).unwrap();
    let daily = When::Daily { hour: 9, minute: 30, days: 0b0011_1110 };
    let id = store.add_schedule(&spec("report"), &daily, 100).unwrap();
    let every = store.add_schedule(&spec("poll"), &When::Every { minutes: 15 }, 100).unwrap();
    let listed = store.list_schedules().unwrap();
    assert_eq!(
        listed.iter().map(|s| (s.id.as_str(), &s.when, s.last_fired)).collect::<Vec<_>>(),
        [(id.as_str(), &daily, 100), (every.as_str(), &When::Every { minutes: 15 }, 100)]
    );

    store.set_fired(&id, 500).unwrap();
    assert_eq!(store.list_schedules().unwrap()[0].last_fired, 500);
    // 改了 `when` 时从现在重新算起；只改 spec 不动它。
    assert!(store.update_schedule(&id, Some(&spec("weekly")), None, 600).unwrap());
    assert_eq!(store.list_schedules().unwrap()[0].last_fired, 500);
    assert!(store.update_schedule(&id, None, Some(&When::Once { at: 9_999 }), 700).unwrap());
    let changed = &store.list_schedules().unwrap()[0];
    assert_eq!(
        (changed.spec.title.as_str(), &changed.when, changed.last_fired),
        ("weekly", &When::Once { at: 9_999 }, 700)
    );
    assert!(!store.update_schedule("nothere", None, None, 700).unwrap());
    // 存不下的时刻不收，原来的不动。
    assert!(store.add_schedule(&spec("never"), &When::Once { at: u64::MAX }, 700).is_err());
    assert!(store.update_schedule(&id, Some(&spec("never")), Some(&When::Once { at: u64::MAX }), 800).is_err());
    let kept = &store.list_schedules().unwrap()[0];
    assert_eq!((kept.spec.title.as_str(), &kept.when), ("weekly", &When::Once { at: 9_999 }));

    assert!(store.remove_schedule(&id).unwrap());
    assert!(!store.remove_schedule(&id).unwrap());
}

#[test]
fn skills_subagent_attachments_and_auto_round_trip() {
    let mut store = Store::open(&temp_db("extras")).unwrap();
    let full = TodoSpec {
        skills: vec!["review".into(), "plugin:fix".into()],
        subagent: Some("reviewer".into()),
        attachments: vec!["/tmp/a b.png".into(), "/src/app/spec.md".into()],
        auto: true,
        auto_done: true,
        time_limit: Some(45),
        ..spec("full")
    };
    let id = store.add_todo(&full, 100, None).unwrap();
    assert_eq!(store.todo_by_id(&id).unwrap().unwrap().spec, full);
    let empty = spec("empty");
    assert!(store.update_todo(&id, Some(&empty), None).unwrap());
    assert_eq!(store.todo_by_id(&id).unwrap().unwrap().spec, empty);
    let schedule = store.add_schedule(&full, &When::Every { minutes: 5 }, 100).unwrap();
    assert_eq!(store.list_schedules().unwrap()[0].spec, full);
    let fired = store.fire_schedule(&schedule, 1_000).unwrap().unwrap();
    assert_eq!(store.todo_by_id(&fired).unwrap().unwrap().spec, full);
    assert!(store.update_schedule(&schedule, Some(&empty), None, 100).unwrap());
    assert_eq!(store.list_schedules().unwrap()[0].spec, empty);
}

/// 版本 3 的库一路升上来：原有的任务和定时留着，`auto_done` 读成关着、没有结局，之后能照常写。
#[test]
fn upgrades_a_version_3_database() {
    let path = temp_db("v3");
    let mut store = Store::open(&path).unwrap();
    let todo = store.add_todo(&spec("old"), 100, None).unwrap();
    store.add_schedule(&spec("old"), &When::Every { minutes: 5 }, 100).unwrap();
    drop(store);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "ALTER TABLE todos DROP COLUMN auto_done;
             ALTER TABLE schedules DROP COLUMN auto_done;
             ALTER TABLE todos DROP COLUMN outcome;
             ALTER TABLE todos DROP COLUMN result;
             ALTER TABLE todos DROP COLUMN time_limit;
             ALTER TABLE todos DROP COLUMN queued;
             ALTER TABLE todos DROP COLUMN after_todo;
             ALTER TABLE schedules DROP COLUMN time_limit;
             ALTER TABLE schedules DROP COLUMN paused;
             ALTER TABLE schedules DROP COLUMN cron;
             DELETE FROM meta WHERE key = 'max_running';
             PRAGMA user_version = 3;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    let read = store.todo_by_id(&todo).unwrap().unwrap();
    assert_eq!((read.spec, read.outcome, read.result, read.queued), (spec("old"), None, None, false));
    let schedule = &store.list_schedules().unwrap()[0];
    assert_eq!((&schedule.spec, schedule.paused), (&spec("old"), false));
    assert_eq!(store.max_running().unwrap(), 0);
    let auto_done = TodoSpec { auto_done: true, ..spec("old") };
    assert!(store.update_todo(&todo, Some(&auto_done), None).unwrap());
    assert!(store.todo_by_id(&todo).unwrap().unwrap().spec.auto_done);
    assert!(store.set_outcome(&todo, Some(Outcome::Ok), Some("done")).unwrap());
}

/// 结局和结果：只改给了的那项；重新开始时清掉，接着上次的对话时留着。
#[test]
fn outcomes_are_kept_until_the_todo_starts_afresh() {
    let store_path = temp_db("outcome");
    let mut store = Store::open(&store_path).unwrap();
    let id = store.add_todo(&spec("t"), 100, None).unwrap();
    let read = |store: &Store| {
        let todo = store.todo_by_id(&id).unwrap().unwrap();
        (todo.outcome, todo.result)
    };
    assert!(store.set_outcome(&id, None, Some("screen")).unwrap());
    assert_eq!(read(&store), (None, Some("screen".into())));
    assert!(store.set_outcome(&id, Some(Outcome::Failed), Some("tests fail")).unwrap());
    assert!(store.set_outcome(&id, Some(Outcome::Silent), None).unwrap());
    assert_eq!(read(&store), (Some(Outcome::Silent), Some("tests fail".into())));
    assert!(!store.set_outcome("nothere", Some(Outcome::Ok), None).unwrap());
    // agent 报的结局把结果整个换掉，没带话时清掉。
    assert!(store.report(&id, Outcome::Ok, None).unwrap());
    assert_eq!(read(&store), (Some(Outcome::Ok), None));
    assert!(store.report(&id, Outcome::Silent, Some("tests fail")).unwrap());
    assert!(!store.report("nothere", Outcome::Ok, None).unwrap());

    store.set_started(&id, 200, SessionId(1), None, false).unwrap();
    assert_eq!(read(&store), (Some(Outcome::Silent), Some("tests fail".into())));
    store.set_started(&id, 300, SessionId(2), None, true).unwrap();
    assert_eq!(read(&store), (None, None));
    // 别的版本写的结局读成 `Unknown`。
    Connection::open(&store_path).unwrap().execute("UPDATE todos SET outcome = 'later'", []).unwrap();
    assert_eq!(read(&store).0, Some(Outcome::Unknown));
}

/// 排队：开始以后不再排队；等的任务不能绕回自己，删掉了就不再等。
#[test]
fn queued_todos_wait_without_cycles() {
    let mut store = Store::open(&temp_db("queue")).unwrap();
    let first = store.add_todo(&spec("first"), 100, None).unwrap();
    let second = store.add_todo(&spec("second"), 100, None).unwrap();
    let third = store.add_todo(&spec("third"), 100, None).unwrap();
    let read = |store: &Store, id: &str| {
        let todo = store.todo_by_id(id).unwrap().unwrap();
        (todo.queued, todo.after)
    };
    assert!(store.set_queued(&second, true, Some(&first)).unwrap());
    assert!(store.set_queued(&third, true, Some(&second)).unwrap());
    assert_eq!(read(&store, &third), (true, Some(second.clone())));
    let err = store.set_queued(&first, true, Some(&third)).unwrap_err();
    assert!(err.contains("wait for itself"), "{err}");
    assert!(store.set_queued(&first, true, Some(&first)).is_err());
    assert!(store.set_queued(&first, true, Some("nothere")).is_err());
    assert!(!store.set_queued("nothere", true, None).unwrap());
    // 不排队时也不再等。
    assert!(store.set_queued(&third, false, Some(&second)).unwrap());
    assert_eq!(read(&store, &third), (false, None));

    store.remove_todo(&first).unwrap();
    assert_eq!(read(&store, &second), (true, None));
    store.set_started(&second, 200, SessionId(1), None, true).unwrap();
    assert_eq!(read(&store, &second), (false, None));
    assert!(store.set_queued(&second, true, None).unwrap_err().contains("has started"));

    assert_eq!(store.max_running().unwrap(), 0);
    store.set_max_running(3).unwrap();
    assert_eq!(store.max_running().unwrap(), 3);
}

/// 停着的定时到点不开任务；接着用时从那一刻算起，停着时错过的不补。定时开出的任务排着队，cron 存得住。
#[test]
fn paused_schedules_do_not_fire() {
    let mut store = Store::open(&temp_db("pause")).unwrap();
    let every = store.add_schedule(&spec("poll"), &When::Every { minutes: 1 }, 100).unwrap();
    assert!(store.set_paused(&every, true, 150).unwrap());
    assert!(store.list_schedules().unwrap()[0].paused);
    assert_eq!(store.fire_schedule(&every, 1_000).unwrap(), None);
    assert!(store.set_paused(&every, false, 2_000).unwrap());
    // 已经在用时再说接着用，不挪起点。
    assert!(store.set_paused(&every, false, 3_000).unwrap());
    let schedule = &store.list_schedules().unwrap()[0];
    assert_eq!((schedule.paused, schedule.last_fired), (false, 2_000));
    assert!(!store.set_paused("nothere", true, 0).unwrap());
    let todo = store.fire_schedule(&every, 2_060).unwrap().unwrap();
    assert!(store.todo_by_id(&todo).unwrap().unwrap().queued);

    let cron = When::Cron { expr: "30 9 * * 1-5".into() };
    store.add_schedule(&spec("standup"), &cron, 100).unwrap();
    assert_eq!(store.list_schedules().unwrap()[1].when, cron);
}

#[test]
fn firing_a_schedule_follows_what_the_database_says_now() {
    let mut store = Store::open(&temp_db("fire")).unwrap();
    let every = store.add_schedule(&spec("poll"), &When::Every { minutes: 1 }, 100).unwrap();
    let once = store.add_schedule(&spec("deploy"), &When::Once { at: 200 }, 100).unwrap();

    // 还没到点：不开任务。
    assert_eq!(store.fire_schedule(&every, 150).unwrap(), None);
    // 到点：开一张照现在的内容的任务，记下到点的时刻。
    assert!(store.update_schedule(&every, Some(&spec("poll v2")), None, 0).unwrap());
    let todo = store.fire_schedule(&every, 160).unwrap().unwrap();
    let todo = store.todo_by_id(&todo).unwrap().unwrap();
    assert_eq!((todo.spec.title.as_str(), todo.schedule.as_deref()), ("poll v2", Some(every.as_str())));
    assert_eq!(store.list_schedules().unwrap()[0].last_fired, 160);
    // 改了时间（从改的时刻重新算起）以后，按旧的样子到点的这次不开。
    assert!(store.update_schedule(&every, None, Some(&When::Every { minutes: 10 }), 300).unwrap());
    assert_eq!(store.fire_schedule(&every, 400).unwrap(), None);

    // 一次性的开完就删；删掉了的不再开。
    assert!(store.fire_schedule(&once, 250).unwrap().is_some());
    assert_eq!(store.fire_schedule(&once, 250).unwrap(), None);
    assert!(store.remove_schedule(&every).unwrap());
    assert_eq!(store.fire_schedule(&every, 10_000).unwrap(), None);
    assert!(store.list_schedules().unwrap().is_empty());
    assert_eq!(store.list_todos(10_000).unwrap().len(), 2);
}

/// 跳过这一次也照库里现在的样子办：改过、已经到过的不拿旧的到点时刻盖掉，一次性的跳过就删掉。
#[test]
fn skipping_a_schedule_follows_what_the_database_says_now() {
    let mut store = Store::open(&temp_db("skip")).unwrap();
    let every = store.add_schedule(&spec("poll"), &When::Every { minutes: 1 }, 100).unwrap();
    let once = store.add_schedule(&spec("deploy"), &When::Once { at: 200 }, 100).unwrap();

    assert!(!store.skip_schedule(&every, 150).unwrap());
    assert!(store.skip_schedule(&every, 160).unwrap());
    assert_eq!(store.list_schedules().unwrap()[0].last_fired, 160);
    // 决定跳过以后被改了时间（从改的时刻重新算起）：按旧的样子到点的这次不记。
    assert!(store.update_schedule(&every, None, Some(&When::Every { minutes: 10 }), 300).unwrap());
    assert!(!store.skip_schedule(&every, 400).unwrap());
    assert_eq!(store.list_schedules().unwrap()[0].last_fired, 300);

    // 一次性的挪到以后了：不删。到了点再跳过就删掉，删掉了的什么都不做。
    assert!(store.update_schedule(&once, None, Some(&When::Once { at: 1_000 }), 300).unwrap());
    assert!(!store.skip_schedule(&once, 400).unwrap());
    assert!(store.skip_schedule(&once, 1_000).unwrap());
    assert!(!store.skip_schedule(&once, 1_000).unwrap());
    assert_eq!(store.list_schedules().unwrap().iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), [every.as_str()]);
    assert!(store.list_todos(1_000).unwrap().is_empty());
}

#[test]
fn a_schedule_that_fails_to_add_its_todo_is_left_as_it_was() {
    let path = temp_db("fire-fails");
    let mut store = Store::open(&path).unwrap();
    let once = store.add_schedule(&spec("deploy"), &When::Once { at: 200 }, 100).unwrap();
    // 加任务那一步失败：发编号的计数没了。
    Connection::open(&path).unwrap().execute("DELETE FROM meta", []).unwrap();
    assert!(store.fire_schedule(&once, 250).is_err());
    let left = store.list_schedules().unwrap();
    assert_eq!((left.len(), left[0].last_fired), (1, 100));
    assert!(store.list_todos(250).unwrap().is_empty());
}

#[test]
fn refuses_a_database_from_another_runode_and_leaves_it_alone() {
    let path = temp_db("newer");
    drop(Store::open(&path).unwrap());
    Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 99").unwrap();
    let err = Store::open(&path).unwrap_err();
    assert!(err.contains("another runode version"), "{err}");
    let version: i64 = Connection::open(&path).unwrap().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
    assert_eq!(version, 99);
}

#[test]
fn does_not_overwrite_a_file_that_is_not_a_database() {
    let path = temp_db("garbage");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "not a database, keep me").unwrap();
    assert!(Store::open(&path).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a database, keep me");
}
#[test]
fn numbers_count_up_and_are_never_reused() {
    let mut store = Store::open(&temp_db("numbers")).unwrap();
    let first = store.add_todo(&spec("a"), 100, None).unwrap();
    let second = store.add_todo(&spec("b"), 101, None).unwrap();
    store.remove_todo(&second).unwrap();
    let third = store.add_todo(&spec("c"), 102, None).unwrap();
    let number = |id: &str| store.todo_by_id(id).unwrap().unwrap().number;
    assert_eq!((number(&first), number(&third)), (1, 3));

    assert_eq!(store.resolve_todo(&first).unwrap(), Some(first.clone()));
    assert_eq!(store.resolve_todo("3").unwrap(), Some(third));
    assert_eq!(store.resolve_todo("#1").unwrap(), Some(first));
    assert_eq!(store.resolve_todo("#2").unwrap(), None);
    assert_eq!(store.resolve_todo("nothere").unwrap(), None);
}

/// 两个进程同时加任务时都等对方写完，不报库被锁：先读后写的事务一开始就拿写锁，`busy_timeout` 才管用。
#[test]
fn concurrent_writers_wait_for_each_other() {
    let path = temp_db("concurrent");
    drop(Store::open(&path).unwrap());
    let writers: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                for at in 0..50 {
                    store.add_todo(&spec("t"), at, None).unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let mut numbers: Vec<u64> =
        Store::open(&path).unwrap().list_todos(100).unwrap().into_iter().map(|todo| todo.number).collect();
    numbers.sort_unstable();
    assert_eq!(numbers, (1..=100).collect::<Vec<_>>());
}

/// 完成了的记录不管多久以前的都翻得到，完成得晚的在前，分页、按定时筛。
#[test]
fn done_todos_page_newest_first() {
    let mut store = Store::open(&temp_db("done")).unwrap();
    let old = store.add_todo(&spec("old"), 100, Some("sched01")).unwrap();
    let new = store.add_todo(&spec("new"), 101, None).unwrap();
    let open = store.add_todo(&spec("open"), 102, Some("sched01")).unwrap();
    let mid = store.add_todo(&spec("mid"), 103, Some("sched01")).unwrap();
    store.update_todo(&old, None, Some(Some(200))).unwrap();
    store.update_todo(&new, None, Some(Some(400))).unwrap();
    store.update_todo(&mid, None, Some(Some(300))).unwrap();
    let done = |schedule, offset, limit| -> (Vec<String>, bool) {
        let (todos, more) = store.list_done_todos(schedule, offset, limit).unwrap();
        (todos.into_iter().map(|todo| todo.id).collect(), more)
    };
    assert_eq!(done(None, 0, 10), (vec![new.clone(), mid.clone(), old.clone()], false));
    // 正好翻到最后一张时说后面没了，少要一张时说还有。
    assert_eq!(done(None, 1, 1), (vec![mid.clone()], true));
    assert_eq!(done(None, 1, 2), (vec![mid.clone(), old.clone()], false));
    assert_eq!(done(Some("sched01"), 0, 10), (vec![mid, old], false));
    assert!(!done(None, 0, 10).0.contains(&open));
    // `DONE_KEPT` 的窗口只管看板，不管记录。
    assert!(ids(&store, 400 + DONE_KEPT + 1).iter().all(|id| *id == open));
}

/// 宿主收会话用的：完成了的任务开过的会话都给，不管完成了多久，没完成的和没开始的不给。
#[test]
fn done_sessions_lists_every_finished_todo_session() {
    let mut store = Store::open(&temp_db("done-sessions")).unwrap();
    let open = store.add_todo(&spec("open"), 100, None).unwrap();
    let recent = store.add_todo(&spec("recent"), 101, None).unwrap();
    let old = store.add_todo(&spec("old"), 102, None).unwrap();
    let never_started = store.add_todo(&spec("never"), 103, None).unwrap();
    store.set_started(&open, 110, SessionId(1), None, true).unwrap();
    store.set_started(&recent, 110, SessionId(2), None, true).unwrap();
    store.set_started(&old, 110, SessionId(3), None, true).unwrap();
    store.update_todo(&recent, None, Some(Some(200 + DONE_KEPT))).unwrap();
    store.update_todo(&old, None, Some(Some(1))).unwrap();
    store.update_todo(&never_started, None, Some(Some(200 + DONE_KEPT))).unwrap();
    let mut sessions = store.done_sessions().unwrap();
    sessions.sort_by_key(|session| session.0);
    assert_eq!(sessions, [SessionId(2), SessionId(3)]);
}
