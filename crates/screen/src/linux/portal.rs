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
use futures::{FutureExt as _, executor::block_on};
use runode_types::{
    input::MouseButton,
    screen::{ScreenInput, ScreenMods},
};

use super::{keysym, pipewire};
use crate::Display;

/// 等屏幕上的人点「允许」最多这么久。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(90);
const BTN_LEFT: i32 = 0x110;
const BTN_RIGHT: i32 = 0x111;
const BTN_MIDDLE: i32 = 0x112;

/// 这个进程开着的会话。列屏幕、采集和注入各自拿一会儿就放手，会话要留在这里，不然每次都另开一个。
static PORTAL: Mutex<Option<Arc<Portal>>> = Mutex::new(None);
/// 上次在屏幕上请求授权时没被允许（拒绝了、没人理）的错误：`ask_again` 之前不再弹框，照样报它。
static REFUSED: Mutex<Option<(io::ErrorKind, String)>> = Mutex::new(None);

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
    /// 用户在屏幕上停掉了，或者流断了：下次要用时另开一个。
    closed: AtomicBool,
}

impl Portal {
    /// 这个进程的会话，没开或者已经停掉时开一个（可能要等屏幕上的人点「允许」）。
    fn get() -> io::Result<Arc<Self>> {
        let mut current = PORTAL.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(portal) = current.as_ref()
            && !portal.closed.load(Ordering::Relaxed)
        {
            return Ok(portal.clone());
        }
        // 停掉了的那个先关掉，再开新的。
        drop(current.take());
        if let Some((kind, reason)) = REFUSED.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return Err(io::Error::new(kind, reason));
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
            let timeout = sleep(APPROVAL_TIMEOUT).fuse();
            futures::pin_mut!(started, timeout);
            futures::select! {
                started = started => Some(started),
                () = timeout => None,
            }
        };
        let Some(started) = started else {
            let _ = session.close().await;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "nobody allowed remote desktop on this computer's screen in time",
            ));
        };
        let selected = started.map_err(other)?.response().map_err(|err| {
            io::Error::new(
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
        Ok(Self { remote, screencast, session, screens, closed: AtomicBool::new(false) })
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
        if let Err(err) = block_on(self.try_inject(screen, input)) {
            tracing::debug!("cannot inject {input:?}: {err}");
            if matches!(err, ashpd::Error::Portal(_) | ashpd::Error::Zbus(_)) {
                self.closed.store(true, Ordering::Relaxed);
            }
        }
    }

    async fn try_inject(&self, screen: Screen, input: &ScreenInput) -> ashpd::Result<()> {
        match input {
            ScreenInput::MouseMove { x, y } => self.move_to(screen, *x, *y).await,
            ScreenInput::MouseButton { button, down, x, y, mods } => {
                self.move_to(screen, *x, *y).await?;
                let held = if *down { keysym::modifiers(*mods) } else { Vec::new() };
                self.keys(&held, true).await?;
                self.button(*button, *down).await?;
                self.keys(&held, false).await
            }
            ScreenInput::Scroll { dx, dy } => {
                // 正的 `dy` 是内容往下走、往上滚，portal 的格数朝下为正；横向同理。
                for (axis, delta) in [(Axis::Vertical, *dy), (Axis::Horizontal, *dx)] {
                    if delta != 0 {
                        let steps = -delta.signum() * (delta.abs() / super::POINTS_PER_NOTCH).max(1);
                        self.remote
                            .notify_pointer_axis_discrete(&self.session, axis, steps, Default::default())
                            .await?;
                    }
                }
                Ok(())
            }
            ScreenInput::Key { key, mods } => {
                let Some(keysym) = keysym::of_key(*key) else { return Ok(()) };
                self.chord(*mods, keysym).await
            }
            ScreenInput::Text { text } => {
                for keysym in text.chars().filter_map(keysym::of_char) {
                    self.keysym(keysym, true).await?;
                    self.keysym(keysym, false).await?;
                }
                Ok(())
            }
        }
    }

    async fn move_to(&self, screen: Screen, x: f64, y: f64) -> ashpd::Result<()> {
        let (width, height) = (f64::from(screen.size.0), f64::from(screen.size.1));
        let (x, y) = (x.clamp(0.0, 1.0) * (width - 1.0).max(0.0), y.clamp(0.0, 1.0) * (height - 1.0).max(0.0));
        self.remote.notify_pointer_motion_absolute(&self.session, screen.node, x, y, Default::default()).await
    }

    async fn button(&self, button: MouseButton, down: bool) -> ashpd::Result<()> {
        let code = match button {
            MouseButton::Left => BTN_LEFT,
            MouseButton::Right => BTN_RIGHT,
            MouseButton::Middle => BTN_MIDDLE,
        };
        self.remote.notify_pointer_button(&self.session, code, key_state(down), Default::default()).await
    }

    async fn keysym(&self, keysym: u32, down: bool) -> ashpd::Result<()> {
        self.remote.notify_keyboard_keysym(&self.session, keysym as i32, key_state(down), Default::default()).await
    }

    async fn keys(&self, keysyms: &[u32], down: bool) -> ashpd::Result<()> {
        let ordered: Vec<u32> = if down { keysyms.to_vec() } else { keysyms.iter().rev().copied().collect() };
        for keysym in ordered {
            self.keysym(keysym, down).await?;
        }
        Ok(())
    }

    /// 像真键盘一样先按下修饰键，按一下 `keysym`，再松开修饰键。
    async fn chord(&self, mods: ScreenMods, keysym: u32) -> ashpd::Result<()> {
        let held = keysym::modifiers(mods);
        self.keys(&held, true).await?;
        self.keysym(keysym, true).await?;
        self.keysym(keysym, false).await?;
        self.keys(&held, false).await
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

/// `duration` 以后完成，在一个睡着的线程上计时。
pub(super) async fn sleep(duration: Duration) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(duration);
        let _ = tx.send(());
    });
    let _ = rx.await;
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
        self.stream.wait(timeout).inspect_err(|_| self.portal.closed.store(true, Ordering::Relaxed))
    }

    fn picture(&self) -> Option<super::capture::Picture<'_>> {
        self.stream.picture()
    }
}

/// 往一块屏幕上注入输入，记着按住的鼠标键；丢掉时松开还按着的鼠标键。
pub(crate) struct Injector {
    portal: Arc<Portal>,
    screen: Screen,
    pressed: Vec<MouseButton>,
}

impl Injector {
    pub(crate) fn new(display: u32) -> io::Result<Self> {
        let portal = Portal::get()?;
        let screen = portal.screen(display);
        Ok(Self { portal, screen, pressed: Vec::new() })
    }

    pub(crate) fn inject(&mut self, input: &ScreenInput) {
        if let ScreenInput::MouseButton { button, down, .. } = input {
            self.pressed.retain(|held| held != button);
            if *down {
                self.pressed.push(*button);
            }
        }
        self.portal.inject(self.screen, input);
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        for button in std::mem::take(&mut self.pressed) {
            let _ = block_on(self.portal.button(button, false));
        }
    }
}
