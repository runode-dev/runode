//! 别的进程经宿主请 app 办的事（`runode open`、`runode focus`）：在某个终端旁边开新终端，切到
//! 某个终端。宿主在连接的线程里把 `UiRequest` 交过来，这里转到主线程，按会话找到它所在的窗口
//! 和分屏再办。

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui::{App, Context, EntityId, Window, WindowHandle};
use runode_host::{ClientMsg, HostMsg, Placement, SessionId, UiRequest};
use runode_shared_types::pane::Axis;

use super::{WindowView, agents::reveal};
use crate::{persist, session_host};

/// 开始收别的进程的请求。
pub fn serve_requests(cx: &mut App) {
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<UiRequest>();
    session_host::set_ui(Box::new(move |request| {
        // app 在退出，主线程不再收了：请求丢掉时宿主替它回话。
        let _ = tx.unbounded_send(request);
    }));
    cx.spawn(async move |cx| {
        while let Some(request) = rx.next().await {
            cx.update(|cx| handle(request, cx));
        }
    })
    .detach();
}

fn handle(request: UiRequest, cx: &mut App) {
    match request.message {
        ClientMsg::Open { req, placement, near, ref cwd, focus } => {
            let target = match near {
                Some(id) => find_session(id, cx),
                None => front_pane(cx),
            };
            let Some((window, pane)) = target else {
                return request.fail(match near {
                    Some(id) => format!("no runode window shows session {id}"),
                    None => "there is no runode window to open it in".into(),
                });
            };
            let cwd = cwd.clone();
            let opened = window
                .update(cx, |view, window, cx| view.open_beside(pane, placement, cwd, focus, window, cx))
                .ok()
                .flatten();
            match opened {
                Some(id) => {
                    if focus {
                        reveal(window, pane_of(window, id, cx).unwrap_or(pane), cx);
                    }
                    request.reply(HostMsg::Opened { req, id });
                }
                None => request.fail("could not start a terminal"),
            }
        }
        ClientMsg::Reveal { req, id } => match find_session(id, cx) {
            Some((window, pane)) => {
                reveal(window, pane, cx);
                request.reply(HostMsg::Done { req });
            }
            None => request.fail(format!("no runode window shows session {id}")),
        },
        _ => request.fail("the runode app does not handle this request"),
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
            let size = near.read(cx).size();
            view.update(cx, |view, cx| view.start_at(size, cx));
        }
        self.save(cx);
        cx.notify();
        Some(session)
    }
}
