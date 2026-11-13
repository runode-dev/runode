//! `todo` 和 `schedule` 命令：经宿主读写看板（见 `runode_protocol::todo`）上的任务和定时。

use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use runode_agent::models::{LAUNCHERS, Pick};
use runode_protocol::{
    ClientMsg, HostMsg, SessionId,
    todo::{Column, DEFAULT_AGENT, Outcome, Todo, TodoSpec, When, column},
};
use runode_remote::now_unix;

use runode_todo::LocalTime;

pub(crate) use crate::args::Spec;
use crate::{
    Env,
    args::{DAYS, Queue, Timing},
    client::{Connection, kind},
    output::{plain, table, tilde},
};

/// 等 `StartTodo`、`RemoveTodo` 回话的最长时间：宿主要先跑工作树和分支的 git 命令，比一般请求久。
const GIT_TIMEOUT: Duration = Duration::from_secs(250);

impl Spec {
    /// 照 `open --cwd`：相对路径按命令行的当前目录算；没给 agent 时用 `DEFAULT_AGENT`，给了模型或强度时按桌面
    /// 挑出来的写法拼进 agent 的命令（`Pick::command`）。
    fn resolve(self) -> Result<TodoSpec> {
        let here = std::env::current_dir()?;
        let agent = self.agent.unwrap_or_else(|| DEFAULT_AGENT.to_owned());
        let agent = if self.model.is_none() && self.effort.is_none() {
            agent
        } else {
            pick(&agent, self.model, self.effort)?.command()
        };
        Ok(TodoSpec {
            title: self.title,
            prompt: self.prompt,
            // 不拿空路径去 join：那样会多出一个尾斜杠，库里的目录和给了 `--cwd` 的写法对不上。
            dir: match self.cwd {
                Some(dir) => here.join(dir),
                None => here,
            },
            agent,
            worktree: self.worktree,
            skills: self.skills,
            auto: self.auto,
            auto_done: self.auto_done,
            time_limit: self.time_limit,
            ..TodoSpec::default()
        })
    }
}

/// `--agent` 认得的一家配上 `--model`、`--effort`：先按 `Launcher::command` 找，找不到再按不分大小写的
/// `Launcher::label` 找（同一家的几个版本共用一个名字，按名字找到的是排在前面的新版本）。
fn pick(agent: &str, model: Option<String>, effort: Option<String>) -> Result<Pick> {
    let Some(index) = LAUNCHERS
        .iter()
        .position(|launcher| launcher.command == agent)
        .or_else(|| LAUNCHERS.iter().position(|launcher| launcher.label.eq_ignore_ascii_case(agent)))
    else {
        let known: Vec<_> = LAUNCHERS.iter().map(|launcher| launcher.command).collect();
        bail!("--model and --effort need --agent to be one of {}, not {agent}", known.join(", "));
    };
    let launcher = &LAUNCHERS[index];
    // 拼进命令时按 shell 的一个词加引号，引号和空白拼不出原样的一个词。
    for (option, word) in [("--model", &model), ("--effort", &effort)] {
        if let Some(word) = word
            && (word.is_empty() || word.chars().any(|ch| ch == '\'' || ch.is_whitespace() || ch.is_control()))
        {
            bail!("{option} takes one word without quotes, not {word:?}");
        }
    }
    if model.is_some() && launcher.model_arg.is_none() {
        bail!("{} cannot pick a model", launcher.label);
    }
    if effort.is_some() && launcher.effort_arg.is_none() {
        bail!("{} cannot pick an effort", launcher.label);
    }
    if effort.is_some() && model.is_none() && launcher.effort_needs_model {
        bail!("{} needs --model to pick an effort", launcher.label);
    }
    Ok(Pick { agent: index, model, effort })
}

/// 连上宿主；宿主不认得看板的消息时报错，不发。
fn open(env: &Env) -> Result<Connection> {
    let connection = Connection::open(env)?;
    if !connection.todos {
        bail!("the running Runode host is too old for the task board; quit Runode and open it again");
    }
    Ok(connection)
}

/// 连上宿主准备交 `spec`；宿主认不得它用到的选项时报错，不让它悄悄丢掉。
fn open_for(env: &Env, spec: &TodoSpec) -> Result<Connection> {
    let connection = open(env)?;
    if spec.time_limit.is_some() {
        need_board(&connection, 2, "--time-limit")?;
    }
    Ok(connection)
}

/// 目录或它的上级里有 `.git`（仓库是目录，工作树和子模块是文件）。先解开符号链接和 `..`，按目录实际
/// 所在的位置找。
// shortcut: 不认 GIT_DIR 这类环境变量指的仓库；认错了宿主会报错，那时再改成问 git。
fn in_git_repo(dir: &Path) -> bool {
    dir.canonicalize().is_ok_and(|dir| dir.ancestors().any(|dir| dir.join(".git").exists()))
}

/// 加一张任务，打印它的标识；`start` 时接着开始它，再打印它的会话的标识。
pub(crate) fn add(env: &Env, spec: Spec, start: bool, queue: Option<Queue>, out: &mut dyn Write) -> Result<()> {
    let mut spec = spec.resolve()?;
    // 宿主不给仓库外的目录开工作树；工作树是默认开着的，这里改成不要，免得命令白白失败。
    spec.worktree &= in_git_repo(&spec.dir);
    let connection = open_for(env, &spec)?;
    if queue.is_some() {
        need_board(&connection, 3, "--queue and --after")?;
    }
    let req = connection.req();
    connection.send(&ClientMsg::AddTodo { req, spec })?;
    let id = added(&connection)?;
    writeln!(out, "{id}")?;
    if start {
        let session = start_todo(&connection, &id).map_err(|err| err.context(format!("added todo {id}")))?;
        writeln!(out, "{session}")?;
    }
    if let Some(Queue { after }) = queue {
        let req = connection.req();
        connection
            .request_done(&ClientMsg::QueueTodo { req, id: id.clone(), queued: true, after })
            .map_err(|err| err.context(format!("added todo {id}")))?;
    }
    Ok(())
}

/// 宿主认得的看板消息不到 `level` 级时报错，`what` 是要用的选项或命令。
fn need_board(connection: &Connection, level: u32, what: &str) -> Result<()> {
    if connection.board < level {
        bail!("the running Runode host is too old for {what}; quit Runode and open it again");
    }
    Ok(())
}

/// 让任务排队（`queue` 给了时）或者不再排队。
pub(crate) fn queue(env: &Env, id: String, queue: Option<Queue>) -> Result<()> {
    let connection = open(env)?;
    need_board(&connection, 3, "todo queue")?;
    let req = connection.req();
    let queued = queue.is_some();
    let after = queue.and_then(|queue| queue.after);
    connection.request_done(&ClientMsg::QueueTodo { req, id, queued, after })
}

/// 打印看板的并发上限（`0` 是不限），给了 `limit` 时改成它。
pub(crate) fn limit(env: &Env, limit: Option<u32>, out: &mut dyn Write) -> Result<()> {
    let connection = open(env)?;
    need_board(&connection, 3, "todo limit")?;
    let req = connection.req();
    match limit {
        Some(max_running) => connection.request_done(&ClientMsg::SetMaxRunning { req, max_running }),
        None => {
            connection.send(&ClientMsg::ListTodos { req })?;
            match connection.reply()? {
                HostMsg::Todos { max_running, .. } => writeln!(out, "{max_running}")?,
                other => bail!("unexpected answer: {}", kind(&other)),
            }
            Ok(())
        }
    }
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
    match connection.reply_by(Instant::now() + GIT_TIMEOUT) {
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

/// 报自己所在的会话开着的那张任务的结局和结果。
pub(crate) fn report(env: &Env, outcome: Outcome, result: String) -> Result<()> {
    let own: SessionId = env
        .session
        .as_deref()
        .and_then(|own| own.parse().ok())
        .ok_or_else(|| anyhow!("todo report runs in the terminal of a task, and this is not a Runode terminal"))?;
    let connection = open(env)?;
    if connection.board < 1 {
        bail!("the running Runode host is too old for todo report; quit Runode and open it again");
    }
    let (todos, _) = connection.board()?;
    // 同一个会话接回过、又开过别的任务时，取最近开始的。
    let todo = todos
        .iter()
        .filter(|todo| todo.session == Some(own))
        .max_by_key(|todo| todo.started)
        .ok_or_else(|| anyhow!("this terminal runs no task on the board"))?;
    let req = connection.req();
    connection.request_done(&ClientMsg::ReportTodo { req, id: todo.id.clone(), outcome, result })
}

pub(crate) fn remove(env: &Env, id: String) -> Result<()> {
    let connection = open(env)?;
    let req = connection.req();
    connection.send(&ClientMsg::RemoveTodo { req, id })?;
    match connection.reply_by(Instant::now() + GIT_TIMEOUT)? {
        HostMsg::Done { .. } => Ok(()),
        other => bail!("unexpected answer: {}", kind(&other)),
    }
}

/// 列名，同 JSON 里的写法（`in_progress`）。
fn column_name(column: Column) -> String {
    serde_json::to_value(column).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default()
}

/// 任务的编号，写成 `#5`；`Todo::number` 为 0（没有编号）时为 `-`。
fn number_text(todo: &Todo) -> String {
    if todo.number == 0 { "-".into() } else { format!("#{}", todo.number) }
}

/// 列出任务和各自在看板的哪一列，按看板从左到右，同一列里照宿主给的先后。
pub(crate) fn list(env: &Env, json: bool, out: &mut dyn Write) -> Result<()> {
    let connection = open(env)?;
    let sessions = connection.sessions()?;
    let (todos, _) = connection.board()?;
    let now = now_unix();
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
            number_text(todo),
            todo.id.clone(),
            if todo.queued && todo.started.is_none() {
                format!("{} (queued)", column_name(*column))
            } else {
                column_name(*column)
            },
            todo.spec.agent.clone(),
            todo.spec.title.clone(),
            tilde(&todo.spec.dir, home),
        ]
    });
    table(&["#", "ID", "COLUMN", "AGENT", "TITLE", "DIR"], rows, out)
}

/// 打印一张任务的全部内容：编号、标识、在哪一列、要做的事、工作树、会话、定时和各个时刻。`id` 是标识或者
/// 编号（`5`、`#5`），完成了多久的都查得到。
pub(crate) fn show(env: &Env, id: &str, json: bool, out: &mut dyn Write) -> Result<()> {
    let connection = open(env)?;
    let req = connection.req();
    connection.send(&ClientMsg::GetTodo { req, id: id.to_owned() })?;
    let todo = match connection.reply()? {
        HostMsg::Todo { todo, .. } => todo,
        other => bail!("unexpected answer: {}", kind(&other)),
    };
    let sessions = connection.sessions()?;
    let session = sessions.iter().find(|info| Some(info.id) == todo.session);
    let column = column(&todo, session, now_unix());
    let (todos, schedules) = connection.board()?;
    let schedule = todo.schedule.as_ref().and_then(|id| schedules.iter().find(|schedule| &schedule.id == id));
    // 等的任务不在看板上（早就完成了）时只写它的标识。
    let queued = (todo.queued && todo.started.is_none()).then(|| match &todo.after {
        Some(after) => match todos.iter().find(|other| &other.id == after) {
            Some(other) => format!("after {}", number_text(other)),
            None => format!("after {after}"),
        },
        None => "yes".into(),
    });
    if json {
        let mut value = serde_json::to_value(&todo)?;
        value["column"] = column_name(column).into();
        value["session_alive"] = session.is_some_and(|info| !info.exited).into();
        serde_json::to_writer_pretty(&mut *out, &value)?;
        writeln!(out)?;
        return Ok(());
    }
    let home = env.dirs.home.as_deref();
    let local = |at: u64| LocalTime::from_unix(at).map_or_else(|| at.to_string(), |at| at.to_string());
    let alive = |session: &SessionId| {
        let state = if sessions.iter().any(|info| info.id == *session && !info.exited) { "open" } else { "gone" };
        format!("{session} ({state})")
    };
    let title =
        if todo.number == 0 { todo.spec.title.clone() } else { format!("#{}  {}", todo.number, todo.spec.title) };
    writeln!(out, "{}", plain(&title))?;
    let fields: [(&str, Option<String>); 17] = [
        ("id", Some(todo.id.clone())),
        ("column", Some(column_name(column))),
        ("queued", queued),
        ("outcome", todo.outcome.map(|outcome| outcome.name().to_owned())),
        ("agent", Some(todo.spec.agent.clone())),
        ("skills", (!todo.spec.skills.is_empty()).then(|| todo.spec.skills.join(", "))),
        ("auto", todo.spec.auto.then(|| "yes".into())),
        ("auto-done", todo.spec.auto_done.then(|| "yes".into())),
        ("time-limit", todo.spec.time_limit.map(|minutes| format!("{minutes} min"))),
        ("dir", Some(tilde(&todo.spec.dir, home))),
        ("worktree", todo.worktree_dir.as_deref().map(|dir| tilde(dir, home))),
        ("branch", todo.branch.clone()),
        ("session", todo.session.as_ref().map(alive)),
        // 定时删掉了时只剩它的标识。
        (
            "schedule",
            todo.schedule.clone().map(|id| schedule.map_or(id.clone(), |s| format!("{} ({id})", s.spec.title))),
        ),
        ("created", Some(local(todo.created))),
        ("started", todo.started.map(local)),
        ("done", todo.done_at.map(local)),
    ];
    for (label, value) in fields {
        if let Some(value) = value {
            writeln!(out, "{label:<9} {}", plain(&value))?;
        }
    }
    writeln!(out, "prompt")?;
    for line in todo.spec.prompt().lines() {
        writeln!(out, "  {}", plain(line))?;
    }
    if let Some(result) = &todo.result {
        // 没报结局时是宿主读下的屏幕。
        writeln!(out, "{}", if todo.outcome.is_some() { "result" } else { "screen" })?;
        for line in result.lines() {
            writeln!(out, "  {}", plain(line))?;
        }
    }
    Ok(())
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
            if unix <= now_unix() {
                bail!("{at} has passed");
            }
            When::Once { at: unix }
        }
    };
    let connection = open_for(env, &spec)?;
    if matches!(when, When::Cron { .. }) {
        need_board(&connection, 3, "--cron")?;
    }
    let req = connection.req();
    connection.send(&ClientMsg::AddSchedule { req, spec, when })?;
    writeln!(out, "{}", added(&connection)?)?;
    Ok(())
}

pub(crate) fn list_schedules(env: &Env, json: bool, out: &mut dyn Write) -> Result<()> {
    let (_, schedules) = open(env)?.board()?;
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
            if schedule.paused { "paused".into() } else { schedule.next_fire.map_or_else(|| "-".into(), local) },
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

pub(crate) fn pause_schedule(env: &Env, id: String, paused: bool) -> Result<()> {
    let connection = open(env)?;
    need_board(&connection, 3, if paused { "schedule pause" } else { "schedule resume" })?;
    let req = connection.req();
    connection.request_done(&ClientMsg::PauseSchedule { req, id, paused })
}

/// 定时什么时候到点，人看的写法：`daily 09:30 mon-fri`、`every 15m`、`once 2026-10-13 14:00`、`cron 30 9 * * 1-5`。
fn when_text(when: &When, local: impl Fn(u64) -> String) -> String {
    match *when {
        When::Once { at } => format!("once {}", local(at)),
        When::Daily { hour, minute, days } => {
            let days = days_text(days);
            let sep = if days.is_empty() { "" } else { " " };
            format!("daily {hour:02}:{minute:02}{sep}{days}")
        }
        When::Every { minutes } => format!("every {minutes}m"),
        When::Cron { ref expr } => format!("cron {expr}"),
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

    #[cfg(unix)]
    #[test]
    fn a_repository_is_found_where_the_directory_really_is() {
        let root = std::env::temp_dir().join(format!("rnc-{}-in-repo", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (repo, outside) = (root.join("repo"), root.join("outside"));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(repo.join("sub"), root.join("link")).unwrap();
        assert!(in_git_repo(&root.join("link")));
        assert!(!in_git_repo(&repo.join("..").join("outside")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
