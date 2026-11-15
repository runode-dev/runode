//! 定时到点先跑的命令（`Schedule::run`）：另起一个线程在 `TodoSpec::dir` 里跑，定时的线程不等它。跑完按退出码
//! 和标准输出定结果（`RunOutcome`）：退出码为 0、有输出时开一张排着队的任务，输出接在提示词后面；结果和任务在
//! 一个事务里记进库。

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock, PoisonError},
    time::Duration,
};

use runode_agent::oneshot::{RunError, login_shell_path, run_command};
use runode_protocol::todo::{DEFAULT_RUN_TIMEOUT, RunOutcome, Schedule, ScheduleRun, TodoSpec};

use super::{
    Todos,
    check::{MAX_PROMPT, check_text},
    unix_now,
};
use crate::Shared;

/// 记进 `ScheduleRun::output` 的标准错误最多留末尾这么多个字。
const RUN_OUTPUT_LIMIT: usize = 2000;

/// 命令的环境里叫它跑的定时的标识（`Schedule::id`）。
const ENV_SCHEDULE: &str = "RUNODE_SCHEDULE";

/// 桌面经 `Host::set_env` 交给宿主的 runode 可执行文件的路径（`runode_cli::ENV_BIN`）。
const ENV_BIN: &str = "RUNODE_BIN";

/// 跑命令的解释器，报找不到时用。
#[cfg(unix)]
const SHELL: &str = "/bin/sh";
#[cfg(windows)]
const SHELL: &str = "cmd.exe";

/// 定时的命令正在跑：占着 `State::running_runs`，丢掉时放开。
pub(super) struct Running {
    todos: Arc<Todos>,
    id: String,
}

impl Running {
    /// 占住定时 `id`；它上一次的命令还没跑完时为空。
    fn new(todos: &Arc<Todos>, id: &str) -> Option<Self> {
        todos.lock().running_runs.insert(id.to_owned()).then(|| Self { todos: todos.clone(), id: id.to_owned() })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.todos.lock().running_runs.remove(&self.id);
    }
}

impl Todos {
    /// 定时 `id` 上一次到点跑的命令还没跑完。
    pub(super) fn run_pending(&self, id: &str) -> bool {
        self.lock().running_runs.contains(id)
    }

    /// 有定时的命令还没跑完。
    pub(super) fn runs_pending(&self) -> bool {
        !self.lock().running_runs.is_empty()
    }
}

impl Shared {
    /// 另起线程跑 `Store::fire_schedule_run` 或 `Store::fire_schedule_now`（`by_hand`）交出的定时 `schedule` 的
    /// 命令，跑完记下结果、按结果开任务（`finish_run`）。它上一次的命令还没跑完时什么都不做，为假。线程只拿着
    /// 宿主的弱引用，宿主没了就不记。
    pub(super) fn start_run(&self, schedule: Schedule, by_hand: bool) -> bool {
        let Some(running) = Running::new(&self.todos, &schedule.id) else {
            tracing::info!("schedule {} is still running its last command", schedule.id);
            return false;
        };
        let env = self.env.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let shared = self.me.clone();
        let fallback = schedule.clone();
        let spawned = std::thread::Builder::new().name("schedule-run".into()).spawn(move || {
            let minutes = schedule.run_timeout.unwrap_or(DEFAULT_RUN_TIMEOUT);
            let timeout = Duration::from_secs(u64::from(minutes) * 60);
            let (run, todo) = execute(&schedule, &env, command_path(&env), timeout);
            if let Some(shared) = shared.upgrade() {
                shared.finish_run(&schedule, &run, todo.as_ref(), Some(running), by_hand);
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("cannot start the thread to run the command of schedule {}: {err}", fallback.id);
            let run = ScheduleRun {
                at: unix_now(),
                outcome: RunOutcome::Failed,
                output: format!("Runode could not start the command: {err}"),
            };
            self.finish_run(&fallback, &run, None, None, by_hand);
        }
        true
    }

    /// 记下定时 `schedule` 这次跑命令的结果 `run`，给了 `todo` 时加一张排着队的任务（`Store::finish_schedule_run`，
    /// `by_hand` 照传），再叫醒定时的线程去开始它；`running` 记进库以后放开。不归这个宿主办（正在交出会话、要
    /// 退出了）时丢掉这次的结果。
    fn finish_run(
        &self,
        schedule: &Schedule,
        run: &ScheduleRun,
        todo: Option<&TodoSpec>,
        running: Option<Running>,
        by_hand: bool,
    ) {
        if let Err(why) = self.todos_writable() {
            tracing::info!("schedule {} ran its command, but the result is dropped: {why}", schedule.id);
            return;
        }
        let finished = self.todos.with_db(|db| db.finish_schedule_run(schedule, run, todo, by_hand));
        // 先放开再通知：叫醒的定时线程马上再看，这个定时又到点时不因还占着跳过。
        drop(running);
        match finished {
            Ok(Some(id)) => {
                tracing::info!("schedule {} fired: todo {id}", schedule.id);
                self.todos_changed();
            }
            Ok(None) => {
                tracing::info!("schedule {} ran its command: {}", schedule.id, run.outcome.name());
                self.todos_changed();
            }
            Err(err) => {
                tracing::warn!("cannot record the command run of schedule {}: {err}", schedule.id);
                // 命令不再占着（`Schedule::running`），看板照样要重读。
                self.todos_changed();
            }
        }
    }
}

/// 在 `schedule.spec.dir` 里跑 `schedule.run`，最多跑 `timeout`，返回要记的结果和要开的任务。环境是宿主自己的
/// 加上 `env`（`Shared::env`），`path` 不为空时换掉 PATH；另设 `ENV_SCHEDULE`，不设 `runode_protocol::ENV_SESSION`：
/// 命令不在哪个会话里。标准输入是空的。
fn execute(
    schedule: &Schedule,
    env: &[(String, OsString)],
    path: Option<OsString>,
    timeout: Duration,
) -> (ScheduleRun, Option<TodoSpec>) {
    let (outcome, output, todo) = match judge(schedule, env, path, timeout) {
        Ok(judged) => judged,
        Err(why) => (RunOutcome::Failed, why, None),
    };
    (ScheduleRun { at: unix_now(), outcome, output }, todo)
}

/// `execute` 跑命令、定结果：没失败时是结果、标准错误的末尾和要开的任务，失败时是记进 `ScheduleRun::output` 的
/// 说明。标准输出超过 `MAX_PROMPT` 时不截断、算失败。
fn judge(
    schedule: &Schedule,
    env: &[(String, OsString)],
    path: Option<OsString>,
    timeout: Duration,
) -> Result<(RunOutcome, String, Option<TodoSpec>), String> {
    let run = schedule.run.as_deref().unwrap_or_default();
    let dir = &schedule.spec.dir;
    // 目录不在时拉起来也报找不到，和找不到解释器分不清。
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let mut command = shell_command(run);
    command
        .current_dir(dir)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .env_remove(runode_protocol::ENV_SESSION)
        .env(ENV_SCHEDULE, &schedule.id);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let finished = run_command(command, None, timeout, MAX_PROMPT).map_err(|err| match err {
        RunError::Timeout => format!("the command did not finish within {}", span(timeout)),
        RunError::Missing => format!("cannot find {SHELL}"),
        RunError::Failed(why) => why,
        RunError::Io(err) => format!("cannot run the command: {err}"),
    })?;
    let stderr = tail(&String::from_utf8_lossy(&finished.stderr));
    if !finished.status.success() {
        return Err(if stderr.is_empty() { format!("the command failed ({})", finished.status) } else { stderr });
    }
    let failed = |why: String| if stderr.is_empty() { why } else { format!("{why}\n\n{stderr}") };
    if finished.overflow {
        return Err(failed(format!("the command's output is longer than {} KiB", MAX_PROMPT / 1024)));
    }
    let stdout = String::from_utf8_lossy(&finished.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok((RunOutcome::Idle, stderr, None));
    }
    let prompt = if schedule.spec.prompt.trim().is_empty() {
        stdout.to_owned()
    } else {
        format!("{}\n\n{stdout}", schedule.spec.prompt)
    };
    let spec = TodoSpec { prompt, ..schedule.spec.clone() };
    check_text(&spec).map_err(failed)?;
    Ok((RunOutcome::Fired, stderr, Some(spec)))
}

/// 用 `SHELL` 跑 `run` 的命令，`run` 原样交给它。
#[cfg(unix)]
fn shell_command(run: &str) -> Command {
    let mut command = Command::new(SHELL);
    command.arg("-c").arg(run);
    command
}

/// 用 `SHELL` 跑 `run` 的命令，`run` 原样交给它：`/S` 让 cmd 只去掉 `/C` 后面最外层的一对引号。
#[cfg(windows)]
fn shell_command(run: &str) -> Command {
    use std::os::windows::process::CommandExt as _;
    let mut command = Command::new(SHELL);
    command.args(["/D", "/S", "/C"]).raw_arg(format!("\"{run}\""));
    command
}

/// 命令的 PATH：登录 shell 配好的（问一次，记住），后面接上 `env` 里 `ENV_BIN` 所在的目录，命令里能直接敲
/// `runode`。问不到时为空，照用宿主环境和 `env` 里的。
fn command_path(env: &[(String, OsString)]) -> Option<OsString> {
    static LOGIN: OnceLock<Option<OsString>> = OnceLock::new();
    let login =
        LOGIN.get_or_init(|| login_shell_path(&runode_terminal::pty::default_shell(), Path::new("/"))).clone()?;
    let Some(bin) = env.iter().find(|(key, _)| key == ENV_BIN).and_then(|(_, exe)| Path::new(exe).parent()) else {
        return Some(login);
    };
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&login).collect();
    if !dirs.iter().any(|dir| dir == bin) {
        dirs.push(bin.to_owned());
    }
    Some(std::env::join_paths(dirs).unwrap_or(login))
}

/// 去掉首尾空白，留末尾 `RUN_OUTPUT_LIMIT` 个字。
fn tail(text: &str) -> String {
    let text = text.trim();
    let start = text.char_indices().rev().nth(RUN_OUTPUT_LIMIT - 1).map_or(0, |(at, _)| at);
    text[start..].to_owned()
}

/// 给人看的时长：整分钟的写分钟，不然写秒。
fn span(timeout: Duration) -> String {
    let secs = timeout.as_secs();
    match (secs / 60, secs % 60) {
        (0, 1) => "1 second".into(),
        (1, 0) => "1 minute".into(),
        (minutes, 0) => format!("{minutes} minutes"),
        _ => format!("{secs} seconds"),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use runode_protocol::todo::When;

    use super::*;

    #[cfg(unix)]
    fn schedule(dir: &Path, run: &str) -> Schedule {
        Schedule {
            id: "s".into(),
            spec: TodoSpec { title: "t".into(), dir: dir.into(), agent: "claude".into(), ..TodoSpec::default() },
            when: When::Every { minutes: 60 },
            last_fired: 0,
            next_fire: None,
            paused: false,
            run: Some(run.into()),
            run_timeout: None,
            last_run: None,
            running: false,
        }
    }

    /// 过了时限没跑完的命令连同子孙一起杀掉，算失败，不开任务。
    #[cfg(unix)]
    #[test]
    fn a_command_past_its_timeout_fails() {
        let schedule = schedule(&std::env::temp_dir(), "echo started; sleep 30");
        let (run, todo) = execute(&schedule, &[], None, Duration::from_secs(1));
        assert_eq!(run.outcome, RunOutcome::Failed);
        assert_eq!(run.output, "the command did not finish within 1 second");
        assert!(todo.is_none());
    }

    /// 命令里看得到定时的标识，看不到宿主自己的会话。
    #[cfg(unix)]
    #[test]
    fn the_command_knows_its_schedule() {
        let env = [(runode_protocol::ENV_SESSION.to_owned(), OsString::from("x"))];
        let run = format!("printf '%s' \"${ENV_SCHEDULE}:${{{}:-none}}\"", runode_protocol::ENV_SESSION);
        let (run, todo) = execute(&schedule(&std::env::temp_dir(), &run), &env, None, Duration::from_secs(10));
        assert_eq!(run.outcome, RunOutcome::Fired, "{}", run.output);
        assert_eq!(todo.unwrap().prompt, "s:none");
    }

    #[test]
    fn the_output_keeps_the_end_of_stderr() {
        let kept = tail(&format!("{}end\n", "x".repeat(RUN_OUTPUT_LIMIT)));
        assert_eq!(kept.chars().count(), RUN_OUTPUT_LIMIT);
        assert!(kept.ends_with("end"));
        assert_eq!(tail("  short \n"), "short");
    }
}
