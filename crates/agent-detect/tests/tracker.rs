//! `Tracker` 按前台进程、标题、进度报告和屏幕文字认出 agent 的状态，并去抖。

use std::time::{Duration, Instant};

use AgentKind::*;
use AgentState::*;
use runode_agent_detect::{BURST_GAP, EVAL_INTERVAL, Foreground, IDLE_RECHECK, RuleBook, Tracker};
use runode_shared_types::agent::{Agent, AgentKind, AgentState};

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
        Self {
            tracker: Tracker::new(),
            rules: RuleBook::new(None),
            t0: Instant::now(),
            screen: String::new(),
            last_input: None,
        }
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

/// 交接后接着交接前的状态：前台进程这时才认出来也不等启动宽限期，交接期间没有输出也不撤
/// 按输出活动判的工作中，一次都不变；输出重新流起来后停下，照常确认空闲。
#[test]
fn a_resumed_agent_keeps_its_state_across_the_handoff() {
    let mut f = Fixture::new();
    // 导入时先喂眼下的信号（这里是前台进程），再接着交接前的状态。
    f.tracker.foreground(Foreground::Program(Some(Omp)), f.at(0));
    f.tracker.resume(agent(Omp, Working), f.at(0));
    assert_eq!(f.tracker.agent(), agent(Omp, Working));
    // 立即求值一次，看的是屏幕，不是宽限期里的「空闲」。
    assert_eq!(f.tracker.deadline(), Some(f.at(0)));
    let mut changed = Vec::new();
    for ms in (0..=2000).step_by(100) {
        changed.push(f.tracker.poll(f.at(ms), None, &f.rules, || Some(String::new())));
    }
    assert!(changed.iter().all(|changed| !changed), "{changed:?}");
    assert_eq!(f.tracker.agent(), agent(Omp, Working));
    // 读线程打开了：输出接着来就一直是工作中。
    f.tracker.output_resumed(f.at(2000));
    for ms in (2100..=3000).step_by(100) {
        f.output(ms, "");
        assert_eq!(f.poll(ms), agent(Omp, Working));
    }
    // 输出停了：过了 `BURST_GAP` 再确认几次就是空闲。
    let stop = 3000 + BURST_GAP.as_millis() as u64 + 1;
    assert_eq!(f.poll(stop), agent(Omp, Working));
    f.poll(stop + 100);
    f.poll(stop + 200);
    assert_eq!(f.poll(stop + 300), agent(Omp, Idle));
}

/// 交接时丢了的信号认不出 agent 时按交接前的算；它的进程后来才认出来也不算刚启动；回到 shell
/// 就不再算。
#[test]
fn a_resumed_agent_survives_lost_signals_until_the_shell_is_back() {
    let mut f = Fixture::new();
    f.screen = "Do you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel\n".into();
    f.tracker.resume(agent(Claude, Blocked), f.at(0));
    assert_eq!(f.poll(0), agent(Claude, Blocked));
    f.tracker.foreground(Foreground::Program(Some(Claude)), f.at(10));
    assert_eq!(f.poll(10), agent(Claude, Blocked));
    assert_eq!(f.tracker.deadline(), None);
    f.tracker.foreground(Foreground::Shell, f.at(20));
    assert_eq!(f.poll(20), None);

    // 换成了另一个 agent：不再沿用交接前的，新的照常从启动宽限期开始。
    let mut f = Fixture::new();
    f.tracker.resume(agent(Claude, Working), f.at(0));
    f.tracker.foreground(Foreground::Program(Some(Codex)), f.at(10));
    assert_eq!(f.poll(10), agent(Codex, Idle));
    assert_eq!(f.tracker.deadline(), Some(f.at(10 + GRACE_MS)));
}

#[test]
fn program_status_records_are_kept_per_id() {
    let mut f = Fixture::new();
    let t = f.at(0);
    f.tracker.foreground(Foreground::Program(None), t);
    // 认不出 `app` 的程序算 `Other`；报告的状态盖过屏幕和输出活动。
    f.tracker.program_status("build", "cargo", Some(Working), t);
    f.tracker.program_status("builder", "cargo", Some(Blocked), t);
    assert_eq!(f.poll(0), agent(Other, Blocked));
    // 清掉 `build` 不连带同名开头的 `builder`。
    f.tracker.program_status("build", "", None, f.at(10));
    assert_eq!(f.poll(10), agent(Other, Blocked));
    f.tracker.program_status("builder", "", Some(Idle), f.at(20));
    assert_eq!(f.poll(20), agent(Other, Idle));
    // 回到 shell 时记录作废。
    f.tracker.foreground(Foreground::Shell, f.at(30));
    assert_eq!(f.poll(30), None);
}

#[test]
fn program_status_does_not_keep_an_expired_startup_grace_due() {
    let mut f = Fixture::new();
    f.tracker.foreground(Foreground::Program(Some(Pi)), f.at(0));
    f.tracker.program_status("a", "pi", Some(Working), f.at(0));
    assert_eq!(f.poll(0), agent(Pi, Working));
    let after = f.at(GRACE_MS + 10);
    assert_eq!(f.poll(GRACE_MS + 10), agent(Pi, Working));
    // 宽限期过了，下次求值的时刻不能停在过去，否则宿主的等待超时恒为 0、空转。
    assert!(f.tracker.deadline().is_none_or(|at| at >= after));
}
