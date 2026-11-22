//! 看板：任务和定时存在宿主自己的 SQLite 库里（`runode_todo::Store`），
//! 前端经 `ClientMsg::ListTodos` 这几条消息要。app 在宿主拿到会话和监听的 socket 以后调
//! `Host::serve_todos` 告诉它库在哪、起定时的线程（`schedule`）；没调时这些消息都回 `Error`。
//!
//! 开始任务时在任务的目录（要工作树时先在仓库的 `.worktrees/` 下新开一个，分支 `runode/<名字>`）里开
//! 一个会话，等 shell 的提示符出来再打 `<agent> "$RUNODE_TODO_PROMPT"`（写法照 shell 来）：
//! 提示词经环境变量交给 shell，不用照各家 shell 的规矩转义。任务会话不进宿主手里的布局，桌面在看板上打开它时再接进窗口。
//!
//! 只有拿着会话的宿主改看板：开始交出会话或者要退出（`Peers::phase` 不是 `Phase::Serving`）以后不改库、
//! 不开任务，定时也不触发；接手的新宿主提交后自己接着办。例外是 `once_fired` 里的运行。
//! 接手的宿主的定时线程醒来时发现库被别的进程改过，就告诉看着看板的连接。两边的库是同一个文件，SQLite 自己管得住两个进程同时开着。锁的先后：拿着 `Todos::state` 可以再拿 `peers` 和 `registry`，反过来不行。
//!
//! 开任务的会话从开出来到记进库都算在 `Peers::launching` 里（`Shared::begin_launch`）。

mod auto_done;
mod check;
mod command;
mod run;
mod schedule;
mod worktree;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use runode_git::Repo;
use runode_protocol::{
    HostMsg, SessionId,
    todo::{DONE_TODOS_PAGE, Outcome, RESULT_LIMIT, SCHEDULE_RUNS_PAGE, Todo, TodoSpec, When},
};
use runode_todo::Store;
use runode_types::agent::AgentState;

use check::{check_run, check_spec, check_text, check_title, check_when};
use command::{
    ENV_TODO_ARG, ENV_TODO_BATCH_ARG, ENV_TODO_PROMPT, ENV_TODO_SETUP, WINDOWS_COMMAND_LINE, agent_command,
    agent_session, agent_with_options, batch_quote, passes_command_line, resume_command_line, windows_quote,
};
use worktree::{add_worktree, clean_worktree, in_worktree};

use crate::{Host, Shared, SpawnOptions, Stopped, server::Phase, session::Inbox};

/// 看板：库和定时的线程要的状态。
#[derive(Default)]
pub(crate) struct Todos {
    state: Mutex<State>,
    /// 看板变了时通知定时的线程。
    changed: Condvar,
    /// `State::running_runs` 少了一项时通知，配 `state`。
    runs_done: Condvar,
    /// 任务用的会话，看板一变就重读。会话线程拿它判断要不要报状态变了、记下自动完成要的 agent 状态；单独
    /// 一把锁，会话线程不等库。
    sessions: Mutex<Watched>,
    /// 会话线程要定时的线程马上再看一轮（自动完成的任务空闲下来了），不等看板变了。
    nudged: AtomicBool,
    /// 触发定时（到点的和手动的）从判断要不要让路到加上任务、占住命令都拿着它，同一个定时不会被两处同时
    /// 触发、各开一张。拿着它时只拿别的锁、不等命令跑完；别处拿着别的锁时不拿它。
    firing: Mutex<()>,
    /// 定时的线程读库失败了的几处。
    unreadable: schedule::Unreadable,
    /// 测试拿住它时，开任务停在开好会话之后、记进库之前。
    #[cfg(test)]
    launch_gate: Mutex<()>,
}

/// 任务用的会话，看板第 `generation` 次变了以后读到的。
#[derive(Default)]
struct Watched {
    /// 读的是看板第几次变（`State::generation`）之后的库。
    generation: u64,
    sessions: HashMap<SessionId, Watch>,
    /// 带 `TodoSpec::auto_done`、没完成的任务的会话里，开始以后（或者上次标完成、取消完成以后）见过 agent
    /// 在干活的；值是它这次空闲下来的时刻，不在空闲时为空。只在见过干活以后才自动完成：agent 起得慢、或者
    /// 没收到提示词停在空输入框上时不算做完。
    worked: HashMap<SessionId, Option<Instant>>,
    /// 新读到的带 `TodoSpec::auto_done`、没完成的任务的会话，定时的线程还没问过它的 agent 在不在干活。会话线程
    /// 只在状态变了时报，读到之前（接手旧宿主的会话、刚开始的任务）报的干活会漏掉。
    unchecked: HashSet<SessionId>,
    /// 自动完成时结束掉的会话，值是会话线程报结束的时刻（还没报时为空）。还没报、报了不到 `ENDING_GRACE`
    /// 的算还在用：报结束时 shell 还没被杀，杀了也要一会儿才退出，这期间进程还占着工作树（Windows 上删不掉，
    /// 清不掉的不再试）。
    ending: HashMap<SessionId, Option<Instant>>,
    /// 带 `TodoSpec::auto_done`、没完成的任务的会话里，agent 停在询问上的，值是开始问的时刻；问满
    /// `BLOCKED_GIVE_UP` 记成没做成。
    blocked: HashMap<SessionId, Instant>,
    /// 排着队却没开始成的任务（按 `Todo::id`）：试过几次，下次什么时候再试；到时候放回队里。宿主重启后忘掉，
    /// 那张任务留在队里照常排。
    retries: HashMap<String, (u32, Instant)>,
    /// 任务会话第一次读到的 Unix 时间：接回（`resume_todo`）的会话不沿用任务第一次开始的时刻，时限从这里重新
    /// 算起。宿主接手或重启后从那时算起，只会宽、不会冤枉。
    begun: HashMap<SessionId, u64>,
}

/// 结束掉的会话报了结束以后，再过这么久才清它的工作树。
const ENDING_GRACE: Duration = Duration::from_secs(5);

/// 报结局时任务被占着，最多再试这么多次，每次隔 100 毫秒。
const REPORT_TRIES: u32 = 30;

/// 一个任务会话。
#[derive(Clone, Copy)]
struct Watch {
    /// 任务还没完成。
    open: bool,
    auto_done: bool,
}

#[derive(Default)]
struct State {
    /// 库的路径，`Host::serve_todos` 给的；为空时看板没开。
    path: Option<PathBuf>,
    /// 打开着的库；打不开时为空，下次用时再试，用户修好文件后自己恢复。
    db: Option<Store>,
    /// `db` 上次读到的数据版本（`Store::data_version`），换了连接时重读；读不到时为空。
    data_version: Option<i64>,
    /// 看板变一次加一，定时的线程据此知道要重新看。
    generation: u64,
    /// 正在开始、或者正在清工作树的任务。
    busy: HashSet<String>,
    /// 到点先跑的命令（`Schedule::run`）还没跑完的定时：在里面的定时到点时跳过，不再跑一份。值是这个宿主停下
    /// 前还没记下结果时要放回库里的定时，只有到点触发（不是手动）的 `When::Once` 才有，记下结果或放回库里后清空。
    running_runs: HashMap<String, Option<runode_protocol::todo::Schedule>>,
}

/// `Todos::claim` 占住的任务，丢掉时放开。
struct Claim<'a> {
    todos: &'a Todos,
    id: String,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.todos.lock().busy.remove(&self.id);
    }
}

/// `Shared::begin_launch` 记上的一个正在开的任务会话，丢掉时减掉。创建和丢掉时都要拿 `peers`，不能拿着 `registry`。
struct Launching<'a> {
    shared: &'a Shared,
}

impl Drop for Launching<'_> {
    fn drop(&mut self) {
        self.shared.peers().launching -= 1;
        self.shared.peers_changed.notify_all();
    }
}

impl Todos {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 占住任务 `id`：同一张任务开始和清工作树一次只做一件，占住以后再读它，读到的才不会过时。已经被占着
    /// 时为空。
    fn claim(&self, id: &str) -> Option<Claim<'_>> {
        self.lock().busy.insert(id.to_owned()).then(|| Claim { todos: self, id: id.to_owned() })
    }

    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// 等到看板变了（`generation` 不再是 `seen`）、被 `nudge` 叫醒，或者过了 `timeout`。
    fn wait_for_change(&self, seen: u64, timeout: Duration) {
        let state = self.lock();
        let _ = self.changed.wait_timeout_while(state, timeout, |state| {
            state.generation == seen && !self.nudged.load(Ordering::Acquire)
        });
        self.nudged.store(false, Ordering::Release);
    }

    /// 叫醒定时的线程再看一轮。不拿 `state`（会话线程调，不等库）：恰好在它查完条件、还没睡下时叫的话
    /// 这次叫不醒，最多晚一个 `timeout`。
    fn nudge(&self) {
        self.nudged.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn watched(&self) -> MutexGuard<'_, Watched> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn firing(&self) -> MutexGuard<'_, ()> {
        self.firing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 拿住期间开任务停在开好会话之后、记进库之前。
    #[cfg(test)]
    pub(crate) fn hold_launch(&self) -> MutexGuard<'_, ()> {
        self.launch_gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 任务 `id` 在库里记的开始时刻。
    #[cfg(all(test, unix))]
    pub(crate) fn started(&self, id: &str) -> Option<u64> {
        self.with_db(|db| db.todo_by_id(id)).unwrap().unwrap().started
    }

    /// 库从上次看过以后被别的连接（别的进程）改过，这个宿主自己的改动不算。读不到库时报错。
    fn changed_elsewhere(&self) -> Result<bool, String> {
        let mut state = self.lock();
        let now = db_in(&mut state, |db| db.data_version())?;
        Ok(state.data_version.replace(now).is_some_and(|seen| seen != now))
    }

    /// 拿着 `state` 在库上做 `f`（`db_in`）。
    fn with_db<T>(&self, f: impl FnOnce(&mut Store) -> runode_todo::Result<T>) -> Result<T, String> {
        db_in(&mut self.lock(), f)
    }
}

/// 在 `state` 的库上做 `f`。看板没开、库打不开时报错；`f` 出错时关掉库，下次重新打开。
fn db_in<T>(state: &mut State, f: impl FnOnce(&mut Store) -> runode_todo::Result<T>) -> Result<T, String> {
    let Some(path) = state.path.clone() else {
        return Err("this host does not keep a todo board".into());
    };
    let db = match &mut state.db {
        Some(db) => db,
        empty => {
            let db = empty.insert(Store::open(&path)?);
            // 打开时就记下，这之后别的进程的改动才都比得出来。
            state.data_version = db.data_version().ok();
            db
        }
    };
    let result = f(db);
    if result.is_err() {
        state.db = None;
    }
    result
}

/// 现在的 Unix 时间，秒。
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

impl Host {
    /// 打开看板：库在 `db`（没有时建一个），起定时的线程。在宿主拿到会话和监听的 socket 以后调：
    /// `Host::listen` 之后，或者 `Host::take_over` 接手成功以后，只有一个宿主办定时。再调一次什么都不做。
    pub fn serve_todos(&self, db: PathBuf) {
        {
            let mut state = self.shared.todos.lock();
            if state.path.is_some() {
                return;
            }
            state.path = Some(db);
        }
        // 监听已经开了：这之前问过 `ListTodos` 的连接拿到的是「没有看板」，叫它们重问。
        self.shared.todos_changed();
        let todos = self.shared.todos.clone();
        let shared = Arc::downgrade(&self.shared);
        // 宿主替前端跑的 git 也要用户的 PATH，先在后台问好。
        if let Err(err) = std::thread::Builder::new().name("login-path".into()).spawn(run::login_path) {
            tracing::warn!("cannot ask the login shell for PATH: {err}");
        }
        if let Err(err) =
            std::thread::Builder::new().name("host-todos".into()).spawn(move || schedule::run(&todos, &shared))
        {
            tracing::error!("cannot start the schedule thread: {err}");
        }
    }

    /// 等定时到点先跑的命令（`Schedule::run`）都跑完、记完结果。交出会话的宿主在用 `waitpid(-1)` 收子进程之前
    /// 调：命令的 shell 是这个进程的子进程，被它先收走的话跑命令的线程拿不到结果。等的是所有在跑的命令，不设
    /// 上限：每条最多跑到它的 `Schedule::run_timeout`，过了连同子孙一起杀掉。
    pub fn wait_for_schedule_runs(&self) {
        let todos = &self.shared.todos;
        // 触发定时从消费定时到占住命令一直拿着 `firing`：拿到它以后，已经触发的命令都在 `running_runs` 里了。
        drop(todos.firing());
        let state = todos.lock();
        drop(
            todos
                .runs_done
                .wait_while(state, |state| !state.running_runs.is_empty())
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// 宿主不交出会话地退出前调：`Peers::phase` 还是 `Phase::Serving` 时置成 `Stopping(Stopped::Shutdown)`，之后
    /// 不再开会话、不改看板、不触发定时；再调 `Shared::put_back_unfinished_runs`，不等命令跑完。正在交出或已经
    /// 交出会话时什么都不做，定时归接手的宿主办。
    pub fn stop_schedules(&self) {
        {
            let mut peers = self.shared.peers();
            match peers.phase {
                Phase::Serving => peers.phase = Phase::Stopping(Stopped::Shutdown),
                Phase::Stopping(Stopped::Idle | Stopped::Shutdown) => {}
                Phase::HandingOver | Phase::Stopping(Stopped::Handoff) => return,
            }
        }
        self.shared.peers_changed.notify_all();
        self.shared.put_back_unfinished_runs();
    }
}

impl Shared {
    /// 能不能改看板：正在交出会话或者要退出了的宿主不改。
    fn todos_writable(&self) -> Result<(), String> {
        crate::refusal(&self.peers()).map_or(Ok(()), |why| Err(why.into()))
    }

    /// 要开一个任务会话：开出来到记进库都拿着返回的守卫。已经不开新会话（`crate::refusal`）时报错，
    /// 和记上在同一把锁里，交接开始以后不会再记上。
    fn begin_launch(&self) -> Result<Launching<'_>, String> {
        let mut peers = self.peers();
        if let Some(why) = crate::refusal(&peers) {
            return Err(why.into());
        }
        peers.launching += 1;
        Ok(Launching { shared: self })
    }

    /// 看板变了：叫醒定时的线程，告诉问过 `ListTodos` 的连接。
    fn todos_changed(&self) {
        let generation = {
            let mut state = self.todos.lock();
            state.generation += 1;
            state.generation
        };
        self.todos.changed.notify_all();
        self.watch_todo_sessions(generation);
        self.peers().todos_changed();
    }

    /// 看板第 `generation` 次变了以后重读任务用的会话。读不到库时照旧，下次变了
    /// 再读。
    fn watch_todo_sessions(&self, generation: u64) {
        match self.todos.with_db(|db| db.list_todos(unix_now())) {
            Ok(todos) => self.remember_todo_sessions(generation, &todos),
            // 前端收到这次的 `TodosChanged` 会来问 `ListTodos`，那时读到了就补上。
            Err(err) => tracing::debug!("cannot list the todos to watch their sessions: {err}"),
        }
    }

    /// 记下看板第 `generation` 次变了以后读到的 `todos` 用的会话。
    fn remember_todo_sessions(&self, generation: u64, todos: &[Todo]) {
        let mut watched = self.todos.watched();
        // 两个线程一起改看板时，先读的库可能后写回来：只留变得最晚的那次之后读到的。
        if watched.generation < generation {
            watched.generation = generation;
            let sessions: HashMap<_, _> = todos
                .iter()
                .filter_map(|todo| {
                    Some((todo.session?, Watch { open: todo.done_at.is_none(), auto_done: todo.spec.auto_done }))
                })
                .collect();
            let Watched { sessions: old, worked, unchecked, ending, blocked, begun, .. } = &mut *watched;
            let now = unix_now();
            begun.retain(|id, _| sessions.contains_key(id));
            for id in sessions.keys() {
                begun.entry(*id).or_insert(now);
            }
            // 没有工作树要清的任务不会来问 `Shared::ending`，过了时限的在这里忘掉。
            ending.retain(|_, at| at.is_none_or(|at| at.elapsed() < ENDING_GRACE));
            let finishing = |id: &SessionId| sessions.get(id).is_some_and(|watch| watch.open && watch.auto_done);
            // 新读到的，和刚打开自动完成、刚取消完成的。
            let was = |id: &SessionId| old.get(id).is_some_and(|watch| watch.open && watch.auto_done);
            let added: Vec<_> = sessions.keys().filter(|id| finishing(id) && !was(id)).copied().collect();
            unchecked.extend(&added);
            unchecked.retain(|id| finishing(id));
            worked.retain(|id, _| finishing(id));
            blocked.retain(|id, _| finishing(id));
            *old = sessions;
            drop(watched);
            // 定时的线程可能在这之前就醒了、看过了，叫它再看一轮。
            if !added.is_empty() {
                self.todos.nudge();
            }
        }
    }

    /// 会话 `id` 的 agent 状态（`agent`）、前台是不是 shell 变了，或者会话结束了（`ended`：shell 退出或者
    /// 会话被收掉，会话线程报的）：告诉问过 `ListTodos` 的连接，前端不用轮询会话的状态。没做完的任务的卡片
    /// 可能换了列；完成了的任务的会话里 agent 还在干活时，桌面侧栏的计数也要算上它，会话结束时卡片上打开终端
    /// 的按钮要跟着收起。自动完成的任务另记下 agent 干过活、什么时候空闲下来的（`Watched::worked`）。
    pub(crate) fn todo_session_changed(&self, id: SessionId, ended: bool, agent: Option<AgentState>) {
        let mut watched = self.todos.watched();
        if ended && let Some(at @ None) = watched.ending.get_mut(&id) {
            *at = Some(Instant::now());
        }
        let Some(watch) = watched.sessions.get(&id).copied() else { return };
        let mut nudge = false;
        if ended || agent != Some(AgentState::Blocked) {
            watched.blocked.remove(&id);
        } else if watch.open && watch.auto_done {
            watched.blocked.entry(id).or_insert_with(Instant::now);
        }
        if ended {
            watched.worked.remove(&id);
        } else if watch.open && watch.auto_done {
            match agent {
                Some(AgentState::Working) => {
                    watched.worked.insert(id, None);
                }
                // 认不出 agent 了（多半是报完结局退回了 shell）也从这时算起，做没做完由 `finished_by_agent` 定。
                Some(AgentState::Idle) | None => {
                    if let Some(since @ None) = watched.worked.get_mut(&id) {
                        *since = Some(Instant::now());
                        nudge = true;
                    }
                }
                Some(AgentState::Blocked) => {
                    if let Some(since) = watched.worked.get_mut(&id) {
                        *since = None;
                    }
                }
            }
        }
        drop(watched);
        if nudge {
            self.todos.nudge();
        }
        self.peers().todos_changed();
    }

    /// 有没有在用的定时或者排着队的任务：有时单独跑的宿主不因空闲退出。
    fn set_scheduled(&self, scheduled: bool) {
        let mut peers = self.peers();
        if peers.scheduled != scheduled {
            peers.scheduled = scheduled;
            drop(peers);
            self.peers_changed.notify_all();
        }
    }

    /// 回 `ClientMsg::ListTodos`。
    pub(crate) fn list_todos(&self, req: u32) -> Result<HostMsg, String> {
        let now = unix_now();
        // 先取：读库之前看板又变了的话记得低一些，那次变了自己会再读。
        let generation = self.todos.generation();
        let (todos, mut schedules, max_running) =
            self.todos.with_db(|db| Ok((db.list_todos(now)?, db.list_schedules()?, db.max_running()?)))?;
        // 看板变了时没读到库的话，会话名单还是旧的，借这次读到的补上。
        self.remember_todo_sessions(generation, &todos);
        for schedule in &mut schedules {
            if !schedule.paused {
                schedule.next_fire = runode_todo::next_fire(&schedule.when, schedule.last_fired);
            }
            schedule.running = self.todos.run_pending(&schedule.id);
        }
        Ok(HostMsg::Todos { req, todos, schedules, max_running })
    }

    /// 回 `ClientMsg::QueueTodo`。
    pub(crate) fn queue_todo(&self, req: u32, id: &str, queued: bool, after: Option<&str>) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
        let after = after.map(|after| self.resolve_todo(after)).transpose()?;
        // 正在开始的任务不能同时排进队里。
        let claim = self.claim_todo(id)?;
        if !self.todos.with_db(|db| db.set_queued(id, queued, after.as_deref()))? {
            return Err(no_todo(id));
        }
        // 不再排队时也不再替它重试开始。
        self.todos.watched().retries.remove(id);
        drop(claim);
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::PauseSchedule`。
    pub(crate) fn pause_schedule(&self, req: u32, id: &str, paused: bool) -> Result<HostMsg, String> {
        self.todos_writable()?;
        if !self.todos.with_db(|db| db.set_paused(id, paused, unix_now()))? {
            return Err(format!("no schedule {id}"));
        }
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::FireSchedule`。
    pub(crate) fn fire_schedule(&self, req: u32, id: &str) -> Result<HostMsg, String> {
        self.todos_writable()?;
        self.fire_now(id)?;
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::SetMaxRunning`。
    pub(crate) fn set_max_running(&self, req: u32, max_running: u32) -> Result<HostMsg, String> {
        self.todos_writable()?;
        self.todos.with_db(|db| db.set_max_running(max_running))?;
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::ListDoneTodos`。
    pub(crate) fn list_done_todos(&self, req: u32, schedule: Option<&str>, offset: u32) -> Result<HostMsg, String> {
        let (todos, more) = self.todos.with_db(|db| db.list_done_todos(schedule, offset, DONE_TODOS_PAGE))?;
        Ok(HostMsg::DoneTodos { req, todos, more })
    }

    /// 回 `ClientMsg::ListScheduleRuns`。
    pub(crate) fn list_schedule_runs(&self, req: u32, schedule: &str, offset: u32) -> Result<HostMsg, String> {
        let (runs, more) = self
            .todos
            .with_db(|db| db.list_schedule_runs(schedule, offset, SCHEDULE_RUNS_PAGE))?
            .ok_or_else(|| format!("no schedule {schedule}"))?;
        Ok(HostMsg::ScheduleRuns { req, runs, more })
    }

    /// 回 `ClientMsg::AddTodo`。
    pub(crate) fn add_todo(&self, req: u32, spec: &TodoSpec) -> Result<HostMsg, String> {
        self.todos_writable()?;
        check_title(spec)?;
        check_spec(spec)?;
        let id = self.todos.with_db(|db| db.add_todo(spec, unix_now(), None))?;
        self.todos_changed();
        Ok(HostMsg::TodoAdded { req, id })
    }

    /// 回 `ClientMsg::UpdateTodo`。
    pub(crate) fn update_todo(
        &self,
        req: u32,
        id: &str,
        spec: Option<&TodoSpec>,
        done: Option<bool>,
    ) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
        // 改目录、工作树的同时有人在开始这个任务的话，开出来的会话和库里记的会对不上。标完成、取消完成也占着：
        // 清工作树的（`sweep_worktrees`）占住以后重读、认定它完成了才清，这期间取消完成的话清掉的就是又要
        // 用的工作树；正在开始的也一样，开出来的会话记在一张完成了的任务上。
        let claim = self.claim_todo(id)?;
        if let Some(spec) = spec {
            check_title(spec)?;
            let old = self.todos.with_db(|db| db.todo_by_id(id))?.ok_or_else(|| no_todo(id))?;
            if old.started.is_none() {
                check_spec(spec)?;
            } else if old.spec.dir != spec.dir || old.spec.worktree != spec.worktree {
                return Err(format!("todo {id} has started; its directory and worktree stay as they are"));
            } else {
                // 目录不变，原目录没了（比如工作树里的 agent 还在跑）也能改标题和提示词。
                check_text(spec)?;
            }
        }
        let done_at = done.map(|done| done.then(unix_now));
        if !self.todos.with_db(|db| db.update_todo(id, spec, done_at))? {
            return Err(no_todo(id));
        }
        // 标完成、取消完成以后，agent 要再干一轮活才自动完成：取消完成后卡片停在需要你给人看。
        if done.is_some()
            && let Ok(Some(Todo { session: Some(session), .. })) = self.todos.with_db(|db| db.todo_by_id(id))
        {
            self.todos.watched().worked.remove(&session);
        }
        // 先放开再通知：叫醒的定时线程马上来清这张刚标完成的任务，还占着的话它跳过，要等下一次看板变了
        // 或者睡满才再看。
        drop(claim);
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::ReportTodo`。
    pub(crate) fn report_todo(&self, req: u32, id: &str, outcome: Outcome, result: &str) -> Result<HostMsg, String> {
        self.todos_writable()?;
        if outcome == Outcome::Unknown {
            return Err("unknown outcome".into());
        }
        let id = &self.resolve_todo(id)?;
        let result = clip_result(result);
        let result = (!result.is_empty()).then_some(result.as_str());
        // 和标完成一样占着：自动完成的定时线程占住以后重读、照结局办，不会读到一半报上来的。它占不了多久，
        // 等一等，不叫做完了的 agent 自己重试。
        let mut tries = 0;
        let claim = loop {
            match self.todos.claim(id) {
                Some(claim) => break claim,
                None if tries < REPORT_TRIES => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                None => return Err(format!("todo {id} is starting or being cleaned up; try again")),
            }
        };
        if !self.todos.with_db(|db| db.report(id, outcome, result))? {
            return Err(no_todo(id));
        }
        drop(claim);
        tracing::info!("todo {id} reported {}", outcome.name());
        self.todos_changed();
        // 报完 agent 多半已经空闲着，不用等它再报一次状态。
        self.todos.nudge();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::RemoveTodo`：它的会话已经不在时，顺带清掉工作树和分支。
    pub(crate) fn remove_todo(&self, req: u32, id: &str) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
        // 不和开始任务、清工作树同时做：那边正用着的工作树这边别删。
        let _claim = self.claim_todo(id)?;
        let todo = self.todos.with_db(|db| db.remove_todo(id))?.ok_or_else(|| no_todo(id))?;
        self.todos_changed();
        if let Some(session) = todo.session {
            self.end_if_exited(session);
        }
        if !self.in_use(&todo) {
            clean_worktree(&todo);
        }
        Ok(HostMsg::Done { req })
    }

    /// 任务的会话还在跑。查过以后这个宿主已经在交出会话了也算：交出去的会话不在 `registry`
    /// 里，看着和结束了一样，其实在新宿主那边接着跑。所以要先查会话再查这个，反过来的话中间交出去的会被
    /// 当成结束了。
    fn in_use(&self, todo: &Todo) -> bool {
        todo.session.is_some_and(|session| self.running(session) || self.ending(session))
            || self.todos_writable().is_err()
    }

    /// 会话 `id` 是自动完成时结束掉的，还算在用；报了结束、过了 `ENDING_GRACE` 的不算，顺带忘掉。
    fn ending(&self, id: SessionId) -> bool {
        let mut watched = self.todos.watched();
        match watched.ending.get(&id) {
            None => false,
            Some(Some(at)) if at.elapsed() >= ENDING_GRACE => {
                watched.ending.remove(&id);
                false
            }
            Some(_) => true,
        }
    }

    /// 会话 `id` 还在跑。shell 退出了的会话留在宿主里给人看最后的输出，不算；没及时回话的算还在，免得
    /// 清掉它正在用的工作树。
    fn running(&self, id: SessionId) -> bool {
        let known = self.registry().sessions.contains_key(&id);
        known && !self.session_info(id).is_some_and(|info| info.exited)
    }

    /// 收掉 shell 已经退出的会话 `id`；还在跑的、开在界面上的不动，界面关掉它时自己收。
    fn end_if_exited(&self, id: SessionId) {
        if self.session_info(id).is_some_and(|info| info.exited && info.clients == 0) {
            self.kill(id);
        }
    }

    /// 收掉完成了的任务的、shell 已经退出的会话：没完成时留着给人看 agent 最后的输出，完成了就没用了。
    /// 定时的线程每一轮都做。
    fn end_finished_sessions(&self) {
        if self.todos_writable().is_err() {
            return;
        }
        // 只看还在 registry 里的会话，别的 `end_if_exited` 本来也不动；先取好再放掉锁，不和库锁同时拿。
        let live: HashSet<SessionId> = self.registry().sessions.keys().copied().collect();
        if live.is_empty() {
            return;
        }
        // 不按看板保留期的规矩滤：完成很久以后 shell 才退出的会话也要收掉，不然单独跑的宿主一直不空闲。
        let sessions = match self.todos.with_db(|db| db.done_sessions()) {
            Ok(sessions) => sessions,
            Err(err) => {
                tracing::warn!("cannot read the todos: {err}");
                return;
            }
        };
        for session in sessions.into_iter().filter(|session| live.contains(session)) {
            self.end_if_exited(session);
        }
    }

    /// 清掉完成了的任务的工作树和分支，每张只试一次；会话还在的等它结束后再清。
    /// 定时的线程每一轮都做，看板一变就有一轮，所以标完成后很快就清。
    fn sweep_worktrees(&self) {
        if self.todos_writable().is_err() {
            return;
        }
        let done = match self.todos.with_db(|db| db.unswept_done()) {
            Ok(done) => done,
            Err(err) => {
                tracing::warn!("cannot read the finished todos: {err}");
                return;
            }
        };
        for todo in &done {
            // 正在开始的下一轮再看；占住以后重读，读出列表以后它可能被取消完成、又开始过。
            let Some(_claim) = self.todos.claim(&todo.id) else { continue };
            let todo = match self.todos.with_db(|db| db.todo_by_id(&todo.id)) {
                Ok(Some(todo)) if todo.done_at.is_some() && todo.worktree_dir.is_some() => todo,
                Ok(_) => continue,
                Err(err) => {
                    tracing::warn!("cannot read todo {}: {err}", todo.id);
                    continue;
                }
            };
            if self.in_use(&todo) {
                continue;
            }
            let (worktree_gone, branch_gone) = clean_worktree(&todo);
            match self.todos.with_db(|db| db.set_swept(&todo.id, worktree_gone, branch_gone)) {
                // 任务的工作树、分支不再记着，看板要重读，不然一直指着删掉了的目录。
                Ok(()) if worktree_gone || branch_gone => self.todos_changed(),
                Ok(()) => {}
                Err(err) => tracing::warn!("cannot record that todo {} was cleaned up: {err}", todo.id),
            }
        }
    }

    /// 回 `ClientMsg::GetTodo`。
    pub(crate) fn get_todo(&self, req: u32, id: &str) -> Result<HostMsg, String> {
        let id = self.resolve_todo(id)?;
        let todo = self.todos.with_db(|db| db.todo_by_id(&id))?.ok_or_else(|| no_todo(&id))?;
        Ok(HostMsg::Todo { req, todo })
    }

    /// 占住任务 `id`，占不住时回一句让前端再试。
    fn claim_todo(&self, id: &str) -> Result<Claim<'_>, String> {
        self.todos.claim(id).ok_or_else(|| format!("todo {id} is starting or being cleaned up; try again"))
    }

    /// 前端写的任务（标识，或者 `5`、`#5` 这样的编号）的标识。
    fn resolve_todo(&self, text: &str) -> Result<String, String> {
        self.todos.with_db(|db| db.resolve_todo(text))?.ok_or_else(|| no_todo(text))
    }

    /// 回 `ClientMsg::MoveTodo`。
    pub(crate) fn move_todo(&self, req: u32, id: &str, before: Option<&str>) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
        let before = before.map(|before| self.resolve_todo(before)).transpose()?;
        self.todos.with_db(|db| db.move_todo(id, before.as_deref()))?;
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::AddSchedule`。
    pub(crate) fn add_schedule(
        &self,
        req: u32,
        spec: &TodoSpec,
        when: &When,
        run: Option<&str>,
        run_timeout: Option<u32>,
    ) -> Result<HostMsg, String> {
        self.todos_writable()?;
        check_title(spec)?;
        check_spec(spec)?;
        let now = unix_now();
        check_when(when, now)?;
        check_run(run, run_timeout)?;
        let id = self.todos.with_db(|db| db.add_schedule(spec, when, run, run_timeout, now))?;
        self.todos_changed();
        Ok(HostMsg::TodoAdded { req, id })
    }

    /// 回 `ClientMsg::UpdateSchedule`。
    pub(crate) fn update_schedule(
        &self,
        req: u32,
        id: &str,
        spec: Option<&TodoSpec>,
        when: Option<&When>,
        run: Option<&str>,
        run_timeout: Option<u32>,
    ) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let now = unix_now();
        spec.map_or(Ok(()), |spec| check_title(spec).and_then(|()| check_spec(spec)))?;
        when.map_or(Ok(()), |when| check_when(when, now))?;
        // 空串和 0 是清掉，不用查。
        check_run(run.filter(|run| !run.is_empty()), run_timeout.filter(|minutes| *minutes != 0))?;
        if !self.todos.with_db(|db| db.update_schedule(id, spec, when, run, run_timeout, now))? {
            return Err(format!("no schedule {id}"));
        }
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::RemoveSchedule`。
    pub(crate) fn remove_schedule(&self, req: u32, id: &str) -> Result<HostMsg, String> {
        self.todos_writable()?;
        if !self.todos.with_db(|db| db.remove_schedule(id))? {
            return Err(format!("no schedule {id}"));
        }
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 开始任务 `id`，返回它的会话：会话还在时就是那个，否则在它的目录（或给它开的工作树）里新开一个。
    pub(crate) fn start_todo(&self, id: &str) -> Result<SessionId, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
        let claim = self.claim_todo(id)?;
        self.start_claimed(id, &claim)
    }

    /// 开始调用方已经占住的任务 `id`（标识，不认编号）。
    fn start_claimed(&self, id: &str, _claim: &Claim<'_>) -> Result<SessionId, String> {
        self.todos_writable()?;
        let todo = self.todos.with_db(|db| db.todo_by_id(id))?.ok_or_else(|| no_todo(id))?;
        if todo.done_at.is_some() {
            return Err(format!("todo {id} is done"));
        }
        if let Some(session) = todo.session {
            if self.running(session) {
                return Ok(session);
            }
            // shell 退出了：收掉旧会话，下面开新的。
            self.end_if_exited(session);
        }
        // 别的程序可以不经宿主直接写库（`runode_todo::Store`），所以开新会话、打进 shell 之前再查一遍；
        // 会话还在时上面已经直接给出，不因原目录没了打不开。
        check_spec(&todo.spec)?;
        let session = self.launch_todo(&todo)?;
        self.todos_changed();
        Ok(session)
    }

    /// 回 `ClientMsg::ResumeTodo`：接回开始过的任务的会话，没了时开一个接着 agent 上次的对话。工作树已经清掉时
    /// 对话的目录没了、接不上，只在任务的目录里开 shell：在那里续上的会是别的对话。
    pub(crate) fn resume_todo(&self, id: &str) -> Result<SessionId, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
        let _claim = self.claim_todo(id)?;
        let todo = self.todos.with_db(|db| db.todo_by_id(id))?.ok_or_else(|| no_todo(id))?;
        let (Some(started), Some(old)) = (todo.started, todo.session) else {
            return Err(format!("todo {id} has not started"));
        };
        if self.running(old) {
            return Ok(old);
        }
        self.end_if_exited(old);
        // 库可以不经宿主直接写，打进 shell 之前再查一遍命令是不是一行。
        check_text(&todo.spec)?;
        let resume =
            runode_agent::models::resume_command(&agent_with_options(&todo.spec), &agent_session(&todo.id, started));
        // 清掉的工作树库里记成空的（`set_swept`），要看任务要没要过工作树。
        let (cwd, command) = match todo.worktree_dir.as_ref().filter(|root| root.is_dir()) {
            Some(root) => (in_worktree(root, &todo.spec.dir), resume),
            None if todo.spec.worktree => (todo.spec.dir.clone(), None),
            None => (todo.spec.dir.clone(), resume),
        };
        if !cwd.is_dir() {
            return Err(format!("{} is not a directory", cwd.display()));
        }
        let _launching = self.begin_launch()?;
        let session = self.spawn_with(SpawnOptions::headless(Some(cwd)), &[]).map_err(|err| format!("{err:#}"))?;
        if let Some(command) = command {
            let command = resume_command_line(&command, &runode_terminal::pty::default_shell());
            if !self.deliver(session, Inbox::TypeAtPrompt(format!("{command}\r").into_bytes())) {
                self.kill(session);
                return Err(format!("the session for todo {id} ended before the agent could resume"));
            }
        }
        if let Err(err) = self.todos.with_db(|db| db.set_started(&todo.id, started, session, None, false)) {
            self.kill(session);
            return Err(err);
        }
        tracing::info!("todo {} resumed in session {session}", todo.id);
        self.todos_changed();
        Ok(session)
    }

    /// 给任务开会话、在提示符上跑 agent，记进库里；要工作树又还没有时先开一个。
    fn launch_todo(&self, todo: &Todo) -> Result<SessionId, String> {
        // 记着的工作树被删了（Git 面板、`git worktree remove`）就当没有、另开一个：PTY 遇到不存在的目录
        // 会悄悄改在家目录里开，agent 就带着任务在那里干活。旧分支上的提交留在旧分支上。
        let kept = todo.worktree_dir.as_ref().filter(|root| root.is_dir());
        let made = match kept {
            None if todo.spec.worktree => Some(add_worktree(todo)?),
            _ => None,
        };
        let cwd = match made.as_ref().map(|(root, _)| root).or(kept) {
            Some(root) => in_worktree(root, &todo.spec.dir),
            None => todo.spec.dir.clone(),
        };
        // 分支也删：留着的话下次开始时名字被占，每失败一次就多占一个名字。
        let undo_worktree = || {
            let Some((root, branch)) = &made else { return };
            let Some(repo) = Repo::open(&todo.spec.dir) else { return };
            if let Err(err) = repo.remove_worktree(root, false) {
                tracing::warn!("cannot remove the worktree {} made for todo {}: {err}", root.display(), todo.id);
            } else if let Err(err) = repo.delete_merged_branch(branch) {
                tracing::warn!("cannot remove the branch {branch} made for todo {}: {err}", todo.id);
            }
        };
        let instructions = todo.spec.instructions();
        let setup = made.as_ref().and_then(|_| runode_tasks::worktree_setup(&todo.spec.dir));
        let shell = runode_terminal::pty::default_shell();
        let quoted = passes_command_line(&shell).then(|| (windows_quote(&instructions), batch_quote(&instructions)));
        if quoted.as_ref().is_some_and(|(arg, _)| arg.encode_utf16().count() >= WINDOWS_COMMAND_LINE) {
            undo_worktree();
            return Err("the prompt is too long for a Windows command line".into());
        }
        let mut env = vec![(ENV_TODO_PROMPT, instructions.as_str())];
        env.extend(setup.as_deref().map(|setup| (ENV_TODO_SETUP, setup)));
        if let Some((arg, batch)) = &quoted {
            env.extend([(ENV_TODO_ARG, arg.as_str()), (ENV_TODO_BATCH_ARG, batch.as_str())]);
        }
        let _launching = match self.begin_launch() {
            Ok(launching) => launching,
            Err(err) => {
                undo_worktree();
                return Err(err);
            }
        };
        let session = match self.spawn_with(SpawnOptions::headless(Some(cwd)), &env) {
            Ok(session) => session,
            Err(err) => {
                undo_worktree();
                return Err(format!("{err:#}"));
            }
        };
        // 任务会话不指定 shell，起的是 `default_shell`。
        let started = unix_now();
        let agent =
            runode_agent::models::start_command(&agent_with_options(&todo.spec), &agent_session(&todo.id, started));
        let command = agent_command(&agent, &shell, setup.is_some());
        if !self.deliver(session, Inbox::TypeAtPrompt(command.into_bytes())) {
            self.kill(session);
            undo_worktree();
            return Err(format!("the session for todo {} ended before the agent could start", todo.id));
        }
        let worktree = made.as_ref().map(|(root, branch)| (root.as_path(), branch.as_str()));
        #[cfg(test)]
        drop(self.todos.hold_launch());
        if let Err(err) = self.todos.with_db(|db| db.set_started(&todo.id, started, session, worktree, true)) {
            self.kill(session);
            undo_worktree();
            return Err(err);
        }
        tracing::info!("todo {} started in session {session}", todo.id);
        Ok(session)
    }
}

fn no_todo(id: &str) -> String {
    format!("no todo {id}")
}

/// 去掉首尾空白，留前 `RESULT_LIMIT` 个字。
fn clip_result(text: &str) -> String {
    text.trim().chars().take(RESULT_LIMIT).collect()
}

#[cfg(test)]
mod tests;
