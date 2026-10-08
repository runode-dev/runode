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
    Aider,
    Goose,
    Crush,
    Auggie,
    ContinueCli,
    Junie,
    OpenHands,
    Trae,
    CodeBuddy,
    Iflow,
    Codebuff,
    MistralVibe,
    Jules,
    Plandex,
    Other,
}

/// 前台 agent 及其状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub kind: AgentKind,
    pub state: AgentState,
}

/// agent 自己报告的模型和用量，目前只有 Claude Code 经它的 statusLine 命令（`runode statusline`）报。
/// 报告里缺的项为 `None`。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AgentUsage {
    /// 模型的显示名，比如 `Opus`。
    pub model: Option<String>,
    /// 上一次请求占了多少上下文（输入加上缓存读写的 token）。
    pub context_tokens: Option<u64>,
    /// 上一次请求从提示词缓存里读的 token。
    pub cache_read_tokens: Option<u64>,
    /// 上一次请求写进提示词缓存的 token。
    pub cache_write_tokens: Option<u64>,
    /// 上一次请求输出的 token。
    pub output_tokens: Option<u64>,
    /// 上下文窗口有多大。
    pub context_window: Option<u64>,
    /// 这次会话累计的花费，百万分之一美元。
    pub cost_micro_usd: Option<u64>,
    /// 五小时用量限额用掉的百分比。
    pub five_hour_percent: Option<u8>,
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
    pub const ALL: [Self; 38] = [
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
        Self::Aider,
        Self::Goose,
        Self::Crush,
        Self::Auggie,
        Self::ContinueCli,
        Self::Junie,
        Self::OpenHands,
        Self::Trae,
        Self::CodeBuddy,
        Self::Iflow,
        Self::Codebuff,
        Self::MistralVibe,
        Self::Jules,
        Self::Plandex,
    ];

    /// 是认得的 agent，不是 `Other`（用 OSC 9;4 报进度的普通程序）。
    pub fn is_known(self) -> bool {
        self != Self::Other
    }

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
            Self::Aider => "aider",
            Self::Goose => "goose",
            Self::Crush => "crush",
            Self::Auggie => "auggie",
            Self::ContinueCli => "cn",
            Self::Junie => "junie",
            Self::OpenHands => "openhands",
            Self::Trae => "trae",
            Self::CodeBuddy => "codebuddy",
            Self::Iflow => "iflow",
            Self::Codebuff => "codebuff",
            Self::MistralVibe => "vibe",
            Self::Jules => "jules",
            Self::Plandex => "plandex",
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
            Self::Aider => "Aider",
            Self::Goose => "Goose",
            Self::Crush => "Crush",
            Self::Auggie => "Auggie",
            Self::ContinueCli => "Continue",
            Self::Junie => "Junie",
            Self::OpenHands => "OpenHands",
            Self::Trae => "Trae Agent",
            Self::CodeBuddy => "CodeBuddy Code",
            Self::Iflow => "iFlow CLI",
            Self::Codebuff => "Codebuff",
            Self::MistralVibe => "Mistral Vibe",
            Self::Jules => "Jules",
            Self::Plandex => "Plandex",
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
