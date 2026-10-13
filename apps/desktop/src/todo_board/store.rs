//! 任务板的数据：宿主手里的任务和定时（`ClientMsg::ListTodos`），和任务会话现在的状态，全 app 一份。
//!
//! 和宿主说话经 app 在启动时交进来的 `Host`（`watch`）。启动时问一次宿主，这一问也让宿主之后在任务板变了时
//! 说一声（app 交进来的通知），说了就重问；换了一条连接也重问。卡片在哪一列要看任务会话里 agent 的状态，宿主不会为它说变了，所以有窗口开着
//! 看板时每 `SESSIONS_POLL` 问一次宿主里的会话，只留任务用到的；不 attach，免得宿主把它们算成有界面连着。
//! 数据变了才重画开着的看板，只有看板读这份数据，不让各窗口整页重画；刚开始的任务过宽限期时列会变、数据
//! 却不一定变，那几轮也重画（`settling`）。
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
    todo::{START_GRACE, Schedule, Todo},
};

use crate::todo_board::TodoBoard;

/// 看板开着时隔多久问一次任务会话的状态：卡片跟着 agent 换列最多晚这么久。
const SESSIONS_POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Default, PartialEq)]
pub(crate) struct TodoData {
    /// 按先后（`Todo::rank`）排好的任务，完成了的只有最近七天的。
    pub(crate) todos: Vec<Todo>,
    /// 按建的先后排好的定时。
    pub(crate) schedules: Vec<Schedule>,
    /// 任务用到的会话现在的样子；不在里面的会话已经没了。
    pub(crate) sessions: HashMap<SessionId, SessionInfo>,
    /// 问不到任务板、或者上一个请求没办成的说明。
    pub(crate) error: Option<String>,
}

impl Global for TodoData {}

/// 全 app 的任务板数据；还没问到时是空的。
pub(crate) fn data(cx: &App) -> &TodoData {
    static EMPTY: std::sync::OnceLock<TodoData> = std::sync::OnceLock::new();
    cx.try_global::<TodoData>().unwrap_or_else(|| EMPTY.get_or_init(TodoData::default))
}

/// 任务用到的会话。侧栏的后台会话一组不列它们，它们在看板上。
pub fn todo_sessions(cx: &App) -> HashSet<SessionId> {
    data(cx).todos.iter().filter_map(|todo| todo.session).collect()
}

/// 任务板经它和宿主说话，app 启动时经 `watch` 交进来。两样都会等宿主回话，只在后台线程上调。
#[derive(Clone, Copy)]
pub struct Host {
    /// 发一条任务板的请求（拿到请求编号造出消息），等宿主回话；宿主不认得任务板时是给人看的原因。
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

/// 在后台跟着任务板：记下和宿主说话的 `host`，现在问一次，`changed`（宿主说任务板变了、换了一条连接）来了
/// 就重问。任务会话的状态在看板开着时才轮询，见 `poll_sessions`。
pub fn watch(host: Host, changed: Option<UnboundedReceiver<()>>, cx: &mut App) {
    cx.set_global(host);
    refresh_todos(cx).detach();
    if let Some(mut changed) = changed {
        cx.spawn(async move |cx| {
            while changed.next().await.is_some() {
                // 一下子变了好几次时只问一次。
                while changed.try_recv().is_ok() {}
                let refreshed = cx.update(refresh_todos);
                refreshed.await;
            }
        })
        .detach();
    }
}

/// 正在轮询任务会话的状态，见 `poll_sessions`。
#[derive(Default)]
struct Polling(bool);

impl Global for Polling {}

/// 看板打开时调：现问一次任务会话的状态，之后每 `SESSIONS_POLL` 问一次，没有窗口开着看板时停下，空闲时
/// 不再定时唤醒主线程。
pub fn poll_sessions(cx: &mut App) {
    refresh_sessions(cx).detach();
    if std::mem::replace(&mut cx.default_global::<Polling>().0, true) {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(SESSIONS_POLL).await;
            let poll = cx.update(|cx| {
                let open = board_open(cx);
                if !open {
                    cx.default_global::<Polling>().0 = false;
                }
                open.then(|| refresh_sessions(cx))
            });
            let Some(poll) = poll else { return };
            poll.await;
        }
    })
    .detach();
}

/// 各窗口开过的看板，收起后就升不回来了。
#[derive(Default)]
struct Boards(Vec<WeakEntity<TodoBoard>>);

impl Global for Boards {}

/// 记下新开的看板，`board_open` 据此判断要不要接着轮询。
pub(crate) fn register(board: WeakEntity<TodoBoard>, cx: &mut App) {
    cx.default_global::<Boards>().0.push(board);
}

/// 重画各窗口开着的看板。
fn notify_boards(cx: &mut App) {
    let boards: Vec<_> = cx.default_global::<Boards>().0.iter().filter_map(WeakEntity::upgrade).collect();
    for board in boards {
        board.update(cx, |_, cx| cx.notify());
    }
}

/// 有窗口开着看板。
fn board_open(cx: &mut App) -> bool {
    let boards = &mut cx.default_global::<Boards>().0;
    boards.retain(|board| board.upgrade().is_some());
    !boards.is_empty()
}

/// 现问一次任务和定时，连同任务会话的状态一起问：刚开始的任务的会话还没问过时，它会先被算成会话没了、
/// 落进需要你那一列，下一轮才回到进行中。变了就重画看板。
fn refresh_todos(cx: &mut App) -> Task<()> {
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
            match answer {
                Ok(HostMsg::Todos { todos, schedules, .. }) => {
                    if let Some(live) = live {
                        let wanted: HashSet<SessionId> = todos.iter().filter_map(|todo| todo.session).collect();
                        data.sessions = live
                            .into_iter()
                            .filter(|info| wanted.contains(&info.id))
                            .map(|info| (info.id, info))
                            .collect();
                    }
                    data.todos = todos;
                    data.schedules = schedules;
                    data.error = None;
                }
                Ok(other) => data.error = Some(format!("unexpected answer from the host: {other:?}")),
                Err(err) => data.error = Some(format!("{err:#}")),
            }
            put(data, cx);
        });
    })
}

/// 现问一次任务会话的状态，变了就重画看板。
fn refresh_sessions(cx: &mut App) -> Task<()> {
    let wanted = todo_sessions(cx);
    let host = Host::get(cx);
    cx.spawn(async move |cx| {
        if wanted.is_empty() {
            return;
        }
        let live = cx.background_executor().spawn(async move { (host.list_sessions)() }).await;
        let Ok(live) = live else { return };
        cx.update(|cx| {
            let now_wanted = todo_sessions(cx);
            let mut data = take(cx);
            merge_sessions(&mut data.sessions, live, &wanted, &now_wanted);
            if settling(&data.todos, crate::todo_board::unix_now()) {
                notify_boards(cx);
            }
            put(data, cx);
        });
    })
}

/// 把问到的会话 `live` 换进 `sessions`：只换问的时候要的（`asked`）那些，问的时候还没有的（等宿主回话时
/// 刚开始的任务的会话，`refresh_todos` 已经放进来了）留着，免得那张卡片落进需要你一轮；现在不再要（`wanted`）
/// 的去掉。
fn merge_sessions(
    sessions: &mut HashMap<SessionId, SessionInfo>,
    live: Vec<SessionInfo>,
    asked: &HashSet<SessionId>,
    wanted: &HashSet<SessionId>,
) {
    sessions.retain(|id, _| wanted.contains(id) && !asked.contains(id));
    let live = live.into_iter().filter(|info| asked.contains(&info.id) && wanted.contains(&info.id));
    sessions.extend(live.map(|info| (info.id, info)));
}

/// 在后台发一条任务板的请求，等宿主回话；办成了的等宿主说变了再重问。
pub(crate) fn ask(cx: &mut App, step: Step) -> Task<Result<HostMsg>> {
    let host = Host::get(cx);
    cx.background_executor().spawn(async move { (host.ask)(step) })
}

/// 一条任务板的请求：拿到请求编号造出消息。
pub type Step = Box<dyn FnOnce(u32) -> ClientMsg + Send>;

/// 在后台依次发几条任务板的请求，前一条没办成就不发后面的，原因显示在看板上；办成了的等宿主说变了再重问。
/// 等到的是都办成了没有。
pub(crate) fn send(cx: &mut App, steps: Vec<Step>) -> Task<bool> {
    let host = Host::get(cx);
    cx.spawn(async move |cx| {
        let sent = cx
            .background_executor()
            .spawn(async move {
                for step in steps {
                    (host.ask)(step)?;
                }
                anyhow::Ok(())
            })
            .await;
        let Err(err) = sent else { return true };
        let message = format!("{err:#}");
        cx.update(|cx| {
            let mut data = take(cx);
            data.error = Some(message);
            put(data, cx);
        });
        false
    })
}

/// 拿一份现在的数据去改，改完用 `put` 放回去；原来的留着，`put` 拿它比。
fn take(cx: &mut App) -> TodoData {
    cx.default_global::<TodoData>().clone()
}

/// 有没做完的任务刚开始不久，宽限期（`START_GRACE`）在这一轮前后到头：到头时它的列可能变（干完了的从进行中
/// 落进需要你），会话的样子却没变，`put` 比不出来。
fn settling(todos: &[Todo], now: u64) -> bool {
    let window = START_GRACE + SESSIONS_POLL.as_secs();
    todos.iter().any(|todo| todo.done_at.is_none() && todo.started.is_some_and(|at| now <= at.saturating_add(window)))
}

/// 放回改过的数据；和原来的不一样时重画看板。
fn put(data: TodoData, cx: &mut App) {
    let old = std::mem::replace(cx.default_global::<TodoData>(), data);
    if old != *cx.global::<TodoData>() {
        notify_boards(cx);
    }
}

#[cfg(test)]
mod tests {
    use runode_protocol::todo::TodoSpec;

    use super::*;

    fn session(id: u128) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            size: runode_shared_types::grid::GridSize { cols: 80, rows: 24, cell_width_px: 0, cell_height_px: 0 },
            meta: Default::default(),
            clients: 0,
            claimed: false,
            exited: false,
            size_owner: None,
        }
    }

    /// 只换问的时候要的会话：等回话时刚开始的任务的会话留着，问到没有的去掉，不再要的去掉。
    #[test]
    fn polled_sessions_only_replace_what_was_asked() {
        let mut sessions: HashMap<_, _> = [1, 2, 3, 4].map(|id| (SessionId(id), session(id))).into();
        let asked = HashSet::from([SessionId(1), SessionId(2), SessionId(3)]);
        let wanted = HashSet::from([SessionId(1), SessionId(2), SessionId(4)]);
        // 2 没了，3 不再要，4 是刚开始的任务的，问的时候还没有。
        merge_sessions(&mut sessions, vec![session(1), session(3)], &asked, &wanted);
        let mut ids: Vec<_> = sessions.keys().map(|id| id.0).collect();
        ids.sort_unstable();
        assert_eq!(ids, [1, 4]);
    }

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

    /// 宽限期到头前后那一轮都算，过了一轮就不算；没开始的、做完了的不算。
    #[test]
    fn settling_covers_the_end_of_the_grace_period() {
        let start = 1_000;
        let todos = [todo(Some(start), None)];
        assert!(settling(&todos, start + 1));
        assert!(settling(&todos, start + START_GRACE));
        assert!(settling(&todos, start + START_GRACE + SESSIONS_POLL.as_secs()));
        assert!(!settling(&todos, start + START_GRACE + SESSIONS_POLL.as_secs() + 1));
        assert!(!settling(&[todo(None, None)], start));
        assert!(!settling(&[todo(Some(start), Some(start + 1))], start + 1));
    }
}
