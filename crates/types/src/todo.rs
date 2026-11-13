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
    /// agent 干完活停下来（空闲，不是在等回答）时由宿主标成完成；没有窗口开着它的终端时一并结束终端。
    #[serde(default)]
    pub auto_done: bool,
    /// 开始以后最多跑这么多分钟，过了还没做完就记成没做成（`Outcome::Failed`），没有窗口开着它的终端时结束终端；
    /// 为空时不限。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit: Option<u32>,
}

impl TodoSpec {
    /// 实际交给 agent 的提示词：没写时用标题。
    pub fn prompt(&self) -> &str {
        if self.prompt.trim().is_empty() { &self.title } else { &self.prompt }
    }

    /// 起 agent 时交给它的整段话：提示词，后面列上要用的 skill 和附上的文件；自动完成的任务没人盯着，另叫
    /// agent 做完时报结局（`runode todo report`，报的是 `Outcome`）。用英文写，各家 agent 都认得。
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
        if self.auto_done {
            text += "\n\nThis task runs unattended. When you are done, run `runode todo report ok \"<summary>\"` \
                     with a short summary of the result; `silent` instead of `ok` when there is nothing worth \
                     telling the user, `failed` with what went wrong when you could not finish.";
        }
        text
    }
}

/// 任务的结局：agent 做完时自己报的（`runode todo report`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 做完了，结果值得告诉用户。
    Ok,
    /// 做完了，没什么要告诉用户的：不推通知。
    Silent,
    /// 没做成：不自动标完成，留在需要你，推一条通知。
    Failed,
    /// 新版本加的、读不懂的结局。
    #[serde(other)]
    Unknown,
}

impl Outcome {
    pub const ALL: [Outcome; 3] = [Outcome::Ok, Outcome::Silent, Outcome::Failed];

    /// 线上和库里的写法；`Unknown` 为 `unknown`。
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Silent => "silent",
            Outcome::Failed => "failed",
            Outcome::Unknown => "unknown",
        }
    }

    /// `name` 的反面；认不得的为 `Unknown`。
    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|outcome| outcome.name() == name).unwrap_or(Outcome::Unknown)
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
    /// 这一次开始以后 agent 报的结局；没报时为空。重新开始时清掉，接着上次的对话时留着。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// 结果：agent 报结局时附的话，或者没报结局的自动完成任务结束终端前宿主读下的屏幕底部（这时
    /// `outcome` 为空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// 排着队：还没开始，等 `after` 那张任务做完、做着的 agent 少于看板的并发上限时由宿主开始。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
    /// 排队时要等哪张任务（`Todo::id`）先做完；为空时不等。等的那张没做成时这张也记成没做成，删掉了时不再等。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
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
    /// 停着：到点也不开任务；接着用时从那一刻算起，停着时错过的不补。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
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
    /// crontab 的五段写法：分、时、日、月、星期（0 和 7 都是星期日），每段是 `*`、数字、`a-b` 和 `/步长`
    /// 用逗号连起来；日和星期两段都不以 `*` 开头时哪个对上都算，有一段以 `*` 开头时两段都要对上。
    Cron { expr: String },
    /// 新版本加的、读不懂的种类；永远不会到点。
    #[serde(other)]
    Unknown,
}
