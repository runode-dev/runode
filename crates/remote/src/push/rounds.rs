//! 什么时候推：按每两秒一次的会话列表和读回来的屏幕文字，决定哪个会话该推一条通知。不碰网络和宿主，
//! 进来的是观察到的东西和时刻，出去的是要做的事（`Action`），便于单独测。
//!
//! 会话的 agent 停下来等回答（`AgentState::Blocked`）以后过了宽限期（`delay`）还在等，才读屏幕、推
//! 一条通知；答得快的不打扰。一次等回答只推一次：等的时候屏幕上的问题变了不再推，不再等了以后再等
//! 才算新的一次。
//!
//! 连上宿主后的第一份列表（`reconnected` 之后）只记下各会话现在的样子：那时已经在等的不算刚开始等，
//! 交接或者重启后不会再推一遍。
//!
//! 看板上的任务另算（`set_todos`、`todo_sessions`）：同一轮的任务表和会话列表都到了才看（两份回话
//! 谁先到不一定，拿上一轮的任务表看会把刚标成完成、刚删掉的任务也推出去），按
//! `runode_protocol::todo::column` 推出每个任务在哪一列，从进行中变成需要你、又不是在等回答（那个有上面
//! 等回答的通知），过了同样的宽限期还是这样时推一条任务做完的通知；一次做完只推一次，推过以后不再是做完
//! 等着的样子（又回去干活、标成完成、删掉了、会话没了或退出了）时还没发出去的别发了，会话正在等回答时除外
//! （撤的是整个会话的通知，那条等回答的要留着）。会话没了、shell 退出了也算需要你，但多半是用户自己关的，
//! 不推。重连时清掉记着的任务表和各任务的列，第一次看到的任务只记基线，规矩同会话。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use runode_protocol::{
    SessionId, SessionInfo,
    push::{BLOCKED_LINES, preview_lines},
    todo::{Column, Todo, column},
};
use runode_types::{agent::AgentState, session::SessionMeta};

/// 要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// 读会话屏幕底部的文字，读回来交给 `Rounds::screen`。
    ReadScreen(SessionId),
    /// 推一条通知：会话的状态和屏幕底部的几行（不带文字时为空）。
    Notify(SessionId, Box<SessionMeta>, Vec<String>),
    /// 会话不再等了（答了、退出了或者没了），或者任务推过做完以后不再是做完等着的样子：还没发出去的通知
    /// 别发了。
    Cancel(SessionId),
    /// 推一条任务做完的通知：任务的会话和标题。
    TodoDone(SessionId, String),
}

/// 各会话等没等回答、推没推过，见模块文档。
pub(crate) struct Rounds {
    tracked: HashMap<SessionId, Track>,
    /// 下一份列表只记现状，见 `reconnected`。
    baseline: bool,
    /// 变成等回答以后过多久才推。
    delay: Duration,
    /// 带屏幕上的文字；不带时不读屏幕，`Action::Notify` 的几行为空。
    text: bool,
    /// 这一轮的任务表和会话列表，先到的那份等着另一份，见 `check_todos`。
    todos: Option<Vec<Todo>>,
    sessions: Option<Vec<SessionInfo>>,
    /// 各任务（按 `Todo::id`）上一轮在哪一列、什么时候做完的。
    todo_tracked: HashMap<String, TodoTrack>,
}

struct TodoTrack {
    column: Column,
    /// 什么时候从进行中变成做完的；这次做完还没推时有。
    finished_at: Option<Instant>,
    /// 这次做完推过了、推的是哪个会话，还没回去干活。
    pushed: Option<SessionId>,
}

#[derive(Default)]
struct Track {
    meta: SessionMeta,
    blocked: bool,
    /// 什么时候变成等回答的；这次等回答还没推时有。
    blocked_at: Option<Instant>,
    /// 读屏幕的请求发出去了还没回，回来了就推。
    reading: bool,
}

impl Rounds {
    /// 接着的第一份列表只记现状。
    pub(crate) fn new(delay: Duration, text: bool) -> Self {
        Self {
            tracked: HashMap::new(),
            baseline: true,
            delay,
            text,
            todos: None,
            sessions: None,
            todo_tracked: HashMap::new(),
        }
    }

    pub(crate) fn set_options(&mut self, delay: Duration, text: bool) {
        self.delay = delay;
        self.text = text;
    }

    /// 和宿主的连接断了又连上（或者第一次连上）：下一份列表只记现状，没回的读屏幕请求不等了。
    pub(crate) fn reconnected(&mut self) {
        self.baseline = true;
        for track in self.tracked.values_mut() {
            track.reading = false;
        }
        // 断开时的任务表过时了；任务全当第一次看到，只记基线。
        self.todos = None;
        self.sessions = None;
        self.todo_tracked.clear();
    }

    /// 又要了一轮任务表和会话列表：上一轮落单、还没配上对的那份不要了，免得和这一轮的配成一对。
    pub(crate) fn new_round(&mut self) {
        self.todos = None;
        self.sessions = None;
    }

    /// 收到这一轮的任务表（不是这一轮的不交进来）。`unix` 是现在的 Unix 秒，推列要用。
    pub(crate) fn set_todos(&mut self, todos: Vec<Todo>, now: Instant, unix: u64) -> Vec<Action> {
        self.todos = Some(todos);
        self.check_todos(now, unix)
    }

    /// 收到一份会话列表，看任务用；参数同 `set_todos`。
    pub(crate) fn todo_sessions(&mut self, sessions: &[SessionInfo], now: Instant, unix: u64) -> Vec<Action> {
        self.sessions = Some(sessions.to_vec());
        self.check_todos(now, unix)
    }

    /// 同一轮的任务表和会话列表都到了时看任务做没做完，见模块文档；看过的两份都放掉。
    fn check_todos(&mut self, now: Instant, unix: u64) -> Vec<Action> {
        let (Some(todos), Some(sessions)) = (&self.todos, &self.sessions) else { return Vec::new() };
        let blocked = |id: SessionId| {
            sessions.iter().any(|info| {
                info.id == id && !info.exited && info.meta.agent.is_some_and(|agent| agent.state == AgentState::Blocked)
            })
        };
        let mut actions = Vec::new();
        let mut tracked = HashMap::new();
        for todo in todos {
            let info = todo.session.and_then(|id| sessions.iter().find(|info| info.id == id));
            let column = column(todo, info, unix);
            // 停下来不是在等回答，会话还在。
            let finished = column == Column::NeedsYou && info.is_some_and(|info| !info.exited && !blocked(info.id));
            let previous = self.todo_tracked.remove(&todo.id);
            let mut pushed = previous.as_ref().and_then(|track| track.pushed).filter(|_| column == Column::NeedsYou);
            // 推过做完、现在不再是做完等着的样子：还在退避、没发出去的那条过时了。会话在等回答时不撤，那条
            // 等回答的通知要留着。
            if let Some(session) = previous.as_ref().and_then(|track| track.pushed)
                && !finished
                && !blocked(session)
            {
                actions.push(Action::Cancel(session));
                pushed = None;
            }
            let mut finished_at = match previous {
                _ if !finished => None,
                Some(TodoTrack { column: Column::InProgress, .. }) => Some(now),
                Some(track) => track.finished_at,
                None => None,
            };
            if let (Some(at), Some(info)) = (finished_at, info)
                && now.duration_since(at) >= self.delay
            {
                finished_at = None;
                pushed = Some(info.id);
                actions.push(Action::TodoDone(info.id, todo.spec.title.clone()));
            }
            tracked.insert(todo.id.clone(), TodoTrack { column, finished_at, pushed });
        }
        // 删掉了的任务推过的也撤。
        for session in self.todo_tracked.values().filter_map(|track| track.pushed) {
            if !blocked(session) {
                actions.push(Action::Cancel(session));
            }
        }
        self.todo_tracked = tracked;
        self.todos = None;
        self.sessions = None;
        actions
    }

    /// 收到一份会话列表。
    pub(crate) fn observe(&mut self, sessions: &[SessionInfo], now: Instant) -> Vec<Action> {
        let baseline = std::mem::take(&mut self.baseline);
        let mut actions = Vec::new();
        self.tracked.retain(|id, track| {
            let listed = sessions.iter().any(|info| info.id == *id);
            if !listed && track.blocked {
                actions.push(Action::Cancel(*id));
            }
            listed
        });
        for info in sessions {
            let blocked = !info.exited && info.meta.agent.is_some_and(|agent| agent.state == AgentState::Blocked);
            let track = self.tracked.entry(info.id).or_default();
            if track.blocked && !blocked {
                actions.push(Action::Cancel(info.id));
            }
            track.meta = info.meta.clone();
            if baseline || !blocked {
                track.blocked_at = None;
                track.reading = false;
            } else if !track.blocked {
                track.blocked_at = Some(now);
            }
            track.blocked = blocked;
            if !track.blocked_at.is_some_and(|at| now.duration_since(at) >= self.delay) {
                continue;
            }
            track.blocked_at = None;
            if self.text {
                track.reading = true;
                actions.push(Action::ReadScreen(info.id));
            } else {
                actions.push(Action::Notify(info.id, Box::new(track.meta.clone()), Vec::new()));
            }
        }
        actions
    }

    /// 会话 `id` 的屏幕文字读回来了；读不了时 `text` 为 `None`，按没有文字办。
    pub(crate) fn screen(&mut self, id: SessionId, text: Option<&str>) -> Vec<Action> {
        let Some(track) = self.tracked.get_mut(&id) else { return Vec::new() };
        if !std::mem::take(&mut track.reading) || !track.blocked {
            return Vec::new();
        }
        let lines = match text {
            Some(text) if self.text => preview_lines(text, BLOCKED_LINES),
            _ => Vec::new(),
        };
        vec![Action::Notify(id, Box::new(track.meta.clone()), lines)]
    }
}

#[cfg(test)]
mod tests;
