//! GNOME 的 Wayland 远程桌面：经会话总线调 Mutter 自己的 RemoteDesktop 和 ScreenCast 接口（gnome-remote-desktop
//! 用的那套），不像 portal 那样要有人在屏幕上点「允许」。一块屏幕一个远程桌面会话（`Session`）：录屏流交给
//! `pipewire` 收画面，键鼠按 keysym 和 evdev 的按键码交给同一个会话注入；同一个进程里采集和注入共用它。屏幕列表
//! 和缩放问 `org.gnome.Mutter.DisplayConfig`。

use std::{
    collections::HashMap,
    io,
    pin::pin,
    sync::{
        Arc, Mutex, OnceLock, PoisonError, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{
    FutureExt as _, StreamExt as _,
    executor::block_on,
    future::{Either, select},
};

use runode_types::{input::MouseButton, screen::ScreenInput};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

use super::{
    pipewire,
    remote_input::{self, RemoteInput},
};
use crate::Display;

const REMOTE_DESKTOP: &str = "org.gnome.Mutter.RemoteDesktop";
const SCREEN_CAST: &str = "org.gnome.Mutter.ScreenCast";
const DISPLAY_CONFIG: &str = "org.gnome.Mutter.DisplayConfig";
/// `RecordMonitor` 的 `cursor-mode`：把指针画进画面。
const CURSOR_EMBEDDED: u32 = 1;
const CURSOR_HIDDEN: u32 = 0;
/// 等 Mutter 报出录屏流的 PipeWire 节点最多这么久。
const STREAM_TIMEOUT: Duration = Duration::from_secs(5);

/// 各块屏幕开着的会话，采集和注入共用；都放手了就关掉。
static SESSIONS: Mutex<Option<HashMap<u32, Weak<Session>>>> = Mutex::new(None);
/// Mutter 拒绝过这个进程调它的接口：之后不再试，`available` 为假。
static DENIED: AtomicBool = AtomicBool::new(false);

/// D-Bus 的错误换成 `io::Error`；Mutter 不许调（`AccessDenied`）时记下，错误是 `io::ErrorKind::PermissionDenied`。
fn other(err: zbus::Error) -> io::Error {
    if let zbus::Error::MethodError(name, ..) = &err
        && name.as_str() == "org.freedesktop.DBus.Error.AccessDenied"
    {
        DENIED.store(true, Ordering::Relaxed);
        return io::Error::new(io::ErrorKind::PermissionDenied, err.to_string());
    }
    io::Error::other(err.to_string())
}

fn connection() -> io::Result<Connection> {
    Connection::session().map_err(|err| io::Error::other(format!("cannot reach the session bus: {err}")))
}

/// 会话总线上有 Mutter 的远程桌面接口（GNOME 的 Wayland 会话），而且它没拒绝过这个进程。总线答过一次有没有就记着：
/// 图形会话在，Mutter 就一直在；问不到时下次再问。
pub(crate) fn available() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    if DENIED.load(Ordering::Relaxed) {
        return false;
    }
    if let Some(present) = PRESENT.get() {
        return *present;
    }
    let asked = connection().ok().and_then(|conn| {
        let bus = zbus::blocking::fdo::DBusProxy::new(&conn).ok()?;
        bus.name_has_owner(zbus::names::BusName::try_from(REMOTE_DESKTOP).ok()?).ok()
    });
    asked.is_some_and(|present| *PRESENT.get_or_init(|| present))
}

/// 一块屏幕：Mutter 按接口名（`connector`）认，编号是它的散列。
#[derive(Clone, Debug, PartialEq)]
struct Monitor {
    connector: String,
    primary: bool,
    /// 排版尺寸（点数），按缩放比例折过。
    logical: (u32, u32),
    /// 当前模式的像素数。
    pixels: (u32, u32),
}

impl Monitor {
    fn id(&self) -> u32 {
        connector_id(&self.connector)
    }
}

/// 接口名的 FNV-1a 散列：同一块屏幕插在同一个口上，编号就不变。
fn connector_id(connector: &str) -> u32 {
    connector.bytes().fold(0x811c_9dc5u32, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193))
}

type MonitorSpec = (String, String, String, String);
type Mode = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
type PhysicalMonitor = (MonitorSpec, Vec<Mode>, HashMap<String, OwnedValue>);
type LogicalMonitor = (i32, i32, f64, u32, bool, Vec<MonitorSpec>, HashMap<String, OwnedValue>);
type CurrentState = (u32, Vec<PhysicalMonitor>, Vec<LogicalMonitor>, HashMap<String, OwnedValue>);

/// 开着的屏幕，主屏排第一。
fn monitors(conn: &Connection) -> io::Result<Vec<Monitor>> {
    let proxy = Proxy::new(conn, DISPLAY_CONFIG, "/org/gnome/Mutter/DisplayConfig", DISPLAY_CONFIG).map_err(other)?;
    let (_, physical, logical, properties): CurrentState = proxy.call("GetCurrentState", &()).map_err(other)?;
    Ok(arrange(&physical, &logical, &properties))
}

fn arrange(
    physical: &[PhysicalMonitor],
    logical: &[LogicalMonitor],
    properties: &HashMap<String, OwnedValue>,
) -> Vec<Monitor> {
    // `layout-mode` 为 1 时按缩放过的点数排版，为 2 时按像素排版；没有这项时是按点数。
    let logical_layout =
        properties.get("layout-mode").and_then(|mode| u32::try_from(mode).ok()).is_none_or(|mode| mode == 1);
    let mut monitors = Vec::new();
    for (_, _, scale, transform, primary, specs, _) in logical {
        for spec in specs {
            let Some((_, modes, _)) = physical.iter().find(|(candidate, _, _)| candidate == spec) else { continue };
            let current = modes
                .iter()
                .find(|mode| mode.6.get("is-current").and_then(|value| bool::try_from(value).ok()).unwrap_or(false));
            let Some((_, width, height, ..)) = current else { continue };
            let (mut width, mut height) = (*width as u32, *height as u32);
            // 转了 90° 或 270° 的屏幕宽高对调。
            if transform % 2 == 1 {
                std::mem::swap(&mut width, &mut height);
            }
            let scale = if logical_layout && *scale > 0.0 { *scale } else { 1.0 };
            let points = ((f64::from(width) / scale).round() as u32, (f64::from(height) / scale).round() as u32);
            monitors.push(Monitor {
                connector: spec.0.clone(),
                primary: *primary,
                logical: points,
                pixels: (width, height),
            });
        }
    }
    monitors.sort_by_key(|monitor| !monitor.primary);
    monitors
}

pub(crate) fn displays() -> io::Result<Vec<Display>> {
    let conn = connection()?;
    Ok(monitors(&conn)?
        .iter()
        .map(|monitor| Display {
            id: monitor.id(),
            width: monitor.logical.0,
            height: monitor.logical.1,
            pixel_width: monitor.pixels.0,
            pixel_height: monitor.pixels.1,
        })
        .collect())
}

/// 一块屏幕的远程桌面会话：开着一路录屏流，键鼠也经它注入。丢掉时停下。
pub(crate) struct Session {
    remote: Proxy<'static>,
    /// 录屏流的对象路径，注入绝对坐标时要带上。
    stream_path: String,
    /// 录屏流的 PipeWire 节点。
    node: u32,
    /// 录屏流的尺寸（点数），商量画面格式时的默认值。
    size: (u32, u32),
    /// 录屏流的像素尺寸：Mutter 收的绝对坐标是流里的像素，屏幕缩放时它再除以缩放比例换成点数。
    pixels: (u32, u32),
    /// 指针画进了画面。录屏流开了就改不了，要换就另开一个会话。
    cursor: bool,
    /// 会话的 `Closed` 信号：屏幕拔掉、本机改了分辨率或缩放时 Mutter 关掉会话，录屏流跟着没了。
    closed_signal: Mutex<zbus::proxy::SignalStream<'static>>,
    closed: AtomicBool,
}

impl Session {
    /// 屏幕 `display` 的会话：这个进程里已经开着、指针画不画和 `cursor` 一样就共用，不然另开一个（`cursor` 为空时
    /// 不挑，新开的画指针），之后再要的都拿新开的；屏幕不在了用主屏。
    pub(crate) fn get(display: u32, cursor: Option<bool>) -> io::Result<Arc<Self>> {
        let mut sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        let sessions = sessions.get_or_insert_default();
        sessions.retain(|_, session| session.strong_count() > 0);
        if let Some(session) = sessions.get(&display).and_then(Weak::upgrade)
            && !session.closed()
            && cursor.is_none_or(|cursor| cursor == session.cursor)
        {
            return Ok(session);
        }
        let session = Arc::new(Self::open(display, cursor.unwrap_or(true))?);
        sessions.insert(display, Arc::downgrade(&session));
        Ok(session)
    }

    /// 屏幕 `display` 现在共用的会话，没开着或者已经关了时为空；不开新的。
    fn current(display: u32) -> Option<Arc<Self>> {
        let sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        let session = sessions.as_ref()?.get(&display)?.upgrade()?;
        (!session.closed()).then_some(session)
    }

    fn open(display: u32, cursor: bool) -> io::Result<Self> {
        let conn = connection()?;
        let monitors = monitors(&conn)?;
        let monitor = monitors
            .iter()
            .find(|monitor| monitor.id() == display)
            .or(monitors.first())
            .ok_or_else(|| io::Error::other("Mutter lists no monitor"))?;
        let manager =
            Proxy::new(&conn, REMOTE_DESKTOP, "/org/gnome/Mutter/RemoteDesktop", REMOTE_DESKTOP).map_err(other)?;
        let path: OwnedObjectPath = manager.call("CreateSession", &()).map_err(other)?;
        let remote = Proxy::new(&conn, REMOTE_DESKTOP, path, format!("{REMOTE_DESKTOP}.Session")).map_err(other)?;
        let session_id: String = remote.get_property("SessionId").map_err(other)?;
        let cast = Proxy::new(&conn, SCREEN_CAST, "/org/gnome/Mutter/ScreenCast", SCREEN_CAST).map_err(other)?;
        let options = HashMap::from([("remote-desktop-session-id", Value::from(session_id))]);
        let cast_path: OwnedObjectPath = cast.call("CreateSession", &(options,)).map_err(other)?;
        let cast_session =
            Proxy::new(&conn, SCREEN_CAST, cast_path, format!("{SCREEN_CAST}.Session")).map_err(other)?;
        let mode = if cursor { CURSOR_EMBEDDED } else { CURSOR_HIDDEN };
        let record = HashMap::from([("cursor-mode", Value::from(mode))]);
        let stream_path: OwnedObjectPath =
            cast_session.call("RecordMonitor", &(monitor.connector.as_str(), record)).map_err(other)?;
        let stream =
            Proxy::new(&conn, SCREEN_CAST, stream_path.clone(), format!("{SCREEN_CAST}.Stream")).map_err(other)?;
        // 信号要在开始之前订上，开始以后 Mutter 才报节点。
        let mut added = block_on(stream.inner().receive_signal("PipeWireStreamAdded")).map_err(other)?;
        let closed_signal = block_on(remote.inner().receive_signal("Closed")).map_err(other)?;
        // 开始以后就要停：先放进 `Session`，下面出错时由它的 `Drop` 停掉。
        let mut this = Self {
            remote,
            stream_path: stream_path.to_string(),
            node: 0,
            size: monitor.logical,
            pixels: monitor.pixels,
            cursor,
            closed_signal: Mutex::new(closed_signal),
            closed: AtomicBool::new(false),
        };
        this.remote.call::<_, _, ()>("Start", &()).map_err(other)?;
        let node = block_on(async {
            match select(pin!(added.next()), pin!(remote_input::sleep(STREAM_TIMEOUT))).await {
                Either::Left((Some(message), _)) => message.body().deserialize::<u32>().ok(),
                _ => None,
            }
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Mutter did not start the screen stream"))?;
        tracing::info!("Mutter screen cast of {} on PipeWire node {node}", monitor.connector);
        this.node = node;
        Ok(this)
    }

    /// Mutter 关掉了这个会话，要重开一个。
    fn closed(&self) -> bool {
        if self.closed.load(Ordering::Relaxed) {
            return true;
        }
        let mut signal = self.closed_signal.lock().unwrap_or_else(PoisonError::into_inner);
        // 信号来过，或者连接断了。
        if signal.next().now_or_never().is_some() {
            tracing::info!("Mutter closed the remote desktop session");
            self.closed.store(true, Ordering::Relaxed);
        }
        self.closed.load(Ordering::Relaxed)
    }

    /// 收这块屏幕画面的流。
    pub(crate) fn stream(&self) -> io::Result<pipewire::Stream> {
        pipewire::Stream::connect(self.node, None, self.size)
    }

    pub(crate) fn inject(&self, input: &ScreenInput) {
        if let Err(err) = remote_input::inject(self, input) {
            tracing::debug!("cannot inject {}: {err}", super::input_kind(input));
        }
    }
}

impl RemoteInput for Session {
    type Error = zbus::Error;

    fn extent(&self) -> (u32, u32) {
        self.pixels
    }

    fn pointer_to(&self, x: f64, y: f64) -> zbus::Result<()> {
        self.remote.call("NotifyPointerMotionAbsolute", &(self.stream_path.as_str(), x, y))
    }

    fn button(&self, code: i32, down: bool) -> zbus::Result<()> {
        self.remote.call("NotifyPointerButton", &(code, down))
    }

    fn scroll(&self, vertical: bool, steps: i32) -> zbus::Result<()> {
        self.remote.call("NotifyPointerAxisDiscrete", &(u32::from(!vertical), steps))
    }

    fn keysym(&self, keysym: u32, down: bool) -> zbus::Result<()> {
        self.remote.call("NotifyKeyboardKeysym", &(keysym, down))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(err) = self.remote.call::<_, _, ()>("Stop", &()) {
            tracing::debug!("cannot stop the Mutter remote desktop session: {err}");
        }
    }
}

/// 往一块屏幕上注入输入。不挑指针画不画，跟着采集另开的会话走，旧的会话才关得掉。
pub(crate) struct Injector {
    display: u32,
    session: Arc<Session>,
}

impl Injector {
    pub(crate) fn new(display: u32) -> io::Result<Self> {
        Ok(Self { display, session: Session::get(display, None)? })
    }

    fn session(&mut self) -> &Session {
        if let Some(current) = Session::current(self.display)
            && !Arc::ptr_eq(&current, &self.session)
        {
            self.session = current;
        }
        &self.session
    }
}

impl super::Inject for Injector {
    fn inject(&mut self, input: &ScreenInput) {
        self.session().inject(input);
    }

    fn release(&mut self, button: MouseButton) {
        let _ = self.session().button(remote_input::button_code(button), false);
    }
}

/// 一块屏幕的画面：会话加上收它的流。会话要活得和流一样久。
pub(crate) struct Source {
    stream: pipewire::Stream,
    session: Arc<Session>,
}

impl Source {
    pub(crate) fn open(display: u32, cursor: bool) -> io::Result<Self> {
        let session = Session::get(display, Some(cursor))?;
        Ok(Self { stream: session.stream()?, session })
    }
}

impl super::capture::Source for Source {
    fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        // 节点没了时流不一定报错，看会话关没关。
        if self.session.closed() {
            return Err(io::Error::other("Mutter closed the remote desktop session"));
        }
        self.stream.wait(timeout)
    }

    fn picture(&self) -> Option<super::capture::Picture<'_>> {
        self.stream.picture()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<T: Into<Value<'static>>>(value: T) -> OwnedValue {
        OwnedValue::try_from(value.into()).unwrap()
    }

    #[test]
    fn arranges_monitors_by_the_logical_layout() {
        let spec =
            |connector: &str| (connector.to_owned(), "vendor".to_owned(), "product".to_owned(), "serial".to_owned());
        let current = HashMap::from([("is-current".to_owned(), value(true))]);
        let physical = vec![
            (
                spec("eDP-1"),
                vec![("2880x1800".to_owned(), 2880, 1800, 60.0, 2.0, vec![1.0, 2.0], current.clone())],
                HashMap::new(),
            ),
            (spec("HDMI-1"), vec![("1920x1080".to_owned(), 1920, 1080, 60.0, 1.0, vec![1.0], current)], HashMap::new()),
        ];
        let logical = vec![
            (0, 0, 1.0, 0, false, vec![spec("HDMI-1")], HashMap::new()),
            (1920, 0, 2.0, 0, true, vec![spec("eDP-1")], HashMap::new()),
        ];
        let properties = HashMap::from([("layout-mode".to_owned(), value(1u32))]);
        let monitors = arrange(&physical, &logical, &properties);
        assert_eq!(monitors[0].connector, "eDP-1", "the primary monitor comes first");
        assert_eq!(monitors[0].logical, (1440, 900));
        assert_eq!(monitors[0].pixels, (2880, 1800));
        assert_eq!(monitors[1].logical, (1920, 1080));
        assert_ne!(monitors[0].id(), monitors[1].id());
    }
}
