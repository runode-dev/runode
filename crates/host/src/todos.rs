//! 看板（见 `runode_protocol::todo`）：任务和定时存在宿主自己的 SQLite 库里（`runode_todo::Store`），
//! 前端经 `ClientMsg::ListTodos` 这几条消息要。app 在宿主拿到会话和监听的 socket 以后调
//! `Host::serve_todos` 告诉它库在哪、起定时的线程（`schedule`）；没调时这些消息都回 `Error`。
//!
//! 开始任务时在任务的目录（要工作树时先在仓库的 `.worktrees/` 下新开一个，分支 `runode/<名字>`）里开
//! 一个会话，等 shell 的提示符出来再打 `<agent> "$RUNODE_TODO_PROMPT"`（写法照 shell 来，见
//! `agent_command`）：提示词经环境变量交给 shell，不用照各家 shell 的规矩转义。任务会话不进宿主手里的布局，桌面在看板上打开它时再接进窗口。
//!
//! 只有拿着会话的宿主改看板：开始交出会话（`Peers::handoff`）或者要退出（`Peers::stop`）以后不改库、
//! 不开任务，定时也不触发；接手的新宿主提交后自己接着办。两边的库是同一个文件，SQLite 自己管得住两个
//! 进程同时开着。锁的先后：拿着 `Todos::state` 可以再拿 `peers` 和 `registry`，反过来不行。

mod schedule;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use runode_git::Repo;
use runode_protocol::{
    HostMsg, SessionId, SessionInfo,
    todo::{DONE_TODOS_PAGE, Todo, TodoSpec, When},
};
use runode_todo::Store;
use runode_types::shell::Shell;

use crate::{Host, Shared, server::REPLY_TIMEOUT, session::Inbox, workspaces::spawn_options};

/// 宿主给任务会话的 shell 设的环境变量：交给 agent 的提示词。
const ENV_TODO_PROMPT: &str = "RUNODE_TODO_PROMPT";

/// 宿主给新开了工作树的任务会话的 shell 设的环境变量：起 agent 之前先跑的命令（`runode_tasks::worktree_setup`）。
const ENV_TODO_SETUP: &str = "RUNODE_TODO_SETUP";

/// 宿主给 Windows 上 PowerShell 的任务会话设的环境变量：按 Windows 命令行的规矩引好的提示词
/// （`windows_quote`），原样放进起 agent 的命令行。
const ENV_TODO_ARG: &str = "RUNODE_TODO_ARG";

/// 宿主给 Windows 上 PowerShell 的任务会话设的另一份提示词，agent 是批处理文件（npm 装的 `.cmd`）时代替
/// `ENV_TODO_ARG` 用：按 cmd 的规矩引好（`batch_quote`）。
const ENV_TODO_BATCH_ARG: &str = "RUNODE_TODO_BATCH_ARG";

/// 提示词最长这么多字节：它要放进环境变量，系统给参数和环境变量的空间有限。
const MAX_PROMPT: usize = 64 * 1024;
/// Windows 上一条命令行最多这么多个 UTF-16 码元（`CreateProcess`），提示词要整个放进去。
const WINDOWS_COMMAND_LINE: usize = 32767;
/// cmd 读的一行最多这么多个字符，起批处理写的 agent（npm 装的 `.cmd`）时程序路径和提示词都算在里面。
const BATCH_COMMAND_LINE: usize = 8191;

/// 新交来的任务和定时的标题最长这么多字节（`check_title`）：看板每张卡片都带着它，列看板（`ListTodos`）
/// 一次回所有未完成的任务。
const MAX_TITLE: usize = 1024;

/// 分支名和工作树目录名里取自标题的部分最长这么多个字符。
const MAX_SLUG: usize = 40;

/// 给任务开工作树时最多拿这么多个没被占的名字去试，见 `add_worktree`。
const WORKTREE_ATTEMPTS: usize = 2;

/// 看板：库和定时的线程要的状态。
#[derive(Default)]
pub(crate) struct Todos {
    state: Mutex<State>,
    /// 看板变了时通知定时的线程，见 `State::generation`。
    changed: Condvar,
    /// 任务用的会话和任务做没做完，看板一变就重读，连同读的是第几次变（`State::generation`）之后的库。会话线程
    /// 拿它判断要不要报状态变了，见 `Shared::todo_session_changed`；单独一把锁，会话线程不等库。
    sessions: Mutex<(u64, HashMap<SessionId, bool>)>,
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

    /// 等到看板变了（`generation` 不再是 `seen`）或者过了 `timeout`。
    fn wait_for_change(&self, seen: u64, timeout: Duration) {
        let state = self.lock();
        let _ = self.changed.wait_timeout_while(state, timeout, |state| state.generation == seen);
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
        let peers = self.peers();
        if peers.handoff.is_some() || peers.stop.is_some() {
            return Err("the host is handing its sessions over; try again in a moment".into());
        }
        Ok(())
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
        let mut watched = self.todos.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        // 两个线程一起改看板时，先读的库可能后写回来：只留变得最晚的那次之后读到的。
        if watched.0 < generation {
            let sessions = todos.iter().filter_map(|todo| Some((todo.session?, todo.done_at.is_none())));
            *watched = (generation, sessions.collect());
        }
    }

    /// 会话 `id` 的 agent 状态、前台是不是 shell 变了，或者会话结束了（`ended`：shell 退出或者会话被收掉，
    /// 会话线程报的）：告诉问过 `ListTodos` 的连接，前端不用轮询会话的状态。没做完的任务的卡片可能换了列；
    /// 完成了的任务只在会话结束时报，卡片上打开终端的按钮要跟着收起，agent 的状态变了不影响它。
    pub(crate) fn todo_session_changed(&self, id: SessionId, ended: bool) {
        let open = self.todos.sessions.lock().unwrap_or_else(PoisonError::into_inner).1.get(&id).copied();
        if open.is_some_and(|open| open || ended) {
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

    /// 会话 `id` 现在的状态；没有这个会话、或者它没及时回话时为空。
    pub(crate) fn session_info(&self, id: SessionId) -> Option<SessionInfo> {
        let (reply, info) = mpsc::channel();
        if !self.deliver(id, Inbox::Info(reply)) {
            return None;
        }
        info.recv_timeout(REPLY_TIMEOUT).ok()
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
        todo.session.is_some_and(|session| self.running(session)) || self.todos_writable().is_err()
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
        let session = self.spawn_with(spawn_options(Some(cwd)), &[]).map_err(|err| format!("{err:#}"))?;
        if let Some(command) = command {
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
        let session = match self.spawn_with(spawn_options(Some(cwd)), &env) {
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

/// 在 `shell` 的提示符上打的命令：跑 `agent`，提示词取自环境变量 `ENV_TODO_PROMPT`、整个当一个参数。
/// `setup` 时先用 `sh -c` 跑环境变量 `ENV_TODO_SETUP` 里的命令，成功了才跑 agent。nushell、elvish 不在
/// 双引号里展开 `$名字`，取环境变量也各有写法，也没有 `&&`：外部命令失败时它们自己停下后面的命令。
/// PowerShell 用 `$env:名字`；Windows 上没有 sh，Windows PowerShell 也不认 `&&`，所以 Windows 上初始化命令由
/// PowerShell 自己跑（别处的 pwsh 照样交给 sh，`tasks.json` 的初始化命令按 sh 的写法写），出错或者退出码不为零时抛出异常，这一整行就停在那里，不跑 agent。Windows 上的
/// PowerShell 给原生程序拼命令行时不一定转义参数里的双引号（`passes_command_line`），提示词改从
/// `ENV_TODO_ARG` 取引好的一份，`--%` 后面的 `%名字%` 原样展开进命令行；agent 先找成程序文件
/// （`powershell_program`）。
fn agent_command(agent: &str, shell_program: &str, setup: bool) -> String {
    let shell = Shell::detect(shell_program);
    let var = |name: &str| match shell {
        Some(Shell::Nushell) => format!("$env.{name}"),
        Some(Shell::Elvish) => format!("$E:{name}"),
        Some(Shell::PowerShell) => format!("$env:{name}"),
        _ => format!("\"${name}\""),
    };
    let setup = match (setup, shell) {
        (false, _) => String::new(),
        (true, Some(Shell::PowerShell)) if cfg!(windows) => format!(
            "& {{ $ErrorActionPreference = 'Stop'; $global:LASTEXITCODE = 0; Invoke-Expression {}; \
             if ($global:LASTEXITCODE) {{ throw \"setup exited with $global:LASTEXITCODE\" }} }}; ",
            var(ENV_TODO_SETUP)
        ),
        (true, Some(Shell::Nushell | Shell::Elvish)) => format!("sh -c {}; ", var(ENV_TODO_SETUP)),
        (true, _) => format!("sh -c {} && ", var(ENV_TODO_SETUP)),
    };
    if passes_command_line(shell_program) {
        return format!("{setup}{} --% %{ENV_TODO_ARG}%\r", powershell_program(agent));
    }
    format!("{setup}{agent} {}\r", var(ENV_TODO_PROMPT))
}

/// `shell` 是 Windows 上的 PowerShell：Windows PowerShell 5.1 和 PowerShell 7.3 以前给原生程序拼命令行时
/// 不转义参数里的双引号，7.3 以后对 `.cmd` 这类批处理（npm 装的 agent 就是）也照旧，提示词里的双引号会被
/// agent 吞掉、结尾的反斜杠会吃掉收尾的引号。
fn passes_command_line(shell: &str) -> bool {
    cfg!(windows) && Shell::detect(shell) == Some(Shell::PowerShell)
}

/// PowerShell 里起 `agent` 的写法：开头的程序名只在 PATH 上找程序文件。只打名字的话 PowerShell 先跑 npm
/// 放在 `.cmd` 旁边的 `.ps1`，执行策略不让跑脚本时报错，`--%` 对脚本也不管用。找到的是批处理时提示词换成
/// `ENV_TODO_BATCH_ARG` 那份，放不进 cmd 的一行（`BATCH_COMMAND_LINE`）时报错；找不到时也报错，这一行停在
/// 那里。程序名带引号、空格这类字符时原样打。
fn powershell_program(agent: &str) -> String {
    let (program, args) = agent.split_once(' ').unwrap_or((agent, ""));
    if program.is_empty() || !program.chars().all(|ch| ch.is_alphanumeric() || "-_.:\\/".contains(ch)) {
        return agent.to_owned();
    }
    format!(
        "$__runode_agent = Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name '{program}'; \
         if ($__runode_agent.Extension -in '.bat', '.cmd') {{ \
         if ($__runode_agent.Path.Length + $env:{ENV_TODO_BATCH_ARG}.Length -ge {BATCH_COMMAND_LINE}) {{ \
         throw 'the prompt is too long for {program}, a batch file' }}; \
         $env:{ENV_TODO_ARG} = $env:{ENV_TODO_BATCH_ARG} }}; \
         & $__runode_agent.Path{}{args}",
        if args.is_empty() { "" } else { " " }
    )
}

/// 按 Windows 命令行的规矩（`CommandLineToArgvW`、C 运行库）把 `arg` 引成一个参数：整个包进双引号，
/// 双引号和紧挨着它、紧挨着收尾引号的反斜杠都转义。
fn windows_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(ch);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// 把 `arg` 引成批处理文件收得到的一个参数：cmd 先读一遍命令行，双引号里的 `&`、`|` 才不分隔命令，所以
/// 双引号写成两个（C 运行库在引号里把它读成一个），紧挨着它的反斜杠照 `windows_quote` 加倍；`%` 后面插一段
/// 展开为空的 `%cd:~,%`，cmd 就不把 `%名字%` 换成环境变量。cmd 读到换行就不往下读了，换行换成空格。
fn batch_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;
    for ch in arg.replace("\r\n", " ").chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                quoted.push(ch);
                continue;
            }
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push_str("\"\"");
            }
            '%' => quoted.push_str("%%cd:~,%"),
            '\r' | '\n' => quoted.push(' '),
            _ => quoted.push(ch),
        }
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes));
    quoted.push('"');
    quoted
}

/// 任务要跑的 agent 命令，带上免确认和自定义 agent 的参数（`runode_agent::models::with_options`）。
fn agent_with_options(spec: &TodoSpec) -> String {
    runode_agent::models::with_options(&spec.agent, spec.auto, spec.subagent.as_deref())
}

/// 任务这次开始时给 agent 的对话号，续接时由同样的任务标识和开始时刻再算出来，不用另存。写成 UUID 的样子，
/// Claude 只认 UUID；每次重新开始的时刻不同，号也不同，不撞上一次的对话。
fn agent_session(todo: &str, started: u64) -> String {
    format!("{todo}-0000-4000-8000-{started:012x}")
}

fn no_todo(id: &str) -> String {
    format!("no todo {id}")
}

/// 前端给的任务内容能不能用：本机任何进程和配对的手机都能发这些消息，见模块文档。
fn check_spec(spec: &TodoSpec) -> Result<(), String> {
    check_text(spec)?;
    if !spec.dir.is_absolute() || !spec.dir.is_dir() {
        return Err(format!("{} is not a directory", spec.dir.display()));
    }
    if spec.worktree && Repo::open(&spec.dir).is_none() {
        return Err(format!("{} is not in a git repository, so it cannot have a worktree", spec.dir.display()));
    }
    Ok(())
}

/// 前端新交来的标题不能太长。只查新交来的：开始任务时重查（`start_todo`）不查它。
fn check_title(spec: &TodoSpec) -> Result<(), String> {
    if spec.title.len() > MAX_TITLE {
        return Err(format!("the title is longer than {} KiB; put the details in the prompt", MAX_TITLE / 1024));
    }
    Ok(())
}

/// `check_spec` 里不看目录的部分：标题、提示词、agent、skill 和附件。附件只查是不是绝对路径，文件可以
/// 后来才有。
fn check_text(spec: &TodoSpec) -> Result<(), String> {
    if spec.title.trim().is_empty() {
        return Err("a todo needs a title".into());
    }
    let one_line = |name: &str| !name.trim().is_empty() && !name.chars().any(char::is_control);
    if !spec.skills.iter().all(|skill| one_line(skill)) {
        return Err("each skill must be a one-line name".into());
    }
    // 名字要原样拼进 shell 命令，只收不用加引号的。
    if spec.subagent.as_deref().is_some_and(|name| !runode_agent::models::subagent_name(name)) {
        return Err("the subagent name may only contain letters, digits and -_.:".into());
    }
    if let Some(path) = spec.attachments.iter().find(|path| !path.is_absolute()) {
        return Err(format!("the attachment {} is not an absolute path", path.display()));
    }
    let prompt = spec.instructions();
    if prompt.len() > MAX_PROMPT {
        return Err(format!("the prompt is longer than {} KiB", MAX_PROMPT / 1024));
    }
    if prompt.contains('\0') {
        return Err("the prompt contains a NUL character".into());
    }
    if spec.agent.trim().is_empty() || spec.agent.chars().any(char::is_control) {
        return Err("the agent must be a one-line command".into());
    }
    Ok(())
}

fn check_when(when: &When, now: u64) -> Result<(), String> {
    match *when {
        When::Once { at } if at <= now => Err("that time has passed".into()),
        When::Daily { hour, minute, days } if hour >= 24 || minute >= 60 || days >= 1 << 7 => {
            Err("not a valid time of day".into())
        }
        When::Every { minutes: 0 } => Err("the interval must be at least a minute".into()),
        When::Unknown => Err("unknown kind of schedule".into()),
        _ => Ok(()),
    }
}

/// 给任务新开工作树：仓库的 `.worktrees/<名字>`，分支 `runode/<名字>`，名字取自标题；已经有了时名字后面
/// 加上任务的标识，还有的话再加上序号。占掉了的（见 `taken`）直接跳过，不用 git 去试；
/// 真去试的最多 `WORKTREE_ATTEMPTS` 个。返回工作树的根目录和分支。
fn add_worktree(todo: &Todo) -> Result<(PathBuf, String), String> {
    let repo =
        Repo::open(&todo.spec.dir).ok_or_else(|| format!("{} is not in a git repository", todo.spec.dir.display()))?;
    let slug = slug(&todo.spec.title).unwrap_or_else(|| todo.id.clone());
    let worktrees = repo.root.join(".worktrees");
    let branches: HashSet<String> =
        repo.branches().into_iter().filter(|branch| !branch.remote).map(|branch| branch.name).collect();
    let names = [slug.clone(), format!("{slug}-{}", todo.id)]
        .into_iter()
        .chain((2..).map(|n| format!("{slug}-{}-{n}", todo.id)));
    // 跳过的名字不是分支已经有了（同名或者上下一级，一个分支最多占一个候选；叫 `runode` 的分支占掉全部，
    // 那样本来就开不出来）、就是 `.worktrees` 下已经有了这一项，最多这么多个；只算分支的话，只占了目录的几个
    // 名字就把候选用完了，一次都没去试。
    let occupied = std::fs::read_dir(&worktrees).map_or(0, Iterator::count);
    let mut last = String::new();
    let mut attempts = 0;
    for name in names.take(branches.len() + occupied + WORKTREE_ATTEMPTS) {
        let (root, branch) = (worktrees.join(&name), format!("runode/{name}"));
        if taken(&root, &branch, &branches) {
            last = format!("{branch} or {} is taken", root.display());
            continue;
        }
        match repo.add_worktree(&root, &branch) {
            Ok(()) => return Ok((root, branch)),
            Err(err) => last = err.to_string(),
        }
        attempts += 1;
        if attempts == WORKTREE_ATTEMPTS {
            break;
        }
    }
    Err(format!("cannot make a worktree for todo {}: {last}", todo.id))
}

/// 工作树 `root` 或者分支 `branch` 已经被占了，git 开不出来。`root` 上有什么都算，悬空的符号链接也算（git
/// 不跟着链接看）。分支和已有的分支（`branches`）同名，或者一个是另一个往下的一级（`runode/a` 和
/// `runode/a/b`）都算：git 的分支名也是路径，两个不能并存。
fn taken(root: &Path, branch: &str, branches: &HashSet<String>) -> bool {
    root.symlink_metadata().is_ok()
        || branches.iter().any(|other| {
            other == branch
                || other.strip_prefix(branch).is_some_and(|rest| rest.starts_with('/'))
                || branch.strip_prefix(other.as_str()).is_some_and(|rest| rest.starts_with('/'))
        })
}

/// 删掉任务的工作树（有改动时 git 拒绝，留着）和分支（只删已经合并、或者没有新提交的），返回各删掉了
/// 没有。不会丢东西：没提交的改动留在工作树里，没合并的提交留在分支上。
fn clean_worktree(todo: &Todo) -> (bool, bool) {
    let Some(worktree) = &todo.worktree_dir else { return (false, false) };
    let Some(repo) = Repo::open(&todo.spec.dir) else { return (false, false) };
    let worktree_gone = match repo.remove_worktree(worktree, false) {
        Ok(()) => true,
        Err(err) => {
            tracing::info!("kept the worktree {} of todo {}: {err}", worktree.display(), todo.id);
            !worktree.exists()
        }
    };
    let branch_gone = worktree_gone
        && todo.branch.as_deref().is_some_and(|branch| match repo.delete_merged_branch(branch) {
            Ok(()) => true,
            Err(err) => {
                tracing::info!("kept the branch {branch} of todo {}: {err}", todo.id);
                false
            }
        });
    (worktree_gone, branch_gone)
}

/// `dir` 在工作树 `root` 里对应的目录：任务的目录在仓库的子目录里时，工作树里也进到同一个子目录。
fn in_worktree(root: &Path, dir: &Path) -> PathBuf {
    let relative = Repo::open(dir).and_then(|repo| dir.strip_prefix(&repo.root).ok().map(Path::to_path_buf));
    let wanted = root.join(relative.unwrap_or_default());
    if wanted.is_dir() { wanted } else { root.to_path_buf() }
}

/// 标题里的 ASCII 字母和数字转成小写，其余的连成一个 `-`，最多 `MAX_SLUG` 个字符；什么都不剩时为空。
fn slug(title: &str) -> Option<String> {
    let mut slug = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= MAX_SLUG {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_sessions_look_like_uuids() {
        assert_eq!(agent_session("0a1b2c3d", 1_760_000_000), "0a1b2c3d-0000-4000-8000-000068e77800");
    }

    #[test]
    fn agent_commands_read_the_prompt_the_way_each_shell_does() {
        assert_eq!(agent_command("claude", "/bin/zsh", false), "claude \"$RUNODE_TODO_PROMPT\"\r");
        assert_eq!(agent_command("claude", "/opt/homebrew/bin/fish", false), "claude \"$RUNODE_TODO_PROMPT\"\r");
        assert_eq!(agent_command("codex --full-auto", "-nu", false), "codex --full-auto $env.RUNODE_TODO_PROMPT\r");
        assert_eq!(agent_command("claude", "/usr/local/bin/elvish", false), "claude $E:RUNODE_TODO_PROMPT\r");
        assert_eq!(
            agent_command("claude", "/bin/zsh", true),
            "sh -c \"$RUNODE_TODO_SETUP\" && claude \"$RUNODE_TODO_PROMPT\"\r"
        );
        assert_eq!(
            agent_command("claude", "/usr/bin/nu", true),
            "sh -c $env.RUNODE_TODO_SETUP; claude $env.RUNODE_TODO_PROMPT\r"
        );
        let claude = if cfg!(windows) {
            "$__runode_agent = Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name 'claude'; \
             if ($__runode_agent.Extension -in '.bat', '.cmd') { \
             if ($__runode_agent.Path.Length + $env:RUNODE_TODO_BATCH_ARG.Length -ge 8191) { \
             throw 'the prompt is too long for claude, a batch file' }; \
             $env:RUNODE_TODO_ARG = $env:RUNODE_TODO_BATCH_ARG }; \
             & $__runode_agent.Path --% %RUNODE_TODO_ARG%"
        } else {
            "claude $env:RUNODE_TODO_PROMPT"
        };
        assert_eq!(agent_command("claude", "pwsh.exe", false), format!("{claude}\r"));
        let setup = if cfg!(windows) {
            "& { $ErrorActionPreference = 'Stop'; $global:LASTEXITCODE = 0; Invoke-Expression $env:RUNODE_TODO_SETUP; \
             if ($global:LASTEXITCODE) { throw \"setup exited with $global:LASTEXITCODE\" } }; "
        } else {
            "sh -c $env:RUNODE_TODO_SETUP && "
        };
        assert_eq!(agent_command("claude", "powershell.exe", true), format!("{setup}{claude}\r"));
    }

    #[test]
    fn powershell_runs_the_agent_program_file() {
        assert_eq!(
            powershell_program("codex --full-auto"),
            "$__runode_agent = Get-Command -ErrorAction Stop -CommandType Application -TotalCount 1 -Name 'codex'; \
             if ($__runode_agent.Extension -in '.bat', '.cmd') { \
             if ($__runode_agent.Path.Length + $env:RUNODE_TODO_BATCH_ARG.Length -ge 8191) { \
             throw 'the prompt is too long for codex, a batch file' }; \
             $env:RUNODE_TODO_ARG = $env:RUNODE_TODO_BATCH_ARG }; \
             & $__runode_agent.Path --full-auto"
        );
        assert_eq!(powershell_program("'C:\\my tools\\agent.exe' -x"), "'C:\\my tools\\agent.exe' -x");
    }

    #[test]
    fn batch_quotes_keep_cmd_out_of_the_prompt() {
        assert_eq!(batch_quote("plain"), "\"plain\"");
        assert_eq!(batch_quote(r#"a" & echo hi & "b"#), r#""a"" & echo hi & ""b""#);
        assert_eq!(batch_quote(r#"a\"b"#), r#""a\\""b""#);
        assert_eq!(batch_quote(r#"C:\dir\"#), r#""C:\dir\\""#);
        assert_eq!(batch_quote("100% %PATH%"), r#""100%%cd:~,% %%cd:~,%PATH%%cd:~,%""#);
        assert_eq!(batch_quote("one\r\ntwo\nthree"), "\"one two three\"");
    }

    #[test]
    fn windows_quotes_survive_the_command_line_rules() {
        assert_eq!(windows_quote("plain"), "\"plain\"");
        assert_eq!(windows_quote(r#"say "hi" \ now"#), r#""say \"hi\" \ now""#);
        assert_eq!(windows_quote(r#"C:\dir\"#), r#""C:\dir\\""#);
        assert_eq!(windows_quote(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(windows_quote(""), "\"\"");
    }

    #[test]
    fn slugs_keep_ascii_words() {
        assert_eq!(slug("Fix the login redirect!").as_deref(), Some("fix-the-login-redirect"));
        assert_eq!(slug("  修复 Login 页  ").as_deref(), Some("login"));
        assert_eq!(slug("修复登录"), None);
        assert_eq!(slug(&"a".repeat(100)).map(|slug| slug.len()), Some(MAX_SLUG));
        assert_eq!(slug("ab-------------------------------------cd x").as_deref(), Some("ab-cd-x"));
    }

    /// 候选的工作树目录上有悬空的符号链接、候选分支和已有的分支一个在另一个下面时都算占了：git 在那开不出来，
    /// 不跳过的话白白用掉一次尝试。
    #[cfg(unix)]
    #[test]
    fn names_git_would_refuse_are_taken() {
        let dir = std::env::temp_dir().join(format!("rnh-unit-{}-todo-taken", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dangling = dir.join("dangling");
        std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).unwrap();
        let free = dir.join("free");
        let branches: HashSet<String> =
            ["main", "runode/a/old", "runode/b", "runode/c-d"].into_iter().map(String::from).collect();

        assert!(taken(&dangling, "runode/x", &branches));
        assert!(!taken(&free, "runode/x", &branches));
        assert!(taken(&free, "runode/b", &branches));
        // 已有的分支在它下面、它在已有的分支下面。
        assert!(taken(&free, "runode/a", &branches));
        assert!(taken(&free, "runode/b/new", &branches));
        // 只是名字开头一样的不冲突。
        assert!(!taken(&free, "runode/c", &branches));
        assert!(!taken(&free, "runode/a-2", &branches));
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn checks_the_spec_and_the_schedule() {
        let dir = std::env::temp_dir();
        let spec = |title: &str, agent: &str| TodoSpec {
            title: title.into(),
            prompt: String::new(),
            dir: dir.clone(),
            agent: agent.into(),
            worktree: false,
            ..TodoSpec::default()
        };
        assert!(check_spec(&spec("t", "claude")).is_ok());
        assert!(check_spec(&spec("  ", "claude")).is_err());
        assert!(check_spec(&spec("t", "claude\nrm -rf ~")).is_err());
        assert!(check_spec(&TodoSpec { dir: "relative".into(), ..spec("t", "claude") }).is_err());
        assert!(check_spec(&TodoSpec { prompt: "x".repeat(MAX_PROMPT + 1), ..spec("t", "claude") }).is_err());
        let named = |skills: &[&str], subagent: Option<&str>| TodoSpec {
            skills: skills.iter().map(|skill| (*skill).to_owned()).collect(),
            subagent: subagent.map(str::to_owned),
            ..spec("t", "claude")
        };
        assert!(check_spec(&named(&["review"], Some("reviewer"))).is_ok());
        assert!(check_spec(&named(&["review\nrm -rf ~"], None)).is_err());
        assert!(check_spec(&named(&[" "], None)).is_err());
        assert!(check_spec(&named(&[], Some("a\nb"))).is_err());
        assert!(check_spec(&named(&[], Some("x'; touch /tmp/pwn; #"))).is_err());
        assert!(check_spec(&TodoSpec { attachments: vec!["notes.md".into()], ..spec("t", "claude") }).is_err());
        // 附上的文件列进提示词，算进长度。
        let long = TodoSpec { attachments: vec![format!("/{}", "x".repeat(MAX_PROMPT)).into()], ..spec("t", "claude") };
        assert!(check_spec(&long).is_err());
        assert!(check_title(&spec(&"x".repeat(MAX_TITLE), "claude")).is_ok());
        assert!(check_title(&spec(&"x".repeat(MAX_TITLE + 1), "claude")).is_err());
        // 开始任务时的重查不管标题长度。
        assert!(check_spec(&spec(&"x".repeat(MAX_TITLE + 1), "claude")).is_ok());

        assert!(check_when(&When::Once { at: 101 }, 100).is_ok());
        assert!(check_when(&When::Once { at: 100 }, 100).is_err());
        assert!(check_when(&When::Daily { hour: 23, minute: 59, days: 0b111_1111 }, 0).is_ok());
        assert!(check_when(&When::Daily { hour: 24, minute: 0, days: 0 }, 0).is_err());
        assert!(check_when(&When::Daily { hour: 9, minute: 0, days: 0b1000_0000 }, 0).is_err());
        assert!(check_when(&When::Every { minutes: 0 }, 0).is_err());
        assert!(check_when(&When::Unknown, 0).is_err());
    }
}
