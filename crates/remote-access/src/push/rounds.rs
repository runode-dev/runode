//! 什么时候推：按每两秒一次的会话列表和读回来的屏幕文字，决定哪个会话开始、刷新、结束一个「回合」。
//! 不碰网络和宿主，进来的是观察到的东西和时刻，出去的是要做的事（`Action`），便于单独测。
//!
//! 一个回合从会话的 agent 停下来等回答（`AgentState::Blocked`）开始：变成等回答以后过了宽限期
//! （`delay`）还在等，才读屏幕、起 Live Activity；答得快的不打扰。还在等、屏幕上的问题变了时刷新，
//! 同一个会话最多 `UPDATE_INTERVAL` 一次。不再等了、shell 退出了或者会话没了，回合结束。
//!
//! 连上宿主后的第一份列表（`reconnected` 之后）只记下各会话现在的样子：那时已经在等的不算刚开始等，
//! 交接或者重启后不会再推一遍。上次没收起的回合（从文件里恢复的）这时对一遍：会话还在等的接着用，
//! 不等了或者没了的结束。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use runode_protocol::{
    SessionId, SessionInfo,
    push::{ActivityContent, BLOCKED_LINES, preview_lines},
};
use runode_shared_types::{agent::AgentState, session::SessionMeta};

/// 同一个会话最多隔这么久刷新一次。
pub(crate) const UPDATE_INTERVAL: Duration = Duration::from_secs(10);

/// 内容一直没变时，隔这么久也照样刷新一次：每次推送都把过时的时刻（`STALE_AFTER`）往后挪，等得久的
/// 问题在手机上不会显示成过时的。比 `STALE_AFTER` 短。
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(20 * 60);

/// 要做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// 读会话屏幕底部的文字，读回来交给 `Rounds::screen`。
    ReadScreen(SessionId),
    /// 起 Live Activity。
    Start(SessionId, ActivityContent),
    /// 刷新内容。
    Update(SessionId, ActivityContent),
    /// 收起，带着最后的内容。
    End(SessionId, ActivityContent),
}

/// 各会话的回合，见模块文档。
pub(crate) struct Rounds {
    tracked: HashMap<SessionId, Track>,
    /// 下一份列表只记现状，见 `reconnected`。
    baseline: bool,
    /// 变成等回答以后过多久才开始一个回合。
    delay: Duration,
    /// 带屏幕上的文字；不带时不读屏幕，内容里的 `lines` 为空。
    text: bool,
}

struct Track {
    meta: SessionMeta,
    blocked: bool,
    /// 什么时候变成等回答的；还没到宽限期、也还没开始回合时有。
    blocked_at: Option<Instant>,
    round: Option<Round>,
}

struct Round {
    /// 推出去的内容；还在等屏幕文字、起 Live Activity 之前为 `None`。
    sent: Option<ActivityContent>,
    /// 上次起或者刷新的时刻；从文件里恢复的为 `None`，可以马上刷新。
    sent_at: Option<Instant>,
    /// 读屏幕的请求发出去了还没回。
    reading: bool,
}

impl Rounds {
    /// `restored` 是上次没收起的回合和推出去的最后内容。接着的第一份列表只记现状。
    pub(crate) fn new(restored: Vec<(SessionId, ActivityContent)>, delay: Duration, text: bool) -> Self {
        let tracked = restored
            .into_iter()
            .map(|(id, content)| {
                let round = Round { sent: Some(content), sent_at: None, reading: false };
                (id, Track { meta: SessionMeta::default(), blocked: true, blocked_at: None, round: Some(round) })
            })
            .collect();
        Self { tracked, baseline: true, delay, text }
    }

    pub(crate) fn set_options(&mut self, delay: Duration, text: bool) {
        self.delay = delay;
        self.text = text;
    }

    /// 和宿主的连接断了又连上（或者第一次连上）：下一份列表只记现状，没回的读屏幕请求不等了。
    pub(crate) fn reconnected(&mut self) {
        self.baseline = true;
        for track in self.tracked.values_mut() {
            if let Some(round) = &mut track.round {
                round.reading = false;
            }
        }
    }

    /// 收到一份会话列表。
    pub(crate) fn observe(&mut self, sessions: &[SessionInfo], now: Instant) -> Vec<Action> {
        let baseline = std::mem::take(&mut self.baseline);
        let mut actions = Vec::new();
        self.tracked.retain(|id, track| {
            let listed = sessions.iter().any(|info| info.id == *id);
            if !listed && let Some(content) = track.round.take().and_then(|round| round.sent) {
                actions.push(Action::End(*id, content));
            }
            listed
        });
        for info in sessions {
            let blocked = !info.exited && info.meta.agent.is_some_and(|agent| agent.state == AgentState::Blocked);
            let track = self.tracked.entry(info.id).or_insert_with(|| Track {
                meta: SessionMeta::default(),
                blocked: false,
                blocked_at: None,
                round: None,
            });
            track.meta = info.meta.clone();
            if baseline {
                track.blocked_at = None;
            } else if blocked && !track.blocked {
                track.blocked_at = Some(now);
            }
            track.blocked = blocked;
            if !blocked {
                track.blocked_at = None;
                if let Some(content) = track.round.take().and_then(|round| round.sent) {
                    actions.push(Action::End(info.id, content));
                }
                continue;
            }
            match &mut track.round {
                None => {
                    if track.blocked_at.is_some_and(|at| now.duration_since(at) >= self.delay) {
                        track.blocked_at = None;
                        let mut round = Round { sent: None, sent_at: None, reading: false };
                        if self.text {
                            round.reading = true;
                            actions.push(Action::ReadScreen(info.id));
                        } else {
                            let content = ActivityContent::new(&track.meta, &[]);
                            round.sent = Some(content.clone());
                            round.sent_at = Some(now);
                            actions.push(Action::Start(info.id, content));
                        }
                        track.round = Some(round);
                    }
                }
                Some(round) => {
                    let due = round.sent_at.is_none_or(|at| now.duration_since(at) >= UPDATE_INTERVAL);
                    if round.sent.is_none() || round.reading || !due {
                        continue;
                    }
                    if self.text {
                        round.reading = true;
                        actions.push(Action::ReadScreen(info.id));
                    } else {
                        actions.extend(refresh(info.id, round, ActivityContent::new(&track.meta, &[]), now));
                    }
                }
            }
        }
        actions
    }

    /// 会话 `id` 的屏幕文字读回来了；读不了时 `text` 为 `None`，按没有文字办。
    pub(crate) fn screen(&mut self, id: SessionId, text: Option<&str>, now: Instant) -> Vec<Action> {
        let Some(track) = self.tracked.get_mut(&id) else { return Vec::new() };
        let Some(round) = &mut track.round else { return Vec::new() };
        if !std::mem::take(&mut round.reading) || !track.blocked {
            return Vec::new();
        }
        let lines = match text {
            Some(text) if self.text => preview_lines(text, BLOCKED_LINES),
            _ => Vec::new(),
        };
        let content = ActivityContent::new(&track.meta, &lines);
        if round.sent.is_none() {
            round.sent = Some(content.clone());
            round.sent_at = Some(now);
            return vec![Action::Start(id, content)];
        }
        refresh(id, round, content, now).into_iter().collect()
    }

    /// 全部收起：关了推送、没有登记了。之后的第一份列表只记现状。
    pub(crate) fn end_all(&mut self) -> Vec<Action> {
        self.baseline = true;
        self.tracked.drain().filter_map(|(id, track)| Some(Action::End(id, track.round?.sent?))).collect()
    }

    /// 有回合开着（推出去过、还没收起）。
    #[cfg(test)]
    fn any_open(&self) -> bool {
        self.tracked.values().any(|track| track.round.as_ref().is_some_and(|round| round.sent.is_some()))
    }
}

/// 内容变了、离上次够久了就刷新；内容没变时到了 `HEARTBEAT` 也刷新。
fn refresh(id: SessionId, round: &mut Round, content: ActivityContent, now: Instant) -> Option<Action> {
    let since = round.sent_at.map(|at| now.duration_since(at));
    let due = since.is_none_or(|since| since >= UPDATE_INTERVAL);
    let heartbeat = since.is_some_and(|since| since >= HEARTBEAT);
    if !due || (round.sent.as_ref() == Some(&content) && !heartbeat) {
        return None;
    }
    round.sent = Some(content.clone());
    round.sent_at = Some(now);
    Some(Action::Update(id, content))
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

    fn content(id: SessionId, lines: &[&str]) -> ActivityContent {
        let lines: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
        ActivityContent::new(&blocked(id).meta, &lines)
    }

    fn secs(start: Instant, n: u64) -> Instant {
        start + Duration::from_secs(n)
    }

    /// 回合的一生：等过宽限期才读屏幕、起；屏幕变了刷新，限频；不等了收起。
    #[test]
    fn a_round_starts_after_the_delay_and_ends_when_answered() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Vec::new(), DELAY, true);
        assert_eq!(rounds.observe(&[working(A)], t), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 10)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 12)), [Action::ReadScreen(A)]);
        // 读屏幕的请求还没回，不再要。
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 14)), []);
        assert_eq!(
            rounds.screen(A, Some("Do you want to proceed?\n❯ 1. Yes\n  2. No\n"), secs(t, 14)),
            [Action::Start(A, content(A, &["Do you want to proceed?", "❯ 1. Yes", "  2. No"]))]
        );
        // 刷新最多十秒一次：之前不读屏幕。
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 16)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 24)), [Action::ReadScreen(A)]);
        // 没变不刷新。
        assert_eq!(rounds.screen(A, Some("Do you want to proceed?\n❯ 1. Yes\n  2. No"), secs(t, 24)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 26)), [Action::ReadScreen(A)]);
        assert_eq!(
            rounds.screen(A, Some("Allow edits?\n❯ 1. Yes"), secs(t, 26)),
            [Action::Update(A, content(A, &["Allow edits?", "❯ 1. Yes"]))]
        );
        assert_eq!(
            rounds.observe(&[working(A)], secs(t, 28)),
            [Action::End(A, content(A, &["Allow edits?", "❯ 1. Yes"]))]
        );
        assert!(!rounds.any_open());
        // 再等回答是新的一个回合，又从宽限期算起。
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 30)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 40)), [Action::ReadScreen(A)]);
    }

    /// 等得久、内容一直没变：到了 `HEARTBEAT` 照样刷新一次，手机上不会过时。
    #[test]
    fn long_waits_are_refreshed_before_they_go_stale() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Vec::new(), DELAY, true);
        rounds.observe(&[working(A)], t);
        rounds.observe(&[blocked(A)], secs(t, 2));
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 12)), [Action::ReadScreen(A)]);
        assert_eq!(rounds.screen(A, Some("Proceed?"), secs(t, 12)), [Action::Start(A, content(A, &["Proceed?"]))]);
        let before = secs(t, 12 + HEARTBEAT.as_secs() - 2);
        assert_eq!(rounds.observe(&[blocked(A)], before), [Action::ReadScreen(A)]);
        assert_eq!(rounds.screen(A, Some("Proceed?"), before), []);
        let after = secs(t, 12 + HEARTBEAT.as_secs());
        assert_eq!(rounds.observe(&[blocked(A)], after), [Action::ReadScreen(A)]);
        assert_eq!(rounds.screen(A, Some("Proceed?"), after), [Action::Update(A, content(A, &["Proceed?"]))]);
    }

    /// 宽限期里答了的不推。
    #[test]
    fn quick_answers_are_not_pushed() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Vec::new(), DELAY, true);
        rounds.observe(&[working(A)], t);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), []);
        assert_eq!(rounds.observe(&[working(A)], secs(t, 8)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 14)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 22)), []);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 24)), [Action::ReadScreen(A)]);
    }

    /// 连上后第一份列表里已经在等的不推；之后新等的照常。
    #[test]
    fn the_first_list_only_records() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Vec::new(), DELAY, true);
        assert_eq!(rounds.observe(&[blocked(A), working(B)], t), []);
        assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 2)), []);
        assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 60)), [Action::ReadScreen(B)]);
        // 断了重连也一样。
        rounds.reconnected();
        rounds.observe(&[working(A), working(B)], secs(t, 62));
        assert!(rounds.observe(&[blocked(A)], secs(t, 64)).is_empty());
        rounds.reconnected();
        assert!(rounds.observe(&[blocked(A)], secs(t, 66)).is_empty());
        assert!(rounds.observe(&[blocked(A)], secs(t, 100)).is_empty());
    }

    /// 上次没收起的回合：会话还在等的接着刷新，不等了或者没了的收起。
    #[test]
    fn restored_rounds_are_reconciled() {
        let t = Instant::now();
        let restored =
            vec![(A, content(A, &["old"])), (B, content(B, &["b"])), (SessionId(0xc), content(SessionId(0xc), &[]))];
        let mut rounds = Rounds::new(restored, DELAY, true);
        let mut actions = rounds.observe(&[blocked(A), working(B)], t);
        actions.sort_by_key(|action| format!("{action:?}"));
        assert_eq!(
            actions,
            [
                Action::End(B, content(B, &["b"])),
                Action::End(SessionId(0xc), content(SessionId(0xc), &[])),
                Action::ReadScreen(A),
            ]
        );
        assert_eq!(rounds.screen(A, Some("new"), t), [Action::Update(A, content(A, &["new"]))]);
        assert_eq!(rounds.end_all(), [Action::End(A, content(A, &["new"]))]);
    }

    /// 会话没了、shell 退出了，回合结束；还没推出去的不用收起。
    #[test]
    fn gone_and_exited_sessions_end_their_rounds() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Vec::new(), DELAY, true);
        rounds.observe(&[], t);
        rounds.observe(&[blocked(A), blocked(B)], secs(t, 1));
        assert_eq!(
            rounds.observe(&[blocked(A), blocked(B)], secs(t, 11)),
            [Action::ReadScreen(A), Action::ReadScreen(B)]
        );
        rounds.screen(A, None, secs(t, 11));
        let exited = SessionInfo { exited: true, ..blocked(A) };
        // B 的屏幕还没读回来，没推过，不用收起；读回来时也不再起。
        assert_eq!(rounds.observe(&[exited], secs(t, 13)), [Action::End(A, content(A, &[]))]);
        assert_eq!(rounds.screen(B, Some("x"), secs(t, 13)), []);
        assert!(!rounds.any_open());
    }

    /// 不带屏幕文字时不读屏幕，直接起；标题变了照样刷新。
    #[test]
    fn without_text_the_screen_is_not_read() {
        let t = Instant::now();
        let mut rounds = Rounds::new(Vec::new(), Duration::ZERO, false);
        rounds.observe(&[], t);
        assert_eq!(rounds.observe(&[blocked(A)], secs(t, 1)), [Action::Start(A, content(A, &[]))]);
        let mut renamed = blocked(A);
        renamed.meta.title = Some("改了名".into());
        assert_eq!(rounds.observe(std::slice::from_ref(&renamed), secs(t, 5)), []);
        assert_eq!(
            rounds.observe(std::slice::from_ref(&renamed), secs(t, 11)),
            [Action::Update(A, ActivityContent::new(&renamed.meta, &[]))]
        );
        // 读回来的文字（开着文字时发出的请求）也不带。
        rounds.set_options(Duration::ZERO, true);
        assert_eq!(rounds.observe(std::slice::from_ref(&renamed), secs(t, 21)), [Action::ReadScreen(A)]);
        rounds.set_options(Duration::ZERO, false);
        assert_eq!(rounds.screen(A, Some("secret"), secs(t, 21)), []);
    }
}
