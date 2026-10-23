//! 前台 agent 的识别：把前台进程组、屏幕底部的文字、标题和进度报告交给
//! `runode_agent::Tracker`，由它判断是哪个 agent、在干什么。
//!
//! 标题和进度在 `HostSession::feed` 里随输出交过去；前台进程组在 `HostSession::refresh_foreground`
//! 里认；屏幕文字只在 tracker 要比规则时才读。tracker 自己按时间节流，调用方在
//! `HostSession::agent_deadline` 到了时调 `HostSession::poll_agent`。

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

use runode_agent::{Foreground, RuleBook, conversation, identify_job};
use runode_types::agent::AgentKind;

use super::{HostSession, log_err};
use crate::{pty, vt::detection_text};

/// 前台程序认不出是 agent 时，它启动后这段时间里隔 `REPROBE_INTERVAL` 再认一次：node 写的
/// agent 启动后才改 argv[0]，npx 这类先起包装进程、再起真正的 agent。
const REPROBE_WINDOW: Duration = Duration::from_secs(10);
const REPROBE_INTERVAL: Duration = Duration::from_secs(1);
/// agent 在前台时隔这么久重读一次它记着的对话：它启动后才写对话文件，`/clear`、`/resume` 换对话时
/// 状态不一定变。
const CONVERSATION_RECHECK: Duration = Duration::from_secs(2);

/// 上次认前台进程组的结果。进程组没换时不再读各进程的参数。
#[derive(Debug, Default)]
pub(super) struct ForegroundProbe {
    leader: Option<crate::pty::Pid>,
    /// 这个进程组第一次被看到的时刻。
    since: Option<Instant>,
    probed_at: Option<Instant>,
    agent: Option<AgentKind>,
    /// 上次认的时候组里的进程，找 agent 记着的对话用。
    pids: Vec<u32>,
    conversation_read_at: Option<Instant>,
    /// 被节流跳过的那次重读补在什么时候：之后没有新输出也要补上，免得漏掉最后一次换对话。
    conversation_due: Option<Instant>,
}

/// 全部终端共用的识别规则，用户规则从 `runode_paths::Dirs::agent_detection_dir` 读。
fn rules() -> &'static RuleBook {
    static RULES: OnceLock<RuleBook> = OnceLock::new();
    RULES.get_or_init(|| RuleBook::new(runode_paths::Dirs::from_env().agent_detection_dir().as_deref()))
}

fn home() -> Option<&'static Path> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| runode_paths::Dirs::from_env().home).as_deref()
}

impl HostSession {
    /// 到了该重新判断前台 agent 的时候就判断，返回 `agent` 是否变了。
    pub fn poll_agent(&mut self) -> bool {
        let changed = self.poll_agent_at(Instant::now());
        self.meta_dirty |= changed;
        changed
    }

    /// 下次该调 `poll_agent` 的时刻：有新输出要看、正在确认 agent 是不是停下了、启动宽限期
    /// 快到了、要补读 agent 的对话等；没有要做的事时为 `None`。
    pub fn agent_deadline(&self) -> Option<Instant> {
        [self.agent_tracker.deadline(), self.foreground_probe.conversation_due].into_iter().flatten().min()
    }

    pub(super) fn poll_agent_at(&mut self, now: Instant) -> bool {
        let terminal = &self.terminal;
        let changed = self.agent_tracker.poll(now, self.input_at, rules(), || {
            log_err("read the screen for agent detection", detection_text(terminal)).flatten()
        });
        for warning in rules().take_warnings() {
            tracing::warn!("{warning}");
        }
        if changed {
            let agent = self.agent_tracker.agent();
            if agent.map(|agent| agent.kind) != self.agent.map(|agent| agent.kind) {
                self.conversation = None;
                self.foreground_probe.conversation_read_at = None;
            }
            self.agent = agent;
            tracing::debug!(agent = ?self.agent, "foreground agent changed");
        }
        self.recheck_conversation(now, home());
        changed
    }

    /// 前台有 agent 时按 `CONVERSATION_RECHECK` 重读它在进行的对话，变了就标脏。读不到（文件还没写、
    /// 正在重写）时留着上次的。
    fn recheck_conversation(&mut self, now: Instant, home: Option<&Path>) {
        let probe = &mut self.foreground_probe;
        probe.conversation_due = None;
        let (Some(agent), Some(home)) = (self.agent, home) else { return };
        if let Some(at) = probe.conversation_read_at
            && now.saturating_duration_since(at) < CONVERSATION_RECHECK
        {
            probe.conversation_due = Some(at + CONVERSATION_RECHECK);
            return;
        }
        probe.conversation_read_at = Some(now);
        let Some(found) = conversation::current(agent.kind, &probe.pids, home) else { return };
        if self.conversation.as_ref() != Some(&found) {
            self.conversation = Some(found);
            self.meta_dirty = true;
        }
    }

    /// 看前台在跑什么程序（`foreground` 是刚读的 `Pty::foreground`），认出是哪个 agent 交给
    /// tracker。取不到前台进程时什么都不做。
    pub(super) fn probe_foreground(&mut self, foreground: Option<(crate::pty::Pid, bool)>, now: Instant) {
        let Some((leader, is_shell)) = foreground else {
            return;
        };
        if is_shell {
            self.foreground_probe = ForegroundProbe::default();
            self.agent_tracker.foreground(Foreground::Shell, now);
            return;
        }
        let probe = &mut self.foreground_probe;
        let fresh = probe.leader != Some(leader);
        if fresh {
            *probe = ForegroundProbe { leader: Some(leader), since: Some(now), ..ForegroundProbe::default() };
        }
        let retry = probe.agent.is_none()
            && probe.since.is_some_and(|since| now.saturating_duration_since(since) < REPROBE_WINDOW)
            && probe.probed_at.is_none_or(|at| now.saturating_duration_since(at) >= REPROBE_INTERVAL);
        if fresh || retry {
            probe.probed_at = Some(now);
            let job = pty::process_group(leader);
            probe.agent = job.as_ref().and_then(identify_job);
            probe.pids = job.map(|job| job.processes.iter().map(|process| process.pid).collect()).unwrap_or_default();
        }
        self.agent_tracker.foreground(Foreground::Program(probe.agent), now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    fn text(session: &HostSession) -> String {
        detection_text(&session.terminal).unwrap().unwrap()
    }

    #[test]
    fn detection_text_is_the_bottom_of_the_active_area() {
        // 20 列 4 行。
        let mut session = idle_host();
        assert_eq!(text(&session), "");
        session.feed(b"one\r\n\r\nthree  ");
        assert_eq!(text(&session), "one\n\nthree\n");
        // 内容超过一屏：只要最后一屏。
        session.feed(b"\r\nfour\r\nfive\r\nsix");
        assert_eq!(text(&session), "three\nfour\nfive\nsix\n");
        // 用户翻到回滚历史里也不影响。
        session.terminal.scroll_viewport(libghostty_vt::terminal::ScrollViewport::Delta(-3));
        assert!(!session.terminal.viewport_active().unwrap());
        assert_eq!(text(&session), "three\nfour\nfive\nsix\n");
        // 清屏后内容只占上面一行：往上带上回滚历史凑满一屏。
        session.feed(b"\x1b[H\x1b[2Jtop");
        let shown = text(&session);
        assert!(shown.ends_with("top\n"), "{shown:?}");
        // 宽字符和超出一行的折行都按屏幕上的行算。
        session.feed("\x1b[H\x1b[2J中文\r\nabcdefghijklmnopqrstuvwxyz".as_bytes());
        assert!(text(&session).ends_with("中文\nabcdefghijklmnopqrst\nuvwxyz\n"), "{:?}", text(&session));
    }

    #[test]
    fn a_title_reported_agent_is_checked_against_its_rules() {
        use runode_agent::EVAL_INTERVAL;
        use runode_types::{
            agent::{Agent, AgentKind, AgentState},
            grid::GridSize,
        };

        let mut session = idle_host();
        session.resize(GridSize { cols: 60, rows: 8, cell_width_px: 8, cell_height_px: 16 });
        let claude = |state| Some(Agent { kind: AgentKind::Claude, state });
        // 标题说空闲，屏幕上摆着要不要执行命令的问题：等用户回答。
        assert!(
            session.feed(
                "\x1b]0;✳ 修 bug\x07 Do you want to proceed?\r\n ❯ 1. Yes\r\n   2. No\r\n Esc to cancel".as_bytes()
            )
        );
        assert_eq!(session.agent, claude(AgentState::Blocked));
        assert_eq!(session.agent_deadline(), None);
        // 回答完回到输入框：新输出节流，到点再看。
        let fed = Instant::now();
        assert!(!session.feed(b"\x1b[H\x1b[2J\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n\xe2\x9d\xaf\r\n\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80"));
        assert!(session.agent_deadline().is_some());
        assert!(session.poll_agent_at(fed + EVAL_INTERVAL));
        assert_eq!(session.agent, claude(AgentState::Idle));
    }

    /// 对话文件在认出 agent 之后才写、状态不变时换了对话号，都要跟上；读不到时不丢上次的。
    #[test]
    fn the_conversation_is_rechecked_without_a_state_change() {
        use runode_types::agent::{Agent, AgentState};

        let home = std::env::temp_dir().join(format!("runode-detect-conversation-{}", std::process::id()));
        let sessions = home.join(".claude").join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let file = sessions.join("4242.json");
        let write = |id: &str| std::fs::write(&file, format!(r#"{{"sessionId":"{id}"}}"#)).unwrap();
        let mut session = idle_host();
        session.agent = Some(Agent { kind: AgentKind::Claude, state: AgentState::Idle });
        session.foreground_probe.pids = vec![4242];
        let start = Instant::now();
        let id = |session: &HostSession| session.conversation.as_ref().map(|c| c.id.clone());

        session.recheck_conversation(start, Some(&home));
        assert_eq!(id(&session), None);
        write("first");
        session.meta_dirty = false;
        session.recheck_conversation(start + CONVERSATION_RECHECK / 2, Some(&home));
        assert_eq!(id(&session), None, "rechecks are throttled");
        assert_eq!(session.agent_deadline(), Some(start + CONVERSATION_RECHECK), "the skipped one is made up");
        session.recheck_conversation(start + CONVERSATION_RECHECK, Some(&home));
        assert_eq!(id(&session).as_deref(), Some("first"));
        assert!(session.meta_dirty);
        write("second");
        session.recheck_conversation(start + CONVERSATION_RECHECK * 2, Some(&home));
        assert_eq!(id(&session).as_deref(), Some("second"));
        std::fs::remove_file(&file).unwrap();
        session.recheck_conversation(start + CONVERSATION_RECHECK * 3, Some(&home));
        assert_eq!(id(&session).as_deref(), Some("second"));
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn detection_text_reads_the_whole_alternate_screen() {
        let mut session = idle_host();
        session.feed(b"shell\r\n\x1b[?1049h\x1b[2;1Hmenu\x1b[4;1Hfooter");
        assert_eq!(text(&session), "\nmenu\n\nfooter\n");
    }
}
