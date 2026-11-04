//! Linux 上的远程桌面：采集进程先接过屏幕上图形会话的环境（`adopt_session`），再按会话挑后端（`Backend`）：X11 的
//! 用 X 协议采集、XTEST 注入（`x11`）；GNOME 的 Wayland 用 Mutter 自己的远程桌面接口（`mutter`），别的 Wayland
//! 桌面用 xdg-desktop-portal（`portal`）。Wayland 的画面都经 PipeWire 来（`pipewire`）。画面在采集线程上缩放、
//! 转成 NV12（`capture`），交给显卡的 VA-API 编码器，没有时交给 OpenH264（`encoder`）。

mod capture;
mod encoder;
mod keysym;
mod logind;
mod mutter;
mod pipewire;
mod portal;
mod remote_input;
pub(crate) mod session;
mod software;
mod vaapi;
mod x11;

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use runode_types::{input::MouseButton, screen::ScreenInput};

pub use capture::{Capture, Keyframes};

use crate::Display;

/// 熄屏前多久重置一次 X 的屏保计时。
const SCREENSAVER_RESET: Duration = Duration::from_secs(30);
/// 滚轮转一格大致滚过的点数：一格滚三行。
const POINTS_PER_NOTCH: i32 = 48;

/// 把屏幕上本用户图形会话的环境变量（`DISPLAY`、`WAYLAND_DISPLAY`、会话总线等）设进本进程，之后的采集和注入
/// 连的都是它。返回有没有这样的会话。
///
/// # Safety
///
/// 本进程还没有别的线程：它改环境变量。
pub unsafe fn adopt_session() -> bool {
    match session::find() {
        Some(found) => {
            session::adopt(&found);
            true
        }
        None => false,
    }
}

/// 下次要用 portal 时可以再在屏幕上请求授权。
pub(crate) fn ask_again() {
    portal::ask_again();
}

/// 采集和注入走哪套接口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    X11,
    Mutter,
    Portal,
}

/// 按接过来的图形会话挑后端；没有图形会话时错误是 `io::ErrorKind::NotConnected`。
fn backend() -> io::Result<Backend> {
    let kind = session::find().map(|session| session.kind).ok_or_else(|| {
        crate::caused_error(
            crate::StopCause::NoDesktop,
            io::ErrorKind::NotConnected,
            "nobody is signed in to a desktop on this screen",
        )
    })?;
    match kind {
        session::Kind::X11 => Ok(Backend::X11),
        session::Kind::Wayland if mutter::available() => Ok(Backend::Mutter),
        session::Kind::Wayland => Ok(Backend::Portal),
    }
}

/// 按 `backend` 办 `job`；Mutter 不许这个进程调它的接口时（`mutter::available` 随之为假）改走 portal 再办一次。
fn with_backend<T>(job: impl Fn(Backend) -> io::Result<T>) -> io::Result<T> {
    let backend = backend()?;
    match job(backend) {
        Err(err) if backend == Backend::Mutter && err.kind() == io::ErrorKind::PermissionDenied => {
            tracing::info!("Mutter refused remote desktop, using the portal: {err}");
            job(Backend::Portal)
        }
        result => result,
    }
}

/// 开着的各块屏幕，主屏排第一；列不出时是空的。
pub(crate) fn displays() -> Vec<Display> {
    let listed = with_backend(|backend| match backend {
        Backend::X11 => x11::displays(),
        Backend::Mutter => mutter::displays(),
        Backend::Portal => portal::displays(),
    });
    listed.unwrap_or_else(|err| {
        tracing::warn!("cannot list the displays: {err}");
        Vec::new()
    })
}

fn open_source(display: u32, cursor: bool) -> io::Result<Box<dyn capture::Source>> {
    with_backend(|backend| -> io::Result<Box<dyn capture::Source>> {
        Ok(match backend {
            Backend::X11 => Box::new(x11::Source::open(display, cursor)?),
            Backend::Mutter => Box::new(mutter::Source::open(display, cursor)?),
            Backend::Portal => Box::new(portal::Source::open(display)?),
        })
    })
}

/// 往一块屏幕上注入输入；后端开不起来时什么都不做。记着按住的鼠标键，丢掉时松开还按着的：观看端断开时
/// 按着的键不会一直按着。
pub struct Injector {
    backend: Option<Box<dyn Inject>>,
    /// 横向、纵向还没攒满一格的滚动点数：各后端只能按整格滚，触控板一次只滚几点。
    scroll: [i32; 2],
    pressed: Vec<MouseButton>,
}

/// 一个后端怎么往屏幕上注入输入。
trait Inject {
    fn inject(&mut self, input: &ScreenInput);
    /// 松开鼠标键 `button`。
    fn release(&mut self, button: MouseButton);
}

impl Injector {
    /// `display` 是 `ScreenInput` 的坐标所在的屏幕，和采集的是同一块。
    pub fn new(display: u32) -> Self {
        let backend = with_backend(|backend| -> io::Result<Box<dyn Inject>> {
            Ok(match backend {
                Backend::X11 => Box::new(x11::Injector::new(display)?),
                Backend::Mutter => Box::new(mutter::Injector::new(display)?),
                Backend::Portal => Box::new(portal::Injector::new(display)?),
            })
        });
        let backend = backend.inspect_err(|err| tracing::warn!("cannot inject input: {err}")).ok();
        Self { backend, scroll: [0; 2], pressed: Vec::new() }
    }

    pub fn inject(&mut self, input: &ScreenInput) {
        let scrolled;
        let input = match input {
            ScreenInput::Scroll { dx, dy } => {
                let [dx, dy] = [(0, *dx), (1, *dy)].map(|(axis, points)| whole_notches(&mut self.scroll[axis], points));
                if dx == 0 && dy == 0 {
                    return;
                }
                scrolled = ScreenInput::Scroll { dx, dy };
                &scrolled
            }
            input => input,
        };
        if let ScreenInput::MouseButton { button, down, .. } = input {
            self.pressed.retain(|held| held != button);
            if *down {
                self.pressed.push(*button);
            }
        }
        if let Some(backend) = &mut self.backend {
            backend.inject(input);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        if let Some(backend) = &mut self.backend {
            for button in std::mem::take(&mut self.pressed) {
                backend.release(button);
            }
        }
    }
}

/// 日志里写的输入种类：输入的文字和按键可能是密码，不进日志。
fn input_kind(input: &ScreenInput) -> &'static str {
    match input {
        ScreenInput::MouseMove { .. } => "a pointer move",
        ScreenInput::MouseButton { .. } => "a mouse button",
        ScreenInput::Scroll { .. } => "a scroll",
        ScreenInput::Key { .. } => "a key",
        ScreenInput::Text { .. } => "text",
    }
}

/// 攒进 `rest` 的滚动点数里满整格的那部分，符号和点数一样；不满一格的留在 `rest`，换了方向时丢掉。
fn whole_notches(rest: &mut i32, points: i32) -> i32 {
    if points.signum() == -rest.signum() {
        *rest = 0;
    }
    *rest += points;
    let whole = *rest / POINTS_PER_NOTCH * POINTS_PER_NOTCH;
    *rest -= whole;
    whole
}

/// 连着时电脑不睡、屏幕不熄，丢掉时放开。
pub struct KeepAwake {
    _inhibitor: logind::Inhibitor,
    stop: Arc<AtomicBool>,
    screensaver: Option<JoinHandle<()>>,
}

impl KeepAwake {
    pub fn acquire() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // X 的屏保和熄屏不全听 D-Bus 的抑制，隔一会儿重置一次它的计时。
        let screensaver = matches!(backend(), Ok(Backend::X11)).then(|| {
            let stop = stop.clone();
            thread::Builder::new()
                .name("screen-awake".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Err(err) = x11::reset_screensaver() {
                            tracing::debug!("cannot reset the X screen saver: {err}");
                        }
                        thread::park_timeout(SCREENSAVER_RESET);
                    }
                })
                .ok()
        });
        Self { _inhibitor: logind::Inhibitor::acquire(), stop, screensaver: screensaver.flatten() }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.screensaver.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

pub(crate) fn screen_locked() -> bool {
    logind::locked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_adds_up_to_whole_notches() {
        let mut rest = 0;
        assert_eq!(whole_notches(&mut rest, 20), 0);
        assert_eq!(whole_notches(&mut rest, 20), 0);
        assert_eq!(whole_notches(&mut rest, 20), POINTS_PER_NOTCH);
        assert_eq!(rest, 12);
        assert_eq!(whole_notches(&mut rest, -10), 0);
        assert_eq!(rest, -10);
        assert_eq!(whole_notches(&mut rest, -100), -2 * POINTS_PER_NOTCH);
        assert_eq!(rest, -14);
        assert_eq!(whole_notches(&mut rest, 0), 0);
        assert_eq!(rest, -14);
    }
}
