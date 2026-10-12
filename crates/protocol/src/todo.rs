//! 任务板：派给 agent 的任务，和到点自动开一张新任务的定时。宿主持有它们（存在自己的库里），
//! 前端经 `ClientMsg::ListTodos` 这几条消息读写。
//!
//! 开始一个任务时，宿主在任务的目录（或给它新开的工作树）里开一个会话，在提示符上跑
//! `<agent> "<提示词>"`。卡片在看板的哪一列不存，由 `column` 按任务和它那个会话的状态推出来，
//! 所以列总和 agent 实际在干什么对得上；拖拽卡片能不能放、放下做什么由 `drop_action` 定。

use std::path::PathBuf;

use runode_shared_types::agent::AgentState;
use serde::{Deserialize, Serialize};

use crate::message::{SessionId, SessionInfo};

/// 没写 agent 时跑的命令。
pub const DEFAULT_AGENT: &str = "claude";

/// 开始后这么多秒内，agent 还没认出来或者还没开始干活也算进行中：会话要先等提示符、再打命令，
/// agent 起来、认出来都要时间。
pub const START_GRACE: u64 = 30;

/// 任务要做的事，新建任务和定时都用它。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoSpec {
    pub title: String,
    /// 交给 agent 的提示词；为空时用标题，见 `TodoSpec::prompt`。
    #[serde(default)]
    pub prompt: String,
    /// 在哪个目录里做，绝对路径。
    pub dir: PathBuf,
    /// 跑哪个 agent：提示词前面的那段命令，比如 `claude`、`codex --full-auto`。
    #[serde(default = "default_agent")]
    pub agent: String,
    /// 给任务新开一个工作树和分支，不在 `dir` 里直接改；`dir` 要在 git 仓库里。
    #[serde(default)]
    pub worktree: bool,
}

impl TodoSpec {
    /// 实际交给 agent 的提示词：没写时用标题。
    pub fn prompt(&self) -> &str {
        if self.prompt.trim().is_empty() { &self.title } else { &self.prompt }
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
    pub spec: TodoSpec,
    pub created: u64,
    /// 什么时候开始的；还没开始时为空。
    #[serde(default)]
    pub started: Option<u64>,
    /// 开始时开的会话。会话可能已经没了，按 `column` 的规矩看。
    #[serde(default)]
    pub session: Option<SessionId>,
    /// 给它新开的工作树的根目录和分支；没开时为空。
    #[serde(default)]
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

/// 看板上的一列。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    /// 还没开始。
    Todo,
    /// agent 在干活。
    InProgress,
    /// 要用户看一眼：agent 在等回答、干完了停下来，或者会话已经没了。
    NeedsYou,
    /// 用户标成了完成。
    Done,
}

impl Column {
    /// 看板上从左到右的四列。
    pub const ALL: [Column; 4] = [Column::Todo, Column::InProgress, Column::NeedsYou, Column::Done];
}

/// `todo` 在看板的哪一列。`session` 是它那个会话现在的状态（`Todo::session` 对应的那个，没找到时为空），
/// `now` 是 Unix 时间的秒数。开始后 `START_GRACE` 秒内，agent 还没认出来或者空闲着也算进行中；
/// 过了这段时间，前台回到 shell（agent 退出了）或者 agent 停下来都算需要你。
pub fn column(todo: &Todo, session: Option<&SessionInfo>, now: u64) -> Column {
    if todo.done_at.is_some() {
        return Column::Done;
    }
    let Some(started) = todo.started else { return Column::Todo };
    let Some(info) = session.filter(|info| Some(info.id) == todo.session && !info.exited) else {
        return Column::NeedsYou;
    };
    // 时钟往回调过时 `started` 可能比现在晚，也算刚开始。
    let fresh = now < started.saturating_add(START_GRACE);
    match info.meta.agent.map(|agent| agent.state) {
        Some(AgentState::Working) => Column::InProgress,
        Some(AgentState::Blocked) => Column::NeedsYou,
        Some(AgentState::Idle) | None if fresh => Column::InProgress,
        // 前台是认不出的程序：多半是 agent 还在跑，只是没有识别规则。
        None if !info.meta.foreground_is_shell => Column::InProgress,
        _ => Column::NeedsYou,
    }
}

/// 把卡片拖到另一列（或同一列的别处）放下时要做的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAction {
    /// 还在同一列，只调先后。
    Reorder,
    /// 开始这个任务。
    Start,
    /// 标成完成。
    Finish,
    /// 取消完成。
    Reopen,
}

/// `todo` 拖到 `to` 列放下时做什么；做完以后它推出来的列不是 `to` 的（比如把需要你的拖进进行中、
/// 把开始过的拖回待办）不接，为空，卡片弹回原处。参数同 `column`。
pub fn drop_action(todo: &Todo, session: Option<&SessionInfo>, to: Column, now: u64) -> Option<DropAction> {
    let from = column(todo, session, now);
    match (from, to) {
        _ if from == to => Some(DropAction::Reorder),
        (_, Column::Done) => Some(DropAction::Finish),
        (Column::Done, _) => {
            let reopened = Todo { done_at: None, ..todo.clone() };
            (column(&reopened, session, now) == to).then_some(DropAction::Reopen)
        }
        // 刚开始的任务在宽限期里，一定算进行中。
        (Column::Todo, Column::InProgress) => Some(DropAction::Start),
        _ => None,
    }
}
