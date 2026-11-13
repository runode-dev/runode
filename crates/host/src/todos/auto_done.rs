//! 带 `TodoSpec::auto_done` 的任务做完后由定时的线程标完成（`Shared::finish_idle_todos`）。

use std::time::{Duration, Instant};

use runode_protocol::todo::{AUTO_DONE_WITHIN, finished_by_agent};
use runode_types::agent::AgentState;

use super::{schedule::MAX_SLEEP, unix_now};
use crate::Shared;

/// agent 空闲下来要稳定这么久才算做完：干活的途中（比如两次调工具之间）它可能一时显得空闲。
const SETTLE: Duration = Duration::from_secs(10);

// 稳定下来以后最多再睡一轮才看到：两段加起来要在说好的时限内。
const _: () = assert!(SETTLE.as_secs() + MAX_SLEEP.as_secs() <= AUTO_DONE_WITHIN);

impl Shared {
    /// 标完成做完了的自动完成任务，没有窗口开着它的会话时一并结束会话；返回还在等 `SETTLE` 的里最早该再看的
    /// 时刻。不归这个宿主办时什么都不做。
    pub(super) fn finish_idle_todos(&self) -> Option<Instant> {
        if self.todos_writable().is_err() {
            return None;
        }
        let unchecked = std::mem::take(&mut self.todos.watched().unchecked);
        let state = |session| self.session_info(session).and_then(|info| info.meta.agent).map(|agent| agent.state);
        for session in unchecked {
            if state(session) != Some(AgentState::Working) {
                continue;
            }
            self.todos.watched().worked.entry(session).or_insert(None);
            // 问过以后、记下之前会话线程报的空闲没记上：再问一次补上。
            if state(session) == Some(AgentState::Idle)
                && let Some(since @ None) = self.todos.watched().worked.get_mut(&session)
            {
                *since = Some(Instant::now());
            }
        }
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        // 先取好再放掉锁，不和库锁同时拿。
        let settled: Vec<_> = {
            let watched = self.todos.watched();
            watched
                .worked
                .iter()
                .filter_map(|(&session, &since)| {
                    let at = since? + SETTLE;
                    if at > now {
                        next = next.into_iter().chain([at]).min();
                        return None;
                    }
                    Some(session)
                })
                .collect()
        };
        if settled.is_empty() {
            return next;
        }
        let todos = match self.todos.with_db(|db| db.list_todos(unix_now())) {
            Ok(todos) => todos,
            Err(err) => {
                tracing::warn!("cannot read the todos: {err}");
                return next;
            }
        };
        for todo in todos.iter().filter(|todo| todo.session.is_some_and(|session| settled.contains(&session))) {
            // 正在开始、清工作树或者用户正在改的下一轮再看；占住以后重读，读出列表以后它可能被改过。
            let Some(claim) = self.todos.claim(&todo.id) else { continue };
            let todo = match self.todos.with_db(|db| db.todo_by_id(&todo.id)) {
                Ok(Some(todo)) => todo,
                Ok(None) => continue,
                Err(err) => {
                    tracing::warn!("cannot read todo {}: {err}", todo.id);
                    continue;
                }
            };
            let Some(session) = todo.session else { continue };
            let info = self.session_info(session);
            if !finished_by_agent(&todo, info.as_ref(), unix_now()) {
                continue;
            }
            if let Err(err) = self.todos.with_db(|db| db.update_todo(&todo.id, None, Some(Some(unix_now())))) {
                tracing::warn!("cannot mark todo {} done: {err}", todo.id);
                continue;
            }
            self.todos.watched().worked.remove(&session);
            tracing::info!("todo {} finished by its agent", todo.id);
            // 没人在看的终端结束掉，留着只占着空闲的 agent，工作树也要等它结束才清；开在窗口里的留给用户。
            // 先记下再结束：会话线程报结束时要找得到它。
            if info.is_some_and(|info| info.clients == 0) {
                self.todos.watched().ending.insert(session, None);
                if !self.kill(session) {
                    self.todos.watched().ending.remove(&session);
                }
            }
            drop(claim);
            self.todos_changed();
        }
        next
    }
}
