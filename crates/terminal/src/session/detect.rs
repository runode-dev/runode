//! 前台 agent 的识别：把前台进程组、屏幕底部的文字、标题和进度报告交给
//! `runode_agent_detect::Tracker`，由它判断是哪个 agent、在干什么。
//!
//! 标题和进度在 `Session::feed` 里随输出交过去；前台进程组在 `Session::refresh_fallback_title`
//! 里认；屏幕文字只在 tracker 要比规则时才读。tracker 自己按时间节流，调用方在
//! `Session::agent_deadline` 到了时调 `Session::poll_agent`。

use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use libghostty_vt::{
    Terminal,
    error::Result,
    fmt::{Format, Formatter, FormatterOptions},
    screen::Screen,
    selection::Selection,
    terminal::{Point, PointCoordinate},
};
use runode_agent_detect::{Foreground, RuleBook, identify_job};
use runode_shared_types::agent::AgentKind;

use super::{Session, log_err};
use crate::pty;

/// 前台程序认不出是 agent 时，它启动后这段时间里隔 `REPROBE_INTERVAL` 再认一次：node 写的
/// agent 启动后才改 argv[0]，npx 这类先起包装进程、再起真正的 agent。
const REPROBE_WINDOW: Duration = Duration::from_secs(10);
const REPROBE_INTERVAL: Duration = Duration::from_secs(1);

/// 上次认前台进程组的结果。进程组没换时不再读各进程的参数。
#[derive(Debug, Default)]
pub(super) struct ForegroundProbe {
    leader: Option<libc::pid_t>,
    /// 这个进程组第一次被看到的时刻。
    since: Option<Instant>,
    probed_at: Option<Instant>,
    agent: Option<AgentKind>,
}

/// 全部终端共用的识别规则，用户规则从 `runode_paths::Dirs::agent_detection_dir` 读。
fn rules() -> &'static RuleBook {
    static RULES: OnceLock<RuleBook> = OnceLock::new();
    RULES.get_or_init(|| RuleBook::new(runode_paths::Dirs::from_env().agent_detection_dir().as_deref()))
}

impl Session {
    /// 到了该重新判断前台 agent 的时候就判断，返回 `agent` 是否变了。
    pub fn poll_agent(&mut self) -> bool {
        self.poll_agent_at(Instant::now())
    }

    /// 下次该调 `poll_agent` 的时刻：有新输出要看、正在确认 agent 是不是停下了、启动宽限期
    /// 快到了等；没有要做的事时为 `None`。
    pub fn agent_deadline(&self) -> Option<Instant> {
        self.agent_tracker.deadline()
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
            self.agent = self.agent_tracker.agent();
            tracing::debug!(agent = ?self.agent, "foreground agent changed");
        }
        changed
    }

    /// 看前台在跑什么程序，认出是哪个 agent 交给 tracker。取不到前台进程时什么都不做。
    pub(super) fn probe_foreground(&mut self, now: Instant) {
        let Some((leader, is_shell)) = self.pty.foreground() else {
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
            probe.agent = pty::process_group(leader).as_ref().and_then(identify_job);
        }
        self.agent_tracker.foreground(Foreground::Program(probe.agent), now);
    }
}

/// 屏幕底部一屏高的文字，给识别规则用：一行一个 `\n`，行尾空白去掉，末尾的空行去掉。
///
/// 读的是活动区，不管用户把视口翻到了哪里。主屏幕上以最后一个有字的行和光标所在行中靠下
/// 的那行为底，往上取一屏高，内容没占满一屏时会带上回滚历史里的几行；备用屏幕上就是整屏。
fn detection_text(terminal: &Terminal<'_, '_>) -> Result<Option<String>> {
    let rows = usize::from(terminal.rows()?);
    let total = terminal.total_rows()?;
    if rows == 0 || total == 0 {
        return Ok(None);
    }
    let active_top = total.saturating_sub(rows);
    let alternate = terminal.active_screen()? == Screen::Alternate;
    // 主屏幕上底可能往上移，最多再多读一屏回滚历史。
    let first = if alternate { active_top } else { active_top.saturating_sub(rows) };
    let lines = screen_lines(terminal, first, total - 1)?;
    let line = |row: usize| lines.get(row - first).map_or("", String::as_str);
    let bottom = if alternate {
        total - 1
    } else {
        let cursor = active_top + usize::from(terminal.cursor_y()?);
        (active_top..total).rev().find(|&row| !line(row).trim().is_empty()).map_or(total - 1, |row| row.max(cursor))
    };
    let top = (bottom + 1).saturating_sub(rows).max(first);
    let mut picked: Vec<&str> = (top..=bottom).map(line).collect();
    while picked.last().is_some_and(|line| line.trim().is_empty()) {
        picked.pop();
    }
    if picked.is_empty() {
        return Ok(Some(String::new()));
    }
    let mut text = picked.join("\n");
    text.push('\n');
    Ok(Some(text))
}

/// 整个屏幕（含回滚历史）第 `first` 到 `last` 行的纯文字，一行一项，行尾空白去掉。软换行
/// 不接起来，和屏幕上的行一一对应；末尾的空行可能没有。
fn screen_lines(terminal: &Terminal<'_, '_>, first: usize, last: usize) -> Result<Vec<String>> {
    let cols = terminal.cols()?;
    let point = |x: u16, y: usize| Point::Screen(PointCoordinate { x, y: u32::try_from(y).unwrap_or(u32::MAX) });
    let selection = Selection::new(terminal.grid_ref(point(0, first))?, terminal.grid_ref(point(cols.saturating_sub(1), last))?, false);
    let options = FormatterOptions::new()
        .with_format(Format::Plain)
        .with_unwrap(false)
        .with_trim(true)
        .with_selection(&selection);
    let bytes = Formatter::new(terminal, options)?.format_alloc(None)?.to_vec();
    Ok(String::from_utf8_lossy(&bytes).split('\n').map(|line| line.trim_end().to_owned()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testing::*;

    fn text(session: &Session) -> String {
        detection_text(&session.terminal).unwrap().unwrap()
    }

    #[test]
    fn detection_text_is_the_bottom_of_the_active_area() {
        // 20 列 4 行。
        let mut session = idle_session();
        assert_eq!(text(&session), "");
        session.feed(b"one\r\n\r\nthree  ");
        assert_eq!(text(&session), "one\n\nthree\n");
        // 内容超过一屏：只要最后一屏。
        session.feed(b"\r\nfour\r\nfive\r\nsix");
        assert_eq!(text(&session), "three\nfour\nfive\nsix\n");
        // 用户翻到回滚历史里也不影响。
        assert!(session.scroll_smoothly(3.));
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
        use runode_agent_detect::EVAL_INTERVAL;
        use runode_shared_types::{
            agent::{Agent, AgentKind, AgentState},
            grid::GridSize,
        };

        let mut session = idle_session();
        session.resize(GridSize { cols: 60, rows: 8, cell_width_px: 8, cell_height_px: 16 });
        let claude = |state| Some(Agent { kind: AgentKind::Claude, state });
        // 标题说空闲，屏幕上摆着要不要执行命令的问题：等用户回答。
        assert!(session.feed(
            "\x1b]0;✳ 修 bug\x07 Do you want to proceed?\r\n ❯ 1. Yes\r\n   2. No\r\n Esc to cancel".as_bytes()
        ));
        assert_eq!(session.agent, claude(AgentState::Blocked));
        assert_eq!(session.agent_deadline(), None);
        // 回答完回到输入框：新输出节流，到点再看。
        let fed = Instant::now();
        assert!(!session.feed(b"\x1b[H\x1b[2J\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n\xe2\x9d\xaf\r\n\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80"));
        assert!(session.agent_deadline().is_some());
        assert!(session.poll_agent_at(fed + EVAL_INTERVAL));
        assert_eq!(session.agent, claude(AgentState::Idle));
    }

    #[test]
    fn detection_text_reads_the_whole_alternate_screen() {
        let mut session = idle_session();
        session.feed(b"shell\r\n\x1b[?1049h\x1b[2;1Hmenu\x1b[4;1Hfooter");
        assert_eq!(text(&session), "\nmenu\n\nfooter\n");
    }
}
