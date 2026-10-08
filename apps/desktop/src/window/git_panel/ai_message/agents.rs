//! 能写提交说明的 agent，以及各自不进交互、只打印结果的调用方式。参数照 Orca
//! （stablyai/orca 的 `COMMIT_MESSAGE_AGENT_SPECS`）：都关掉工具或者只读，不留会话记录。模型不在这里
//! 选，要换模型时在对话框的 CLI 参数里写，比如 `--model sonnet`。

use runode_shared_types::agent::AgentKind;

use crate::window::agents::logo::DEEPSEEK_LOGO;

/// `tail` 里换成提示词的占位；`tail` 里没有它时提示词经标准输入给（改动可能很大，命令行放不下）。
pub(super) const PROMPT: &str = "{prompt}";

/// agent 打印出来的是什么。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Output {
    /// 就是提交说明。
    Text,
    /// OpenCode 的 `--format json`：一行一个事件，说明在 `text` 事件里。
    OpenCodeEvents,
}

pub(in crate::window) struct AgentSpec {
    /// 存进预设的名字，和 Orca 的一样。
    pub id: &'static str,
    pub label: &'static str,
    /// 画哪个 agent 的 logo；runode 不认识的 agent 为空，画 `logo`。
    pub kind: Option<AgentKind>,
    /// `kind` 为空时按原色画的 logo 文件。
    pub logo: Option<&'static str>,
    pub binary: &'static str,
    /// 用户写的 CLI 参数前面的参数。
    pub args: &'static [&'static str],
    /// 用户写的 CLI 参数后面的参数：放提示词和必须排在最后的位置参数。
    pub tail: &'static [&'static str],
    pub output: Output,
}

/// 菜单里按这个顺序列，常用的在前。
pub(in crate::window) const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        id: "claude",
        label: "Claude",
        kind: Some(AgentKind::Claude),
        logo: None,
        binary: "claude",
        args: &["-p", "--output-format", "text", "--permission-mode", "plan", "--no-session-persistence"],
        tail: &[],
        output: Output::Text,
    },
    AgentSpec {
        id: "codex",
        label: "Codex",
        kind: Some(AgentKind::Codex),
        logo: None,
        binary: "codex",
        args: &["exec", "--ephemeral", "--skip-git-repo-check", "-s", "read-only"],
        tail: &[],
        output: Output::Text,
    },
    AgentSpec {
        id: "pi",
        label: "Pi",
        kind: Some(AgentKind::Pi),
        logo: None,
        binary: "pi",
        args: &["--print", "--no-session", "--no-tools", "--no-skills", "--no-context-files", "--mode", "text"],
        tail: &[],
        output: Output::Text,
    },
    AgentSpec {
        id: "omp",
        label: "OMP",
        kind: Some(AgentKind::Omp),
        logo: None,
        binary: "omp",
        args: &[
            "--print",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-rules",
            "--mode",
            "text",
        ],
        tail: &[],
        output: Output::Text,
    },
    AgentSpec {
        id: "opencode",
        label: "OpenCode",
        kind: Some(AgentKind::OpenCode),
        logo: None,
        binary: "opencode",
        args: &["run", "--agent", "build", "--format", "json"],
        tail: &[],
        output: Output::OpenCodeEvents,
    },
    AgentSpec {
        id: "opencode2",
        label: "OpenCode 2",
        kind: Some(AgentKind::OpenCode),
        logo: None,
        binary: "opencode2",
        args: &["run", "--agent", "build", "--format", "json"],
        tail: &[],
        output: Output::OpenCodeEvents,
    },
    AgentSpec {
        id: "amp",
        label: "Amp",
        kind: Some(AgentKind::Amp),
        logo: None,
        binary: "amp",
        args: &["--execute", "--no-notifications", "--no-ide", "--no-jetbrains"],
        tail: &[],
        output: Output::Text,
    },
    AgentSpec {
        id: "cursor",
        label: "Cursor",
        kind: Some(AgentKind::Cursor),
        logo: None,
        binary: "cursor-agent",
        args: &["--print", "--mode", "ask", "--trust", "--output-format", "text"],
        tail: &[PROMPT],
        output: Output::Text,
    },
    AgentSpec {
        id: "kimi",
        label: "Kimi",
        kind: Some(AgentKind::Kimi),
        logo: None,
        binary: "kimi",
        // kimi 只认 `--prompt` 带的提示词，不读标准输入。
        args: &["--quiet"],
        tail: &["--prompt", PROMPT],
        output: Output::Text,
    },
    AgentSpec {
        id: "muse",
        label: "Muse",
        kind: Some(AgentKind::Muse),
        logo: None,
        binary: "muse",
        args: &[
            "exec",
            "--no-session-log",
            "--approval-mode",
            "never",
            "--disable-sandbox",
            "--disable-shell",
            "--disable-write",
            "--disable-web-tools",
        ],
        tail: &["--", PROMPT],
        output: Output::Text,
    },
    AgentSpec {
        id: "dsh",
        label: "DeepSeek Harness",
        kind: None,
        logo: Some(DEEPSEEK_LOGO),
        binary: "dsh",
        // `-` 是 dsh 明说的「从标准输入读」，不写它时就算标准输入是管道也报缺任务。
        args: &["--profile", "headless"],
        tail: &["-"],
        output: Output::Text,
    },
    AgentSpec {
        id: "copilot",
        label: "GitHub Copilot",
        kind: Some(AgentKind::GithubCopilot),
        logo: None,
        binary: "copilot",
        args: &["--silent", "--stream", "off", "--no-custom-instructions"],
        tail: &["--prompt", PROMPT],
        output: Output::Text,
    },
    AgentSpec {
        id: "antigravity",
        label: "Antigravity",
        kind: Some(AgentKind::Antigravity),
        logo: None,
        binary: "agy",
        args: &["--sandbox"],
        // 写成 `--print=…`：提示词以 `-` 开头时也归 `--print`，不被当成别的选项。
        tail: &["--print={prompt}"],
        output: Output::Text,
    },
];

/// 选它时不用上面哪一家，跑用户自己写的命令（`Recipe::command`）。
pub(in crate::window) const CUSTOM_AGENT: &str = "custom";

/// 没选过时用的 agent。
pub(in crate::window) const DEFAULT_AGENT: &str = "claude";

pub(in crate::window) fn agent(id: &str) -> Option<&'static AgentSpec> {
    AGENTS.iter().find(|agent| agent.id == id)
}

impl AgentSpec {
    /// 完整的参数：自己的参数、用户写的参数、末尾的参数，见 `with_prompt`。
    pub(super) fn command_line(&self, user_args: Vec<String>, prompt: &str) -> (Vec<String>, Option<String>) {
        let mut args: Vec<String> = self.args.iter().map(|&arg| arg.to_owned()).collect();
        args.extend(user_args);
        args.extend(self.tail.iter().map(|&arg| arg.to_owned()));
        with_prompt(args, prompt)
    }
}

/// 把参数里的 `PROMPT` 换成 `prompt`，再给出要经标准输入给的提示词：参数里放了提示词时为空。
pub(super) fn with_prompt(args: Vec<String>, prompt: &str) -> (Vec<String>, Option<String>) {
    let on_argv = args.iter().any(|arg| arg.contains(PROMPT));
    let args = args.into_iter().map(|arg| arg.replace(PROMPT, prompt)).collect();
    (args, (!on_argv).then(|| prompt.to_owned()))
}
