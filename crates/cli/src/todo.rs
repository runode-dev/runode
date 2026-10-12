//! `todo` 和 `schedule` 命令：经宿主读写任务板（见 `runode_protocol::todo`）上的任务和定时。

use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};
use runode_protocol::{
    ClientMsg, HostMsg, SessionId, SessionInfo,
    todo::{Column, DEFAULT_AGENT, Schedule, Todo, TodoSpec, When, column},
};

use crate::{
    Env,
    args::{DAYS, Timing},
    client::{Connection, kind},
    commands::tilde,
    local_time::LocalTime,
};

/// 等 `StartTodo` 回话的最长时间：宿主可能先在大仓库里跑 `git worktree add`，git 自己的上限是 120 秒。
const START_TIMEOUT: Duration = Duration::from_secs(130);

/// `todo add`、`schedule add` 共有的部分，见 `args::Command::TodoAdd`。
pub(crate) struct Spec {
    pub(crate) title: String,
    pub(crate) prompt: String,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) agent: Option<String>,
    pub(crate) worktree: bool,
}

impl Spec {
    /// 照 `open --cwd`：相对路径按命令行的当前目录算；没给 agent 时用 `DEFAULT_AGENT`。
    fn resolve(self) -> Result<TodoSpec> {
        Ok(TodoSpec {
            title: self.title,
            prompt: self.prompt,
            dir: std::env::current_dir()?.join(self.cwd.unwrap_or_default()),
            agent: self.agent.unwrap_or_else(|| DEFAULT_AGENT.to_owned()),
            worktree: self.worktree,
        })
    }
}

/// 连上宿主；宿主不认得任务板的消息时报错，不发。
fn open(env: &Env) -> Result<Connection> {
    let connection = Connection::open(env)?;
    if !connection.todos {
        bail!("the running runode host is too old for the task board; quit runode and open it again");
    }
    Ok(connection)
}

/// 目录或它的上级里有 `.git`（仓库是目录，工作树和子模块是文件）。
// shortcut: 不认 GIT_DIR 这类环境变量指的仓库；认错了宿主会报错，那时再改成问 git。
fn in_git_repo(dir: &Path) -> bool {
    dir.ancestors().any(|dir| dir.join(".git").exists())
}

/// 加一张任务，打印它的标识；`start` 时接着开始它，再打印它的会话的标识。
pub(crate) fn add(env: &Env, spec: Spec, start: bool, out: &mut dyn Write) -> Result<()> {
    let mut spec = spec.resolve()?;
    // 宿主不给仓库外的目录开工作树；工作树是默认开着的，这里改成不要，免得命令白白失败。
    spec.worktree &= in_git_repo(&spec.dir);
    let connection = open(env)?;
    let req = connection.req();
    connection.send(&ClientMsg::AddTodo { req, spec })?;
    let id = added(&connection)?;
    writeln!(out, "{id}")?;
    if start {
        let session = start_todo(&connection, &id).map_err(|err| err.context(format!("added todo {id}")))?;
        writeln!(out, "{session}")?;
    }
    Ok(())
}

/// 开始任务，打印它的会话的标识。
pub(crate) fn start(env: &Env, id: &str, out: &mut dyn Write) -> Result<()> {
    let session = start_todo(&open(env)?, id)?;
    writeln!(out, "{session}")?;
    Ok(())
}

fn start_todo(connection: &Connection, id: &str) -> Result<SessionId> {
    let req = connection.req();
    connection.send(&ClientMsg::StartTodo { req, id: id.to_owned() })?;
    match connection.reply_by(Instant::now() + START_TIMEOUT) {
        Ok(HostMsg::Opened { id, .. }) => Ok(id),
        Ok(other) => bail!("unexpected answer: {}", kind(&other)),
        Err(err) => Err(err.context(format!("cannot start todo {id}"))),
    }
}

/// 回 `AddTodo`、`AddSchedule` 的 `TodoAdded` 里的标识。
fn added(connection: &Connection) -> Result<String> {
    match connection.reply()? {
        HostMsg::TodoAdded { id, .. } => Ok(id),
        other => bail!("unexpected answer: {}", kind(&other)),
    }
}

/// 把任务标成完成。
pub(crate) fn done(env: &Env, id: String) -> Result<()> {
    let connection = open(env)?;
    let req = connection.req();
    connection.request_done(&ClientMsg::UpdateTodo { req, id, spec: None, done: Some(true) })
}

pub(crate) fn remove(env: &Env, id: String) -> Result<()> {
    let connection = open(env)?;
    let req = connection.req();
    connection.request_done(&ClientMsg::RemoveTodo { req, id })
}

/// 任务板上的任务和定时。
fn board(connection: &Connection) -> Result<(Vec<Todo>, Vec<Schedule>)> {
    let req = connection.req();
    connection.send(&ClientMsg::ListTodos { req })?;
    match connection.reply()? {
        HostMsg::Todos { todos, schedules, .. } => Ok((todos, schedules)),
        other => bail!("unexpected answer: {}", kind(&other)),
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// 列名，同 JSON 里的写法（`in_progress`）。
fn column_name(column: Column) -> String {
    serde_json::to_value(column).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default()
}

/// 列出任务和各自在看板的哪一列，按看板从左到右，同一列里照宿主给的先后。
pub(crate) fn list(env: &Env, json: bool, out: &mut dyn Write) -> Result<()> {
    let connection = open(env)?;
    connection.send(&ClientMsg::ListSessions)?;
    let sessions: Vec<SessionInfo> = match connection.reply()? {
        HostMsg::SessionList { sessions } => sessions,
        other => bail!("unexpected answer: {}", kind(&other)),
    };
    let (todos, _) = board(&connection)?;
    let now = unix_now();
    let mut todos: Vec<(Todo, Column)> = todos
        .into_iter()
        .map(|todo| {
            let column = column(&todo, sessions.iter().find(|info| Some(info.id) == todo.session), now);
            (todo, column)
        })
        .collect();
    todos.sort_by_key(|(_, column)| Column::ALL.iter().position(|c| c == column));
    if json {
        let todos = todos
            .iter()
            .map(|(todo, column)| {
                let mut value = serde_json::to_value(todo)?;
                value["column"] = column_name(*column).into();
                Ok(value)
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        serde_json::to_writer_pretty(&mut *out, &todos)?;
        writeln!(out)?;
        return Ok(());
    }
    if todos.is_empty() {
        writeln!(out, "No todos. Add one with `runode todo add`.")?;
        return Ok(());
    }
    let home = env.dirs.home.as_deref();
    let rows = todos.iter().map(|(todo, column)| {
        vec![
            todo.id.clone(),
            column_name(*column),
            todo.spec.agent.clone(),
            todo.spec.title.clone(),
            tilde(&todo.spec.dir, home),
        ]
    });
    table(&["ID", "COLUMN", "AGENT", "TITLE", "DIR"], rows, out)
}

/// 加一个定时，打印它的标识。工作树要用户要了才开，目录不在 git 仓库里时报错。
pub(crate) fn add_schedule(env: &Env, spec: Spec, timing: Timing, out: &mut dyn Write) -> Result<()> {
    let spec = spec.resolve()?;
    if spec.worktree && !in_git_repo(&spec.dir) {
        bail!("{} is not in a git repository, so it cannot have a worktree", spec.dir.display());
    }
    let when = match timing {
        Timing::Repeat(when) => when,
        Timing::At(at) => {
            let unix = at.unix().ok_or_else(|| anyhow!("there is no {at} here"))?;
            if unix <= unix_now() {
                bail!("{at} has passed");
            }
            When::Once { at: unix }
        }
    };
    let connection = open(env)?;
    let req = connection.req();
    connection.send(&ClientMsg::AddSchedule { req, spec, when })?;
    writeln!(out, "{}", added(&connection)?)?;
    Ok(())
}

pub(crate) fn list_schedules(env: &Env, json: bool, out: &mut dyn Write) -> Result<()> {
    let (_, schedules) = board(&open(env)?)?;
    if json {
        serde_json::to_writer_pretty(&mut *out, &schedules)?;
        writeln!(out)?;
        return Ok(());
    }
    if schedules.is_empty() {
        writeln!(out, "No schedules. Add one with `runode schedule add`.")?;
        return Ok(());
    }
    let home = env.dirs.home.as_deref();
    let local = |at: u64| LocalTime::from_unix(at).map_or_else(|| at.to_string(), |at| at.to_string());
    let rows = schedules.iter().map(|schedule| {
        vec![
            schedule.id.clone(),
            when_text(&schedule.when, local),
            schedule.next_fire.map_or_else(|| "-".into(), local),
            schedule.spec.agent.clone(),
            schedule.spec.title.clone(),
            tilde(&schedule.spec.dir, home),
        ]
    });
    table(&["ID", "WHEN", "NEXT", "AGENT", "TITLE", "DIR"], rows, out)
}

pub(crate) fn remove_schedule(env: &Env, id: String) -> Result<()> {
    let connection = open(env)?;
    let req = connection.req();
    connection.request_done(&ClientMsg::RemoveSchedule { req, id })
}

/// 定时什么时候到点，人看的写法：`daily 09:30 mon-fri`、`every 15m`、`once 2026-10-13 14:00`。
fn when_text(when: &When, local: impl Fn(u64) -> String) -> String {
    match *when {
        When::Once { at } => format!("once {}", local(at)),
        When::Daily { hour, minute, days } => {
            let days = days_text(days);
            let sep = if days.is_empty() { "" } else { " " };
            format!("daily {hour:02}:{minute:02}{sep}{days}")
        }
        When::Every { minutes } => format!("every {minutes}m"),
        When::Unknown => "unknown".into(),
    }
}

/// `When::Daily` 的 `days` 写成 `mon-fri`、`sun,sat` 这样；每天（0 或者七天都在）时为空。连着三天以上
/// 写成范围。
fn days_text(days: u8) -> String {
    if days & 0x7f == 0 || days & 0x7f == 0x7f {
        return String::new();
    }
    let mut parts = Vec::new();
    let mut day = 0;
    while day < 7 {
        if days & (1 << day) == 0 {
            day += 1;
            continue;
        }
        let first = day;
        while day + 1 < 7 && days & (1 << (day + 1)) != 0 {
            day += 1;
        }
        match day - first {
            0 => parts.push(DAYS[first].to_owned()),
            1 => parts.extend([DAYS[first].to_owned(), DAYS[day].to_owned()]),
            _ => parts.push(format!("{}-{}", DAYS[first], DAYS[day])),
        }
        day += 1;
    }
    parts.join(",")
}

/// 打印表格：各列按最宽的格子补齐，列间两个空格。
fn table(header: &[&str], rows: impl Iterator<Item = Vec<String>>, out: &mut dyn Write) -> Result<()> {
    let header: Vec<String> = header.iter().map(|&cell| cell.to_owned()).collect();
    let rows: Vec<Vec<String>> = std::iter::once(header).chain(rows).collect();
    let mut widths = vec![0; rows[0].len()];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in &rows {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell}{}", " ".repeat(width - cell.chars().count())))
            .collect();
        writeln!(out, "{}", cells.join("  ").trim_end())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_are_written_for_people() {
        let local = |at: u64| format!("@{at}");
        assert_eq!(when_text(&When::Daily { hour: 9, minute: 5, days: 0 }, local), "daily 09:05");
        assert_eq!(when_text(&When::Daily { hour: 9, minute: 30, days: 0b011_1110 }, local), "daily 09:30 mon-fri");
        assert_eq!(when_text(&When::Every { minutes: 15 }, local), "every 15m");
        assert_eq!(when_text(&When::Once { at: 7 }, local), "once @7");
        assert_eq!(days_text(0b100_0001), "sun,sat");
        assert_eq!(days_text(0b000_0110), "mon,tue");
        assert_eq!(days_text(0b010_1010), "mon,wed,fri");
        assert_eq!(days_text(0x7f), "");
    }
}
