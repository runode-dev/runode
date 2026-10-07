//! 读线程（`host-link-reader`）：读宿主发来的帧，按会话分发成 `LinkEvent`。输出帧按通道找到会话，
//! 快照帧拼好到 `SnapshotEnd` 作为一份 `Screen` 交出去，带 `req` 的回话交给等着的调用方，
//! `UiRequest` 交给界面；连接断开或者宿主说 `Goodbye` 时退出。

use std::{io::BufReader, os::unix::net::UnixStream, sync::mpsc};

use runode_protocol::{AttachMode, ClientMsg, FrameKind, GoodbyeReason, HostMsg, SessionId, read_frame};

use super::{Attached, Inner, LinkEvent, READ_BUFFER, Screen, State, UiTicket};

/// 读线程：读到连接断开（或者宿主说 `Goodbye`）为止。
pub(super) fn read_loop(inner: &Inner, generation: u64, stream: UnixStream) {
    let mut reader = BufReader::with_capacity(READ_BUFFER, stream);
    loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(err) => {
                tracing::debug!("the host connection broke: {err}");
                return;
            }
        };
        match frame.kind {
            FrameKind::Output => output(inner, frame.channel, frame.payload),
            FrameKind::Snapshot => snapshot(inner, frame.channel, &frame.payload),
            FrameKind::Control => match frame.message::<HostMsg>() {
                Ok(HostMsg::Goodbye { reason }) => {
                    tracing::info!("the host said goodbye: {reason:?}");
                    if reason == GoodbyeReason::Shutdown {
                        inner.ended_all(generation);
                    }
                    return;
                }
                Ok(message) => dispatch(inner, message),
                Err(err) => tracing::debug!("unreadable message from the host: {err}"),
            },
            FrameKind::Input => tracing::debug!("the host sent an input frame"),
        }
    }
}

/// 一块输出：交给这个通道的会话；通道不认识（旧的订阅、已经不看了）时丢掉。
pub(super) fn output(inner: &Inner, channel: u32, data: Vec<u8>) {
    let mut state = inner.state();
    let Some(&id) = state.channels.get(&channel) else { return };
    let alive = match state.sessions.get_mut(&id) {
        Some(route) if route.attaching == 0 && route.assembling.is_none() => route.deliver(LinkEvent::Output(data)),
        _ => true,
    };
    if !alive {
        drop_route(&mut state, id);
    }
}

pub(super) fn snapshot(inner: &Inner, channel: u32, data: &[u8]) {
    let mut state = inner.state();
    let Some(&id) = state.channels.get(&channel) else { return };
    if let Some(route) = state.sessions.get_mut(&id)
        && let Some((_, assembled)) = &mut route.assembling
    {
        assembled.extend_from_slice(data);
    }
}

/// 忘掉这个会话的登记，之后它的帧丢掉：收的一方不要了时在这里（视图丢掉时自己发 `Detach` 或
/// `Kill`，这里不发），发了 `Detach`、`Kill` 时在 `Link::forget`。
pub(super) fn drop_route(state: &mut State, id: SessionId) {
    if let Some(route) = state.sessions.remove(&id)
        && let Some(channel) = route.channel
    {
        state.channels.remove(&channel);
    }
}

/// 一条控制消息：回话交给等着的调用方，会话的消息交给那个会话。
pub(super) fn dispatch(inner: &Inner, message: HostMsg) {
    let mut state = inner.state();
    match message {
        HostMsg::Spawned { req, id } => match state.replies.remove(&req) {
            Some(reply) => {
                let _ = reply.send(HostMsg::Spawned { req, id });
            }
            None => {
                // 等的一方已经超时走了，没人要这个会话。
                drop(state);
                tracing::debug!("ending session {id}, spawned after its caller gave up");
                let _ = inner.control(&ClientMsg::Kill { id });
            }
        },
        HostMsg::ProjectTasks { req, dir, sources } => {
            if let Some(reply) = state.replies.remove(&req) {
                let _ = reply.send(HostMsg::ProjectTasks { req, dir, sources });
            }
        }
        HostMsg::SessionList { sessions } => {
            // 等的一方超时走了，它的位置还排在队里：跳过这些，交给下一个还在等的。回话按先后
            // 到，交出去的可能是前一个请求的，那也只早一点。
            let mut sessions = sessions;
            while let Some(reply) = state.lists.pop_front() {
                match reply.send(sessions) {
                    Ok(()) => break,
                    Err(mpsc::SendError(back)) => sessions = back,
                }
            }
        }
        HostMsg::UiRequest { ui, request } => {
            let ticket = UiTicket::new(ui, state.generation);
            drop(state);
            if inner.ui.unbounded_send((ticket, *request)).is_err() {
                let reply = HostMsg::Error { req: None, id: None, message: "the runode app is quitting".into() };
                let _ = inner.control(&ClientMsg::UiReply { ui, reply: Box::new(reply) });
            }
        }
        HostMsg::Attached { id, channel, size, mode, meta, settings } => {
            let Some(route) = state.sessions.get_mut(&id) else {
                // 等着的时候不看了（`Detach`、`Kill` 已经跟在 `Attach` 后面发出去了）。
                return;
            };
            route.attaching = route.attaching.saturating_sub(1);
            if route.attaching > 0 {
                // 之前那个 `Attach` 的，新的还在后面。
                return;
            }
            route.channel = Some(channel);
            let queued = std::mem::take(&mut route.queued);
            let attached = Attached { id, channel, size, mode, meta, settings };
            let alive = if mode == AttachMode::MetaOnly {
                route.deliver(LinkEvent::Screen(Screen { attached, data: Vec::new() })) && route.settle()
            } else {
                route.assembling = Some((attached, Vec::new()));
                true
            };
            state.channels.insert(channel, id);
            if !alive {
                drop_route(&mut state, id);
                return;
            }
            // 拿着 `state` 写：别的线程要等这里写完才看得到通道，之后的输入排在攒着的后面。
            for data in queued {
                if inner.write(FrameKind::Input, channel, &data).is_err() {
                    break;
                }
            }
        }
        HostMsg::SnapshotEnd { id } => {
            let Some(route) = state.sessions.get_mut(&id) else { return };
            if let Some((attached, data)) = route.assembling.take()
                && !(route.deliver(LinkEvent::Screen(Screen { attached, data })) && route.settle())
            {
                drop_route(&mut state, id);
            }
        }
        HostMsg::Error { req: Some(req), id, message } if state.replies.contains_key(&req) => {
            if let Some(reply) = state.replies.remove(&req) {
                let _ = reply.send(HostMsg::Error { req: Some(req), id, message });
            }
        }
        HostMsg::Error { req, id: Some(id), message } => {
            let Some(route) = state.sessions.get_mut(&id) else {
                tracing::debug!("error from the host about session {id}: {message}");
                return;
            };
            // 正连着时的错误当成这次没连成（多半是会话已经没了）。
            if route.attaching > 0 {
                route.attaching -= 1;
                if let Some(first) = route.first_screen.take() {
                    let _ = first.send(Err(message));
                    return;
                }
            }
            if !(route.deliver(LinkEvent::Msg(HostMsg::Error { req, id: Some(id), message })) && route.settle()) {
                drop_route(&mut state, id);
            }
        }
        HostMsg::Error { message, .. } => tracing::warn!("the host reported an error: {message}"),
        message => {
            let Some(id) = session_of(&message) else {
                tracing::debug!("ignored a message from the host: {message:?}");
                return;
            };
            let alive = match state.sessions.get_mut(&id) {
                Some(route) if route.attaching == 0 => route.deliver(LinkEvent::Msg(message)),
                // 正连着时旧订阅的消息用不上：新的屏幕和 `Attached` 带着最新的状态。只有会话结束了
                // 要留着：新的订阅可能连不上（会话已经没了），视图只能靠它知道。
                Some(route) if matches!(message, HostMsg::Exited { .. }) => {
                    route.exited = Some(message);
                    true
                }
                _ => true,
            };
            if !alive {
                drop_route(&mut state, id);
            }
        }
    }
}

/// 属于某个会话、要交给它的消息是哪个会话的。
fn session_of(message: &HostMsg) -> Option<SessionId> {
    match message {
        HostMsg::Resized { id, .. }
        | HostMsg::ThemeApplied { id, .. }
        | HostMsg::Meta { id, .. }
        | HostMsg::CommandFinished { id, .. }
        | HostMsg::Resync { id, .. }
        | HostMsg::Exited { id, .. }
        | HostMsg::Bell { id }
        | HostMsg::ScreenText { id, .. }
        | HostMsg::SizeOwner { id, .. } => Some(*id),
        _ => None,
    }
}
