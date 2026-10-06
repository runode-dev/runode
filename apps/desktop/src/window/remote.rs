//! 别的进程经宿主请 app 办的事（`runode open`、`runode focus`，命令行问各个终端摆在哪，以及手机
//! 新建工作区）：在某个终端旁边开新终端，切到某个终端，回答布局（见 `layout_report`），在最前面
//! 那个窗口里开一个某个目录的 workspace（已经有了就用它）。宿主把请求包成 `HostMsg::UiRequest`
//! 经连接转过来（见 `host_client::serve_ui`），这里在主线程上按会话找到它所在的窗口和分屏再办，
//! 用 `Link::ui_reply` 回话。终端里的程序读写剪贴板也这样转过来，见 `clipboard`。

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use gpui::{App, Context, EntityId, Window, WindowHandle};
use runode_protocol::{ClientMsg, HostMsg, Placement, SessionId};
use runode_shared_types::pane::Axis;

use super::persist::format;
use super::{WindowView, agents::reveal, clipboard, layout_report};
use crate::host_client;

/// 开始收别的进程的请求。
pub fn serve_requests(cx: &mut App) {
    layout_report::track_windows(cx);
    let Some(mut requests) = host_client::serve_ui() else {
        tracing::warn!("requests from other processes are already being served");
        return;
    };
    cx.spawn(async move |cx| {
        while let Some((ticket, request)) = requests.next().await {
            // 读剪贴板可能要等用户点询问框，自己回话，不挡住后面的请求。
            if let ClientMsg::ReadClipboard { id, ask, program } = request {
                cx.update(|cx| clipboard::read(ticket, id, ask, program, cx));
                continue;
            }
            let reply = cx.update(|cx| handle(request, cx));
            host_client::link().ui_reply(ticket, reply);
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
        ClientMsg::OpenWorkspace { req, dir, focus } => {
            let Some(window) = front_window(cx) else {
                return fail(req, "there is no runode window to open it in".into());
            };
            if !dir.is_absolute() || !dir.is_dir() {
                return fail(req, format!("{} is not a directory", dir.display()));
            }
            let opened = window.update(cx, |view, window, cx| view.open_workspace_at(&dir, focus, window, cx));
            match opened {
                Ok(Ok((pane, id))) => {
                    if focus {
                        reveal(window, pane, cx);
                    }
                    HostMsg::Opened { req, id }
                }
                Ok(Err(message)) => fail(req, message.into()),
                Err(_) => fail(req, "the runode window went away".into()),
            }
        }
        ClientMsg::Reveal { req, id } => match find_session(id, cx) {
            Some((window, pane)) => {
                reveal(window, pane, cx);
                HostMsg::Done { req }
            }
            None => fail(req, format!("no runode window shows session {id}")),
        },
        ClientMsg::Layout { req } => HostMsg::Layout { req, windows: layout_report::current(cx) },
        ClientMsg::WriteClipboard { text, .. } => clipboard::write(text.0, cx),
        other => HostMsg::Error { req: None, id: None, message: format!("the runode app does not handle {other:?}") },
    }
}

/// 显示这个会话的窗口和分屏。
pub(super) fn find_session(id: SessionId, cx: &App) -> Option<(WindowHandle<WindowView>, EntityId)> {
    windows(cx).into_iter().find_map(|window| Some((window, pane_of(window, id, cx)?)))
}

fn pane_of(window: WindowHandle<WindowView>, id: SessionId, cx: &App) -> Option<EntityId> {
    let view = window.read(cx).ok()?;
    view.workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| &tab.panes)
        .find_map(|(pane, (terminal, _))| (terminal.read(cx).session_id() == Some(id)).then_some(*pane))
}

/// 最前面那个窗口当前的分屏。
fn front_pane(cx: &App) -> Option<(WindowHandle<WindowView>, EntityId)> {
    let window = front_window(cx)?;
    Some((window, window.read(cx).ok()?.tab().focused))
}

/// 最前面那个窗口。app 不在前台时没有活动窗口，按窗口的前后顺序取最前面的。
pub(super) fn front_window(cx: &App) -> Option<WindowHandle<WindowView>> {
    let front = cx
        .active_window()
        .and_then(|window| window.downcast::<WindowView>())
        .or_else(|| cx.window_stack()?.into_iter().find_map(|window| window.downcast::<WindowView>()));
    front.or_else(|| windows(cx).into_iter().next())
}

pub(super) fn windows(cx: &App) -> Vec<WindowHandle<WindowView>> {
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
        let cwd = format::start_dir(cwd.as_deref(), &self.workspaces[wi].dir);
        let view = self.spawn_terminal(cwd.as_deref(), window, cx)?;
        // `spawn` 建的视图当场开好了会话。
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
            view.update(cx, |view, cx| view.start_at(size, window, cx));
            // 开在不显示的标签里的，到时丢掉界面这份 VT。
            self.sync_visibility(window, cx);
        }
        self.save(cx);
        cx.notify();
        session
    }

    /// 切到或者新建目录是 `dir` 的 workspace，返回它当前标签里有焦点的分屏和那个分屏的会话。已经有
    /// 这个目录的 workspace 时不新建（目录按规范化后的比，`dir` 经过符号链接也认得出）；那个分屏还没
    /// 开会话时在这个 workspace 末尾另开一个标签。没有时在 `dir` 开一个终端，新 workspace 插在当前
    /// workspace 后面，和菜单里新建的一样。不切过去（`focus` 为假）时当前的 workspace 不动，新终端按
    /// 当前显示的分屏的尺寸现在就启动 shell，别的进程马上就能往里打字；切过去时由调用方 `reveal` 它。
    fn open_workspace_at(
        &mut self,
        dir: &Path,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(EntityId, SessionId), &'static str> {
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let dir = canonical(dir);
        let dir = dir.as_path();
        if let Some(wi) = self.workspaces.iter().position(|workspace| canonical(&workspace.dir) == dir) {
            let workspace = &self.workspaces[wi];
            let tab = &workspace.tabs[workspace.active];
            if let Some(session) = tab.focused_view().read(cx).session_id() {
                return Ok((tab.focused, session));
            }
            // 恢复布局时看不见的终端要等显示出来才开会话，见 `TerminalView::deferred`。
            let view = self.spawn_terminal(Some(dir), window, cx).ok_or("could not start a terminal")?;
            let session = view.read(cx).session_id().ok_or("could not start a terminal")?;
            let pane = view.entity_id();
            let size = self.tab().focused_view().read(cx).size();
            let tab = self.single_pane_tab(view.clone(), window, cx);
            self.workspaces[wi].tabs.push(tab);
            view.update(cx, |view, cx| view.start_at(size, window, cx));
            self.sync_visibility(window, cx);
            self.save(cx);
            cx.notify();
            return Ok((pane, session));
        }
        let view = self.spawn_terminal(Some(dir), window, cx).ok_or("could not start a terminal")?;
        // `spawn` 建的视图当场开好了会话。
        let session = view.read(cx).session_id().ok_or("could not start a terminal")?;
        let pane = view.entity_id();
        let ix = self.active + 1;
        if focus {
            self.insert_workspace(ix, dir.to_path_buf(), view, window, cx);
        } else {
            let size = self.tab().focused_view().read(cx).size();
            self.add_workspace(ix, dir.to_path_buf(), view.clone(), window, cx);
            view.update(cx, |view, cx| view.start_at(size, window, cx));
            // 开在不显示的 workspace 里，到时丢掉界面这份 VT。
            self.sync_visibility(window, cx);
        }
        self.save(cx);
        cx.notify();
        Ok((pane, session))
    }
}
