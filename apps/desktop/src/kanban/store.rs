//! 看板的数据：宿主手里的任务和定时（`ClientMsg::ListTodos`），和任务会话现在的状态，全 app 一份；记录页翻的
//! 完成了的任务各个看板自己问（`Kanban::reload_history`）。
//!
//! 和宿主说话经 app 在启动时交进来的 `Host`（`watch`）。启动时问一次宿主，这一问也让宿主之后在看板变了时
//! 说一声（app 交进来的通知），说了就重问；换了一条连接也重问。卡片在哪一列要看任务会话里 agent 的状态，
//! 宿主在没做完的任务的会话里 agent 的状态、前台是不是 shell 变了或者 shell 退出时也说一声，所以每次连同
//! 宿主里的会话一起问，全留着（记录页上完成了七天以上的任务也要看会话还在不在），不轮询；不 attach，
//! 免得宿主把它们算成有界面连着。
//! 数据变了才重画开着的看板，各窗口只在需要你的数（`needs_you`）变了时重画；刚开始的任务过宽限期时列会变、
//! 数据却不变，所以排一个定时器等到那时（`wait_for_grace`）。
//! 请求都在后台线程上等宿主回话。

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Result, anyhow};
use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui::{App, Global, Task, WeakEntity};
use runode_protocol::{
    ClientMsg, HostMsg, SessionId, SessionInfo,
    todo::{Column, START_GRACE, Schedule, Todo, column},
};

use crate::kanban::Kanban;

#[derive(Clone, Default, PartialEq)]
pub(crate) struct TodoData {
    /// 按先后（`Todo::rank`）排好的任务，完成了的只有最近七天的。
    pub(crate) todos: Vec<Todo>,
    /// 按建的先后排好的定时。
    pub(crate) schedules: Vec<Schedule>,
    /// 宿主里的会话现在的样子，任务按标识来查；不在里面的会话已经没了。
    pub(crate) sessions: HashMap<SessionId, SessionInfo>,
    /// 问不到看板的说明；请求没办成的原因由调用方弹提示条说。
    pub(crate) error: Option<String>,
}

impl Global for TodoData {}

/// 全 app 的看板数据；还没问到时是空的。
pub(crate) fn data(cx: &App) -> &TodoData {
    static EMPTY: std::sync::OnceLock<TodoData> = std::sync::OnceLock::new();
    cx.try_global::<TodoData>().unwrap_or_else(|| EMPTY.get_or_init(TodoData::default))
}

/// 任务用到的会话。侧栏的后台会话一组不列它们，它们在看板上。
pub fn todo_sessions(cx: &App) -> HashSet<SessionId> {
    data(cx).todos.iter().filter_map(|todo| todo.session).collect()
}

/// 需要你那一列有几张任务，侧栏的看板入口上显示。
pub fn needs_you(cx: &App) -> usize {
    cx.try_global::<NeedsYou>().map_or(0, |count| count.0)
}

/// 上一次算出的需要你的数，见 `count_needs_you`。
#[derive(Default)]
struct NeedsYou(usize);

impl Global for NeedsYou {}

/// 按现在的数据和时间重算需要你的数，变了就重画各窗口。
fn count_needs_you(cx: &mut App) {
    let data = data(cx);
    let now = crate::kanban::unix_now();
    let count = data
        .todos
        .iter()
        .filter(|todo| column(todo, todo.session.and_then(|id| data.sessions.get(&id)), now) == Column::NeedsYou)
        .count();
    if std::mem::replace(&mut cx.default_global::<NeedsYou>().0, count) != count {
        cx.refresh_windows();
    }
}

/// 看板经它和宿主说话，app 启动时经 `watch` 交进来。两样都会等宿主回话，只在后台线程上调。
#[derive(Clone, Copy)]
pub struct Host {
    /// 发一条看板的请求（拿到请求编号造出消息），等宿主回话；宿主不认得看板时是给人看的原因。
    pub ask: fn(Step) -> Result<HostMsg>,
    /// 宿主里现在的会话。
    pub list_sessions: fn() -> Result<Vec<SessionInfo>>,
}

impl Global for Host {}

impl Host {
    /// app 交进来的那个；还没交时什么都问不到。
    fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or(Self {
            ask: |_| Err(anyhow!("no terminal host")),
            list_sessions: || Err(anyhow!("no terminal host")),
        })
    }
}

/// 问到任务却没问到会话时，隔多久补问一次，见 `watch`。
const SESSIONS_RETRY: Duration = Duration::from_secs(2);

/// 在后台跟着看板：记下和宿主说话的 `host`，现在问一次，`changed`（宿主说看板变了、换了一条连接）来了
/// 就重问。几次问一个接一个，不重叠，免得慢回来的旧答案盖掉新的。
pub fn watch(host: Host, changed: Option<UnboundedReceiver<()>>, cx: &mut App) {
    cx.set_global(host);
    cx.spawn(async move |cx| {
        let mut changed = changed;
        let mut retried = false;
        loop {
            let refreshed = cx.update(refresh_todos);
            // 任务问到了、会话没问到：刚开始的任务找不到会话，算进了需要你。宿主不会为此再说变了，过一会儿
            // 补问一次；只补一次，宿主一直答不上来时不变成轮询。
            if refreshed.await && !std::mem::replace(&mut retried, true) {
                cx.background_executor().timer(SESSIONS_RETRY).await;
                continue;
            }
            retried = false;
            let Some(changed) = changed.as_mut() else { return };
            if changed.next().await.is_none() {
                return;
            }
            // 一下子变了好几次时只问一次。
            while changed.try_recv().is_ok() {}
        }
    })
    .detach();
}

/// 各窗口开过的看板，收起后就升不回来了。
#[derive(Default)]
struct Boards(Vec<WeakEntity<Kanban>>);

impl Global for Boards {}

/// 记下新开的看板，数据变了时重画它。
pub(crate) fn register(board: WeakEntity<Kanban>, cx: &mut App) {
    let boards = &mut cx.default_global::<Boards>().0;
    boards.retain(|board| board.upgrade().is_some());
    boards.push(board);
}

/// 重画各窗口开着的看板；任务变了（`changed`）时开着记录页的重翻记录。
fn notify_boards(changed: bool, cx: &mut App) {
    let boards: Vec<_> = cx.default_global::<Boards>().0.iter().filter_map(WeakEntity::upgrade).collect();
    for board in boards {
        board.update(cx, |board, cx| {
            if changed {
                board.reload_history(cx);
            }
            cx.notify();
        });
    }
}

/// 现问一次任务和定时，连同任务会话的状态一起问。变了就重画看板。任务问到了、会话没问到时返回真。
fn refresh_todos(cx: &mut App) -> Task<bool> {
    let host = Host::get(cx);
    cx.spawn(async move |cx| {
        let (answer, live) = cx
            .background_executor()
            .spawn(async move {
                let answer = (host.ask)(Box::new(|req| ClientMsg::ListTodos { req }));
                let live = (host.list_sessions)().ok();
                (answer, live)
            })
            .await;
        cx.update(|cx| {
            let mut data = take(cx);
            let mut missed = false;
            match answer {
                Ok(HostMsg::Todos { todos, schedules, .. }) => {
                    missed = live.is_none();
                    if let Some(live) = live {
                        data.sessions = live.into_iter().map(|info| (info.id, info)).collect();
                    }
                    data.todos = todos;
                    data.schedules = schedules;
                    data.error = None;
                }
                Ok(other) => data.error = Some(format!("unexpected answer from the host: {other:?}")),
                Err(err) => data.error = Some(format!("{err:#}")),
            }
            put(data, cx);
            missed
        })
    })
}

/// 在后台发一条看板的请求，等宿主回话；办成了的等宿主说变了再重问。
pub(crate) fn ask(cx: &mut App, step: Step) -> Task<Result<HostMsg>> {
    let host = Host::get(cx);
    cx.background_executor().spawn(async move { (host.ask)(step) })
}

/// 一条看板的请求：拿到请求编号造出消息。
pub type Step = Box<dyn FnOnce(u32) -> ClientMsg + Send>;

/// 在后台依次发几条看板的请求，前一条没办成就不发后面的；办成了的等宿主说变了再重问。
/// 等到的是没办成的原因。
pub(crate) fn send(cx: &mut App, steps: Vec<Step>) -> Task<Result<(), String>> {
    let host = Host::get(cx);
    cx.background_executor().spawn(async move {
        for step in steps {
            (host.ask)(step).map_err(|err| format!("{err:#}"))?;
        }
        Ok(())
    })
}

/// 拿一份现在的数据去改，改完用 `put` 放回去；原来的留着，`put` 拿它比。
fn take(cx: &mut App) -> TodoData {
    cx.default_global::<TodoData>().clone()
}

/// 放回改过的数据；和原来的不一样时重画看板、重排宽限期的定时器，再重算需要你的数。
fn put(data: TodoData, cx: &mut App) {
    let old = std::mem::replace(cx.default_global::<TodoData>(), data);
    let new = cx.global::<TodoData>();
    if old != *new {
        // 会话的状态一变数据就不同，只在任务本身变了时重翻记录，不然 agent 一动记录页就跳回第一页。
        notify_boards(old.todos != new.todos, cx);
        wait_for_grace(cx);
    }
    count_needs_you(cx);
}

/// 等下一张任务过宽限期的定时器，换上新的时旧的丢掉、不再响。
#[derive(Default)]
struct GraceTimer(Option<Task<()>>);

impl Global for GraceTimer {}

/// 刚开始的任务过了宽限期（`START_GRACE`）时列可能变（干完了的从进行中落进需要你），会话的样子却没变，
/// 宿主不会说：等到最早过期的那张，到时重画看板、重算需要你的数，再等下一张。
fn wait_for_grace(cx: &mut App) {
    let now = crate::kanban::unix_now();
    let next = grace_ends(&data(cx).todos).filter(|&end| end > now).min();
    let timer = next.map(|end| {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(end - now)).await;
            cx.update(|cx| {
                notify_boards(false, cx);
                count_needs_you(cx);
                wait_for_grace(cx);
            });
        })
    });
    cx.default_global::<GraceTimer>().0 = timer;
}

/// 没做完的任务过宽限期的 Unix 时间。
fn grace_ends(todos: &[Todo]) -> impl Iterator<Item = u64> + '_ {
    todos
        .iter()
        .filter(|todo| todo.done_at.is_none())
        .filter_map(|todo| todo.started.map(|at| at.saturating_add(START_GRACE)))
}

#[cfg(test)]
mod tests {
    use runode_protocol::todo::TodoSpec;

    use super::*;

    fn todo(started: Option<u64>, done_at: Option<u64>) -> Todo {
        Todo {
            id: "1".into(),
            number: 1,
            spec: TodoSpec {
                title: "t".into(),
                prompt: String::new(),
                dir: "/tmp".into(),
                agent: "claude".into(),
                worktree: false,
            },
            created: 0,
            started,
            session: Some(SessionId(1)),
            worktree_dir: None,
            branch: None,
            done_at,
            schedule: None,
            rank: 0,
        }
    }

    /// 没开始的、做完了的任务不等宽限期。
    #[test]
    fn grace_ends_only_for_started_open_todos() {
        let todos = [todo(Some(1_000), None), todo(None, None), todo(Some(2_000), Some(2_001))];
        assert_eq!(grace_ends(&todos).collect::<Vec<_>>(), [1_000 + START_GRACE]);
    }
}
