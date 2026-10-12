//! 任务板的数据：宿主手里的任务和定时（`ClientMsg::ListTodos`），和任务会话现在的状态，全 app 一份。
//!
//! 启动时问一次宿主，这一问也让宿主之后在任务板变了时说一声（`host_client::todos_changed`），说了就重问；
//! 换了一条连接也重问。卡片在哪一列要看任务会话里 agent 的状态，宿主不会为它说变了，所以有窗口开着
//! 看板时每 `SESSIONS_POLL` 问一次宿主里的会话，只留任务用到的；不 attach，免得宿主把它们算成有界面连着。
//! 数据变了才重画各窗口。请求都在后台线程上等宿主回话。

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use futures::StreamExt as _;
use gpui::{App, Global, Task};
use runode_protocol::{
    ClientMsg, HostMsg, SessionId, SessionInfo,
    todo::{Schedule, Todo},
};

use super::super::remote;
use crate::host_client;

/// 看板开着时隔多久问一次任务会话的状态：卡片跟着 agent 换列最多晚这么久。
const SESSIONS_POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Default, PartialEq)]
pub(in crate::window) struct TodoData {
    /// 按先后（`Todo::rank`）排好的任务，完成了的只有最近七天的。
    pub(in crate::window) todos: Vec<Todo>,
    /// 按建的先后排好的定时。
    pub(in crate::window) schedules: Vec<Schedule>,
    /// 任务用到的会话现在的样子；不在里面的会话已经没了。
    pub(in crate::window) sessions: HashMap<SessionId, SessionInfo>,
    /// 问不到任务板、或者上一个请求没办成的说明。
    pub(in crate::window) error: Option<String>,
}

impl Global for TodoData {}

/// 全 app 的任务板数据；还没问到时是空的。
pub(in crate::window) fn data(cx: &App) -> &TodoData {
    static EMPTY: std::sync::OnceLock<TodoData> = std::sync::OnceLock::new();
    cx.try_global::<TodoData>().unwrap_or_else(|| EMPTY.get_or_init(TodoData::default))
}

/// 任务用到的会话。侧栏的后台会话一组不列它们，它们在看板上。
pub(in crate::window) fn todo_sessions(cx: &App) -> HashSet<SessionId> {
    data(cx).todos.iter().filter_map(|todo| todo.session).collect()
}

/// 在后台跟着任务板：现在问一次，宿主说变了就重问；有窗口开着看板时轮询任务会话的状态。
pub fn watch(cx: &mut App) {
    refresh_todos(cx).detach();
    if let Some(mut changed) = host_client::todos_changed() {
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
    cx.spawn(async move |cx| {
        loop {
            let poll = cx.update(|cx| board_open(cx).then(|| refresh_sessions(cx)));
            if let Some(poll) = poll {
                poll.await;
            }
            cx.background_executor().timer(SESSIONS_POLL).await;
        }
    })
    .detach();
}

/// 有窗口开着看板。
fn board_open(cx: &App) -> bool {
    remote::windows(cx).iter().any(|handle| handle.read(cx).is_ok_and(|view| view.todo_board.is_some()))
}

/// 现问一次任务和定时，变了就重画各窗口。
pub(in crate::window) fn refresh_todos(cx: &mut App) -> Task<()> {
    cx.spawn(async move |cx| {
        let answer =
            cx.background_executor().spawn(async { host_client::ask_todos(|req| ClientMsg::ListTodos { req }) }).await;
        cx.update(|cx| {
            let mut data = take(cx);
            match answer {
                Ok(HostMsg::Todos { todos, schedules, .. }) => {
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

/// 现问一次任务会话的状态，变了就重画各窗口。
pub(in crate::window) fn refresh_sessions(cx: &mut App) -> Task<()> {
    let wanted = todo_sessions(cx);
    cx.spawn(async move |cx| {
        if wanted.is_empty() {
            return;
        }
        let live = cx.background_executor().spawn(async { host_client::list_sessions() }).await;
        let Ok(live) = live else { return };
        cx.update(|cx| {
            let mut data = take(cx);
            data.sessions =
                live.into_iter().filter(|info| wanted.contains(&info.id)).map(|info| (info.id, info)).collect();
            put(data, cx);
        });
    })
}

/// 在后台发一条任务板的请求，等宿主回话；办成了的等宿主说变了再重问。
pub(in crate::window) fn ask(cx: &mut App, step: Step) -> Task<Result<HostMsg>> {
    cx.background_executor().spawn(async move { host_client::ask_todos(step) })
}

/// 一条任务板的请求：拿到请求编号造出消息。
pub(in crate::window) type Step = Box<dyn FnOnce(u32) -> ClientMsg + Send>;

/// 在后台依次发几条任务板的请求，前一条没办成就不发后面的，原因显示在看板上；办成了的等宿主说变了再重问。
pub(in crate::window) fn send(cx: &mut App, steps: Vec<Step>) {
    cx.spawn(async move |cx| {
        let sent = cx
            .background_executor()
            .spawn(async move {
                for step in steps {
                    host_client::ask_todos(step)?;
                }
                anyhow::Ok(())
            })
            .await;
        if let Err(err) = sent {
            let message = format!("{err:#}");
            cx.update(|cx| {
                let mut data = take(cx);
                data.error = Some(message);
                put(data, cx);
            });
        }
    })
    .detach();
}

/// 拿出现在的数据改，改完用 `put` 放回去。
fn take(cx: &mut App) -> TodoData {
    std::mem::take(cx.default_global::<TodoData>())
}

/// 放回改过的数据；和原来的不一样时重画各窗口。
fn put(data: TodoData, cx: &mut App) {
    let old = std::mem::replace(cx.default_global::<TodoData>(), data);
    if old != *cx.global::<TodoData>() {
        cx.refresh_windows();
    }
}
