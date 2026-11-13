//! 看板（见 `runode_protocol::todo`）：任务和定时存在宿主自己的 SQLite 库里（`runode_todo::Store`），
//! 前端经 `ClientMsg::ListTodos` 这几条消息要。app 在宿主拿到会话和监听的 socket 以后调
//! `Host::serve_todos` 告诉它库在哪、起定时的线程（`schedule`）；没调时这些消息都回 `Error`。
//!
//! 开始任务时在任务的目录（要工作树时先在仓库的 `.worktrees/` 下新开一个，分支 `runode/<名字>`）里开
//! 一个会话，等 shell 的提示符出来再打 `<agent> "$RUNODE_TODO_PROMPT"`（写法照 shell 来，见
//! `agent_command`）：提示词经环境变量交给 shell，不用照各家 shell 的规矩转义。任务会话不进宿主手里的布局，桌面在看板上打开它时再接进窗口。
//!
//! 只有拿着会话的宿主改看板：开始交出会话或者要退出（`Peers::phase` 不是 `Phase::Serving`）以后不改库、
//! 不开任务，定时也不触发；接手的新宿主提交后自己接着办。两边的库是同一个文件，SQLite 自己管得住两个
//! 进程同时开着。锁的先后：拿着 `Todos::state` 可以再拿 `peers` 和 `registry`，反过来不行。

mod auto_done;
mod check;
mod command;
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
    todo::{DONE_TODOS_PAGE, Todo, TodoSpec, When},
};
use runode_todo::Store;
use runode_types::agent::AgentState;

use check::{check_spec, check_text, check_title, check_when};
use command::{
    ENV_TODO_ARG, ENV_TODO_BATCH_ARG, ENV_TODO_PROMPT, ENV_TODO_SETUP, WINDOWS_COMMAND_LINE, agent_command,
    agent_session, agent_with_options, batch_quote, passes_command_line, resume_command_line, windows_quote,
};
use worktree::{add_worktree, clean_worktree, in_worktree};

use crate::{Host, Shared, SpawnOptions, session::Inbox};

/// 看板：库和定时的线程要的状态。
#[derive(Default)]
pub(crate) struct Todos {
    state: Mutex<State>,
    /// 看板变了时通知定时的线程，见 `State::generation`。
    changed: Condvar,
    /// 任务用的会话，看板一变就重读。会话线程拿它判断要不要报状态变了、记下自动完成要的 agent 状态；单独
    /// 一把锁，会话线程不等库。
    sessions: Mutex<Watched>,
    /// 会话线程要定时的线程马上再看一轮（自动完成的任务空闲下来了），不等看板变了。
    nudged: AtomicBool,
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
}

/// 结束掉的会话报了结束以后，再过这么久才清它的工作树。
const ENDING_GRACE: Duration = Duration::from_secs(5);

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
    /// 看板变一次加一，定时的线程据此知道要重新看。
    generation: u64,
    /// 正在开始、或者正在清工作树的任务，见 `Todos::claim`。
    busy: HashSet<String>,
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

    /// 在库上做 `f`。看板没开、库打不开时报错；`f` 出错时关掉库，下次重新打开。
    fn with_db<T>(&self, f: impl FnOnce(&mut Store) -> runode_todo::Result<T>) -> Result<T, String> {
        let mut state = self.lock();
        let Some(path) = state.path.clone() else {
            return Err("this host does not keep a todo board".into());
        };
        let db = match &mut state.db {
            Some(db) => db,
            empty => empty.insert(Store::open(&path)?),
        };
        let result = f(db);
        if result.is_err() {
            state.db = None;
        }
        result
    }
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
        if let Err(err) =
            std::thread::Builder::new().name("host-todos".into()).spawn(move || schedule::run(&todos, &shared))
        {
            tracing::error!("cannot start the schedule thread: {err}");
        }
    }
}

impl Shared {
    /// 能不能改看板：正在交出会话或者要退出了的宿主不改，见模块文档。
    fn todos_writable(&self) -> Result<(), String> {
        crate::refusal(&self.peers()).map_or(Ok(()), |why| Err(why.into()))
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

    /// 看板第 `generation` 次变了以后重读任务用的会话，见 `Todos::sessions`。读不到库时照旧，下次变了
    /// 再读。
    fn watch_todo_sessions(&self, generation: u64) {
        match self.todos.with_db(|db| db.list_todos(unix_now())) {
            Ok(todos) => self.remember_todo_sessions(generation, &todos),
            // 前端收到这次的 `TodosChanged` 会来问 `ListTodos`，那时读到了就补上，见 `list_todos`。
            Err(err) => tracing::debug!("cannot list the todos to watch their sessions: {err}"),
        }
    }

    /// 记下看板第 `generation` 次变了以后读到的 `todos` 用的会话，见 `Todos::sessions`。
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
            let Watched { sessions: old, worked, unchecked, ending, .. } = &mut *watched;
            // 没有工作树要清的任务不会来问 `Shared::ending`，过了时限的在这里忘掉。
            ending.retain(|_, at| at.is_none_or(|at| at.elapsed() < ENDING_GRACE));
            let finishing = |id: &SessionId| sessions.get(id).is_some_and(|watch| watch.open && watch.auto_done);
            // 新读到的，和刚打开自动完成、刚取消完成的。
            let was = |id: &SessionId| old.get(id).is_some_and(|watch| watch.open && watch.auto_done);
            let added: Vec<_> = sessions.keys().filter(|id| finishing(id) && !was(id)).copied().collect();
            unchecked.extend(&added);
            unchecked.retain(|id| finishing(id));
            worked.retain(|id, _| finishing(id));
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
    /// 可能换了列；完成了的任务只在会话结束时报，卡片上打开终端的按钮要跟着收起，agent 的状态变了不影响它。
    /// 自动完成的任务另记下 agent 干过活、什么时候空闲下来的（`Watched::worked`）。
    pub(crate) fn todo_session_changed(&self, id: SessionId, ended: bool, agent: Option<AgentState>) {
        let mut watched = self.todos.watched();
        if ended && let Some(at @ None) = watched.ending.get_mut(&id) {
            *at = Some(Instant::now());
        }
        let Some(watch) = watched.sessions.get(&id).copied() else { return };
        let mut nudge = false;
        if ended {
            watched.worked.remove(&id);
        } else if watch.open && watch.auto_done {
            match agent {
                Some(AgentState::Working) => {
                    watched.worked.insert(id, None);
                }
                Some(AgentState::Idle) => {
                    if let Some(since @ None) = watched.worked.get_mut(&id) {
                        *since = Some(Instant::now());
                        nudge = true;
                    }
                }
                _ => {
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
        if watch.open || ended {
            self.peers().todos_changed();
        }
    }

    /// 有没有定时：有时单独跑的宿主不因空闲退出，见 `Host::run_until_idle`。
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
        let (todos, mut schedules) = self.todos.with_db(|db| Ok((db.list_todos(now)?, db.list_schedules()?)))?;
        // 看板变了时没读到库的话，会话名单还是旧的，借这次读到的补上。
        self.remember_todo_sessions(generation, &todos);
        for schedule in &mut schedules {
            schedule.next_fire = runode_todo::next_fire(&schedule.when, schedule.last_fired);
        }
        Ok(HostMsg::Todos { req, todos, schedules })
    }

    /// 回 `ClientMsg::ListDoneTodos`。
    pub(crate) fn list_done_todos(&self, req: u32, schedule: Option<&str>, offset: u32) -> Result<HostMsg, String> {
        let (todos, more) = self.todos.with_db(|db| db.list_done_todos(schedule, offset, DONE_TODOS_PAGE))?;
        Ok(HostMsg::DoneTodos { req, todos, more })
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

    /// 回 `ClientMsg::RemoveTodo`：它的会话已经不在时，顺带清掉工作树和分支，见 `clean_worktree`。
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

    /// 任务的会话还在跑，见 `running`。查过以后这个宿主已经在交出会话了也算：交出去的会话不在 `registry`
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
    /// 定时的线程每一轮都做，见 `sweep_worktrees`。
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

    /// 清掉完成了的任务的工作树和分支（见 `clean_worktree`），每张只试一次；会话还在的等它结束后再清。
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

    /// 占住任务 `id`（见 `Todos::claim`），占不住时回一句让前端再试。
    fn claim_todo(&self, id: &str) -> Result<Claim<'_>, String> {
        self.todos.claim(id).ok_or_else(|| format!("todo {id} is starting or being cleaned up; try again"))
    }

    /// 前端写的任务（标识，或者 `5`、`#5` 这样的编号，见 `ClientMsg::GetTodo`）的标识。
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
    pub(crate) fn add_schedule(&self, req: u32, spec: &TodoSpec, when: &When) -> Result<HostMsg, String> {
        self.todos_writable()?;
        check_title(spec)?;
        check_spec(spec)?;
        let now = unix_now();
        check_when(when, now)?;
        let id = self.todos.with_db(|db| db.add_schedule(spec, when, now))?;
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
    ) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let now = unix_now();
        spec.map_or(Ok(()), |spec| check_title(spec).and_then(|()| check_spec(spec)))?;
        when.map_or(Ok(()), |when| check_when(when, now))?;
        if !self.todos.with_db(|db| db.update_schedule(id, spec, when, now))? {
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
        let _claim = self.claim_todo(id)?;
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
        // 库可以不经宿主直接写（见 `start_todo`），打进 shell 之前再查一遍命令是不是一行。
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
        let session = self.spawn_with(SpawnOptions::headless(Some(cwd)), &[]).map_err(|err| format!("{err:#}"))?;
        if let Some(command) = command {
            let command = resume_command_line(&command, &runode_terminal::pty::default_shell());
            self.deliver(session, Inbox::TypeAtPrompt(format!("{command}\r").into_bytes()));
        }
        if let Err(err) = self.todos.with_db(|db| db.set_started(&todo.id, started, session, None)) {
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
        self.deliver(session, Inbox::TypeAtPrompt(command.into_bytes()));
        let worktree = made.as_ref().map(|(root, branch)| (root.as_path(), branch.as_str()));
        if let Err(err) = self.todos.with_db(|db| db.set_started(&todo.id, started, session, worktree)) {
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

#[cfg(test)]
mod tests {
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
            let (state, _) = todos
                .changed
                .wait_timeout_while(state, Duration::from_secs(10), |state| state.generation == seen)
                .unwrap();
            assert_ne!(state.generation, seen, "the board never changed");
            state.busy.contains(&watched)
        });
        // 让它先等上。
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(shared.update_todo(2, &id, None, Some(true)), Ok(HostMsg::Done { req: 2 }));
        assert!(!busy_on_wake.join().unwrap(), "the todo was still claimed when the scheduler woke up");
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
        shared.todos.with_db(|db| db.set_started(&id, unix_now(), session, None)).unwrap();
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
    }
}
