//! 定时到点先跑的命令（`Schedule::run`）：另起一个线程在 `TodoSpec::dir` 里跑，定时的线程不等它。跑完按退出码
//! 和标准输出定结果（`RunOutcome`）：退出码为 0、有输出时开一张排着队的任务，输出接在提示词后面；结果和任务在
//! 一个事务里记进库。

use std::{
    collections::hash_map::Entry,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock, PoisonError},
    time::Duration,
};

use runode_agent::oneshot::{RunError, login_shell_path, run_command};
use runode_protocol::todo::{DEFAULT_RUN_TIMEOUT, RunOutcome, Schedule, ScheduleRun, TodoSpec, When};

use super::{
    State, Todos,
    check::{MAX_PROMPT, check_text},
    db_in, unix_now,
};
use crate::{Shared, Stopped, server::Phase};

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
    /// 占住定时 `schedule`（`by_hand` 是手动触发的）；它上一次的命令还没跑完时为空。
    fn new(todos: &Arc<Todos>, schedule: &Schedule, by_hand: bool) -> Option<Self> {
        let mut state = todos.lock();
        let Entry::Vacant(entry) = state.running_runs.entry(schedule.id.clone()) else { return None };
        entry.insert(once_fired(schedule, by_hand).then(|| schedule.clone()));
        drop(state);
        Some(Self { todos: todos.clone(), id: schedule.id.clone() })
    }
}

/// `schedule` 是到点触发（不是手动，`by_hand`）的一次性定时。它触发时已经从库里删掉：交出会话后接手的宿主不会
/// 再触发它，结果由这个宿主照记；不交接就退出、命令还没记下结果时要放回库里，下次启动才会再到点，命令可能再
/// 跑一遍。
fn once_fired(schedule: &Schedule, by_hand: bool) -> bool {
    !by_hand && matches!(schedule.when, When::Once { .. })
}

impl Drop for Running {
    fn drop(&mut self) {
        self.todos.lock().running_runs.remove(&self.id);
        self.todos.runs_done.notify_all();
    }
}

impl Todos {
    /// 定时 `id` 上一次到点跑的命令还没跑完。
    pub(super) fn run_pending(&self, id: &str) -> bool {
        self.lock().running_runs.contains_key(id)
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
        let Some(running) = Running::new(&self.todos, &schedule, by_hand) else {
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

    /// 记下定时 `schedule` 这次跑命令的结果 `run`（`record_run`），再叫醒定时的线程去开始开出的任务；`running`
    /// 记进库以后放开。这个宿主不记时丢掉这次的结果。
    fn finish_run(
        &self,
        schedule: &Schedule,
        run: &ScheduleRun,
        todo: Option<&TodoSpec>,
        running: Option<Running>,
        by_hand: bool,
    ) {
        let finished = match self.record_run(schedule, run, todo, by_hand) {
            Ok(finished) => finished,
            Err(why) => {
                tracing::info!("schedule {} ran its command, but the result is dropped: {why}", schedule.id);
                return;
            }
        };
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

    /// 把定时 `schedule` 这次跑命令的结果记进库（`Store::finish_schedule_run`，参数照传），返回记库的结果。照常
    /// 办事时记；正在交出会话时只记 `once_fired` 的；要退出了不记，`once_fired` 的放回库里（`put_back`）。不记时
    /// 为不记的原因。记下以后 `State::running_runs` 里这个定时不再要放回库里。
    fn record_run(
        &self,
        schedule: &Schedule,
        run: &ScheduleRun,
        todo: Option<&TodoSpec>,
        by_hand: bool,
    ) -> Result<Result<Option<String>, String>, &'static str> {
        let mut state = self.todos.lock();
        {
            let peers = self.peers();
            if let Some(why) = crate::refusal(&peers) {
                let handing_over = matches!(peers.phase, Phase::HandingOver | Phase::Stopping(Stopped::Handoff));
                if !(handing_over && once_fired(schedule, by_hand)) {
                    drop(peers);
                    // 交出会话时要放回的定时上面照记，这里还留着的只会是要退出了的。
                    if let Some(restore) = state.running_runs.get_mut(&schedule.id).and_then(Option::take) {
                        put_back(&mut state, &restore);
                    }
                    return Err(why);
                }
            }
        }
        let finished = db_in(&mut state, |db| db.finish_schedule_run(schedule, run, todo, by_hand));
        if finished.is_ok()
            && let Some(restore) = state.running_runs.get_mut(&schedule.id)
        {
            *restore = None;
        }
        Ok(finished)
    }

    /// 宿主要退出、`Peers::phase` 已是 `Stopping(Stopped::Shutdown | Stopped::Idle)` 以后调：把命令还没
    /// 记下结果的 `once_fired` 的定时放回库里（`put_back`）。
    ///
    /// 一个定时的结果记下和放回恰好发生一件：`record_run` 在同一把 `state` 锁里读 `phase`，读到要退出了的不记、
    /// 改为放回；两处都从 `State::running_runs` 里取走要放回的定时，取走的只放一次。先等拿着 `firing` 的触发
    /// 办完，已经消费了的定时都占进了 `running_runs`；放开以后到点的定时不再被消费。
    pub(crate) fn put_back_unfinished_runs(&self) {
        drop(self.todos.firing());
        let mut state = self.todos.lock();
        let unfinished: Vec<Schedule> = state.running_runs.values_mut().filter_map(Option::take).collect();
        for schedule in &unfinished {
            put_back(&mut state, schedule);
        }
    }
}

/// 把到点触发、命令没记下结果的定时 `schedule` 放回 `state` 的库里（`Store::restore_schedule`）；放不回时只记日志。
/// 这次运行不留记录：库里没有它的 `ScheduleRun`。
fn put_back(state: &mut State, schedule: &Schedule) {
    match db_in(state, |db| db.restore_schedule(schedule)) {
        Ok(true) => tracing::info!(
            "schedule {} is put back: the host is stopping, the result of its command is not recorded",
            schedule.id
        ),
        Ok(false) => {}
        Err(err) => tracing::warn!("cannot put back schedule {}: {err}", schedule.id),
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

/// 登录 shell 配好的 PATH，问一次、记住，问不到时为空。问到时也交给 `runode_git::set_path`：从访达打开的 app
/// 拉起的宿主继承的 PATH 里没有 brew 这类目录，git 拉起的 git-lfs 和钩子在那里找不到。第一次问要起一个交互的
/// shell，会卡住，不在要马上回话的地方第一个调。
pub(super) fn login_path() -> Option<OsString> {
    static LOGIN: OnceLock<Option<OsString>> = OnceLock::new();
    LOGIN
        .get_or_init(|| {
            let path = login_shell_path(&runode_terminal::pty::default_shell(), Path::new("/"))?;
            runode_git::set_path(path.clone());
            Some(path)
        })
        .clone()
}

/// 命令的 PATH：`login_path`，后面接上 `env` 里 `ENV_BIN` 所在的目录，命令里能直接敲 `runode`。问不到时为空，
/// 照用宿主环境和 `env` 里的。
fn command_path(env: &[(String, OsString)]) -> Option<OsString> {
    let login = login_path()?;
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

    /// 起一个开着看板的宿主，加一个 `when` 的定时：命令开跑以后宿主开始停下（`stopped`），再让命令跑完、输出
    /// 一行，等它记完结果。返回库里这个定时开出的任务和库里还在不在这个定时。
    #[cfg(unix)]
    fn run_while_stopping(name: &str, when: When, stopped: Stopped) -> (Vec<runode_protocol::todo::Todo>, bool) {
        let (host, dir, id) = running(name, when);
        host.shared.peers().phase = Phase::Stopping(stopped);
        finish(&host, &dir, &id)
    }

    /// 库里还在不在定时 `id`。
    #[cfg(unix)]
    fn kept(host: &crate::Host, id: &str) -> bool {
        host.shared.todos.with_db(|db| db.list_schedules()).unwrap().iter().any(|kept| kept.id == id)
    }

    /// 单独跑的宿主因为 `stopped` 退出（`run_until_idle` 返回它）时，到点触发、命令还在跑的一次性定时放回了库里
    /// 没有（返回的第一项）；再让命令跑完，返回 `finish` 的结果。
    #[cfg(unix)]
    fn exit_while_running(name: &str, stopped: Stopped) -> (bool, (Vec<runode_protocol::todo::Todo>, bool)) {
        let (host, dir, id) = running(name, When::Once { at: 2 });
        host.shared.peers().phase = Phase::Stopping(stopped);
        assert_eq!(host.run_until_idle(Duration::from_secs(60)), stopped);
        let put_back = kept(&host, &id);
        (put_back, finish(&host, &dir, &id))
    }

    /// 不是交接的退出：命令没跑完的一次性定时放回库里，命令之后跑完的结果不记。
    #[cfg(unix)]
    #[test]
    fn a_once_schedule_running_when_idle_exits_is_put_back() {
        let (put_back, (todos, kept)) = exit_while_running("once-idle", Stopped::Idle);
        assert!(put_back, "the schedule was not put back");
        assert!(todos.is_empty(), "the result of the command was recorded");
        assert!(kept);
    }

    /// 交接以后退出：不放回，接手的宿主不会再触发它；命令跑完照样记结果、开任务。
    #[cfg(unix)]
    #[test]
    fn a_once_schedule_running_when_handed_over_is_not_put_back() {
        let (put_back, (todos, kept)) = exit_while_running("once-handoff-exit", Stopped::Handoff);
        assert!(!put_back, "the schedule was put back for the old host");
        assert_eq!(todos.len(), 1, "the result of the command was dropped");
        assert!(!kept);
    }

    /// 起一个单独跑、开着看板的宿主，加一个 `when` 的定时，等它的命令开跑；命令等目录里出现 `go` 才跑完、输出
    /// 一行。返回宿主、目录和定时的标识。
    #[cfg(unix)]
    fn running(name: &str, when: When) -> (crate::Host, PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("rnh-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let host = crate::Host::new(runode_protocol::BuildId("test".into()));
        host.mark_standalone();
        host.listen(&dir.join("host.sock"), &dir.join("host.lock")).unwrap();
        host.serve_todos(dir.join("todos.db"));
        let spec = TodoSpec { title: "t".into(), dir: dir.clone(), agent: "true".into(), ..TodoSpec::default() };
        let (started, go) = (dir.join("started"), dir.join("go"));
        let run = format!(
            "touch '{}'; while [ ! -e '{}' ]; do sleep 0.05; done; printf task",
            started.display(),
            go.display()
        );
        let id = host.shared.todos.with_db(|db| db.add_schedule(&spec, &when, Some(&run), None, 1)).unwrap();
        host.shared.todos_changed();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(std::time::Instant::now() < deadline, "the command of the schedule did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        (host, dir, id)
    }

    /// 让 `running` 起的命令跑完，等它记完结果。返回库里定时 `id` 开出的任务和库里还在不在这个定时。
    #[cfg(unix)]
    fn finish(host: &crate::Host, dir: &Path, id: &str) -> (Vec<runode_protocol::todo::Todo>, bool) {
        std::fs::write(dir.join("go"), "").unwrap();
        host.wait_for_schedule_runs();
        let todos = host.shared.todos.with_db(|db| db.list_todos(unix_now())).unwrap();
        let kept = kept(host, id);
        let _ = std::fs::remove_dir_all(dir);
        (todos.into_iter().filter(|todo| todo.schedule.as_deref() == Some(id)).collect(), kept)
    }

    /// 一次性的定时触发时已从库里删掉，接手的宿主不会再触发它：交出会话以后旧宿主照样记它的结果、开任务。
    #[cfg(unix)]
    #[test]
    fn a_once_schedule_run_is_recorded_after_a_handoff() {
        let (todos, _) = run_while_stopping("once-run", When::Once { at: 2 }, Stopped::Handoff);
        assert_eq!(todos.len(), 1, "the result of the command was dropped");
        assert!(todos[0].queued);
        assert!(todos[0].spec.prompt.ends_with("task"), "{}", todos[0].spec.prompt);
    }

    /// 循环的定时还在库里，接手的宿主到点再触发：交出会话以后旧宿主丢掉这次的结果。
    #[cfg(unix)]
    #[test]
    fn a_repeating_schedule_run_is_dropped_after_a_handoff() {
        let (todos, kept) = run_while_stopping("every-run", When::Every { minutes: 60 }, Stopped::Handoff);
        assert!(todos.is_empty());
        assert!(kept);
    }

    /// 要退出了才跑完的一次性定时不记结果、不开任务，放回库里，下次启动再到点。
    #[cfg(unix)]
    #[test]
    fn a_once_schedule_run_finishing_at_shutdown_is_put_back() {
        let (todos, kept) = run_while_stopping("once-shutdown", When::Once { at: 2 }, Stopped::Shutdown);
        assert!(todos.is_empty(), "the result of the command was recorded");
        assert!(kept, "the schedule was not put back");
    }

    #[test]
    fn the_output_keeps_the_end_of_stderr() {
        let kept = tail(&format!("{}end\n", "x".repeat(RUN_OUTPUT_LIMIT)));
        assert_eq!(kept.chars().count(), RUN_OUTPUT_LIMIT);
        assert!(kept.ends_with("end"));
        assert_eq!(tail("  short \n"), "short");
    }
}
