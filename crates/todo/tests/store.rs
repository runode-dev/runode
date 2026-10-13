//! 任务库：任务和定时读写的来回、排序和编号，旧库的升级，不碰比自己新的库和不是库的文件。

use std::path::{Path, PathBuf};

use runode_todo::{DONE_KEPT, Store};
use runode_types::{
    session::SessionId,
    todo::{TodoSpec, When},
};
use rusqlite::Connection;
fn spec(title: &str) -> TodoSpec {
    TodoSpec {
        title: title.into(),
        prompt: String::new(),
        dir: "/src/app".into(),
        agent: "claude".into(),
        worktree: true,
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
    store.set_started(&b, 150, session, Some((Path::new("/src/app/.worktrees/b"), "runode/b"))).unwrap();
    assert!(store.set_started("nothere", 150, session, None).is_err());
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

    // 完成了的只给最近七天的，取消完成后又回来。
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
fn refuses_a_database_from_a_newer_runode_and_leaves_it_alone() {
    let path = temp_db("newer");
    drop(Store::open(&path).unwrap());
    Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 99").unwrap();
    let err = Store::open(&path).unwrap_err();
    assert!(err.contains("newer runode"), "{err}");
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

#[test]
fn numbers_the_todos_of_a_version_1_database_in_the_order_they_were_added() {
    let path = temp_db("v1");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE todos (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, prompt TEXT NOT NULL,
                 dir BLOB NOT NULL, agent TEXT NOT NULL, worktree INTEGER NOT NULL, created INTEGER NOT NULL,
                 started INTEGER, session TEXT, worktree_dir BLOB, branch TEXT, done_at INTEGER, schedule TEXT,
                 rank INTEGER NOT NULL, swept INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE schedules (id TEXT PRIMARY KEY NOT NULL, title TEXT NOT NULL, prompt TEXT NOT NULL,
                 dir BLOB NOT NULL, agent TEXT NOT NULL, worktree INTEGER NOT NULL, kind TEXT NOT NULL,
                 at INTEGER, hour INTEGER, minute INTEGER, days INTEGER, minutes INTEGER,
                 last_fired INTEGER NOT NULL);
             INSERT INTO todos (id, title, prompt, dir, agent, worktree, created, rank)
                 VALUES ('0000000b', 'b', '', x'2f', 'claude', 0, 1, 2), ('0000000a', 'a', '', x'2f', 'claude', 0, 2, 1);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    let numbers: Vec<(String, u64)> =
        store.list_todos(10).unwrap().into_iter().map(|todo| (todo.id, todo.number)).collect();
    assert_eq!(numbers, [("0000000a".into(), 2), ("0000000b".into(), 1)]);
    let added = store.add_todo(&spec("c"), 3, None).unwrap();
    assert_eq!(store.todo_by_id(&added).unwrap().unwrap().number, 3);
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
