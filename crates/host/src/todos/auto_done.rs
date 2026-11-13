//! 定时的线程收尾任务：带 `TodoSpec::auto_done` 的做完后标完成（`Shared::finish_idle_todos`），卡住的记成
//! 没做成（`Shared::fail_stuck_todos`）。

use std::{
    collections::HashMap,
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_protocol::{
    SessionId,
    todo::{AUTO_DONE_WITHIN, BLOCKED_GIVE_UP, Column, Outcome, RESULT_LIMIT, Todo, column, finished_by_agent},
};
use runode_types::agent::AgentState;

use super::{schedule::MAX_SLEEP, unix_now};
use crate::{Shared, session::Inbox};

/// agent 空闲下来要稳定这么久才算做完：干活的途中（比如两次调工具之间）它可能一时显得空闲。
const SETTLE: Duration = Duration::from_secs(10);

/// 没报结果的任务结束终端前读屏幕底部的这么多行。
const SCREEN_LINES: u32 = 40;

/// 读屏幕最多等这么久：定时的线程不能卡在一个不回话的会话上。
const SCREEN_WAIT: Duration = Duration::from_secs(1);

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
            match state(session) {
                Some(AgentState::Working) => {}
                Some(AgentState::Blocked) => {
                    self.todos.watched().blocked.entry(session).or_insert_with(Instant::now);
                    continue;
                }
                _ => continue,
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
            // 报了没做成的留给用户，这次干活不再看。
            if todo.outcome == Some(Outcome::Failed) {
                self.todos.watched().worked.remove(&session);
                continue;
            }
            let info = self.session_info(session);
            if !finished_by_agent(&todo, info.as_ref(), unix_now()) {
                continue;
            }
            let ending = info.as_ref().is_some_and(|info| info.clients == 0);
            // agent 没报结果、终端又要结束时，留下屏幕底部，看得出它最后说了什么。
            if ending
                && todo.result.is_none()
                && let Some(text) = self.screen_tail(session, RESULT_LIMIT)
                && let Err(err) = self.todos.with_db(|db| db.set_outcome(&todo.id, None, Some(&text)))
            {
                tracing::warn!("cannot keep the screen of todo {}: {err}", todo.id);
            }
            if let Err(err) = self.todos.with_db(|db| db.update_todo(&todo.id, None, Some(Some(unix_now())))) {
                tracing::warn!("cannot mark todo {} done: {err}", todo.id);
                continue;
            }
            self.todos.watched().worked.remove(&session);
            tracing::info!("todo {} finished by its agent", todo.id);
            // 没人在看的终端结束掉，留着只占着空闲的 agent，工作树也要等它结束才清；开在窗口里的留给用户。
            if ending {
                self.end_unviewed(session);
            }
            drop(claim);
            self.todos_changed();
        }
        next
    }

    /// 记成没做成：agent 还在干活或者在问、跑过了 `TodoSpec::time_limit` 的任务，和自动完成的任务里停在询问上超过
    /// `BLOCKED_GIVE_UP` 的。
    /// 结果写上原因和屏幕底部，没有窗口开着它的终端时结束终端。只管还没报结局、会话还在跑的；不归这个宿主办时
    /// 什么都不做。
    pub(super) fn fail_stuck_todos(&self) {
        if self.todos_writable().is_err() {
            return;
        }
        let (asking, begun): (Vec<SessionId>, HashMap<SessionId, u64>) = {
            let watched = self.todos.watched();
            let limit = Duration::from_secs(BLOCKED_GIVE_UP);
            let asking =
                watched.blocked.iter().filter(|(_, at)| at.elapsed() >= limit).map(|(&session, _)| session).collect();
            (asking, watched.begun.clone())
        };
        let todos = match self.todos.with_db(|db| db.list_todos(unix_now())) {
            Ok(todos) => todos,
            Err(err) => {
                tracing::warn!("cannot read the todos: {err}");
                return;
            }
        };
        let why = |todo: &Todo, now: u64| -> Option<String> {
            let (Some(started), Some(session), None, None) = (todo.started, todo.session, todo.done_at, todo.outcome)
            else {
                return None;
            };
            // 时限只管 agent 还在干活或者在问的：做完了停下来等人看的不算超时。接回的会话从接回时算起。
            // 还没记上这个会话时这一轮不判：可能是刚接回的，不能拿第一次开始的时刻算。
            let since = begun.get(&session).map(|&begun| begun.max(started));
            let busy = || {
                let info = self.session_info(session);
                column(todo, info.as_ref(), now) == Column::InProgress
                    || info.and_then(|info| info.meta.agent).is_some_and(|agent| agent.state == AgentState::Blocked)
            };
            if let (Some(minutes), Some(since)) = (todo.spec.time_limit, since)
                && now >= since.saturating_add(u64::from(minutes) * 60)
                && busy()
            {
                return Some(format!("Runode stopped it: it ran past its time limit of {minutes} minutes."));
            }
            asking.contains(&session).then(|| {
                format!("Runode stopped it: it waited for an answer for over {} minutes.", BLOCKED_GIVE_UP / 60)
            })
        };
        for todo in todos.iter().filter(|todo| why(todo, unix_now()).is_some()) {
            let Some(claim) = self.todos.claim(&todo.id) else { continue };
            let todo = match self.todos.with_db(|db| db.todo_by_id(&todo.id)) {
                Ok(Some(todo)) => todo,
                Ok(None) => continue,
                Err(err) => {
                    tracing::warn!("cannot read todo {}: {err}", todo.id);
                    continue;
                }
            };
            let (Some(reason), Some(session)) = (why(&todo, unix_now()), todo.session) else { continue };
            if !self.running(session) {
                continue;
            }
            let mut result = reason;
            if let Some(screen) = self.screen_tail(session, RESULT_LIMIT.saturating_sub(result.chars().count() + 2)) {
                result = format!("{result}\n\n{screen}");
            }
            if let Err(err) = self.todos.with_db(|db| db.set_outcome(&todo.id, Some(Outcome::Failed), Some(&result))) {
                tracing::warn!("cannot record that todo {} is stuck: {err}", todo.id);
                continue;
            }
            tracing::info!("todo {} stopped: {}", todo.id, result.lines().next().unwrap_or_default());
            {
                let mut watched = self.todos.watched();
                watched.blocked.remove(&session);
                watched.worked.remove(&session);
            }
            if self.session_info(session).is_some_and(|info| info.clients == 0) {
                self.end_unviewed(session);
            }
            drop(claim);
            self.todos_changed();
        }
    }

    /// 结束没人在看的任务会话。先记下再结束：会话线程报结束时要找得到它。
    fn end_unviewed(&self, session: SessionId) {
        self.todos.watched().ending.insert(session, None);
        if !self.kill(session) {
            self.todos.watched().ending.remove(&session);
        }
    }

    /// 会话屏幕底部最后 `SCREEN_LINES` 行的文字，去掉空行，最多留最后 `limit` 个字；读不到、没回话时为空。
    fn screen_tail(&self, session: SessionId, limit: usize) -> Option<String> {
        let (reply, answer) = mpsc::channel();
        if !self.deliver(session, Inbox::ReadScreen { lines: Some(SCREEN_LINES), command: None, reply }) {
            return None;
        }
        let (text, _) = answer.recv_timeout(SCREEN_WAIT).ok()?.ok()?;
        let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()).collect();
        let text = lines.join("\n");
        // 太长时留后面的：agent 最后说的在最下面。
        let skip = text.chars().count().saturating_sub(limit);
        let text: String = text.chars().skip(skip).collect();
        (!text.is_empty()).then_some(text)
    }
}
