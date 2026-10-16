//! 看板：派给 agent 的任务，和到点自动开一张新任务的定时（数据本身在 `runode_types::todo`）。
//! 宿主持有它们（存在自己的库里），前端经 `ClientMsg::ListTodos` 这几条消息读写。
//!
//! 卡片在看板的哪一列不存，由 `column` 按任务和它那个会话的状态推出来，所以列总和 agent 实际
//! 在干什么对得上；拖拽卡片能不能放、放下做什么由 `drop_action` 定。

use runode_types::agent::AgentState;
pub use runode_types::todo::{DEFAULT_AGENT, Schedule, Todo, TodoSpec, When};
use serde::{Deserialize, Serialize};

use crate::message::SessionInfo;

/// 开始后这么多秒内，agent 还没认出来或者还没开始干活也算进行中：会话要先等提示符、再打命令，
/// agent 起来、认出来都要时间。
pub const START_GRACE: u64 = 30;

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
