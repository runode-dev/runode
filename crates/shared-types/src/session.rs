//! 一个终端会话对外公布的状态：标签上的名字、前台 agent、所在目录和 shell 集成报告的东西。
//! 管着会话的一方随时把它发给连着的各个界面，界面不用自己去读 VT 或问操作系统。

use std::{ffi::OsString, path::PathBuf, sync::Arc};

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
    /// shell 最近一次等着输入时所在的目录，命令历史的建议按它挑在这个目录里用过的命令；还没
    /// 等过输入时为 `None`。插件管理器在提示符出来后可能临时切进别的目录，所以和 `cwd` 分开。
    pub prompt_cwd: Option<PathBuf>,
    /// 前台是不是 shell 自己，即没有命令在运行。
    pub foreground_is_shell: bool,
    /// shell 集成报告的 shell 自己的 PATH；补全跑生成器命令时用。PATH 不一定是合法的 UTF-8，
    /// 所以按操作系统的字符串存。
    pub shell_path: Option<OsString>,
    /// shell 集成报告的别名、函数、内建命令和关键字。列表可能很长，状态每次变了都要整份交出去，
    /// 所以共享着放：名字没变时各份状态指向同一份，不必每次深拷贝。
    pub shell_names: Arc<ShellNames>,
}
