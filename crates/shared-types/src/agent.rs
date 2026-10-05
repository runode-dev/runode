//! claude、codex、pi 这类编程 agent 的种类和状态。

use std::time::Duration;

/// agent 当前的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    Idle,
    /// 停下来等用户回答：要不要执行命令、选哪一项、信不信任这个目录。
    Blocked,
}

/// 是哪个 agent；`Other` 是其他用 OSC 9;4 报告进度的程序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Pi,
    Claude,
    Codex,
    Gemini,
    Cursor,
    Devin,
    Antigravity,
    Cline,
    Omp,
    Mastracode,
    OpenCode,
    GithubCopilot,
    Kimi,
    Kiro,
    Droid,
    Amp,
    Grok,
    Hermes,
    Kilo,
    Qodercli,
    Qwen,
    Letta,
    Maki,
    Muse,
    Other,
}

/// 前台 agent 及其状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub kind: AgentKind,
    pub state: AgentState,
}

impl Agent {
    pub fn is_working(self) -> bool {
        self.state == AgentState::Working
    }

    pub fn is_blocked(self) -> bool {
        self.state == AgentState::Blocked
    }
}

const CLAUDE_FRAMES: [&str; 12] = ["·", "✢", "✳", "✶", "✻", "✽", "✽", "✻", "✶", "✳", "✢", "·"];
const CODEX_FRAMES: [&str; 2] = ["•", "◦"];
const BRAILLE_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl AgentKind {
    /// 认得的全部 agent，不含 `Other`。
    pub const ALL: [Self; 24] = [
        Self::Pi,
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::Devin,
        Self::Antigravity,
        Self::Cline,
        Self::Omp,
        Self::Mastracode,
        Self::OpenCode,
        Self::GithubCopilot,
        Self::Kimi,
        Self::Kiro,
        Self::Droid,
        Self::Amp,
        Self::Grok,
        Self::Hermes,
        Self::Kilo,
        Self::Qodercli,
        Self::Qwen,
        Self::Letta,
        Self::Maki,
        Self::Muse,
    ];

    /// 短名：识别规则文件按它命名（`<短名>.toml`），也是它最常见的命令名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Devin => "devin",
            Self::Antigravity => "agy",
            Self::Cline => "cline",
            Self::Omp => "omp",
            Self::Mastracode => "mastracode",
            Self::OpenCode => "opencode",
            Self::GithubCopilot => "copilot",
            Self::Kimi => "kimi",
            Self::Kiro => "kiro",
            Self::Droid => "droid",
            Self::Amp => "amp",
            Self::Grok => "grok",
            Self::Hermes => "hermes",
            Self::Kilo => "kilo",
            Self::Qodercli => "qodercli",
            Self::Qwen => "qwen",
            Self::Letta => "letta",
            Self::Maki => "maki",
            Self::Muse => "muse",
            Self::Other => "other",
        }
    }

    /// 给人看的名字。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Pi => "Pi",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::Cursor => "Cursor Agent",
            Self::Devin => "Devin",
            Self::Antigravity => "Antigravity",
            Self::Cline => "Cline",
            Self::Omp => "Oh My Pi",
            Self::Mastracode => "Mastra Code",
            Self::OpenCode => "OpenCode",
            Self::GithubCopilot => "GitHub Copilot",
            Self::Kimi => "Kimi Code",
            Self::Kiro => "Kiro",
            Self::Droid => "Droid",
            Self::Amp => "Amp",
            Self::Grok => "Grok",
            Self::Hermes => "Hermes",
            Self::Kilo => "Kilo Code",
            Self::Qodercli => "Qoder CLI",
            Self::Qwen => "Qwen Code",
            Self::Letta => "Letta Code",
            Self::Maki => "Maki",
            Self::Muse => "Muse",
            Self::Other => "Agent",
        }
    }

    /// 工作中的转圈各帧和每帧的时长，跟各 agent 自己界面里的工作动画一致；没有专门动画的
    /// 用盲文点阵转圈。
    pub fn spinner(self) -> (&'static [&'static str], Duration) {
        match self {
            Self::Claude => (&CLAUDE_FRAMES, Duration::from_millis(120)),
            Self::Codex => (&CODEX_FRAMES, Duration::from_millis(600)),
            _ => (&BRAILLE_FRAMES, Duration::from_millis(80)),
        }
    }

    /// 工作动画的颜色，跟 agent 自己界面里的一致；`None` 时沿用文字颜色。
    pub fn spinner_color(self) -> Option<u32> {
        match self {
            Self::Claude => Some(0xD77757),
            _ => None,
        }
    }
}
