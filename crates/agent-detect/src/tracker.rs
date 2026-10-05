//! 把各路信号合成前台 agent 的状态，并去抖。
//!
//! 信号有：前台进程认出来的 agent（`Tracker::foreground`）、标题开头的状态字符和 OSC 9;4 进度
//! （`Tracker::title`、`Tracker::progress`，见 `crate::title`）、屏幕底部的文字和输出活动
//! （`Tracker::output`）。合成的规则：
//!
//! - 是哪个 agent：前台进程认出来的优先，其次是标题前缀报告的，只有进度报告时是 pi（标题像
//!   pi 的）或 `AgentKind::Other`。都没有就不算有 agent。
//! - 什么状态：先用这种 agent 的识别规则（`RuleBook`）去比屏幕、标题和进度报告的原文；规则
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

use std::time::{Duration, Instant};

use runode_shared_types::agent::{Agent, AgentKind, AgentState};

use crate::{
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

    /// 程序的一段输出，进 VT 之前或之后都行。
    pub fn output(&mut self, bytes: &[u8], now: Instant) {
        if bytes.is_empty() {
            return;
        }
        self.progress_text.observe(bytes);
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

    /// 前台程序。回到 shell 时 agent 已经退出，它留下的标题和进度不再代表任何状态；换成
    /// 另一个 agent 时也不沿用上一个的。
    pub fn foreground(&mut self, foreground: Foreground, now: Instant) {
        match foreground {
            Foreground::Shell => {
                if self.process.is_some() || self.title_agent.is_some() || self.progress.is_some() {
                    self.urgent = Some(now);
                }
                self.process = None;
                self.grace_from = None;
                self.forget_reports();
            }
            Foreground::Program(kind) if kind != self.process => {
                if self.process.is_some() {
                    self.forget_reports();
                }
                self.process = kind;
                self.grace_from = kind.map(|_| now);
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
            .or(self.progress.map(|_| if self.pi_title { AgentKind::Pi } else { AgentKind::Other }))
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
        self.title_text.clear();
        self.progress_text.clear();
        self.pending_idle = None;
        self.by_activity = false;
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

        self.by_activity = state == AgentState::Idle && !visible && self.activity.sustained(now, last_input);
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

#[cfg(test)]
mod tests {
    use super::*;
    use AgentKind::*;
    use AgentState::*;

    fn agent(kind: AgentKind, state: AgentState) -> Option<Agent> {
        Some(Agent { kind, state })
    }

    /// 只有内置规则的测试环境。
    struct Fixture {
        tracker: Tracker,
        rules: RuleBook,
        t0: Instant,
        screen: String,
        last_input: Option<Instant>,
    }

    impl Fixture {
        fn new() -> Self {
            Self { tracker: Tracker::new(), rules: RuleBook::new(None), t0: Instant::now(), screen: String::new(), last_input: None }
        }

        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }

        fn poll(&mut self, ms: u64) -> Option<Agent> {
            let now = self.at(ms);
            let screen = self.screen.clone();
            self.tracker.poll(now, self.last_input, &self.rules, || Some(screen));
            self.tracker.agent()
        }

        fn output(&mut self, ms: u64, screen: &str) {
            self.screen = screen.into();
            self.tracker.output(b"x", self.at(ms));
        }

        /// 认出 `kind` 并等启动宽限期过去。
        fn started(kind: AgentKind) -> Self {
            let mut f = Self::new();
            f.tracker.foreground(Foreground::Program(Some(kind)), f.at(0));
            f.poll(0);
            f
        }
    }

    const GRACE_MS: u64 = 3000;

    #[test]
    fn title_and_progress_reports_work_without_a_known_process() {
        let mut f = Fixture::new();
        assert_eq!(f.tracker.title("⠋ 美化图标 | runode", f.at(0), || true), "美化图标 | runode");
        assert_eq!(f.poll(0), agent(Codex, Working));
        assert_eq!(f.tracker.title("✳ 美化图标", f.at(10), || true), "美化图标");
        assert_eq!(f.poll(10), agent(Claude, Idle));
        // 前台已经回到 shell，不带前缀的标题不再算 agent 的。
        f.tracker.title("~", f.at(20), || true);
        assert_eq!(f.poll(20), None);

        f.tracker.title("π - runode", f.at(30), || true);
        f.tracker.progress(true, f.at(30), || true);
        assert_eq!(f.poll(30), agent(Pi, Working));
        f.tracker.progress(false, f.at(40), || false);
        assert_eq!(f.poll(40), agent(Pi, Idle));
        f.tracker.progress(false, f.at(50), || true);
        assert_eq!(f.poll(50), None);
    }

    #[test]
    fn a_known_process_shows_up_idle_and_skips_the_screen_while_starting() {
        let mut f = Fixture::new();
        f.screen = "Do you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel\n".into();
        f.tracker.foreground(Foreground::Program(Some(Claude)), f.at(0));
        assert_eq!(f.poll(0), agent(Claude, Idle));
        // 宽限期里不看屏幕，期满时求值一次。
        assert_eq!(f.tracker.deadline(), Some(f.at(GRACE_MS)));
        assert_eq!(f.poll(GRACE_MS), agent(Claude, Blocked));
        assert_eq!(f.tracker.deadline(), None);
        f.tracker.foreground(Foreground::Shell, f.at(GRACE_MS + 10));
        assert_eq!(f.poll(GRACE_MS + 10), None);
    }

    #[test]
    fn plain_idle_after_working_needs_confirming() {
        let mut f = Fixture::started(Codex);
        let working = "• Working (3s • esc to interrupt)\n\n› \n";
        f.output(GRACE_MS, working);
        assert_eq!(f.poll(GRACE_MS), agent(Codex, Working));
        // 状态行消失、又没有别的证据：先按住，再看三次才算空闲。
        f.output(GRACE_MS + 300, "• Done\n\n› \n");
        assert_eq!(f.poll(GRACE_MS + 300), agent(Codex, Working));
        assert_eq!(f.tracker.deadline(), Some(f.at(GRACE_MS + 400)));
        assert_eq!(f.poll(GRACE_MS + 400), agent(Codex, Working));
        assert_eq!(f.poll(GRACE_MS + 500), agent(Codex, Working));
        assert_eq!(f.poll(GRACE_MS + 600), agent(Codex, Idle));
        assert_eq!(f.tracker.deadline(), None);
    }

    #[test]
    fn idle_confirmation_gives_up_waiting_after_the_cap() {
        let mut f = Fixture::started(Codex);
        f.output(GRACE_MS, "• Working (3s • esc to interrupt)\n\n› \n");
        f.poll(GRACE_MS);
        f.output(GRACE_MS + 300, "• Done\n\n› \n");
        f.poll(GRACE_MS + 300);
        // 中间没机会再看，过了上限直接算空闲。
        assert_eq!(f.poll(GRACE_MS + 300 + 700), agent(Codex, Idle));
    }

    #[test]
    fn visible_idle_and_blockers_count_at_once() {
        let mut f = Fixture::started(Claude);
        f.output(GRACE_MS, "✻ Thinking… (3s · esc to interrupt)\n");
        assert_eq!(f.poll(GRACE_MS), agent(Claude, Working));
        f.output(GRACE_MS + 300, "Bash command\n\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n");
        assert_eq!(f.poll(GRACE_MS + 300), agent(Claude, Blocked));
        f.output(GRACE_MS + 600, "done\n────────\n❯ \n────────\n  ? for shortcuts\n");
        assert_eq!(f.poll(GRACE_MS + 600), agent(Claude, Idle));
    }

    #[test]
    fn evaluation_is_throttled_and_only_follows_new_output() {
        let mut f = Fixture::started(Codex);
        f.output(GRACE_MS, "• Working (3s • esc to interrupt)\n\n› \n");
        f.poll(GRACE_MS);
        // 刚求过值：新输出要等到间隔满了再看。
        f.output(GRACE_MS + 50, "• Working (3s • esc to interrupt)\n\n› \n");
        assert_eq!(f.tracker.deadline(), Some(f.at(GRACE_MS) + EVAL_INTERVAL));
        let mut read = false;
        f.tracker.poll(f.at(GRACE_MS + 100), None, &f.rules, || {
            read = true;
            None
        });
        assert!(!read);
        f.tracker.poll(f.at(GRACE_MS + 250), None, &f.rules, || {
            read = true;
            Some(String::new())
        });
        assert!(read);
        // 没有新输出就不再看屏幕。
        assert_eq!(f.tracker.deadline(), Some(f.at(GRACE_MS + 250) + IDLE_RECHECK));
    }

    #[test]
    fn sustained_output_counts_as_working_unless_it_echoes_typing() {
        let mut f = Fixture::started(Omp);
        assert_eq!(f.poll(GRACE_MS), agent(Omp, Idle));
        // 用户敲回车之后连续输出了两秒。
        f.last_input = Some(f.at(GRACE_MS));
        for ms in (GRACE_MS + 100..=GRACE_MS + 2000).step_by(100) {
            f.output(ms, "");
            f.poll(ms);
        }
        assert_eq!(f.tracker.agent(), agent(Omp, Working));
        assert_eq!(f.poll(GRACE_MS + 2150), agent(Omp, Working));
        // 输出停了：过了 `BURST_GAP` 再确认几次就是空闲。
        let stop = GRACE_MS + 2000 + BURST_GAP.as_millis() as u64 + 1;
        assert_eq!(f.tracker.deadline(), Some(f.at(stop)));
        assert_eq!(f.poll(stop), agent(Omp, Working));
        f.poll(stop + 100);
        f.poll(stop + 200);
        assert_eq!(f.poll(stop + 300), agent(Omp, Idle));

        // 边打字边回显：输入一直在输出里面，不算干活。
        let base = stop + 1000;
        for ms in (base..=base + 2000).step_by(100) {
            f.last_input = Some(f.at(ms));
            f.output(ms + 5, "");
            f.poll(ms + 5);
        }
        assert_eq!(f.tracker.agent(), agent(Omp, Idle));
    }

    #[test]
    fn a_new_agent_does_not_inherit_the_old_reports() {
        let mut f = Fixture::started(Claude);
        f.tracker.title("◐ 修 bug", f.at(GRACE_MS), || false);
        assert_eq!(f.poll(GRACE_MS), agent(Claude, Working));
        f.tracker.foreground(Foreground::Program(Some(Codex)), f.at(GRACE_MS + 10));
        assert_eq!(f.poll(GRACE_MS + 10), agent(Codex, Idle));
        f.tracker.foreground(Foreground::Program(None), f.at(GRACE_MS + 20));
        assert_eq!(f.poll(GRACE_MS + 20), None);
    }

    #[test]
    fn skip_rules_keep_the_current_state() {
        let mut f = Fixture::started(Claude);
        f.output(GRACE_MS, "✻ Thinking… (3s · esc to interrupt)\n");
        assert_eq!(f.poll(GRACE_MS), agent(Claude, Working));
        f.output(GRACE_MS + 300, "history\n\nShowing detailed transcript · ctrl+o to toggle\n");
        assert_eq!(f.poll(GRACE_MS + 300), agent(Claude, Working));
    }

    #[test]
    fn skip_rules_end_the_wait_for_output_to_stop() {
        let mut f = Fixture::started(Claude);
        f.last_input = Some(f.at(GRACE_MS));
        for ms in (GRACE_MS + 100..=GRACE_MS + 2000).step_by(100) {
            f.output(ms, "");
            f.poll(ms);
        }
        assert_eq!(f.tracker.agent(), agent(Claude, Working));
        // 停在跳过求值的界面上：不再等输出停下，截止时刻不能一直停在过去。
        f.output(GRACE_MS + 2100, "history\n\nShowing detailed transcript · ctrl+o to toggle\n");
        let at = GRACE_MS + 2100 + BURST_GAP.as_millis() as u64 + 1;
        assert_eq!(f.poll(at), agent(Claude, Working));
        assert!(f.tracker.deadline().is_none_or(|deadline| deadline > f.at(at)));
    }
}
