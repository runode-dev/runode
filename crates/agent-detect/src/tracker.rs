//! 把各路信号合成前台 agent 的状态，并去抖。
//!
//! 信号有：前台进程认出来的 agent（`Tracker::foreground`）、标题开头的状态字符和 OSC 9;4 进度
//! （`Tracker::title`、`Tracker::progress`，见 `crate::title`）、程序用 OSC 7501 报告的状态
//! （`Tracker::program_status`）、屏幕底部的文字和输出活动（`Tracker::output`）。合成的规则：
//!
//! - 是哪个 agent：前台进程认出来的优先，其次是标题前缀报告的，再次是 OSC 7501 报告的 `app`
//!   认得出的；只有 OSC 7501 或进度报告时是 pi（标题像 pi 的）或 `AgentKind::Other`。都没有
//!   就不算有 agent。
//! - 什么状态：程序用 OSC 7501 报告了的，就是它报告的，不比规则、不看输出、也不等确认空闲。
//!   否则先用这种 agent 的识别规则（`RuleBook`）去比屏幕、标题和进度报告的原文；规则
//!   说不准时用标题前缀和进度报告里 agent 自己报告的状态；再没有就当它空闲。
//! - 规则没给出明摆着的状态、只是推测空闲时，持续有输出（不是在回显用户的按键）就算工作中。
//! - 从工作中变成推测的空闲要多看几次才算数（`IDLE_CONFIRMATIONS` 次，每次隔
//!   `IDLE_RECHECK`，最多等 `IDLE_CAP`）：工具调用之间、两段输出之间常有片刻什么都没有。
//!   界面上明摆着空闲、等用户回答，或者 agent 自己报告停下，都立即算数。
//! - 刚认出 agent 的 `STARTUP_GRACE` 里不看屏幕：启动画面上的字不代表状态。这期间只用 agent
//!   自己报告的状态，没有就是空闲。
//!
//! 求值要读屏幕、跑正则，不在每段输出时都做：只在有新输出、信号变了、或者正等着确认空闲时
//! 求值，而且两次之间至少隔 `EVAL_INTERVAL`。agent 自己报告的状态变了、前台换了程序时立即
//! 求值。调用方在 `Tracker::deadline` 到了时再调 `Tracker::poll`。
//!
//! 宿主升级时会话换到新宿主，`Tracker` 也换成新的一个：`Tracker::resume` 接着交接前公布的状态，
//! 不从「没有 agent」重新认起，见那里。

use std::time::{Duration, Instant};

use runode_shared_types::agent::{Agent, AgentKind, AgentState};

use crate::{
    agent_from_name,
    book::RuleBook,
    osc::ProgressCapture,
    rules::{Signals, Verdict},
    title,
};

/// 有新输出时两次求值之间的最短间隔。
pub const EVAL_INTERVAL: Duration = Duration::from_millis(250);
/// 等着确认空闲时多久再看一次。
pub const IDLE_RECHECK: Duration = Duration::from_millis(100);
/// 推测的空闲要连着看到几次才算数。
pub const IDLE_CONFIRMATIONS: u8 = 3;
/// 确认空闲最多等这么久。
pub const IDLE_CAP: Duration = Duration::from_millis(700);
/// 刚认出 agent 后不看屏幕的时长。
pub const STARTUP_GRACE: Duration = Duration::from_secs(3);
/// 最多记这么多条 OSC 7501 记录，再多就丢掉最久没更新的。协议要求至少 64、至多 256 条。
const MAX_STATUS_RECORDS: usize = 256;
/// 两段输出隔得比这短就算同一阵输出。
pub const BURST_GAP: Duration = Duration::from_millis(800);
/// 一阵输出持续这么久（从用户最后一次输入之后算）才算 agent 在干活。
pub const BURST_MIN: Duration = Duration::from_millis(1500);

/// 前台在跑什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreground {
    /// shell 自己，没有在跑别的程序。
    Shell,
    /// 别的程序，认得出是哪个 agent 时带上。
    Program(Option<AgentKind>),
}

#[derive(Debug, Default)]
pub struct Tracker {
    /// 前台进程认出来的 agent。
    process: Option<AgentKind>,
    /// 认出它的时刻，启动宽限期从这里算；宽限期过完、求值过一次后清掉。
    grace_from: Option<Instant>,
    /// 标题前缀报告的 agent，见 `title::split_status`。
    title_agent: Option<Agent>,
    /// 去掉前缀后的标题是 pi 的。
    pi_title: bool,
    /// OSC 9;4 进度：`Some(true)` 进行中，`Some(false)` 已停下但程序还在前台。
    progress: Option<bool>,
    /// 程序用 OSC 7501 报告的记录：id 和状态，最近更新的在后。done 和 error 记成空闲。
    statuses: Vec<(String, AgentState)>,
    /// OSC 7501 报告的 `app` 认得出的 agent，记录清空时一起清掉。
    status_agent: Option<AgentKind>,
    /// 给规则的标题原文，换了 agent 时清空。
    title_text: String,
    /// 给规则的 OSC 9 原文。
    progress_text: ProgressCapture,
    activity: Activity,
    /// 对外的状态。
    published: Option<Agent>,
    /// 正在确认的空闲。
    pending_idle: Option<PendingIdle>,
    /// 上次求值后又有了输出。
    dirty: bool,
    /// 信号变了，从这个时刻起该立即求值。
    urgent: Option<Instant>,
    last_eval: Option<Instant>,
    /// 上次求值时是靠输出活动判成工作中的，输出停下后要再求值一次。
    by_activity: bool,
    /// 交接前公布的 agent，见 `resume`：前台换程序或回到 shell 之前，别的信号认不出是哪个 agent
    /// 时按它算；它的进程这时才被认出来的，不算刚启动，没有启动宽限期。
    carried: Option<Agent>,
    /// 交接前那阵输出还算在继续，见 `resume`：输出重新流起来（`output_resumed` 或下一段输出）
    /// 之前，按输出活动判的工作中不撤。
    held_burst: bool,
}

#[derive(Clone, Copy, Debug)]
struct PendingIdle {
    since: Instant,
    confirmations: u8,
}

/// 最近一阵输出。
#[derive(Clone, Copy, Debug, Default)]
struct Activity {
    start: Option<Instant>,
    last: Option<Instant>,
}

impl Activity {
    fn record(&mut self, now: Instant) {
        if self.last.is_none_or(|last| now.saturating_duration_since(last) > BURST_GAP) {
            self.start = Some(now);
        }
        self.last = Some(now);
    }

    /// 输出还没停，而且在用户最后一次输入之后已经持续了 `BURST_MIN`。用户打字时的回显
    /// 因此不算。
    fn sustained(self, now: Instant, last_input: Option<Instant>) -> bool {
        let (Some(start), Some(last)) = (self.start, self.last) else {
            return false;
        };
        let start = last_input.map_or(start, |input| start.max(input));
        now.saturating_duration_since(last) <= BURST_GAP && last.saturating_duration_since(start) >= BURST_MIN
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 对外的状态：前台 agent 和它在干什么；前台不是 agent 时为 `None`。
    pub fn agent(&self) -> Option<Agent> {
        self.published
    }

    /// 接着交接前公布的状态 `agent`：宿主升级时会话换了一个 `Tracker`，先喂好眼下能拿到的信号
    /// （前台进程、标题），再调这里。对外的状态直接是 `agent`；`agent` 的进程已经认出来的，不再
    /// 等启动宽限期；交接时丢了的信号（进度报告、空闲的 codex 不带前缀的标题）认不出是哪个
    /// agent 时，前台换程序或回到 shell 之前按 `agent` 算。交接前靠输出活动判成工作中的，交接
    /// 期间读线程停着、没有输出，等输出重新流起来（`output_resumed`）再从那一刻接着算。之后照常
    /// 立即求值一次，按眼前的屏幕和信号校正；结论和 `agent` 一样时 `poll` 不报变化，界面上的
    /// agent 指示不会闪一下。
    pub fn resume(&mut self, agent: Option<Agent>, now: Instant) {
        self.published = agent;
        self.carried = agent;
        self.pending_idle = None;
        if agent.is_some_and(|agent| self.process == Some(agent.kind)) {
            self.grace_from = None;
        }
        self.held_burst = agent.is_some_and(Agent::is_working);
        self.urgent = Some(now);
    }

    /// 交接后输出又开始流了（接手的宿主打开了读线程）：`resume` 按住的那阵输出从 `now` 接着算，
    /// 之后输出停下时照常确认空闲。没有按住时什么都不做。
    pub fn output_resumed(&mut self, now: Instant) {
        if std::mem::take(&mut self.held_burst) {
            self.activity = Activity { start: Some(now.checked_sub(BURST_MIN).unwrap_or(now)), last: Some(now) };
        }
    }

    /// 程序的一段输出，进 VT 之前或之后都行。
    pub fn output(&mut self, bytes: &[u8], now: Instant) {
        if bytes.is_empty() {
            return;
        }
        self.progress_text.observe(bytes);
        self.output_resumed(now);
        self.activity.record(now);
        self.dirty = true;
    }

    /// 程序设了新标题（OSC 0/2），返回去掉状态前缀、用来显示的标题。`foreground_is_shell`
    /// 只在需要时调用：标题没带前缀、刚才还有前缀时，要看那个 agent 是不是还在前台。
    pub fn title<'a>(&mut self, raw: &'a str, now: Instant, foreground_is_shell: impl FnOnce() -> bool) -> &'a str {
        let (shown, agent) = match title::split_status(raw) {
            Some((agent, rest)) => (rest, Some(agent)),
            // codex 空闲时不带前缀：刚才还在报告状态的 agent 只要仍在前台，就是停下来了。
            None => (
                raw,
                self.title_agent
                    .filter(|_| !foreground_is_shell())
                    .map(|agent| Agent { state: AgentState::Idle, ..agent }),
            ),
        };
        let pi_title = title::is_pi_title(shown);
        if (agent, pi_title) != (self.title_agent, self.pi_title) {
            self.urgent = Some(now);
        }
        self.title_agent = agent;
        self.pi_title = pi_title;
        if self.title_text != raw {
            self.title_text.clear();
            self.title_text.push_str(raw);
            self.dirty = true;
        }
        shown
    }

    /// 程序报告了 OSC 9;4 进度，`active` 是进行中。停下时前台已经回到 shell 的不再算 agent。
    pub fn progress(&mut self, active: bool, now: Instant, foreground_is_shell: impl FnOnce() -> bool) {
        // 工作中每秒重发一次进度，没变时不算信号变化。
        let progress = if active { Some(true) } else { (!foreground_is_shell()).then_some(false) };
        if progress != self.progress {
            self.urgent = Some(now);
        }
        self.progress = progress;
    }

    /// 程序用 OSC 7501 报告了 id 为 `id` 的记录在做什么，`state` 为 `None` 是清掉这条记录和它
    /// 下面的全部记录（`id` 为空时清掉所有记录）。`app` 是程序报告的名字，认得出是哪个 agent
    /// 时按它算。程序退出、回到 shell 时记录随 `foreground` 一起清掉。
    pub fn program_status(&mut self, id: &str, app: &str, state: Option<AgentState>, now: Instant) {
        let before = (self.status_state(), self.status_agent);
        match state {
            Some(state) => {
                self.statuses.retain(|(record, _)| record != id);
                if self.statuses.len() == MAX_STATUS_RECORDS {
                    self.statuses.remove(0);
                }
                self.statuses.push((id.to_owned(), state));
                if let Some(kind) = agent_from_name(app) {
                    self.status_agent = Some(kind);
                }
            }
            None => {
                self.statuses.retain(|(record, _)| {
                    !(id.is_empty()
                        || record.strip_prefix(id).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')))
                });
                if self.statuses.is_empty() {
                    self.status_agent = None;
                }
            }
        }
        if (self.status_state(), self.status_agent) != before {
            self.urgent = Some(now);
        }
    }

    /// 前台程序。回到 shell 时 agent 已经退出，它留下的标题和进度不再代表任何状态；换成
    /// 另一个 agent 时也不沿用上一个的。
    pub fn foreground(&mut self, foreground: Foreground, now: Instant) {
        match foreground {
            Foreground::Shell => {
                if self.process.is_some()
                    || self.title_agent.is_some()
                    || self.progress.is_some()
                    || !self.statuses.is_empty()
                    || self.carried.is_some()
                {
                    self.urgent = Some(now);
                }
                self.process = None;
                self.grace_from = None;
                self.forget_reports();
                self.forget_carried();
            }
            Foreground::Program(kind) if kind != self.process => {
                if self.process.is_some() {
                    self.forget_reports();
                }
                // 交接前就在跑的同一个 agent 不算刚启动。
                let carried = kind.is_some() && kind == self.carried.map(|agent| agent.kind);
                if !carried {
                    self.forget_carried();
                }
                self.process = kind;
                self.grace_from = kind.filter(|_| !carried).map(|_| now);
                self.pending_idle = None;
                self.urgent = Some(now);
            }
            Foreground::Program(_) => {}
        }
    }

    /// 下次该调 `poll` 的时刻；没有要做的事时为 `None`。
    pub fn deadline(&self) -> Option<Instant> {
        if let Some(at) = self.urgent {
            return Some(at);
        }
        self.kind()?;
        let last = self.last_eval?;
        let in_grace = self.grace_from.map(|from| from + STARTUP_GRACE);
        [
            self.pending_idle.map(|_| last + IDLE_RECHECK),
            (self.dirty && in_grace.is_none()).then_some(last + EVAL_INTERVAL),
            in_grace,
            self.by_activity.then(|| self.activity.last.map(|at| at + BURST_GAP + Duration::from_millis(1))).flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// 到了该求值的时候就求值，返回对外的状态是否变了。`last_input` 是最近一次向程序发输入
    /// 的时刻；`screen` 读屏幕底部的文字，只在要比规则时才调用，读不到时为 `None`。
    pub fn poll(
        &mut self,
        now: Instant,
        last_input: Option<Instant>,
        rules: &RuleBook,
        screen: impl FnOnce() -> Option<String>,
    ) -> bool {
        if self.deadline().is_none_or(|at| at > now) {
            // 没有 agent 时 `deadline` 也是 `None`，但之前发布过的状态要撤掉。
            if self.kind().is_none() && self.published.is_some() {
                self.published = None;
                return true;
            }
            return false;
        }
        self.urgent = None;
        self.dirty = false;
        self.last_eval = Some(now);
        let next = self.evaluate(now, last_input, rules, screen);
        let changed = next != self.published;
        self.published = next;
        changed
    }

    fn kind(&self) -> Option<AgentKind> {
        self.process
            .or(self.title_agent.map(|agent| agent.kind))
            .or(self.status_agent)
            .or((self.progress.is_some() || !self.statuses.is_empty()).then_some(if self.pi_title {
                AgentKind::Pi
            } else {
                AgentKind::Other
            }))
            .or(self.carried.map(|agent| agent.kind))
    }

    /// OSC 7501 记录合起来的状态：有一条在等用户就是等用户，否则有一条在干活就是干活。
    fn status_state(&self) -> Option<AgentState> {
        [AgentState::Blocked, AgentState::Working, AgentState::Idle]
            .into_iter()
            .find(|state| self.statuses.iter().any(|(_, record)| record == state))
    }

    /// agent 在标题和进度里自己报告的状态。
    fn reported_state(&self) -> Option<AgentState> {
        match self.progress {
            Some(true) => Some(AgentState::Working),
            progress => self.title_agent.map(|agent| agent.state).or(progress.map(|_| AgentState::Idle)),
        }
    }

    fn forget_reports(&mut self) {
        self.title_agent = None;
        self.pi_title = false;
        self.progress = None;
        self.statuses.clear();
        self.status_agent = None;
        self.title_text.clear();
        self.progress_text.clear();
        self.pending_idle = None;
        self.by_activity = false;
    }

    /// 交接前的 agent 已经不在前台了，不再按它算。
    fn forget_carried(&mut self) {
        self.carried = None;
        self.held_burst = false;
    }

    fn evaluate(
        &mut self,
        now: Instant,
        last_input: Option<Instant>,
        rules: &RuleBook,
        screen: impl FnOnce() -> Option<String>,
    ) -> Option<Agent> {
        let Some(kind) = self.kind() else {
            self.pending_idle = None;
            self.by_activity = false;
            return None;
        };
        if let Some(state) = self.status_state() {
            self.pending_idle = None;
            self.by_activity = false;
            return Some(Agent { kind, state });
        }
        let reported = self.reported_state();
        if let Some(from) = self.grace_from {
            if now < from + STARTUP_GRACE {
                self.pending_idle = None;
                self.by_activity = false;
                return Some(Agent { kind, state: reported.unwrap_or(AgentState::Idle) });
            }
            self.grace_from = None;
        }

        let screen = rules.rules(kind).and_then(|set| Some((set, screen()?)));
        let verdict = screen.as_ref().map_or(Verdict::Unknown, |(set, screen)| {
            set.evaluate(Signals { screen, title: &self.title_text, progress: self.progress_text.latest() })
        });
        let (state, visible) = match verdict {
            Verdict::Matched { state, visible, .. } => (state, visible),
            Verdict::Hold { .. } => {
                self.pending_idle = None;
                self.by_activity = false;
                let kept = self.published.filter(|agent| agent.kind == kind).map(|agent| agent.state);
                return Some(Agent { kind, state: kept.or(reported).unwrap_or(AgentState::Idle) });
            }
            // agent 自己报告的状态算明摆着的。
            Verdict::Unknown => reported.map_or((AgentState::Idle, false), |state| (state, true)),
        };

        self.by_activity =
            state == AgentState::Idle && !visible && (self.held_burst || self.activity.sustained(now, last_input));
        let state = if self.by_activity { AgentState::Working } else { state };

        let was_working = self.published.is_some_and(|agent| agent.kind == kind && agent.is_working());
        if !(was_working && state == AgentState::Idle && !visible) {
            self.pending_idle = None;
            return Some(Agent { kind, state });
        }
        let pending = self.pending_idle.get_or_insert(PendingIdle { since: now, confirmations: 0 });
        if pending.since != now {
            pending.confirmations += 1;
        }
        if pending.confirmations >= IDLE_CONFIRMATIONS || now.saturating_duration_since(pending.since) >= IDLE_CAP {
            self.pending_idle = None;
            return Some(Agent { kind, state: AgentState::Idle });
        }
        Some(Agent { kind, state: AgentState::Working })
    }
}
