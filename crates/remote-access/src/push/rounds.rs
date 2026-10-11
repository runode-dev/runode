//! 什么时候推：按每两秒一次的会话列表和读回来的屏幕文字，决定哪个会话该推一条通知。不碰网络和宿主，
//! 进来的是观察到的东西和时刻，出去的是要做的事（`Action`），便于单独测。
//!
//! 会话的 agent 停下来等回答（`AgentState::Blocked`）以后过了宽限期（`delay`）还在等，才读屏幕、推
//! 一条通知；答得快的不打扰。一次等回答只推一次：等的时候屏幕上的问题变了不再推，不再等了以后再等
//! 才算新的一次。
//!
//! 连上宿主后的第一份列表（`reconnected` 之后）只记下各会话现在的样子：那时已经在等的不算刚开始等，
//! 交接或者重启后不会再推一遍。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use runode_protocol::{
    SessionId, SessionInfo,
    push::{BLOCKED_LINES, preview_lines},
};
use runode_shared_types::{agent::AgentState, session::SessionMeta};

/// 要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// 读会话屏幕底部的文字，读回来交给 `Rounds::screen`。
    ReadScreen(SessionId),
    /// 推一条通知：会话的状态和屏幕底部的几行（不带文字时为空）。
    Notify(SessionId, Box<SessionMeta>, Vec<String>),
    /// 会话不再等了（答了、退出了或者没了）：还没发出去的通知别发了。
    Cancel(SessionId),
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
        Self { tracked: HashMap::new(), baseline: true, delay, text }
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
mod tests {
    use runode_protocol::SessionInfo;
    use runode_shared_types::{
        agent::{Agent, AgentKind},
        grid::GridSize,
    };

    use super::*;

    const DELAY: Duration = Duration::from_secs(10);
    const A: SessionId = SessionId(0xa);
    const B: SessionId = SessionId(0xb);

    fn session(id: SessionId, state: Option<AgentState>) -> SessionInfo {
        SessionInfo {
            id,
            size: GridSize { cols: 80, rows: 24, cell_width_px: 0, cell_height_px: 0 },
            meta: SessionMeta {
                title: Some(format!("会话 {}", id.0)),
                agent: state.map(|state| Agent { kind: AgentKind::Claude, state }),
                ..SessionMeta::default()
            },
            clients: 0,
            claimed: false,
            exited: false,
            size_owner: None,
        }
    }

    fn blocked(id: SessionId) -> SessionInfo {
        session(id, Some(AgentState::Blocked))
    }

    fn working(id: SessionId) -> SessionInfo {
        session(id, Some(AgentState::Working))
    }

    fn notify(id: SessionId, lines: &[&str]) -> Action {
        Action::Notify(id, Box::new(blocked(id).meta), lines.iter().map(|line| (*line).to_owned()).collect())
    }

    fn secs(start: Instant, n: u64) -> Instant {
        start + Duration::from_secs(n)
    }

    /// 等过宽限期才读屏幕、推；一次等回答只推一次，问题变了也不再推；答了再等是新的一次。
    #[test]
    fn one_push_per_wait() {
        let t = Instant::now();
        let mut rounds = Rounds::new(DELAY, true);
        assert_eq!(rounds.observe(&[working(A)], t), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 10)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 12)), [Action::ReadScreen(A)]);
        // 读屏幕的请求还没回，不再要。
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 14)), []);
        assert_eq!(
            rounds.screen(A, Some("Do you want to proceed?\n❯ 1. Yes\n  2. No\n")),
            [notify(A, &["Do you want to proceed?"])]
        );
        // 还在等、问题变了：不再读屏幕，也不再推。
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 30)), []);
        assert_eq!(rounds.screen(A, Some("Allow edits?\n❯ 1. Yes")), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 600)), []);
        // 答了：还没发出去的通知别发了；再等是新的一次，又从宽限期算起。
        assert_eq!(rounds.observe(&[working(A)], secs(t, 602)), [Action::Cancel(A)]);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 604)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 614)), [Action::ReadScreen(A)]);
    }

    /// 宽限期里答了的不推。
    #[test]
    fn quick_answers_are_not_pushed() {
        let t = Instant::now();
        let mut rounds = Rounds::new(DELAY, true);
        rounds.observe(&[working(A)], t);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), []);
        assert_eq!(rounds.observe(&[working(A)], secs(t, 8)), [Action::Cancel(A)]);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 14)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 22)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 24)), [Action::ReadScreen(A)]);
    }

    /// 读屏幕时已经答了：读回来也不推。
    #[test]
    fn answered_while_reading_is_not_pushed() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Duration::ZERO, true);
        rounds.observe(&[working(A)], t);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), [Action::ReadScreen(A)]);
        rounds.observe(&[working(A)], secs(t, 4));
        assert_eq!(rounds.screen(A, Some("Proceed?")), []);
    }

    /// 连上后第一份列表里已经在等的不推；之后新等的照常。
    #[test]
    fn the_first_list_only_records() {
        let t = Instant::now();
        let mut rounds = Rounds::new(DELAY, true);
        assert_eq!(rounds.observe(&[blocked(A), working(B)], t), []);
        assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 2)), []);
        assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 60)), [Action::ReadScreen(B)]);
        // 断了重连也一样，没回的读屏幕请求不等了。
        rounds.reconnected();
        assert_eq!(rounds.screen(B, Some("x")), []);
        rounds.observe(&[working(A), working(B)], secs(t, 62));
        assert!(rounds.observe(&[blocked(A)], secs(t, 64)).is_empty());
        rounds.reconnected();
        assert!(rounds.observe(&[blocked(A)], secs(t, 66)).is_empty());
        assert!(rounds.observe(&[blocked(A)], secs(t, 100)).is_empty());
    }

    /// 会话没了、shell 退出了：读回来的屏幕不推，还没发出去的通知也别发了。
    #[test]
    fn gone_and_exited_sessions_are_not_pushed() {
        let t = Instant::now();
        let mut rounds = Rounds::new(DELAY, true);
        rounds.observe(&[], t);
        rounds.observe(&[blocked(A), blocked(B)], secs(t, 1));
        assert_eq!(
            rounds.observe(&[blocked(A), blocked(B)], secs(t, 11)),
            [Action::ReadScreen(A), Action::ReadScreen(B)]
        );
        let exited = SessionInfo { exited: true, ..blocked(A) };
        assert_eq!(rounds.observe(&[exited], secs(t, 13)), [Action::Cancel(B), Action::Cancel(A)]);
        assert_eq!(rounds.screen(A, Some("x")), []);
        assert_eq!(rounds.screen(B, Some("x")), []);
    }

    /// 不带屏幕文字时不读屏幕，直接推；读回来的文字（开着文字时发出的请求）也不带。
    #[test]
    fn without_text_the_screen_is_not_read() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Duration::ZERO, false);
        rounds.observe(&[], t);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 1)), [notify(A, &[])]);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 30)), []);
        rounds.set_options(Duration::ZERO, true);
        rounds.observe(&[working(B)], secs(t, 31));
        assert_eq!(rounds.observe(&[blocked(B)], secs(t, 32)), [Action::ReadScreen(B)]);
        rounds.set_options(Duration::ZERO, false);
        assert_eq!(rounds.screen(B, Some("secret")), [notify(B, &[])]);
    }
}
