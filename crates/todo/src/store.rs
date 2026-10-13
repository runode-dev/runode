//! 任务库：任务和定时存在一个 SQLite 文件里，表结构的版本记在 `PRAGMA user_version`。比自己认得的
//! 新的库（装回了旧版本）不读也不写，免得弄坏新版本的数据。路径存成原样的字节，不要求是 UTF-8。

use std::{
    ffi::OsStr,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    str::FromStr as _,
    time::Duration,
};

use runode_types::{
    session::SessionId,
    todo::{Schedule, Todo, TodoSpec, When},
};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, TransactionBehavior, params};

/// 这个版本认得的表结构。
const VERSION: i64 = 2;

/// 完成了的任务给前端最近这么久（秒）的，更早的留在库里。
pub const DONE_KEPT: u64 = 7 * 24 * 60 * 60;

const SCHEMA: &str = "
CREATE TABLE todos (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    prompt TEXT NOT NULL,
    dir BLOB NOT NULL,
    agent TEXT NOT NULL,
    worktree INTEGER NOT NULL,
    created INTEGER NOT NULL,
    started INTEGER,
    session TEXT,
    worktree_dir BLOB,
    branch TEXT,
    done_at INTEGER,
    schedule TEXT,
    rank INTEGER NOT NULL,
    -- 完成后清过它的工作树和分支了（清不掉的留着，不再试）；取消完成时重置。
    swept INTEGER NOT NULL DEFAULT 0,
    number INTEGER NOT NULL
);
CREATE UNIQUE INDEX todos_number ON todos (number);
-- 下一张任务的编号：删掉的任务的编号不再发，所以不能按现有的最大值算。
CREATE TABLE meta (key TEXT PRIMARY KEY NOT NULL, value INTEGER NOT NULL);
INSERT INTO meta VALUES ('next_number', 1);
CREATE TABLE schedules (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    prompt TEXT NOT NULL,
    dir BLOB NOT NULL,
    agent TEXT NOT NULL,
    worktree INTEGER NOT NULL,
    kind TEXT NOT NULL,
    at INTEGER,
    hour INTEGER,
    minute INTEGER,
    days INTEGER,
    minutes INTEGER,
    last_fired INTEGER NOT NULL
);
";

const TODO_COLUMNS: &str = "id, number, title, prompt, dir, agent, worktree, created, started, session, worktree_dir, \
     branch, done_at, schedule, rank";
const SCHEDULE_COLUMNS: &str =
    "id, title, prompt, dir, agent, worktree, kind, at, hour, minute, days, minutes, last_fired";

/// 库出了错：错误说明。
pub type Result<T> = std::result::Result<T, String>;

fn failed(err: rusqlite::Error) -> String {
    format!("the todo database failed: {err}")
}

/// 打开着的任务库。
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn path_from(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&bytes))
}

fn secs(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn spec(row: &Row<'_>) -> rusqlite::Result<TodoSpec> {
    Ok(TodoSpec {
        title: row.get("title")?,
        prompt: row.get("prompt")?,
        dir: path_from(row.get("dir")?),
        agent: row.get("agent")?,
        worktree: row.get("worktree")?,
    })
}

fn todo(row: &Row<'_>) -> rusqlite::Result<Todo> {
    Ok(Todo {
        id: row.get("id")?,
        number: u64::try_from(row.get::<_, i64>("number")?).unwrap_or(0),
        spec: spec(row)?,
        created: secs(row.get("created")?),
        started: row.get::<_, Option<i64>>("started")?.map(secs),
        session: row.get::<_, Option<String>>("session")?.and_then(|id| SessionId::from_str(&id).ok()),
        worktree_dir: row.get::<_, Option<Vec<u8>>>("worktree_dir")?.map(path_from),
        branch: row.get("branch")?,
        done_at: row.get::<_, Option<i64>>("done_at")?.map(secs),
        schedule: row.get("schedule")?,
        rank: row.get("rank")?,
    })
}

fn schedule(row: &Row<'_>) -> rusqlite::Result<Schedule> {
    let small = |name: &str| -> rusqlite::Result<u8> {
        Ok(u8::try_from(row.get::<_, Option<i64>>(name)?.unwrap_or(0)).unwrap_or(u8::MAX))
    };
    let when = match row.get::<_, String>("kind")?.as_str() {
        "once" => When::Once { at: secs(row.get::<_, Option<i64>>("at")?.unwrap_or(0)) },
        "daily" => When::Daily { hour: small("hour")?, minute: small("minute")?, days: small("days")? },
        "every" => {
            When::Every { minutes: u32::try_from(row.get::<_, Option<i64>>("minutes")?.unwrap_or(0)).unwrap_or(0) }
        }
        _ => When::Unknown,
    };
    Ok(Schedule {
        id: row.get("id")?,
        spec: spec(row)?,
        when,
        last_fired: secs(row.get("last_fired")?),
        next_fire: None,
    })
}

/// 新的标识：8 个小写十六进制数字，和两张表里已有的都不重。
fn new_id(tx: &Transaction<'_>) -> Result<String> {
    for _ in 0..16 {
        let random = SessionId::random().map_err(|err| format!("cannot pick an id: {err}"))?;
        let id = format!("{:08x}", random.0 as u32);
        let taken: bool = tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM todos WHERE id = ?1) OR EXISTS (SELECT 1 FROM schedules WHERE id = ?1)",
                [&id],
                |row| row.get(0),
            )
            .map_err(failed)?;
        if !taken {
            return Ok(id);
        }
    }
    Err("cannot pick an unused id".into())
}

/// 在 `tx` 里加一张待办的任务，见 `Store::add_todo`。
fn insert_todo(tx: &Transaction<'_>, spec: &TodoSpec, now: u64, schedule: Option<&str>) -> Result<String> {
    let id = new_id(tx)?;
    let number: i64 =
        tx.query_row("SELECT value FROM meta WHERE key = 'next_number'", [], |row| row.get(0)).map_err(failed)?;
    tx.execute("UPDATE meta SET value = ?1 WHERE key = 'next_number'", [number + 1]).map_err(failed)?;
    tx.execute(
        "INSERT INTO todos (id, number, title, prompt, dir, agent, worktree, created, schedule, rank)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, (SELECT COALESCE(MAX(rank), 0) + 1 FROM todos))",
        params![
            id,
            number,
            spec.title,
            spec.prompt,
            path_bytes(&spec.dir),
            spec.agent,
            spec.worktree,
            now as i64,
            schedule
        ],
    )
    .map_err(failed)?;
    Ok(id)
}

/// `when` 拆成表里的几列：种类，以及 `at`、`hour`、`minute`、`days`、`minutes`。存不下的时刻报错：转成负数存进去
/// 会读回成 0，定时就再也不到点了。
fn when_columns(when: &When) -> Result<(&'static str, [Option<i64>; 5])> {
    Ok(match *when {
        When::Once { at } => {
            let at = i64::try_from(at).map_err(|_| format!("{at} is too far in the future"))?;
            ("once", [Some(at), None, None, None, None])
        }
        When::Daily { hour, minute, days } => {
            ("daily", [None, Some(hour.into()), Some(minute.into()), Some(days.into()), None])
        }
        When::Every { minutes } => ("every", [None, None, None, None, Some(minutes.into())]),
        When::Unknown => ("unknown", [None; 5]),
    })
}

impl Store {
    /// 打开 `path` 的库，没有时建一个空的。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        let mut conn = Connection::open(path).map_err(|err| format!("cannot open {}: {err}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(failed)?;
        // 事务一开始就拿写锁：几个进程同时先读后写时，DEFERRED 事务抢写锁会直接报库被锁，不按
        // `busy_timeout` 等。查版本和建表、升级也在同一个事务里，两个进程同时第一次打开时后到的看到建好的表。
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        let tx = conn.transaction().map_err(failed)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0)).map_err(failed)?;
        match version {
            0 => tx.execute_batch(&format!("{SCHEMA} PRAGMA user_version = {VERSION};")).map_err(failed)?,
            VERSION => {}
            newer => return Err(format!("{} was written by a newer runode (schema {newer})", path.display())),
        }
        tx.commit().map_err(failed)?;
        Ok(Self { conn })
    }

    /// 没完成的任务和最近 `DONE_KEPT` 内完成的，按先后排好。
    pub fn list_todos(&self, now: u64) -> Result<Vec<Todo>> {
        let since = now.saturating_sub(DONE_KEPT) as i64;
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {TODO_COLUMNS} FROM todos WHERE done_at IS NULL OR done_at >= ?1 ORDER BY rank, created"
            ))
            .map_err(failed)?;
        let rows = statement.query_map([since], todo).map_err(failed)?;
        rows.collect::<rusqlite::Result<_>>().map_err(failed)
    }

    pub fn todo_by_id(&self, id: &str) -> Result<Option<Todo>> {
        self.conn
            .query_row(&format!("SELECT {TODO_COLUMNS} FROM todos WHERE id = ?1"), [id], todo)
            .optional()
            .map_err(failed)
    }

    /// 前端写的任务（标识，或者 `5`、`#5` 这样的编号）对应的标识；正好也是哪张任务的标识时按标识算。没有时为空。
    pub fn resolve_todo(&self, text: &str) -> Result<Option<String>> {
        if self.todo_by_id(text)?.is_some() {
            return Ok(Some(text.to_owned()));
        }
        let Ok(number) = text.strip_prefix('#').unwrap_or(text).parse::<i64>() else { return Ok(None) };
        self.conn
            .query_row("SELECT id FROM todos WHERE number = ?1", [number], |row| row.get(0))
            .optional()
            .map_err(failed)
    }

    /// 定时开出来的、最新的一张没完成的任务。
    pub fn latest_open_todo_of(&self, schedule: &str) -> Result<Option<Todo>> {
        self.conn.query_row(
        &format!(
            "SELECT {TODO_COLUMNS} FROM todos WHERE schedule = ?1 AND done_at IS NULL ORDER BY created DESC LIMIT 1"
        ),
        [schedule],
        todo,
    )
    .optional()
    .map_err(failed)
    }

    /// 按建的先后排好的定时，`next_fire` 没填。
    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let mut statement =
            self.conn.prepare(&format!("SELECT {SCHEDULE_COLUMNS} FROM schedules ORDER BY rowid")).map_err(failed)?;
        let rows = statement.query_map([], schedule).map_err(failed)?;
        rows.collect::<rusqlite::Result<_>>().map_err(failed)
    }

    /// 加一张待办的任务，排在最后；`schedule` 是开出它的定时。返回新任务的标识。
    pub fn add_todo(&mut self, spec: &TodoSpec, now: u64, schedule: Option<&str>) -> Result<String> {
        let tx = self.conn.transaction().map_err(failed)?;
        let id = insert_todo(&tx, spec, now, schedule)?;
        tx.commit().map_err(failed)?;
        Ok(id)
    }

    /// 换掉任务要做的事，`done` 给了时改完成的时刻（`Some(None)` 是取消完成）。没有这个任务时为假。
    pub fn update_todo(&mut self, id: &str, spec: Option<&TodoSpec>, done_at: Option<Option<u64>>) -> Result<bool> {
        let tx = self.conn.transaction().map_err(failed)?;
        if let Some(spec) = spec {
            tx.execute(
                "UPDATE todos SET title = ?2, prompt = ?3, dir = ?4, agent = ?5, worktree = ?6 WHERE id = ?1",
                params![id, spec.title, spec.prompt, path_bytes(&spec.dir), spec.agent, spec.worktree],
            )
            .map_err(failed)?;
        }
        if let Some(done_at) = done_at {
            tx.execute(
                "UPDATE todos SET done_at = ?2, swept = 0 WHERE id = ?1",
                params![id, done_at.map(|at| at as i64)],
            )
            .map_err(failed)?;
        }
        let exists = tx
            .query_row("SELECT EXISTS (SELECT 1 FROM todos WHERE id = ?1)", [id], |row| row.get(0))
            .map_err(failed)?;
        tx.commit().map_err(failed)?;
        Ok(exists)
    }

    /// 记下任务开始了：时刻、会话，以及给它开的工作树和分支。任务已经不在了（开的途中被别的进程删掉）时
    /// 报错，调用方好收掉刚开的会话和工作树。
    pub fn set_started(
        &self,
        id: &str,
        started: u64,
        session: SessionId,
        worktree: Option<(&Path, &str)>,
    ) -> Result<()> {
        let changed = self
            .conn
            .execute(
                "UPDATE todos SET started = ?2, session = ?3,
             worktree_dir = COALESCE(?4, worktree_dir), branch = COALESCE(?5, branch)
         WHERE id = ?1",
                params![
                    id,
                    started as i64,
                    session.to_string(),
                    worktree.map(|(dir, _)| path_bytes(dir)),
                    worktree.map(|(_, branch)| branch),
                ],
            )
            .map_err(failed)?;
        if changed == 0 {
            return Err(format!("no todo {id}"));
        }
        Ok(())
    }

    /// 完成了、有工作树、还没清过的任务：宿主清掉它们的工作树和分支后用 `set_swept` 记下。
    pub fn unswept_done(&self) -> Result<Vec<Todo>> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {TODO_COLUMNS} FROM todos WHERE done_at IS NOT NULL AND worktree_dir IS NOT NULL AND swept = 0"
            ))
            .map_err(failed)?;
        let rows = statement.query_map([], todo).map_err(failed)?;
        rows.collect::<rusqlite::Result<_>>().map_err(failed)
    }

    /// 记下清过任务的工作树了：删掉了的工作树、分支不再记着。
    pub fn set_swept(&self, id: &str, worktree_gone: bool, branch_gone: bool) -> Result<()> {
        self.conn
            .execute(
                "UPDATE todos SET swept = 1,
             worktree_dir = CASE WHEN ?2 THEN NULL ELSE worktree_dir END,
             branch = CASE WHEN ?3 THEN NULL ELSE branch END
         WHERE id = ?1",
                params![id, worktree_gone, branch_gone],
            )
            .map_err(failed)?;
        Ok(())
    }

    /// 删掉任务，返回删掉的那张；没有时为空。
    pub fn remove_todo(&self, id: &str) -> Result<Option<Todo>> {
        // 一条语句删掉并取回：先读再删的话，中间别的进程改过的会读成旧的。
        self.conn
            .query_row(&format!("DELETE FROM todos WHERE id = ?1 RETURNING {TODO_COLUMNS}"), [id], todo)
            .optional()
            .map_err(failed)
    }

    /// 把任务挪到 `before` 前面，为空时挪到最后。没有这个任务、或者没有 `before` 时报错。
    pub fn move_todo(&mut self, id: &str, before: Option<&str>) -> Result<()> {
        let tx = self.conn.transaction().map_err(failed)?;
        let rank = |id: &str| -> Result<i64> {
            tx.query_row("SELECT rank FROM todos WHERE id = ?1", [id], |row| row.get(0))
                .optional()
                .map_err(failed)?
                .ok_or_else(|| format!("no todo {id}"))
        };
        rank(id)?;
        let target = match before {
            Some(before) if before == id => return Ok(()),
            Some(before) => {
                let target = rank(before)?;
                tx.execute("UPDATE todos SET rank = rank + 1 WHERE rank >= ?1", [target]).map_err(failed)?;
                target
            }
            None => {
                tx.query_row("SELECT COALESCE(MAX(rank), 0) + 1 FROM todos", [], |row| row.get(0)).map_err(failed)?
            }
        };
        tx.execute("UPDATE todos SET rank = ?2 WHERE id = ?1", params![id, target]).map_err(failed)?;
        tx.commit().map_err(failed)
    }

    /// 加一个定时，从 `now` 算起。返回新定时的标识。
    pub fn add_schedule(&mut self, spec: &TodoSpec, when: &When, now: u64) -> Result<String> {
        let tx = self.conn.transaction().map_err(failed)?;
        let id = new_id(&tx)?;
        let (kind, [at, hour, minute, days, minutes]) = when_columns(when)?;
        tx.execute(
        &format!(
            "INSERT INTO schedules ({SCHEDULE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
        ),
        params![
            id,
            spec.title,
            spec.prompt,
            path_bytes(&spec.dir),
            spec.agent,
            spec.worktree,
            kind,
            at,
            hour,
            minute,
            days,
            minutes,
            now as i64
        ],
    )
    .map_err(failed)?;
        tx.commit().map_err(failed)?;
        Ok(id)
    }

    /// 改定时，只改给了的部分；改了 `when` 时从 `now` 重新算起。没有这个定时时为假。
    pub fn update_schedule(
        &mut self,
        id: &str,
        spec: Option<&TodoSpec>,
        when: Option<&When>,
        now: u64,
    ) -> Result<bool> {
        let tx = self.conn.transaction().map_err(failed)?;
        if let Some(spec) = spec {
            tx.execute(
                "UPDATE schedules SET title = ?2, prompt = ?3, dir = ?4, agent = ?5, worktree = ?6 WHERE id = ?1",
                params![id, spec.title, spec.prompt, path_bytes(&spec.dir), spec.agent, spec.worktree],
            )
            .map_err(failed)?;
        }
        if let Some(when) = when {
            let (kind, [at, hour, minute, days, minutes]) = when_columns(when)?;
            tx.execute(
                "UPDATE schedules SET kind = ?2, at = ?3, hour = ?4, minute = ?5, days = ?6, minutes = ?7,
                     last_fired = ?8
                 WHERE id = ?1",
                params![id, kind, at, hour, minute, days, minutes, now as i64],
            )
            .map_err(failed)?;
        }
        let exists = tx
            .query_row("SELECT EXISTS (SELECT 1 FROM schedules WHERE id = ?1)", [id], |row| row.get(0))
            .map_err(failed)?;
        tx.commit().map_err(failed)?;
        Ok(exists)
    }

    /// 定时 `id` 到点了：照库里它现在的样子加一张任务、记下到点的时刻（一次性的删掉），都在一个事务里，
    /// 中途失败或者进程退出时定时原样留着、下一轮再试。定时已经删掉了、或者照现在的样子还没到点（读出来
    /// 以后被改过）时什么都不做，为空。返回新任务的标识。
    pub fn fire_schedule(&mut self, id: &str, now: u64) -> Result<Option<String>> {
        let tx = self.conn.transaction().map_err(failed)?;
        let Some(current) = tx
            .query_row(&format!("SELECT {SCHEDULE_COLUMNS} FROM schedules WHERE id = ?1"), [id], schedule)
            .optional()
            .map_err(failed)?
        else {
            return Ok(None);
        };
        if !crate::next_fire(&current.when, current.last_fired).is_some_and(|at| at <= now) {
            return Ok(None);
        }
        let todo = insert_todo(&tx, &current.spec, now, Some(id))?;
        if matches!(current.when, When::Once { .. }) {
            tx.execute("DELETE FROM schedules WHERE id = ?1", [id]).map_err(failed)?;
        } else {
            tx.execute("UPDATE schedules SET last_fired = ?2 WHERE id = ?1", params![id, now as i64])
                .map_err(failed)?;
        }
        tx.commit().map_err(failed)?;
        Ok(Some(todo))
    }

    /// 记下定时到点了、这次跳过，不开任务。
    pub fn set_fired(&self, id: &str, at: u64) -> Result<()> {
        self.conn
            .execute("UPDATE schedules SET last_fired = ?2 WHERE id = ?1", params![id, at as i64])
            .map_err(failed)?;
        Ok(())
    }

    /// 删掉定时；没有时为假。
    pub fn remove_schedule(&self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM schedules WHERE id = ?1", [id]).map_err(failed)? > 0)
    }
}
