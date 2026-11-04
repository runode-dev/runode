//! 能写提交说明的 agent：存进预设的名字，菜单里的名字和 logo，各自只打印结果的调用方式。模型不在这里选，
//! 要换模型时在对话框的 CLI 参数里写，比如 `--model sonnet`。

use runode_agent::oneshot;
use runode_types::agent::AgentKind;

pub(crate) struct AgentSpec {
    /// 存进预设的名字。
    pub id: &'static str,
    pub label: &'static str,
    /// 画哪个 agent 的 logo。
    pub kind: AgentKind,
    pub cli: &'static oneshot::Cli,
}

/// 菜单里按这个顺序列，常用的在前。提示词里有别人写的文字（改动、最近的提交说明），只列确认关得掉全部
/// 工具（执行命令、读写文件）的 agent；别家要用时用户自己写命令（`CUSTOM_AGENT`）。
pub(crate) const AGENTS: &[AgentSpec] = &[
    AgentSpec { id: "claude", label: "Claude", kind: AgentKind::Claude, cli: &oneshot::CLAUDE },
    AgentSpec { id: "codex", label: "Codex", kind: AgentKind::Codex, cli: &oneshot::CODEX },
    AgentSpec { id: "pi", label: "Pi", kind: AgentKind::Pi, cli: &oneshot::PI },
    AgentSpec { id: "omp", label: "OMP", kind: AgentKind::Omp, cli: &oneshot::OMP },
];

/// 选它时不用上面哪一家，跑用户自己写的命令（`Recipe::command`）。
pub(crate) const CUSTOM_AGENT: &str = "custom";

/// 没选过时用的 agent。
pub(crate) const DEFAULT_AGENT: &str = "claude";

pub(crate) fn agent(id: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|agent| agent.id == id)
}
