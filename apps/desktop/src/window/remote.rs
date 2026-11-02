//! 别的进程经宿主请 app 办的事（`runode open`、`runode focus`、`runode move`、`runode swap`，命令行问
//! 各个终端摆在哪，以及手机新建工作区、给工作区改名）：在某个终端旁边开新终端，切到某个终端，挪动、
//! 对调分屏，回答布局（`layout_report`），在最前面那个窗口里开一个某个目录的 workspace（已经有了就用
//! 它），按布局里的序号给 workspace 改名。宿主把请求包成 `HostMsg::UiRequest` 经连接转过来
//! （`host_client::serve_ui`），这里在主线程上按会话找到它所在的窗口和分屏再办，用 `Link::ui_reply` 回话。终端里的程序读写剪贴板也这样转过来，见 `clipboard`。

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use gpui::{AnyWindowHandle, App, Context, Entity, EntityId, Task, Window, WindowHandle};
use runode_protocol::{ClientMsg, HostMsg, Placement, SessionId};
use runode_types::pane::Axis;

use super::persist::format;
use super::{WindowView, agents::reveal, cards, clipboard, layout_report};
use crate::host_client::hosts::HostId;
use crate::{
    config::AppConfig,
    host_client::{self, Link},
    terminal_view::TerminalView,
};

/// 开始收别的进程经本机的宿主转来的请求。
pub fn serve_requests(cx: &mut App) {
    layout_report::track_windows(cx);
    serve(HostId::Local, host_client::link().clone(), cx);
}

/// 开始收主机 `host` 上别的进程经 `link` 转来的请求：那台机器的终端里跑的 `runode` 命令行只找、只开
/// 那台机器上的终端。
pub fn serve(host: HostId, link: Link, cx: &mut App) {
    let Some(mut requests) = link.ui_requests() else {
        tracing::warn!("requests from other processes are already being served");
        return;
    };
    cx.spawn(async move |cx| {
        while let Some((ticket, request)) = requests.next().await {
            // 读剪贴板可能要等用户点询问框，自己回话，不挡住后面的请求。
            if let ClientMsg::ReadClipboard { id, ask, program } = request {
                cx.update(|cx| clipboard::read(&host, link.clone(), ticket, id, ask, program, cx));
                continue;
            }
            // 查目录可能卡在挂死的网络卷上，在单独的线程上查，自己回话，不挡住后面的请求。远端主机上的
            // 目录由那边的命令行查过。
            if let ClientMsg::OpenWorkspace { req, dir, focus, name } = request {
                let (host, link) = (host.clone(), link.clone());
                cx.update(|cx| {
                    cx.spawn(async move |cx| {
                        let (is_dir, real) = match host {
                            HostId::Local => super::model::inspect_dir(dir.clone()).await,
                            HostId::Ssh(_) | HostId::Paired(_) => (true, dir.clone()),
                        };
                        let reply = cx.update(|cx| open_workspace(&host, req, &dir, is_dir, &real, focus, name, cx));
                        link.ui_reply(ticket, reply.wait().await);
                    })
                    .detach();
                });
                continue;
            }
            // 要等远端主机上的会话开好的回话自己等，不挡住后面的请求。
            match cx.update(|cx| handle(&host, request, cx)) {
                Reply::Now(reply) => link.ui_reply(ticket, reply),
                Reply::Later(task) => {
                    let link = link.clone();
                    cx.spawn(async move |_| link.ui_reply(ticket, task.await)).detach();
                }
            }
        }
    })
    .detach();
}

/// 请求的回话：当场有的，或者要等远端主机上的会话开好。
enum Reply {
    Now(HostMsg),
    Later(Task<HostMsg>),
}

impl Reply {
    async fn wait(self) -> HostMsg {
        match self {
            Self::Now(reply) => reply,
            Self::Later(task) => task.await,
        }
    }
}

/// 开好了终端 `view`（在 `window` 里）以后回 `Opened`，`focus` 时切过去；远端主机上的会话还在后台开时等它。
fn opened(req: u32, window: WindowHandle<WindowView>, view: &Entity<TerminalView>, focus: bool, cx: &mut App) -> Reply {
    let pane = view.entity_id();
    if let Some(id) = view.read(cx).session_id() {
        if focus {
            reveal(window, pane, cx);
        }
        return Reply::Now(HostMsg::Opened { req, id });
    }
    let waiting = view.update(cx, |view, _| view.opened());
    Reply::Later(cx.spawn(async move |cx| match waiting.await {
        Ok(Some(id)) => {
            if focus {
                cx.update(|cx| reveal(window, pane, cx));
            }
            HostMsg::Opened { req, id }
        }
        _ => HostMsg::Error { req: Some(req), id: None, message: "could not start a terminal".into() },
    }))
}

/// 在最前面的窗口里开主机 `host` 上 `dir` 的 workspace（`OpenWorkspace`），返回回话。`is_dir`、`real` 是在
/// 别的线程上查好的（`inspect_dir`）。
#[allow(clippy::too_many_arguments)]
fn open_workspace(
    host: &HostId,
    req: u32,
    dir: &Path,
    is_dir: bool,
    real: &Path,
    focus: bool,
    name: Option<String>,
    cx: &mut App,
) -> Reply {
    let fail = |message: String| Reply::Now(HostMsg::Error { req: Some(req), id: None, message });
    let Some(window) = front_window(cx) else {
        return fail("there is no Runode window to open it in".into());
    };
    // Windows 远端的目录到这里已经是 `C:/…`，在别的系统上 `Path` 不认它是绝对路径。
    let absolute = dir.is_absolute() || dir.to_str().is_some_and(runode_ssh::bootstrap::windows::is_absolute);
    if !absolute || !is_dir {
        return fail(format!("{} is not a directory", dir.display()));
    }
    let name = name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty());
    let opened_view = window.update(cx, |view, window, cx| view.open_workspace_at(host, real, name, focus, window, cx));
    match opened_view {
        Ok(Ok(view)) => opened(req, window, &view, focus, cx),
        Ok(Err(message)) => fail(message.into()),
        Err(_) => fail("the Runode window went away".into()),
    }
}

/// 主机 `host` 转来的请求，只找那台主机上的终端。
fn handle(host: &HostId, request: ClientMsg, cx: &mut App) -> Reply {
    let fail = |req: u32, message: String| HostMsg::Error { req: Some(req), id: None, message };
    let reply = match request {
        ClientMsg::Open { req, placement, near, cwd, focus, fold, from } => {
            let target = match near {
                Some(id) => find_session(host, id, cx).map(|(window, pane)| (window, Some(pane))),
                None => front_pane(host, cx),
            };
            let Some((window, pane)) = target else {
                return Reply::Now(fail(
                    req,
                    match near {
                        Some(id) => format!("no runode window shows session {id}"),
                        None => "there is no Runode window to open it in".into(),
                    },
                ));
            };
            // 没说折不折叠时，agent 在自己的终端里分出的分屏按配置项 `agent-split-fold` 默认折叠。
            let fold = fold.unwrap_or_else(|| {
                !focus && cx.global::<AppConfig>().0.agent_split_fold && from.is_some_and(|id| runs_agent(host, id, cx))
            });
            let opened = window
                .update(cx, |view, window, cx| view.open_beside(pane, placement, cwd, focus, fold, window, cx))
                .ok()
                .flatten();
            return match opened {
                Some(view) => self::opened(req, window, &view, focus, cx),
                None => Reply::Now(fail(req, "could not start a terminal".into())),
            };
        }
        ClientMsg::RenameWorkspace { req, window, workspace, name } => {
            let name = name.trim();
            if name.is_empty() {
                return Reply::Now(fail(req, "the workspace name is empty".into()));
            }
            let renamed = layout_report::window_at(window, cx).and_then(|handle| {
                handle
                    .update(cx, |view, window, cx| {
                        // 那台机器的命令行只改那台机器上的 workspace。
                        let ix = (workspace as usize)
                            .checked_sub(1)
                            .filter(|ix| view.workspaces.get(*ix).is_some_and(|it| it.host == *host))?;
                        view.set_workspace_name(ix, name.into(), window, cx);
                        Some(())
                    })
                    .ok()
                    .flatten()
            });
            match renamed {
                Some(()) => HostMsg::Done { req },
                None => fail(req, format!("there is no workspace {workspace} in window {window}")),
            }
        }
        ClientMsg::Reveal { req, id } => match find_session(host, id, cx) {
            Some((window, pane)) => {
                reveal(window, pane, cx);
                HostMsg::Done { req }
            }
            None => fail(req, format!("no runode window shows session {id}")),
        },
        ClientMsg::MovePane { req, id, placement, near, focus } => {
            let Some((window, pane)) = find_session(host, id, cx) else {
                return Reply::Now(fail(req, format!("no runode window shows session {id}")));
            };
            let target = match near {
                Some(near) => find_session(host, near, cx),
                None => front_pane(host, cx).and_then(|(window, pane)| Some((window, pane?))),
            };
            let Some((near_window, near_pane)) = target else {
                return Reply::Now(fail(
                    req,
                    match near {
                        Some(near) => format!("no runode window shows session {near}"),
                        None => "the front runode window shows no terminal to move it next to".into(),
                    },
                ));
            };
            if near_window.window_id() != window.window_id() {
                let message = match near {
                    Some(_) => "the two sessions are in different windows".into(),
                    None => format!("session {id} is not in the front runode window"),
                };
                return Reply::Now(fail(req, message));
            }
            if near_pane == pane && placement != Placement::Tab {
                return Reply::Now(fail(req, "cannot move a session next to itself".into()));
            }
            let moved = window
                .update(cx, |view, window, cx| {
                    let to = view.destination(near_pane, placement)?;
                    view.move_pane(pane, to, false, window, cx).then_some(())
                })
                .ok()
                .flatten();
            if moved.is_none() {
                return Reply::Now(fail(req, format!("could not move session {id}")));
            }
            if focus {
                reveal(window, pane, cx);
            }
            HostMsg::Done { req }
        }
        ClientMsg::SwapPanes { req, a, b, focus } => {
            let (found_a, found_b) = (find_session(host, a, cx), find_session(host, b, cx));
            let (Some((window, first)), Some((other, second))) = (found_a, found_b) else {
                let missing = if found_a.is_none() { a } else { b };
                return Reply::Now(fail(req, format!("no runode window shows session {missing}")));
            };
            if other.window_id() != window.window_id() {
                return Reply::Now(fail(req, "the two sessions are in different windows".into()));
            }
            if first == second {
                return Reply::Now(fail(req, "cannot swap a session with itself".into()));
            }
            let swapped = window.update(cx, |view, window, cx| view.swap_panes(first, second, false, window, cx));
            if !matches!(swapped, Ok(true)) {
                return Reply::Now(fail(req, format!("could not swap sessions {a} and {b}")));
            }
            if focus {
                reveal(window, first, cx);
            }
            HostMsg::Done { req }
        }
        ClientMsg::Layout { req } => HostMsg::Layout { req, windows: layout_report::current(host, cx) },
        ClientMsg::WriteClipboard { text, .. } => clipboard::write(text.0, cx),
        other => HostMsg::Error { req: None, id: None, message: format!("the runode app does not handle {other:?}") },
    };
    Reply::Now(reply)
}

/// 显示主机 `host` 上这个会话的窗口和分屏。
pub(super) fn find_session(host: &HostId, id: SessionId, cx: &App) -> Option<(WindowHandle<WindowView>, EntityId)> {
    windows(cx).into_iter().find_map(|window| Some((window, pane_of(window, host, id, cx)?)))
}

/// 主机 `host` 上这个会话的前台跑着认得出的 agent。
fn runs_agent(host: &HostId, id: SessionId, cx: &App) -> bool {
    windows(cx).into_iter().filter_map(|window| window.read(cx).ok()).any(|view| {
        view.panes().any(|(_, terminal)| {
            let terminal = terminal.read(cx);
            terminal.host() == host
                && terminal.session_id() == Some(id)
                && terminal.agent().is_some_and(|agent| agent.kind.is_known())
        })
    })
}

fn pane_of(window: WindowHandle<WindowView>, host: &HostId, id: SessionId, cx: &App) -> Option<EntityId> {
    let view = window.read(cx).ok()?;
    view.panes().find_map(|(pane, terminal)| {
        let terminal = terminal.read(cx);
        (terminal.host() == host && terminal.session_id() == Some(id)).then_some(pane)
    })
}

/// 最前面那个窗口，以及它当前的分屏；当前 workspace 里没有标签时没有分屏。当前 workspace 不在主机
/// `host` 上时没有。
fn front_pane(host: &HostId, cx: &App) -> Option<(WindowHandle<WindowView>, Option<EntityId>)> {
    let window = front_window(cx)?;
    let view = window.read(cx).ok()?;
    if view.workspace().host != *host {
        return None;
    }
    Some((window, view.tab().map(|tab| tab.focused)))
}

/// 最前面那个窗口。app 不在前台时没有活动窗口，按窗口的前后顺序取最前面的。
pub(crate) fn front_window(cx: &App) -> Option<WindowHandle<WindowView>> {
    let front = cx
        .active_window()
        .and_then(|window| window.downcast::<WindowView>())
        .or_else(|| cx.window_stack()?.into_iter().find_map(|window| window.downcast::<WindowView>()));
    front.or_else(|| windows(cx).into_iter().next())
}

/// 最前面那个窗口，终端窗口和单独的远程桌面窗口都算。app 不在前台时（辅助工具、自动化按菜单）没有活动窗口，
/// 按窗口的前后顺序取最前面的。
pub(crate) fn front_any_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.active_window().or_else(|| cx.window_stack()?.into_iter().next()).or_else(|| cx.windows().into_iter().next())
}

pub(crate) fn windows(cx: &App) -> Vec<WindowHandle<WindowView>> {
    cx.windows().into_iter().filter_map(|window| window.downcast::<WindowView>()).collect()
}

impl WindowView {
    /// 在 `beside` 这个分屏旁边开一个新终端，返回它（远端主机上的会话还在后台开，见 `TerminalView::opened`）。`cwd` 为空时沿用旁边那个终端的
    /// 目录。不切过去（`focus` 为假）时当前的 workspace、标签和焦点都不动，看不见的新终端也马上
    /// 启动 shell。`beside` 为空时当前 workspace 里没有标签：新终端不管 `placement`，作为它的第一个
    /// 标签当场显示。`fold` 为真时新分屏直接折叠，经典样式或标签里没有别的展开着的分屏而折不了时照常展开。
    #[allow(clippy::too_many_arguments)]
    fn open_beside(
        &mut self,
        beside: Option<EntityId>,
        placement: Placement,
        cwd: Option<PathBuf>,
        focus: bool,
        fold: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        let Some(beside) = beside else {
            let (host, fallback) = (self.workspace().host.clone(), self.workspace().dir.clone());
            let cwd = format::start_dir(cwd.as_deref(), &fallback);
            let view = self.spawn_terminal_on(&host, Some(&cwd), Some(&fallback), window, cx)?;
            self.insert_tab(0, view.clone(), window, cx);
            return Some(view);
        };
        let (wi, ti) = self.locate(beside)?;
        let near = self.workspaces[wi].tabs[ti].panes[&beside].0.clone();
        let cwd = cwd.or_else(|| near.read(cx).cwd());
        let (host, fallback) = (self.workspaces[wi].host.clone(), self.workspaces[wi].dir.clone());
        let cwd = format::start_dir(cwd.as_deref(), &fallback);
        let view = self.spawn_terminal_on(&host, Some(&cwd), Some(&fallback), window, cx)?;
        match placement {
            Placement::Tab => {
                let tab = self.single_pane_tab(view.clone(), window, cx);
                self.workspaces[wi].add_tab(ti + 1, tab);
            }
            Placement::Right | Placement::Down => {
                let axis = if placement == Placement::Right { Axis::Horizontal } else { Axis::Vertical };
                let split_id = self.next_id();
                let (id, entry) = self.pane_entry(view.clone(), window, cx);
                let tab = &mut self.workspaces[wi].tabs[ti];
                tab.root.split(beside, id, axis, split_id);
                tab.panes.insert(id, entry);
                // 放大着时新分屏看不见。
                tab.zoomed = false;
                if fold && cards(cx) && tab.fold_successor(id).is_some() {
                    tab.collapsed.insert(id);
                }
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
        Some(view)
    }

    /// 切到或者新建主机 `host` 上目录是 `dir`（已经规范化过，见 `inspect_dir`）的 workspace，返回它当前标签里
    /// 有焦点的终端。已经有这个目录的 workspace 时不新建、不改名；那个分屏还没开会话、或者
    /// workspace 里没有标签时在它末尾另开一个标签。没有时在 `dir` 开一个终端，新 workspace 插在当前
    /// workspace 后面，`name` 为空时按目录取名。不切过去（`focus` 为假）时当前的 workspace 不动，新终端
    /// 马上启动 shell；切过去时由调用方 `reveal` 它。
    fn open_workspace_at(
        &mut self,
        host: &HostId,
        dir: &Path,
        name: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Entity<TerminalView>, &'static str> {
        if let Some(wi) =
            self.workspaces.iter().position(|workspace| workspace.host == *host && workspace.real_dir() == dir)
        {
            let workspace = &self.workspaces[wi];
            if let Some(tab) = workspace.tabs.get(workspace.active)
                && tab.focused_view().read(cx).session_id().is_some()
            {
                return Ok(tab.focused_view().clone());
            }
            // 恢复布局时看不见的终端要等显示出来才开会话，见 `TerminalView::deferred`。
            let view = self.spawn_terminal_on(host, Some(dir), None, window, cx).ok_or("could not start a terminal")?;
            if wi == self.active && self.tab().is_none() {
                // 显示着的空 workspace：新标签当场显示出来。
                self.insert_tab(0, view.clone(), window, cx);
                return Ok(view);
            }
            let size = self.reference_size(cx);
            let tab = self.single_pane_tab(view.clone(), window, cx);
            self.workspaces[wi].tabs.push(tab);
            view.update(cx, |view, cx| view.start_at(size, window, cx));
            self.sync_visibility(window, cx);
            self.save(cx);
            cx.notify();
            return Ok(view);
        }
        let view = self.spawn_terminal_on(host, Some(dir), None, window, cx).ok_or("could not start a terminal")?;
        let ix = self.active + 1;
        if focus {
            self.insert_workspace(ix, dir.to_path_buf(), name, view.clone(), window, cx);
        } else {
            let size = self.reference_size(cx);
            self.add_workspace(ix, dir.to_path_buf(), name, view.clone(), window, cx);
            view.update(cx, |view, cx| view.start_at(size, window, cx));
            // 开在不显示的 workspace 里，到时丢掉界面这份 VT。
            self.sync_visibility(window, cx);
        }
        self.save(cx);
        cx.notify();
        Ok(view)
    }
}
