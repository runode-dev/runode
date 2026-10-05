//! 一个终端会话对外公布的状态：标签上的名字、前台 agent、所在目录和 shell 集成报告的东西。
//! 管着会话的一方随时把它发给连着的各个界面，界面不用自己去读 VT 或问操作系统。

use std::{ffi::OsString, path::PathBuf};

use crate::{agent::Agent, shell::ShellNames};

/// 一个终端会话对外公布的状态。缺的字段按默认值读，以后加字段时旧的一方照样能读。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    /// 程序设置的标题，agent 的状态前缀已经拆到 `agent` 里；没设置时为 `None`。
    pub title: Option<String>,
    /// 程序没设置标题时用的名字：前台程序名，或者 shell 所在目录的名字。
    pub fallback_title: Option<String>,
    /// 前台 agent 和它的状态；不是 agent 在前台时为 `None`。
    pub agent: Option<Agent>,
    /// shell 当前所在的目录。
    pub cwd: Option<PathBuf>,
    /// 前台是不是 shell 自己，即没有命令在运行。
    pub foreground_is_shell: bool,
    /// shell 集成报告的 shell 自己的 PATH；补全跑生成器命令时用。PATH 不一定是合法的 UTF-8，
    /// 所以按操作系统的字符串存。
    pub shell_path: Option<OsString>,
    /// shell 集成报告的别名、函数、内建命令和关键字。
    pub shell_names: ShellNames,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentKind, AgentState};

    #[test]
    fn missing_fields_read_as_defaults() {
        let meta: SessionMeta = serde_json::from_str(r#"{"title":"vim","unknown":1}"#).unwrap();
        assert_eq!(meta, SessionMeta { title: Some("vim".into()), ..SessionMeta::default() });
    }

    #[test]
    fn round_trips_through_json() {
        let meta = SessionMeta {
            title: Some("修 bug".into()),
            fallback_title: Some("runode".into()),
            agent: Some(Agent { kind: AgentKind::GithubCopilot, state: AgentState::Blocked }),
            cwd: Some("/tmp/中文".into()),
            foreground_is_shell: false,
            shell_path: Some("/usr/bin:/bin".into()),
            shell_names: ShellNames { aliases: vec!["ll".into()], ..ShellNames::default() },
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains(r#""kind":"github_copilot","state":"blocked""#), "{json}");
        assert_eq!(serde_json::from_str::<SessionMeta>(&json).unwrap(), meta);
    }
}
