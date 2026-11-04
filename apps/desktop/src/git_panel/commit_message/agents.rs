//! 能写提交说明的 agent：存进预设的名字，菜单里的名字和 logo，各自只打印结果的调用方式。模型不在这里选，
//! 要换模型时在对话框的 CLI 参数里写，比如 `--model sonnet`。

use runode_agent::oneshot;
use runode_types::agent::AgentKind;

use crate::window::agents::logo::DEEPSEEK_LOGO;

pub(crate) struct AgentSpec {
    /// 存进预设的名字。
    pub id: &'static str,
    pub label: &'static str,
    /// 画哪个 agent 的 logo；runode 不认识的 agent 为空，画 `logo`。
    pub kind: Option<AgentKind>,
    /// `kind` 为空时按原色画的 logo 文件。
    pub logo: Option<&'static str>,
    pub cli: &'static oneshot::Cli,
}

/// 菜单里按这个顺序列，常用的在前。提示词里有别人写的文字，只列关得掉执行命令和写文件的 agent：
/// Kimi 的只打印模式自动批准所有工具调用，Amp 的执行模式没有关工具的开关，都不列。
pub(crate) const AGENTS: &[AgentSpec] = &[
    AgentSpec { id: "claude", label: "Claude", kind: Some(AgentKind::Claude), logo: None, cli: &oneshot::CLAUDE },
    AgentSpec { id: "codex", label: "Codex", kind: Some(AgentKind::Codex), logo: None, cli: &oneshot::CODEX },
    AgentSpec { id: "pi", label: "Pi", kind: Some(AgentKind::Pi), logo: None, cli: &oneshot::PI },
    AgentSpec { id: "omp", label: "OMP", kind: Some(AgentKind::Omp), logo: None, cli: &oneshot::OMP },
    AgentSpec {
        id: "opencode",
        label: "OpenCode",
        kind: Some(AgentKind::OpenCode),
        logo: None,
        cli: &oneshot::OPENCODE,
    },
    AgentSpec {
        id: "opencode2",
        label: "OpenCode 2",
        kind: Some(AgentKind::OpenCode),
        logo: None,
        cli: &oneshot::OPENCODE2,
    },
    AgentSpec { id: "cursor", label: "Cursor", kind: Some(AgentKind::Cursor), logo: None, cli: &oneshot::CURSOR },
    AgentSpec { id: "muse", label: "Muse", kind: Some(AgentKind::Muse), logo: None, cli: &oneshot::MUSE },
    AgentSpec { id: "dsh", label: "DeepSeek Harness", kind: None, logo: Some(DEEPSEEK_LOGO), cli: &oneshot::DSH },
    AgentSpec {
        id: "copilot",
        label: "GitHub Copilot",
        kind: Some(AgentKind::GithubCopilot),
        logo: None,
        cli: &oneshot::COPILOT,
    },
    AgentSpec {
        id: "antigravity",
        label: "Antigravity",
        kind: Some(AgentKind::Antigravity),
        logo: None,
        cli: &oneshot::ANTIGRAVITY,
    },
];

/// 选它时不用上面哪一家，跑用户自己写的命令（`Recipe::command`）。
pub(crate) const CUSTOM_AGENT: &str = "custom";

/// 没选过时用的 agent。
pub(crate) const DEFAULT_AGENT: &str = "claude";

pub(crate) fn agent(id: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|agent| agent.id == id)
}
