//! 看板上的数据：派给 agent 的任务，和到点自动开一张新任务的定时。宿主持有它们，存在
//! 看板的库里，前端经宿主的消息读写；卡片在看板的哪一列另由协议按会话的状态推出来。
//!
//! 开始一个任务时，宿主在任务的目录（或给它新开的工作树）里开一个会话，在提示符上跑
//! `<agent> "<提示词>"`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::session::SessionId;

/// 没写 agent 时跑的命令。
pub const DEFAULT_AGENT: &str = "claude";

/// 任务要做的事，新建任务和定时都用它。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoSpec {
    pub title: String,
    /// 交给 agent 的提示词；为空时用标题，见 `TodoSpec::prompt`。
    #[serde(default)]
    pub prompt: String,
    /// 在哪个目录里做，绝对路径。
    #[serde(serialize_with = "crate::lossy_path::serialize")]
    pub dir: PathBuf,
    /// 跑哪个 agent：提示词前面的那段命令，比如 `claude`、`codex --full-auto`。
    #[serde(default = "default_agent")]
    pub agent: String,
    /// 给任务新开一个工作树和分支，不在 `dir` 里直接改；`dir` 要在 git 仓库里。
    #[serde(default)]
    pub worktree: bool,
    /// 要 agent 用的 skill 的名字，列进提示词，见 `TodoSpec::instructions`。
    #[serde(default)]
    pub skills: Vec<String>,
    /// 以哪个自定义 agent 的身份跑（Claude、Copilot 的 `--agent`）；为空时不指定，那家没有这个参数时不管它。
    #[serde(default)]
    pub subagent: Option<String>,
    /// 附上的文件，绝对路径，列进提示词。
    #[serde(default, serialize_with = "crate::lossy_path::serialize_vec")]
    pub attachments: Vec<PathBuf>,
    /// 不问权限、一路跑完：起 agent 时加上那家免确认的参数。
    #[serde(default)]
    pub auto: bool,
}

impl TodoSpec {
    /// 实际交给 agent 的提示词：没写时用标题。
    pub fn prompt(&self) -> &str {
        if self.prompt.trim().is_empty() { &self.title } else { &self.prompt }
    }

    /// 起 agent 时交给它的整段话：提示词，后面列上要用的 skill 和附上的文件。用英文写，各家 agent 都认得。
    pub fn instructions(&self) -> String {
        let mut text = self.prompt().to_owned();
        if !self.skills.is_empty() {
            text += &format!("\n\nUse these skills: {}.", self.skills.join(", "));
        }
        if !self.attachments.is_empty() {
            text += "\n\nAttached files:";
            for path in &self.attachments {
                text += &format!("\n- {}", path.display());
            }
        }
        text
    }
}

fn default_agent() -> String {
    DEFAULT_AGENT.to_owned()
}

/// 一张任务卡片。时间都是 Unix 时间的秒数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    /// 宿主给的标识，8 个小写十六进制数字。
    pub id: String,
    /// 宿主给的编号：从 1 起按加的先后，删掉了也不再给别的任务，界面和命令行上写成 `#5`。要标识的消息
    /// 也认编号（`5` 或 `#5`），见 `ClientMsg::GetTodo`。
    #[serde(default)]
    pub number: u64,
    pub spec: TodoSpec,
    pub created: u64,
    /// 什么时候开始的；还没开始时为空。
    #[serde(default)]
    pub started: Option<u64>,
    /// 开始时开的会话。会话可能已经没了。
    #[serde(default)]
    pub session: Option<SessionId>,
    /// 给它新开的工作树的根目录和分支；没开时为空。
    #[serde(default, serialize_with = "crate::lossy_path::serialize_opt")]
    pub worktree_dir: Option<PathBuf>,
    #[serde(default)]
    pub branch: Option<String>,
    /// 标成完成的时刻；没完成时为空。
    #[serde(default)]
    pub done_at: Option<u64>,
    /// 由哪个定时开出来的（`Schedule::id`）；用户自己加的为空。
    #[serde(default)]
    pub schedule: Option<String>,
    /// 先后：所有任务排成一列，各列里的卡片按它排，越小越靠前。新任务排在最后。
    #[serde(default)]
    pub rank: i64,
}

/// 一个定时：到点时按 `spec` 新建一张任务并开始它。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub spec: TodoSpec,
    pub when: When,
    /// 上一次到点的时刻；还没到过时是建好或者改了 `when` 的时刻，下一次从这里算起。
    pub last_fired: u64,
    /// 宿主按本地时间算好的下一次；不会再到点（一次性的已经过了、读不懂的 `when`）时为空。
    #[serde(default)]
    pub next_fire: Option<u64>,
}

/// 定时什么时候到点，按宿主所在电脑的本地时间。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum When {
    /// 只在 `at`（Unix 时间的秒数）到一次，到过以后这个定时就删掉。
    Once { at: u64 },
    /// 每天 `hour:minute`；`days` 不为 0 时只在这几天，第 0 位是星期日，第 6 位是星期六。
    Daily {
        hour: u8,
        minute: u8,
        #[serde(default)]
        days: u8,
    },
    /// 每隔 `minutes` 分钟。
    Every { minutes: u32 },
    /// 新版本加的、读不懂的种类；永远不会到点。
    #[serde(other)]
    Unknown,
}
