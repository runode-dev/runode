//! 任务板（见 `runode_protocol::todo`）：任务和定时存在宿主自己的 SQLite 库里（`runode_todo::Store`），
//! 前端经 `ClientMsg::ListTodos` 这几条消息要。app 在宿主拿到会话和监听的 socket 以后调
//! `Host::serve_todos` 告诉它库在哪、起定时的线程（`schedule`）；之前这些消息都回 `Error`。
//!
//! 开始任务时在任务的目录（要工作树时先在仓库的 `.worktrees/` 下新开一个，分支 `runode/<名字>`）里开
//! 一个会话，等 shell 的提示符出来再打 `<agent> "$RUNODE_TODO_PROMPT"`：提示词经环境变量交给 shell，
//! 不用照各家 shell 的规矩转义。任务会话不进宿主手里的布局，桌面在任务板上打开它时再接进窗口。
//!
//! 只有拿着会话的宿主改任务板：开始交出会话（`Peers::handoff`）或者要退出（`Peers::stop`）以后不改库、
//! 不开任务，定时也不触发；接手的新宿主提交后自己接着办。两边的库是同一个文件，SQLite 自己管得住两个
//! 进程同时开着。锁的先后：拿着 `Todos::state` 可以再拿 `peers` 和 `registry`，反过来不行。

mod schedule;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use runode_git::Repo;
use runode_protocol::{
    HostMsg, SessionId, SessionInfo,
    todo::{Todo, TodoSpec, When},
};
use runode_todo::Store;

use crate::{Host, Shared, server::REPLY_TIMEOUT, session::Inbox, workspaces::spawn_options};

/// 宿主给任务会话的 shell 设的环境变量：交给 agent 的提示词。
const ENV_TODO_PROMPT: &str = "RUNODE_TODO_PROMPT";

/// 提示词最长这么多字节：它要放进环境变量，系统给参数和环境变量的空间有限。
const MAX_PROMPT: usize = 64 * 1024;

/// 分支名和工作树目录名里取自标题的部分最长这么多个字符。
const MAX_SLUG: usize = 40;

/// 任务板：库和定时的线程要的状态。
#[derive(Default)]
pub(crate) struct Todos {
    state: Mutex<State>,
    /// 任务板变了时通知定时的线程，见 `State::generation`。
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// 库的路径，`Host::serve_todos` 给的；为空时任务板没开。
    path: Option<PathBuf>,
    /// 打开着的库；打不开时为空，下次用时再试，用户修好文件后自己恢复。
    db: Option<Store>,
    /// 任务板变一次加一，定时的线程据此知道要重新看。
    generation: u64,
    /// 正在开始的任务，同一个不会同时开两次。
    starting: HashSet<String>,
}

impl Todos {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// 等到任务板变了（`generation` 不再是 `seen`）或者过了 `timeout`。
    fn wait_for_change(&self, seen: u64, timeout: Duration) {
        let state = self.lock();
        let _ = self.changed.wait_timeout_while(state, timeout, |state| state.generation == seen);
    }

    /// 在库上做 `f`。任务板没开、库打不开时报错；`f` 出错时关掉库，下次重新打开。
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
    /// 打开任务板：库在 `db`（没有时建一个），起定时的线程。在宿主拿到会话和监听的 socket 以后调：
    /// `Host::listen` 之后，或者 `Host::take_over` 接手成功以后，只有一个宿主办定时。再调一次什么都不做。
    pub fn serve_todos(&self, db: PathBuf) {
        {
            let mut state = self.shared.todos.lock();
            if state.path.is_some() {
                return;
            }
            state.path = Some(db);
        }
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
    /// 能不能改任务板：正在交出会话或者要退出了的宿主不改，见模块文档。
    fn todos_writable(&self) -> Result<(), String> {
        let peers = self.peers();
        if peers.handoff.is_some() || peers.stop.is_some() {
            return Err("the host is handing its sessions over; try again in a moment".into());
        }
        Ok(())
    }

    /// 任务板变了：叫醒定时的线程，告诉问过 `ListTodos` 的连接。
    fn todos_changed(&self) {
        self.todos.lock().generation += 1;
        self.todos.changed.notify_all();
        self.peers().todos_changed();
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
    fn session_info(&self, id: SessionId) -> Option<SessionInfo> {
        let (reply, info) = mpsc::channel();
        if !self.deliver(id, Inbox::Info(reply)) {
            return None;
        }
        info.recv_timeout(REPLY_TIMEOUT).ok()
    }

    /// 回 `ClientMsg::ListTodos`。
    pub(crate) fn list_todos(&self, req: u32) -> Result<HostMsg, String> {
        let now = unix_now();
        let (todos, mut schedules) = self.todos.with_db(|db| Ok((db.list_todos(now)?, db.list_schedules()?)))?;
        for schedule in &mut schedules {
            schedule.next_fire = runode_todo::next_fire(&schedule.when, schedule.last_fired);
        }
        Ok(HostMsg::Todos { req, todos, schedules })
    }

    /// 回 `ClientMsg::AddTodo`。
    pub(crate) fn add_todo(&self, req: u32, spec: &TodoSpec) -> Result<HostMsg, String> {
        self.todos_writable()?;
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
        if let Some(spec) = spec {
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
        self.todos_changed();
        Ok(HostMsg::Done { req })
    }

    /// 回 `ClientMsg::RemoveTodo`：它的会话已经不在时，顺带清掉工作树和分支，见 `clean_worktree`。
    pub(crate) fn remove_todo(&self, req: u32, id: &str) -> Result<HostMsg, String> {
        self.todos_writable()?;
        let id = &self.resolve_todo(id)?;
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

    /// 任务的会话还在跑，见 `running`。
    fn in_use(&self, todo: &Todo) -> bool {
        todo.session.is_some_and(|session| self.running(session))
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
        let todos = match self.todos.with_db(|db| db.list_todos(unix_now())) {
            Ok(todos) => todos,
            Err(err) => {
                tracing::warn!("cannot read the todos: {err}");
                return;
            }
        };
        for todo in todos.iter().filter(|todo| todo.done_at.is_some()) {
            if let Some(session) = todo.session {
                self.end_if_exited(session);
            }
        }
    }

    /// 清掉完成了的任务的工作树和分支（见 `clean_worktree`），每张只试一次；会话还在的等它结束后再清。
    /// 定时的线程每一轮都做，任务板一变就有一轮，所以标完成后很快就清。
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
        for todo in done.iter().filter(|todo| !self.in_use(todo)) {
            let (worktree_gone, branch_gone) = clean_worktree(todo);
            if let Err(err) = self.todos.with_db(|db| db.set_swept(&todo.id, worktree_gone, branch_gone)) {
                tracing::warn!("cannot record that todo {} was cleaned up: {err}", todo.id);
            }
        }
    }

    /// 回 `ClientMsg::GetTodo`。
    pub(crate) fn get_todo(&self, req: u32, id: &str) -> Result<HostMsg, String> {
        let id = self.resolve_todo(id)?;
        let todo = self.todos.with_db(|db| db.todo_by_id(&id))?.ok_or_else(|| no_todo(&id))?;
        Ok(HostMsg::Todo { req, todo })
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
        spec.map_or(Ok(()), check_spec)?;
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
        if !self.todos.lock().starting.insert(id.to_owned()) {
            return Err(format!("todo {id} is already starting"));
        }
        let started = self.launch_todo(&todo);
        self.todos.lock().starting.remove(id);
        let session = started?;
        self.todos_changed();
        Ok(session)
    }

    /// 给任务开会话、在提示符上跑 agent，记进库里；要工作树又还没有时先开一个。
    fn launch_todo(&self, todo: &Todo) -> Result<SessionId, String> {
        let made = match &todo.worktree_dir {
            None if todo.spec.worktree => Some(add_worktree(todo)?),
            _ => None,
        };
        let cwd = match made.as_ref().map(|(root, _)| root).or(todo.worktree_dir.as_ref()) {
            Some(root) => in_worktree(root, &todo.spec.dir),
            None => todo.spec.dir.clone(),
        };
        let undo_worktree = || {
            if let Some((root, _)) = &made
                && let Some(repo) = Repo::open(&todo.spec.dir)
                && let Err(err) = repo.remove_worktree(root, false)
            {
                tracing::warn!("cannot remove the worktree {} made for todo {}: {err}", root.display(), todo.id);
            }
        };
        let env = [(ENV_TODO_PROMPT, todo.spec.prompt())];
        let session = match self.spawn_with(spawn_options(Some(cwd)), &env) {
            Ok(session) => session,
            Err(err) => {
                undo_worktree();
                return Err(format!("{err:#}"));
            }
        };
        let command = format!("{} \"${ENV_TODO_PROMPT}\"\r", todo.spec.agent);
        self.deliver(session, Inbox::TypeAtPrompt(command.into_bytes()));
        let worktree = made.as_ref().map(|(root, branch)| (root.as_path(), branch.as_str()));
        if let Err(err) = self.todos.with_db(|db| db.set_started(&todo.id, unix_now(), session, worktree)) {
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

/// `check_spec` 里不看目录的部分：标题、提示词和 agent。
fn check_text(spec: &TodoSpec) -> Result<(), String> {
    if spec.title.trim().is_empty() {
        return Err("a todo needs a title".into());
    }
    let prompt = spec.prompt();
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
/// 加上任务的标识再试一次。返回工作树的根目录和分支。
fn add_worktree(todo: &Todo) -> Result<(PathBuf, String), String> {
    let repo =
        Repo::open(&todo.spec.dir).ok_or_else(|| format!("{} is not in a git repository", todo.spec.dir.display()))?;
    let slug = slug(&todo.spec.title).unwrap_or_else(|| todo.id.clone());
    let worktrees = repo.root.join(".worktrees");
    let mut last = String::new();
    for name in [slug.clone(), format!("{slug}-{}", todo.id)] {
        let (root, branch) = (worktrees.join(&name), format!("runode/{name}"));
        match repo.add_worktree(&root, &branch) {
            Ok(()) => return Ok((root, branch)),
            Err(err) => last = err.to_string(),
        }
    }
    Err(format!("cannot make a worktree for todo {}: {last}", todo.id))
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
    fn slugs_keep_ascii_words() {
        assert_eq!(slug("Fix the login redirect!").as_deref(), Some("fix-the-login-redirect"));
        assert_eq!(slug("  修复 Login 页  ").as_deref(), Some("login"));
        assert_eq!(slug("修复登录"), None);
        assert_eq!(slug(&"a".repeat(100)).map(|slug| slug.len()), Some(MAX_SLUG));
        assert_eq!(slug("ab-------------------------------------cd x").as_deref(), Some("ab-cd-x"));
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
        };
        assert!(check_spec(&spec("t", "claude")).is_ok());
        assert!(check_spec(&spec("  ", "claude")).is_err());
        assert!(check_spec(&spec("t", "claude\nrm -rf ~")).is_err());
        assert!(check_spec(&TodoSpec { dir: "relative".into(), ..spec("t", "claude") }).is_err());
        assert!(check_spec(&TodoSpec { prompt: "x".repeat(MAX_PROMPT + 1), ..spec("t", "claude") }).is_err());

        assert!(check_when(&When::Once { at: 101 }, 100).is_ok());
        assert!(check_when(&When::Once { at: 100 }, 100).is_err());
        assert!(check_when(&When::Daily { hour: 23, minute: 59, days: 0b111_1111 }, 0).is_ok());
        assert!(check_when(&When::Daily { hour: 24, minute: 0, days: 0 }, 0).is_err());
        assert!(check_when(&When::Daily { hour: 9, minute: 0, days: 0b1000_0000 }, 0).is_err());
        assert!(check_when(&When::Every { minutes: 0 }, 0).is_err());
        assert!(check_when(&When::Unknown, 0).is_err());
    }
}
