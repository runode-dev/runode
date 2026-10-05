//! claude、codex、pi 这类编程 agent 的状态。

use std::time::Duration;

/// agent 当前的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Idle,
}

/// 是哪个 agent；`Other` 是其他用 OSC 9;4 报告进度的程序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Codex,
    Pi,
    Other,
}

/// 前台 agent 及其状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agent {
    pub kind: AgentKind,
    pub state: AgentState,
}

impl Agent {
    pub fn is_working(self) -> bool {
        self.state == AgentState::Working
    }
}

const CLAUDE_FRAMES: [&str; 12] = ["·", "✢", "✳", "✶", "✻", "✽", "✽", "✻", "✶", "✳", "✢", "·"];
const CODEX_FRAMES: [&str; 2] = ["•", "◦"];
const BRAILLE_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl AgentKind {
    /// 工作中的转圈各帧和每帧的时长，跟各 agent 自己界面里的工作动画一致。
    pub fn spinner(self) -> (&'static [&'static str], Duration) {
        match self {
            Self::Claude => (&CLAUDE_FRAMES, Duration::from_millis(120)),
            Self::Codex => (&CODEX_FRAMES, Duration::from_millis(600)),
            Self::Pi | Self::Other => (&BRAILLE_FRAMES, Duration::from_millis(80)),
        }
    }

    /// 工作动画的颜色，跟 agent 自己界面里的一致；`None` 时沿用文字颜色。
    pub fn spinner_color(self) -> Option<u32> {
        match self {
            Self::Claude => Some(0xD77757),
            Self::Codex | Self::Pi | Self::Other => None,
        }
    }
}
