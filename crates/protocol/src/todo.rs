//! 看板：派给 agent 的任务，和到点自动开一张新任务的定时（数据本身在 `runode_types::todo`）。
//! 宿主持有它们（存在自己的库里），前端经 `ClientMsg::ListTodos` 这几条消息读写。
//!
//! 卡片在看板的哪一列不存，由 `column` 按任务和它那个会话的状态推出来，所以列总和 agent 实际
//! 在干什么对得上；拖拽卡片能不能放、放下做什么由 `drop_action` 定。完成要么是用户标的，要么是带
//! `TodoSpec::auto_done` 的任务做完后宿主标的（`finished_by_agent`）。

use runode_types::agent::AgentState;
pub use runode_types::todo::{DEFAULT_AGENT, Outcome, Schedule, Todo, TodoSpec, When};
use serde::{Deserialize, Serialize};

use crate::message::SessionInfo;

/// 开始后这么多秒内，agent 还没认出来或者还没开始干活也算进行中：会话要先等提示符、再打命令，
/// agent 起来、认出来都要时间。
pub const START_GRACE: u64 = 30;

/// 带 `TodoSpec::auto_done` 的任务，agent 空闲下来以后（`finished_by_agent` 一直为真的话）宿主最晚这么多秒内
/// 标完成。
pub const AUTO_DONE_WITHIN: u64 = 60;

/// `HostMsg::DoneTodos` 一页最多给这么多张任务。
pub const DONE_TODOS_PAGE: u32 = 100;

/// 这个版本的宿主认得的看板消息到哪一级（`HostMsg::Welcome::board`），前端发某一级的消息前先看宿主够不够：
/// 1 加了 `ClientMsg::ReportTodo`，2 认得 `TodoSpec::time_limit`（旧宿主收到时悄悄丢掉），3 加了
/// `ClientMsg::QueueTodo`、`ClientMsg::PauseSchedule`、`ClientMsg::SetMaxRunning`，认得 `When::Cron`。
pub const BOARD_LEVEL: u32 = 3;

/// 带 `TodoSpec::auto_done` 的任务的 agent 停在询问上这么多秒还没人答，宿主就记成没做成（`Outcome::Failed`），
/// 没有窗口开着它的终端时结束终端。
pub const BLOCKED_GIVE_UP: u64 = 30 * 60;

/// `Todo::result` 最多这么多个字。
pub const RESULT_LIMIT: usize = 4000;

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
    /// 标成了完成：用户标的，或者带 `TodoSpec::auto_done` 的任务做完后宿主标的。
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

/// 定时上次开出、还没完成的 `todo` 挡不挡着它下一次到点：没开始成、还在干活或者停在询问上时挡着，到点跳过，
/// 免得两个 agent 改同一处、也免得每到点就多一张开不起来的待办；试过几次都开始不了、记成没做成的不再挡。停在
/// 权限询问上的看板算「需要你」，可用户一答应它就接着改，所以也挡着。`session` 是它那个会话现在的状态，为空
/// 表示找不到那个会话；`now` 是 Unix 时间的秒数。
pub fn holds_schedule(todo: &Todo, session: Option<&SessionInfo>, now: u64) -> bool {
    let Some(id) = todo.session else { return todo.outcome != Some(Outcome::Failed) };
    session.filter(|info| info.id == id).is_some_and(|info| {
        column(todo, Some(info), now) == Column::InProgress
            || !info.exited && info.meta.agent.is_some_and(|agent| agent.state == AgentState::Blocked)
    })
}

/// 带 `TodoSpec::auto_done` 的 `todo` 是不是 agent 自己做完了：还没完成，会话还在，过了 `START_GRACE`，agent
/// 认得出、空闲着；报过做完了的（`Outcome::Ok`、`Outcome::Silent`）退回 shell 也算。报了没做成的、停在询问上、
/// 会话没了都不算。`session` 是它那个会话现在的状态，`now` 是 Unix 时间的秒数。
pub fn finished_by_agent(todo: &Todo, session: Option<&SessionInfo>, now: u64) -> bool {
    let reported = matches!(todo.outcome, Some(Outcome::Ok | Outcome::Silent));
    todo.spec.auto_done
        && (todo.outcome.is_none() || reported)
        && column(todo, session, now) == Column::NeedsYou
        && session.is_some_and(|info| {
            Some(info.id) == todo.session
                && !info.exited
                && match info.meta.agent {
                    Some(agent) => agent.state == AgentState::Idle,
                    None => reported && info.meta.foreground_is_shell,
                }
        })
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
