//! 别的进程经宿主请 app 办的事（`runode open`、`runode focus`）：在某个终端旁边开新终端，切到
//! 某个终端。宿主把请求包成 `HostMsg::UiRequest` 经连接转过来（见 `session_host::serve_ui`），这里
//! 在主线程上按会话找到它所在的窗口和分屏再办，用 `Link::ui_reply` 回话。

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui::{App, Context, EntityId, Window, WindowHandle};
use runode_protocol::{ClientMsg, HostMsg, Placement, SessionId};
use runode_shared_types::pane::Axis;

use super::{WindowView, agents::reveal};
use crate::{persist, session_host};

/// 开始收别的进程的请求。
pub fn serve_requests(cx: &mut App) {
    let Some(mut requests) = session_host::serve_ui() else {
        tracing::warn!("requests from other processes are already being served");
        return;
    };
    cx.spawn(async move |cx| {
        while let Some((ui, request)) = requests.next().await {
            let reply = cx.update(|cx| handle(request, cx));
            session_host::link().ui_reply(ui, reply);
        }
    })
    .detach();
}

/// 办一条请求，返回回话。
fn handle(request: ClientMsg, cx: &mut App) -> HostMsg {
    let fail = |req: u32, message: String| HostMsg::Error { req: Some(req), id: None, message };
    match request {
        ClientMsg::Open { req, placement, near, cwd, focus } => {
            let target = match near {
                Some(id) => find_session(id, cx),
                None => front_pane(cx),
            };
            let Some((window, pane)) = target else {
                return fail(
                    req,
                    match near {
                        Some(id) => format!("no runode window shows session {id}"),
                        None => "there is no runode window to open it in".into(),
                    },
                );
            };
            let opened = window
                .update(cx, |view, window, cx| view.open_beside(pane, placement, cwd, focus, window, cx))
                .ok()
                .flatten();
            match opened {
                Some(id) => {
                    if focus {
                        reveal(window, pane_of(window, id, cx).unwrap_or(pane), cx);
                    }
                    HostMsg::Opened { req, id }
                }
                None => fail(req, "could not start a terminal".into()),
            }
        }
        ClientMsg::Reveal { req, id } => match find_session(id, cx) {
            Some((window, pane)) => {
                reveal(window, pane, cx);
                HostMsg::Done { req }
            }
            None => fail(req, format!("no runode window shows session {id}")),
        },
        ClientMsg::Layout { req } => fail(req, "the runode app does not handle this request".into()),
        other => HostMsg::Error { req: None, id: None, message: format!("the runode app does not handle {other:?}") },
    }
}

/// 显示这个会话的窗口和分屏。
fn find_session(id: SessionId, cx: &App) -> Option<(WindowHandle<WindowView>, EntityId)> {
    windows(cx).into_iter().find_map(|window| Some((window, pane_of(window, id, cx)?)))
}

fn pane_of(window: WindowHandle<WindowView>, id: SessionId, cx: &App) -> Option<EntityId> {
    let view = window.read(cx).ok()?;
    view.workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| &tab.panes)
        .find_map(|(pane, (terminal, _))| (terminal.read(cx).session_id() == id).then_some(*pane))
}

/// 最前面那个窗口当前的分屏。app 不在前台时没有活动窗口，按窗口的前后顺序取最前面的。
fn front_pane(cx: &App) -> Option<(WindowHandle<WindowView>, EntityId)> {
    let front = cx
        .active_window()
        .and_then(|window| window.downcast::<WindowView>())
        .or_else(|| cx.window_stack()?.into_iter().find_map(|window| window.downcast::<WindowView>()));
    let window = front.or_else(|| windows(cx).into_iter().next())?;
    Some((window, window.read(cx).ok()?.tab().focused))
}

fn windows(cx: &App) -> Vec<WindowHandle<WindowView>> {
    cx.windows().into_iter().filter_map(|window| window.downcast::<WindowView>()).collect()
}

impl WindowView {
    /// 在 `beside` 这个分屏旁边开一个新终端，返回它的会话。`cwd` 为空时沿用旁边那个终端的
    /// 目录。不切过去（`focus` 为假）时当前的 workspace、标签和焦点都不动，新终端看不见的话
    /// 按旁边那个终端的尺寸现在就启动 shell，别的进程马上就能往里打字。
    fn open_beside(
        &mut self,
        beside: EntityId,
        placement: Placement,
        cwd: Option<PathBuf>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<SessionId> {
        let (wi, ti) = self.locate(beside)?;
        let near = self.workspaces[wi].tabs[ti].panes[&beside].0.clone();
        let cwd = cwd.or_else(|| near.read(cx).cwd());
        let cwd = persist::start_dir(cwd.as_deref(), &self.workspaces[wi].dir);
        let view = self.spawn_terminal(cwd.as_deref(), window, cx)?;
        let session = view.read(cx).session_id();
        match placement {
            Placement::Tab => {
                let tab = self.single_pane_tab(view.clone(), window, cx);
                let workspace = &mut self.workspaces[wi];
                workspace.tabs.insert(ti + 1, tab);
                if workspace.active > ti {
                    workspace.active += 1;
                }
            }
            Placement::Right | Placement::Down => {
                let axis = if placement == Placement::Right { Axis::Horizontal } else { Axis::Vertical };
                let split_id = self.next_id();
                let (id, entry) = self.pane_entry(view.clone(), window, cx);
                let tab = &mut self.workspaces[wi].tabs[ti];
                tab.root.split(beside, id, axis, split_id);
                tab.panes.insert(id, entry);
            }
        }
        if !focus {
            // 旁边那个终端在后台标签里时没有界面这份 VT，尺寸是它最后量出的。
            let size = near.read(cx).size();
            view.update(cx, |view, cx| view.start_at(size, cx));
            // 开在不显示的标签里的，到时丢掉界面这份 VT。
            self.sync_visibility(window, cx);
        }
        self.save(cx);
        cx.notify();
        Some(session)
    }
}
