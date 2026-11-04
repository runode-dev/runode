//! 别的 Wayland 桌面（KDE 等）的远程桌面：经 xdg-desktop-portal 的 RemoteDesktop 加 ScreenCast 开一个会话，键鼠和
//! 屏幕一起要。第一次开时 portal 在这台电脑的屏幕上弹框，要有人点「允许」；允许后交回的恢复令牌存在
//! `Dirs::screen_restore_token_file`，之后再开就不再问，直到用户在系统设置里收回。
//!
//! 一个进程只开一个会话（`Portal`），各块屏幕是它的各路流，采集和注入共用，一直开到进程退出（采集进程没人看时
//! 自己退出）；会话被用户在屏幕上停掉以后，下次要用时再开一个。ashpd 的接口是异步的，这里都在调用线程上 `block_on`。

use std::{
    io,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ashpd::{
    WindowIdentifier,
    desktop::{
        PersistMode, Session,
        remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions},
        screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    },
    enumflags2::BitFlags,
};
use futures::{FutureExt as _, StreamExt as _, executor::block_on};
use runode_types::{input::MouseButton, screen::ScreenInput};

use super::{
    pipewire,
    remote_input::{self, RemoteInput},
};
use crate::Display;

/// 等屏幕上的人点「允许」最多这么久。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(90);

/// 这个进程开着的会话。列屏幕、采集和注入各自拿一会儿就放手，会话要留在这里，不然每次都另开一个。
static PORTAL: Mutex<Option<Arc<Portal>>> = Mutex::new(None);
/// 上次在屏幕上请求授权时没被允许（拒绝了、没人理）的错误：`ask_again` 之前不再弹框，照样报它。
static REFUSED: Mutex<Option<(io::ErrorKind, String)>> = Mutex::new(None);

/// 屏幕上的人没允许：拒绝了是 `PermissionDenied`，没人理是 `TimedOut`。
fn declined(kind: io::ErrorKind, message: impl Into<String>) -> io::Error {
    crate::caused_error(crate::StopCause::Declined, kind, message)
}

/// 下次要会话时可以再在屏幕上请求授权。
pub(crate) fn ask_again() {
    *REFUSED.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

fn other(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

/// 会话里的一路屏幕流。
#[derive(Clone, Copy, Debug)]
struct Screen {
    node: u32,
    /// 排版尺寸（点数），绝对坐标按它算。
    size: (u32, u32),
}

/// 开着的 portal 远程桌面会话，丢掉时关掉。
pub(crate) struct Portal {
    remote: RemoteDesktop,
    screencast: Screencast,
    session: Session<RemoteDesktop>,
    screens: Vec<Screen>,
    /// 会话的 `Closed` 信号：用户在屏幕上停掉、portal 收回会话时来，PipeWire 的流不一定跟着报错。
    closed_signal: Mutex<zbus::proxy::SignalStream<'static>>,
    /// 会话停了，或者流断了：下次要用时另开一个。
    closed: AtomicBool,
}

impl Portal {
    /// 这个进程的会话，没开或者已经停掉时开一个（可能要等屏幕上的人点「允许」）。
    fn get() -> io::Result<Arc<Self>> {
        let mut current = PORTAL.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(portal) = current.as_ref()
            && !portal.closed()
        {
            return Ok(portal.clone());
        }
        // 停掉了的那个先关掉，再开新的。
        drop(current.take());
        if let Some((kind, reason)) = REFUSED.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return Err(declined(kind, reason));
        }
        let portal = block_on(Self::open()).inspect_err(|err| {
            if matches!(err.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::TimedOut) {
                *REFUSED.lock().unwrap_or_else(PoisonError::into_inner) = Some((err.kind(), err.to_string()));
            }
        })?;
        let portal = Arc::new(portal);
        *current = Some(portal.clone());
        Ok(portal)
    }

    async fn open() -> io::Result<Self> {
        let remote = RemoteDesktop::new().await.map_err(other)?;
        let screencast = Screencast::new().await.map_err(other)?;
        let session = remote.create_session(Default::default()).await.map_err(other)?;
        let closed_signal = match closed_signal(&remote, &session).await {
            Ok(signal) => signal,
            Err(err) => {
                let _ = session.close().await;
                return Err(other(err));
            }
        };
        let token = restore_token();
        remote
            .select_devices(
                &session,
                SelectDevicesOptions::default()
                    .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                    .set_persist_mode(PersistMode::ExplicitlyRevoked)
                    .set_restore_token(token.as_deref()),
            )
            .await
            .map_err(other)?;
        let cursor = match screencast.available_cursor_modes().await {
            Ok(modes) if modes.contains(CursorMode::Embedded) => CursorMode::Embedded,
            _ => CursorMode::Hidden,
        };
        screencast
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(cursor)
                    .set_sources(BitFlags::from(SourceType::Monitor))
                    .set_multiple(true),
            )
            .await
            .map_err(other)?;
        if token.is_none() {
            tracing::info!("asking on this computer's screen to allow remote desktop");
        }
        let started = {
            let started = remote.start(&session, None::<&WindowIdentifier>, Default::default()).fuse();
            let timeout = remote_input::sleep(APPROVAL_TIMEOUT).fuse();
            futures::pin_mut!(started, timeout);
            futures::select! {
                started = started => Some(started),
                () = timeout => None,
            }
        };
        let Some(started) = started else {
            let _ = session.close().await;
            return Err(declined(
                io::ErrorKind::TimedOut,
                "nobody allowed remote desktop on this computer's screen in time",
            ));
        };
        let selected = started.map_err(other)?.response().map_err(|err| {
            declined(
                io::ErrorKind::PermissionDenied,
                format!("remote desktop was not allowed on this computer's screen: {err}"),
            )
        })?;
        if let Some(token) = selected.restore_token() {
            save_restore_token(token);
        }
        if !selected.devices().contains(DeviceType::Pointer) {
            tracing::warn!("the portal did not allow controlling the pointer");
        }
        let screens: Vec<Screen> = selected
            .streams()
            .iter()
            .map(|stream| {
                let (width, height) = stream.size().unwrap_or((1920, 1080));
                Screen { node: stream.pipe_wire_node_id(), size: (width.max(1) as u32, height.max(1) as u32) }
            })
            .collect();
        if screens.is_empty() {
            let _ = session.close().await;
            return Err(io::Error::other("the portal shared no screen"));
        }
        tracing::info!("portal remote desktop with {} screen(s)", screens.len());
        Ok(Self {
            remote,
            screencast,
            session,
            screens,
            closed_signal: Mutex::new(closed_signal),
            closed: AtomicBool::new(false),
        })
    }

    /// 会话停了，要另开一个。
    fn closed(&self) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return true;
        }
        let mut signal = self.closed_signal.lock().unwrap_or_else(PoisonError::into_inner);
        // 信号来过，或者连接断了。
        if signal.next().now_or_never().is_some() {
            tracing::info!("the portal closed the remote desktop session");
            self.closed.store(true, Ordering::Relaxed);
        }
        self.closed.load(Ordering::Relaxed)
    }

    /// 编号是 `display` 的那路屏幕；没有时是第一路。
    fn screen(&self, display: u32) -> Screen {
        self.screens.iter().find(|screen| screen.node == display).copied().unwrap_or(self.screens[0])
    }

    fn stream(&self, screen: Screen) -> io::Result<pipewire::Stream> {
        let fd = block_on(self.screencast.open_pipe_wire_remote(&self.session, Default::default())).map_err(other)?;
        pipewire::Stream::connect(screen.node, Some(fd), screen.size)
    }

    fn inject(&self, screen: Screen, input: &ScreenInput) {
        if let Err(err) = remote_input::inject(&Target { portal: self, screen }, input) {
            tracing::debug!("cannot inject {}: {err}", super::input_kind(input));
            if matches!(err, ashpd::Error::Portal(_) | ashpd::Error::Zbus(_)) {
                self.closed.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// 订 `session` 的 `Closed` 信号。portal 只把它发给开会话的那条连接，所以订在 ashpd 用的连接上。
async fn closed_signal(
    remote: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
) -> zbus::Result<zbus::proxy::SignalStream<'static>> {
    // ashpd 不交出会话的对象路径，会话按 D-Bus 的对象路径序列化。
    let context = zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::LE, 0);
    let (path, _): (zbus::zvariant::OwnedObjectPath, _) = zbus::zvariant::to_bytes(context, session)?.deserialize()?;
    let proxy =
        zbus::Proxy::new(remote.connection(), "org.freedesktop.portal.Desktop", path, "org.freedesktop.portal.Session")
            .await?;
    proxy.receive_signal("Closed").await
}

/// 会话里的一路屏幕，注入的目标；ashpd 的每次调用都在调用线程上 `block_on`。
struct Target<'a> {
    portal: &'a Portal,
    screen: Screen,
}

impl RemoteInput for Target<'_> {
    type Error = ashpd::Error;

    fn extent(&self) -> (u32, u32) {
        self.screen.size
    }

    fn pointer_to(&self, x: f64, y: f64) -> ashpd::Result<()> {
        let Portal { remote, session, .. } = self.portal;
        block_on(remote.notify_pointer_motion_absolute(session, self.screen.node, x, y, Default::default()))
    }

    fn button(&self, code: i32, down: bool) -> ashpd::Result<()> {
        let Portal { remote, session, .. } = self.portal;
        block_on(remote.notify_pointer_button(session, code, key_state(down), Default::default()))
    }

    fn scroll(&self, vertical: bool, steps: i32) -> ashpd::Result<()> {
        let Portal { remote, session, .. } = self.portal;
        let axis = if vertical { Axis::Vertical } else { Axis::Horizontal };
        block_on(remote.notify_pointer_axis_discrete(session, axis, steps, Default::default()))
    }

    fn keysym(&self, keysym: u32, down: bool) -> ashpd::Result<()> {
        let Portal { remote, session, .. } = self.portal;
        block_on(remote.notify_keyboard_keysym(session, keysym as i32, key_state(down), Default::default()))
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        if let Err(err) = block_on(self.session.close()) {
            tracing::debug!("cannot close the portal session: {err}");
        }
    }
}

fn key_state(down: bool) -> KeyState {
    if down { KeyState::Pressed } else { KeyState::Released }
}

fn restore_token() -> Option<String> {
    let path = runode_paths::Dirs::from_env().screen_restore_token_file()?;
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn save_restore_token(token: &str) {
    let Some(path) = runode_paths::Dirs::from_env().screen_restore_token_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(err) = runode_paths::replace_file(&path, token.as_bytes()) {
        tracing::warn!("cannot save the remote desktop restore token: {err}");
    }
}

/// portal 共享的各块屏幕，编号是流的 PipeWire 节点；要开会话，第一次可能要等屏幕上的人允许。
pub(crate) fn displays() -> io::Result<Vec<Display>> {
    let portal = Portal::get()?;
    Ok(portal
        .screens
        .iter()
        .map(|screen| Display {
            id: screen.node,
            width: screen.size.0,
            height: screen.size.1,
            pixel_width: screen.size.0,
            pixel_height: screen.size.1,
        })
        .collect())
}

/// 一块屏幕的画面。流断了时记下会话停了，下次另开。
pub(crate) struct Source {
    stream: pipewire::Stream,
    portal: Arc<Portal>,
}

impl Source {
    pub(crate) fn open(display: u32) -> io::Result<Self> {
        let portal = Portal::get()?;
        let stream =
            portal.stream(portal.screen(display)).inspect_err(|_| portal.closed.store(true, Ordering::Relaxed))?;
        Ok(Self { stream, portal })
    }
}

impl super::capture::Source for Source {
    fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        // 会话停了时流不一定报错，可能一直停着不来新画面。
        if self.portal.closed() {
            return Err(io::Error::other("the portal closed the remote desktop session"));
        }
        self.stream.wait(timeout).inspect_err(|_| self.portal.closed.store(true, Ordering::Relaxed))
    }

    fn picture(&self) -> Option<super::capture::Picture<'_>> {
        self.stream.picture()
    }
}

/// 往一块屏幕上注入输入。
pub(crate) struct Injector {
    portal: Arc<Portal>,
    screen: Screen,
}

impl Injector {
    pub(crate) fn new(display: u32) -> io::Result<Self> {
        let portal = Portal::get()?;
        let screen = portal.screen(display);
        Ok(Self { portal, screen })
    }
}

impl super::Inject for Injector {
    fn inject(&mut self, input: &ScreenInput) {
        self.portal.inject(self.screen, input);
    }

    fn release(&mut self, button: MouseButton) {
        let _ = Target { portal: &self.portal, screen: self.screen }.button(remote_input::button_code(button), false);
    }
}
