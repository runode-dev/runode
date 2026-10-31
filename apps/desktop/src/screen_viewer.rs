//! 远程桌面页：看一台 SSH 主机的屏幕，用本机的键鼠操作它。经 `host_client::remote::screen` 连上那边的
//! 采集进程，说的和手机一样（`runode_protocol::screen`）。
//!
//! 画面在读连接的线程上解好（`video`），界面只留最新的一帧，来一帧只通知这一页。要多大的画面按这一页
//! 实际占的物理像素算，窗口缩放停下来以后重新要（`RESIZE_DELAY`）。页面拿着焦点时本机按的键（快捷键
//! 也算）都交给远端，换到别处（点侧栏、工具条）才回到本机。

mod input;
mod video;

use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use futures::{StreamExt as _, channel::mpsc::unbounded};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollEvent, Subscription, Task, WeakEntity, Window, canvas,
    div, prelude::*, px,
};
use gpui::{Role, SharedString};
use runode_protocol::screen::{ScreenClientMsg, ScreenDisplay, ScreenHostMsg, ScreenInput};
use runode_ssh::bootstrap::Family;
use runode_types::input::MouseButton as RemoteButton;

use crate::{
    assets::{CLOSE_ICON, MONITOR_ICON, REFRESH_ICON},
    host_client::remote::{
        self, Status,
        screen::{self, ScreenEvent, ScreenLink, ScreenSender},
    },
    remote_hosts::status_text,
    ui::{
        a11y::Press as _,
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
};
pub use video::SUPPORTED;
use video::{Picture, VideoDecoder};

/// 要的帧率。
const FPS: u32 = 60;
/// 画面长边最多要这么多像素：再大解码和传输都吃力，看着也分不出。
const MAX_SIDE: u32 = 2560;
/// 每个像素每帧给多少 bit 的码率，限在 `MIN_BITRATE` 和 `MAX_BITRATE` 之间。
const BITS_PER_PIXEL: f64 = 0.08;
const MIN_BITRATE: u32 = 2_000_000;
const MAX_BITRATE: u32 = 20_000_000;
/// 页面尺寸停下来这么久以后再按新尺寸要画面，拖着窗口边时不一直重开采集。
const RESIZE_DELAY: Duration = Duration::from_millis(300);
/// 解不了画面时最多这么久要一次关键帧。
const KEYFRAME_INTERVAL: Duration = Duration::from_millis(500);
/// 等主机连上最多等这么久。
const HOST_TIMEOUT: Duration = Duration::from_secs(120);
/// 等主机连上时隔多久看一次它的状态。
const HOST_POLL: Duration = Duration::from_millis(200);
/// 连接意外断开后第几次重连前等多久；用完了按 `RECONNECT_LAST`，一直试到连上或者页面关掉。
const RECONNECT_DELAYS: [Duration; 4] =
    [Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5), Duration::from_secs(10)];
const RECONNECT_LAST: Duration = Duration::from_secs(30);

/// 这一页要窗口办的事。
pub enum ScreenViewerEvent {
    Close,
}

/// 连接走到了哪一步。
enum Phase {
    /// 等主机连上、起远端的桥接。
    Connecting,
    /// 远端回了 `Ready`，等画面。
    Opening,
    /// 有画面了。
    Live,
    /// 连接意外断开或者远端屏幕上没人登录，等着第几次重连。
    Reconnecting(u32),
    /// 远端停了或者开不起来，带给人看的原因；不自己重连，等用户点「重新连接」。
    Stopped(String),
}

/// 画面区域在窗口里的位置，和窗口的缩放比例（一点几个物理像素）。
type Placement = (Bounds<Pixels>, f32);

/// 读线程交给界面的。
enum UiEvent {
    Message(ScreenHostMsg),
    /// `latest` 里有新的一帧。
    Picture,
    /// 这台机器上解不了画面，带给人看的原因。
    Undecodable(String),
    Closed,
}

/// 远端回的 `Ready`。
struct Remote {
    displays: Vec<ScreenDisplay>,
    screen_capture: bool,
    post_event: bool,
}

pub struct ScreenViewer {
    host: String,
    focus: FocusHandle,
    phase: Phase,
    link: Option<ScreenLink>,
    remote: Option<Remote>,
    /// 要采的屏幕；不选时是远端报的第一块（主屏）。
    display: Option<u32>,
    /// 上回 `Start` 要的屏幕和尺寸。
    requested: Option<(u32, u32, u32)>,
    picture: Option<Picture>,
    locked: bool,
    unlock_failed: bool,
    /// 远端说过屏幕上没人登录，到再出画面为止一直重连。
    signed_out: bool,
    password: Entity<InputState>,
    /// 上一帧画面区域的位置，由画的时候记下。
    bounds: Rc<Cell<Option<Placement>>>,
    /// 按下着、还没松开的键，松开在页面外面时也照样告诉远端。
    pressed: Vec<RemoteButton>,
    scroll: Point<f32>,
    resize: Option<Task<()>>,
    connection: Option<Task<()>>,
    /// 上回有画面以后连着断了几次。
    retries: u32,
    /// 等着重连的计时。
    retry: Option<Task<()>>,
    _intercept: Subscription,
    _password: Subscription,
}

impl EventEmitter<ScreenViewerEvent> for ScreenViewer {}

impl Focusable for ScreenViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ScreenViewer {
    pub fn new(host: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let intercept = {
            let (handle, focus, view) = (window.window_handle(), focus.clone(), cx.entity().downgrade());
            cx.intercept_keystrokes(move |event, window, cx: &mut App| {
                if window.window_handle() != handle || !focus.is_focused(window) {
                    return;
                }
                let Some(view) = view.upgrade() else { return };
                if !view.read(cx).controllable() {
                    return;
                }
                if let Some(input) = input::key(&event.keystroke) {
                    cx.stop_propagation();
                    view.read(cx).send_input(input);
                }
            })
        };
        let password = cx.new(|cx| {
            InputState::new(String::new(), cx)
                .with_masked()
                .with_label(rust_i18n::t!("screen_viewer.password").into_owned())
                .with_placeholder(rust_i18n::t!("screen_viewer.password").into_owned())
        });
        let password_events = cx.subscribe_in(&password, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Next) {
                this.unlock(window, cx);
            }
        });
        let mut viewer = Self {
            host,
            focus,
            phase: Phase::Connecting,
            link: None,
            remote: None,
            display: None,
            requested: None,
            picture: None,
            locked: false,
            unlock_failed: false,
            signed_out: false,
            password,
            bounds: Rc::default(),
            pressed: Vec::new(),
            scroll: Point::default(),
            resize: None,
            connection: None,
            retries: 0,
            retry: None,
            _intercept: intercept,
            _password: password_events,
        };
        viewer.connect(window, cx);
        viewer
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// 有画面、远端能注入输入时才把本机的键鼠交过去。
    fn controllable(&self) -> bool {
        matches!(self.phase, Phase::Live)
            && !self.locked
            && self.remote.as_ref().is_some_and(|remote| remote.post_event)
    }

    fn sender(&self) -> Option<ScreenSender> {
        self.link.as_ref().map(ScreenLink::sender)
    }

    fn send(&self, message: ScreenClientMsg) {
        if let Some(sender) = self.sender() {
            sender.send(message);
        }
    }

    fn send_input(&self, input: ScreenInput) {
        self.send(ScreenClientMsg::Input { input });
    }

    /// （重新）连上远端，丢掉旧的连接和画面。
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.link = None;
        self.remote = None;
        self.requested = None;
        self.locked = false;
        self.unlock_failed = false;
        self.pressed.clear();
        self.drop_picture(window);
        if !SUPPORTED {
            self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.unsupported").into_owned());
            return;
        }
        self.phase = Phase::Connecting;
        let (tx, mut rx) = unbounded::<UiEvent>();
        let latest = Arc::new(Mutex::new(None::<Picture>));
        let reader_latest = latest.clone();
        let name = self.host.clone();
        let executor = cx.background_executor().clone();
        self.connection = Some(cx.spawn_in(window, async move |this, cx| {
            let opened = async {
                let host = wait_for_host(&name, &executor).await?;
                let on_event = reader(tx, reader_latest);
                executor.spawn(async move { screen::open(&host, on_event) }).await
            };
            match opened.await {
                Ok(link) => {
                    let alive = this.update(cx, |this, cx| {
                        this.link = Some(link);
                        cx.notify();
                    });
                    if alive.is_err() {
                        return;
                    }
                }
                // 自己重连时连不上（主机还没连回来、远端还没起来）接着重连。
                Err(err) => {
                    this.update_in(cx, |this, window, cx| {
                        if this.retries > 0 {
                            tracing::info!("cannot reopen the remote desktop of {}: {err}", this.host);
                            this.reconnect_later(window, cx);
                        } else {
                            this.phase = Phase::Stopped(err);
                        }
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
            while let Some(event) = rx.next().await {
                let alive = this.update_in(cx, |this, window, cx| this.handle(event, &latest, window, cx));
                if alive.is_err() {
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn handle(&mut self, event: UiEvent, latest: &Mutex<Option<Picture>>, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            UiEvent::Picture => {
                let Some(picture) = latest.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
                if matches!(self.phase, Phase::Stopped(_)) {
                    return;
                }
                self.phase = Phase::Live;
                self.retries = 0;
                self.signed_out = false;
                self.drop_picture(window);
                self.picture = Some(picture);
            }
            UiEvent::Message(ScreenHostMsg::Ready { displays, screen_capture, post_event }) => {
                self.remote = Some(Remote { displays, screen_capture, post_event });
                self.phase = Phase::Opening;
                self.request_size(cx);
            }
            UiEvent::Message(ScreenHostMsg::Incompatible { .. }) => {
                self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.incompatible").into_owned());
            }
            UiEvent::Message(ScreenHostMsg::Started { display, .. }) => self.display = Some(display),
            UiEvent::Message(ScreenHostMsg::Stopped { signed_out: true, .. }) => {
                self.signed_out = true;
                self.drop_picture(window);
            }
            UiEvent::Message(ScreenHostMsg::Stopped { reason, .. }) => {
                // 缺屏幕录制授权时系统报的原因看不懂，换成去哪里打开的指引。
                let unauthorized = self.remote.as_ref().is_some_and(|remote| !remote.screen_capture);
                self.phase = Phase::Stopped(if unauthorized {
                    rust_i18n::t!("screen_viewer.no_screen_capture").into_owned()
                } else {
                    reason
                });
                self.drop_picture(window);
            }
            UiEvent::Message(ScreenHostMsg::Lock { locked }) => {
                self.locked = locked;
                self.unlock_failed = false;
            }
            UiEvent::Message(ScreenHostMsg::UnlockFailed { reason }) => {
                tracing::info!("cannot unlock the remote screen: {reason}");
                self.unlock_failed = true;
            }
            UiEvent::Undecodable(reason) => {
                self.link = None;
                self.phase = Phase::Stopped(reason);
                self.drop_picture(window);
            }
            // 远端说了为什么停的，等用户来办；屏幕上没人登录的等有人登录，一直重连。别的断开（ssh 断了、
            // 远端进程没了）在出过画面以后自己重连；还没出过画面就断了的，多半重连也一样，停下等用户点
            // 「重新连接」。
            UiEvent::Closed => {
                self.link = None;
                self.drop_picture(window);
                match self.phase {
                    Phase::Stopped(_) => {}
                    Phase::Live => self.reconnect_later(window, cx),
                    _ if self.retries > 0 || self.signed_out => self.reconnect_later(window, cx),
                    _ => self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.closed").into_owned()),
                }
            }
        }
        cx.notify();
    }

    /// 过一会儿重连，等得一次比一次久（`RECONNECT_DELAYS`）。
    fn reconnect_later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retries += 1;
        let attempt = self.retries;
        self.phase = Phase::Reconnecting(attempt);
        let delay = RECONNECT_DELAYS.get(attempt as usize - 1).copied().unwrap_or(RECONNECT_LAST);
        tracing::info!("reconnecting to the remote desktop of {} in {delay:?} (attempt {attempt})", self.host);
        self.retry = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update_in(cx, |this, window, cx| this.connect(window, cx)).ok();
        }));
    }

    /// 用户点了「重新连接」：不等计时，从头连。
    fn reconnect_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retry = None;
        self.retries = 0;
        self.connect(window, cx);
    }

    /// 丢掉显示着的那帧。Windows 上它占着窗口的图片缓存，要从窗口里拿掉。
    fn drop_picture(&mut self, window: &mut Window) {
        let picture = self.picture.take();
        #[cfg(windows)]
        if let Some(picture) = picture {
            let _ = window.drop_image(picture.image);
        }
        #[cfg(not(windows))]
        let _ = (picture, window);
    }

    /// 页面占的地方变了：按新的尺寸要画面，第一次立即要，之后等尺寸停下来再要。
    fn bounds_changed(&mut self, cx: &mut Context<Self>) {
        if self.requested.is_none() {
            self.request_size(cx);
        } else if self.wanted().is_some_and(|wanted| Some(wanted) != self.requested) {
            self.resize = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESIZE_DELAY).await;
                this.update(cx, |this, cx| {
                    this.resize = None;
                    this.request_size(cx);
                })
                .ok();
            }));
        }
    }

    /// 要采的屏幕和画面尺寸；还不知道远端的屏幕或页面多大时为空。
    fn wanted(&self) -> Option<(u32, u32, u32)> {
        let remote = self.remote.as_ref()?;
        let display = self
            .display
            .and_then(|id| remote.displays.iter().find(|display| display.id == id))
            .or_else(|| remote.displays.first())?;
        let (bounds, scale) = self.bounds.get()?;
        let available = (f32::from(bounds.size.width) * scale, f32::from(bounds.size.height) * scale);
        let (width, height) = input::capture_size(available, (display.pixel_width, display.pixel_height), MAX_SIDE);
        Some((display.id, width, height))
    }

    /// 按现在的页面尺寸向远端要画面；和上回要的一样时不再要。远端没有屏幕录制授权时也要：采集开不起来，但
    /// 那台 Mac 由此弹框、把这个程序列进系统设置，用户才有地方打开。
    fn request_size(&mut self, cx: &mut Context<Self>) {
        let Some(wanted) = self.wanted() else { return };
        if Some(wanted) == self.requested || matches!(self.phase, Phase::Stopped(_) | Phase::Connecting) {
            return;
        }
        let (display, width, height) = wanted;
        let bitrate =
            ((width as f64 * height as f64 * f64::from(FPS) * BITS_PER_PIXEL) as u32).clamp(MIN_BITRATE, MAX_BITRATE);
        // 本机的指针就落在远端的那个位置上，画面里不再画远端那个慢半拍的指针。
        self.send(ScreenClientMsg::Start {
            display: Some(display),
            width,
            height,
            fps: FPS,
            bitrate,
            hide_cursor: true,
        });
        self.requested = Some(wanted);
        cx.notify();
    }

    fn select_display(&mut self, id: u32, cx: &mut Context<Self>) {
        self.display = Some(id);
        self.request_size(cx);
    }

    fn unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = self.password.read(cx).query().to_owned();
        if password.is_empty() {
            return;
        }
        self.password.update(cx, |field, cx| field.set_query(String::new(), cx));
        self.unlock_failed = false;
        self.send(ScreenClientMsg::Unlock { password });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// 窗口里的位置落在远端屏幕上的比例。
    fn ratio(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
        let (bounds, _) = self.bounds.get()?;
        let (width, height) = match (&self.picture, self.requested) {
            (Some(picture), _) => (picture.width, picture.height),
            (None, Some((_, width, height))) => (width, height),
            (None, None) => return None,
        };
        Some(input::ratio(input::fitted(bounds, width, height), position))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let (Some(button), Some((x, y))) = (remote_button(event.button), self.ratio(event.position)) else { return };
        if !self.controllable() {
            return;
        }
        cx.stop_propagation();
        self.pressed.push(button);
        let mods = input::mods(&event.modifiers);
        self.send_input(ScreenInput::MouseButton { button, down: true, x, y, mods });
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(button) = remote_button(event.button) else { return };
        let Some(ix) = self.pressed.iter().position(|pressed| *pressed == button) else { return };
        self.pressed.remove(ix);
        let Some((x, y)) = self.ratio(event.position) else { return };
        cx.stop_propagation();
        let mods = input::mods(&event.modifiers);
        self.send_input(ScreenInput::MouseButton { button, down: false, x, y, mods });
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if !self.controllable() {
            return;
        }
        if let Some((x, y)) = self.ratio(event.position) {
            self.send_input(ScreenInput::MouseMove { x, y });
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.controllable() {
            return;
        }
        cx.stop_propagation();
        if let Some(input) = input::scroll(event.delta, &mut self.scroll) {
            // 远端在指针所在的地方滚：先把指针挪到这里，不然没动过鼠标时滚的是别处。
            if let Some((x, y)) = self.ratio(event.position) {
                self.send_input(ScreenInput::MouseMove { x, y });
            }
            self.send_input(input);
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let (border, fg) = (cx.theme().border, cx.theme().foreground);
        let title = rust_i18n::t!("screen_viewer.title", host = self.host.as_str()).into_owned();
        let displays: Vec<AnyElement> = self
            .remote
            .as_ref()
            .filter(|remote| remote.displays.len() > 1)
            .map(|remote| {
                let current = self.display.or_else(|| remote.displays.first().map(|display| display.id));
                remote
                    .displays
                    .iter()
                    .enumerate()
                    .map(|(ix, display)| {
                        let id = display.id;
                        let label = rust_i18n::t!("screen_viewer.display", number = ix + 1).into_owned();
                        Button::new(("screen-display", id as usize), label)
                            .with_size(ButtonSize::Sm)
                            .toggled(Some(current == Some(id)))
                            .on_press(cx, move |this, _, cx| this.select_display(id, cx))
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();
        // 断了、等着重连，或者远端缺授权、等用户去那台电脑上打开以后重连。
        let stuck = matches!(self.phase, Phase::Stopped(_) | Phase::Reconnecting(_))
            || self.remote.as_ref().is_some_and(|remote| !remote.screen_capture || !remote.post_event);
        let reconnect = stuck.then(|| {
            let label = rust_i18n::t!("screen_viewer.reconnect").into_owned();
            Button::new("screen-reconnect", label.clone())
                .icon(REFRESH_ICON)
                .with_size(ButtonSize::IconSm)
                .tooltip(tooltip(label, None))
                .on_press(cx, |this, window, cx| this.reconnect_now(window, cx))
        });
        let close = rust_i18n::t!("screen_viewer.close").into_owned();
        div()
            .flex_none()
            .h(scaled(32.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .border_b_1()
            .border_color(border)
            .text_color(fg)
            .text_size(FontSize::Body)
            .child(svg_icon(MONITOR_ICON, fg.opacity(0.6)))
            .child(div().min_w_0().truncate().child(title))
            .children(displays)
            .child(div().flex_1())
            .children(reconnect)
            .child(
                Button::new("screen-close", close.clone())
                    .icon(CLOSE_ICON)
                    .with_size(ButtonSize::IconSm)
                    .tooltip(tooltip(close, None))
                    .on_press(cx, |_, _, cx| cx.emit(ScreenViewerEvent::Close)),
            )
            .into_any_element()
    }

    /// 盖在画面上的提示：连接中、开不起来、缺授权；没有要提示的为空。
    fn notice(&self) -> Option<String> {
        let notice = match &self.phase {
            Phase::Connecting => rust_i18n::t!("screen_viewer.connecting", host = self.host.as_str()),
            Phase::Stopped(reason) => return Some(reason.clone()),
            Phase::Reconnecting(_) if self.signed_out => rust_i18n::t!("screen_viewer.signed_out"),
            Phase::Reconnecting(attempt) => rust_i18n::t!("screen_viewer.reconnecting", attempt = attempt),
            Phase::Opening if self.remote.as_ref().is_some_and(|remote| !remote.screen_capture) => {
                rust_i18n::t!("screen_viewer.no_screen_capture")
            }
            // 锁着屏的 Windows 不出画面，由锁屏的那块说明。
            Phase::Opening if self.locked => return None,
            Phase::Opening => rust_i18n::t!("screen_viewer.opening"),
            Phase::Live => return None,
        };
        Some(notice.into_owned())
    }

    /// 锁屏时盖在画面中间的一块：能代敲密码的远端给密码框和「解锁」；Windows 的锁屏在安全桌面上，注入不了按键，
    /// 只说要在那台电脑前解锁。
    fn render_lock(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let unlock = rust_i18n::t!("screen_viewer.unlock").into_owned();
        let typed = !remote::find(&self.host).is_some_and(|host| host.family() == Family::Windows);
        let label = if !typed {
            rust_i18n::t!("screen_viewer.locked_elsewhere")
        } else if self.unlock_failed {
            rust_i18n::t!("screen_viewer.unlock_failed")
        } else {
            rust_i18n::t!("screen_viewer.locked")
        };
        let form = typed.then(|| {
            div().flex().flex_col().gap(px(8.)).child(Input::new(&self.password)).child(
                Button::new("screen-unlock", unlock)
                    .variant(ButtonVariant::Default)
                    .on_press(cx, |this, window, cx| this.unlock(window, cx)),
            )
        });
        let panel = div()
            .id("screen-lock")
            .role(Role::Group)
            .aria_label(label.clone().into_owned())
            .w(scaled(260.))
            .p(px(12.))
            .rounded(px(8.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .id("screen-lock-label")
                    .role(Role::Label)
                    .text_size(FontSize::Small)
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .children(form);
        div().absolute().inset_0().flex().items_center().justify_center().p(px(24.)).child(panel).into_any_element()
    }
}

impl Render for ScreenViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let label = rust_i18n::t!("screen_viewer.screen", host = self.host.as_str()).into_owned();
        // 画面是自己画的，另报一个图像节点给辅助工具，名字说是哪台的屏幕。
        let picture = self.picture.as_ref().map(|picture| {
            div()
                .id("remote-screen-picture")
                .role(Role::Image)
                .aria_label(SharedString::from(label))
                .absolute()
                .inset_0()
                .child(picture.render())
        });
        // 提示可能很长，限宽、折行、居中，不在一行里两头截断。
        let notice = self.notice().map(|notice| {
            div().absolute().inset_0().flex().items_center().justify_center().p(px(24.)).child(
                div()
                    .id("screen-notice")
                    .role(Role::Label)
                    .aria_label(notice.clone())
                    .max_w(scaled(480.))
                    .text_center()
                    .text_size(FontSize::Body)
                    .text_color(theme.muted_foreground)
                    .child(notice),
            )
        });
        let readonly = matches!(self.phase, Phase::Live)
            && self.remote.as_ref().is_some_and(|remote| !remote.post_event)
            && !self.locked;
        let banner = readonly.then(|| {
            div()
                .id("screen-readonly")
                .role(Role::Label)
                .aria_label(rust_i18n::t!("screen_viewer.no_post_event").into_owned())
                .absolute()
                .top(px(8.))
                .left(px(8.))
                .right(px(8.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(6.))
                .bg(theme.background)
                .border_1()
                .border_color(theme.border)
                .text_size(FontSize::Small)
                .text_color(theme.foreground)
                .child(rust_i18n::t!("screen_viewer.no_post_event"))
        });
        let lock = (self.locked && matches!(self.phase, Phase::Live | Phase::Opening)).then(|| self.render_lock(cx));
        // 记下画面区域，按它换算鼠标的位置、算要多大的画面；变了时等这一帧画完再处理。
        let tracker = {
            let (cell, view) = (self.bounds.clone(), cx.entity().downgrade());
            canvas(
                move |bounds, window, cx| {
                    let now = Some((bounds, window.scale_factor()));
                    if cell.replace(now) != now {
                        let view: WeakEntity<Self> = view;
                        cx.defer(move |cx| {
                            view.update(cx, |view, cx| view.bounds_changed(cx)).ok();
                        });
                    }
                },
                |_, (), _, _| {},
            )
            .absolute()
            .size_full()
        };
        div()
            .id("screen-viewer")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .id("remote-screen")
                    .track_focus(&self.focus)
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .bg(gpui::black())
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(|this, event, _, cx| this.mouse_up(event, cx)))
                    .on_mouse_up(MouseButton::Right, cx.listener(|this, event, _, cx| this.mouse_up(event, cx)))
                    .on_mouse_up(MouseButton::Middle, cx.listener(|this, event, _, cx| this.mouse_up(event, cx)))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|this, event, _, cx| this.mouse_up(event, cx)))
                    .on_mouse_up_out(MouseButton::Right, cx.listener(|this, event, _, cx| this.mouse_up(event, cx)))
                    .on_mouse_up_out(MouseButton::Middle, cx.listener(|this, event, _, cx| this.mouse_up(event, cx)))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_scroll(cx.listener(Self::scroll_wheel))
                    .child(tracker)
                    .children(picture)
                    .children(notice)
                    .children(banner)
                    .children(lock),
            )
    }
}

fn svg_icon(path: &'static str, color: gpui::Hsla) -> impl IntoElement {
    gpui::svg().flex_none().path(path).size(px(14.)).text_color(color)
}

fn remote_button(button: MouseButton) -> Option<RemoteButton> {
    match button {
        MouseButton::Left => Some(RemoteButton::Left),
        MouseButton::Right => Some(RemoteButton::Right),
        MouseButton::Middle => Some(RemoteButton::Middle),
        _ => None,
    }
}

/// 读线程上处理远端交来的：画面就地解好，只把最新的一帧留在 `latest` 里，界面取走以前再来的顶掉它；解不了时
/// 向远端要关键帧（最多 `KEYFRAME_INTERVAL` 一次）。解码器在读线程上建：Windows 上它是 COM 对象，要在用它的
/// 线程上建。
fn reader(
    tx: futures::channel::mpsc::UnboundedSender<UiEvent>,
    latest: Arc<Mutex<Option<Picture>>>,
) -> impl FnMut(ScreenEvent, &ScreenSender) + Send + 'static {
    let mut decoder: Option<VideoDecoder> = None;
    let mut asked: Option<Instant> = None;
    move |event, sender| {
        let frame = match event {
            ScreenEvent::Message(message) => {
                let _ = tx.unbounded_send(UiEvent::Message(message));
                return;
            }
            ScreenEvent::Closed => {
                let _ = tx.unbounded_send(UiEvent::Closed);
                return;
            }
            ScreenEvent::Video(frame) => frame,
        };
        if decoder.is_none() {
            match VideoDecoder::new() {
                Ok(created) => decoder = Some(created),
                Err(err) => {
                    tracing::warn!("cannot decode the remote desktop: {err}");
                    let reason = rust_i18n::t!("screen_viewer.no_decoder", error = err.to_string()).into_owned();
                    let _ = tx.unbounded_send(UiEvent::Undecodable(reason));
                    return;
                }
            }
        }
        let Some(decoder) = decoder.as_mut() else { return };
        match decoder.decode(&frame) {
            Ok(Some(picture)) => {
                let empty = latest.lock().unwrap_or_else(PoisonError::into_inner).replace(picture).is_none();
                if empty {
                    let _ = tx.unbounded_send(UiEvent::Picture);
                }
                return;
            }
            Ok(None) if !decoder.needs_keyframe() => return,
            Ok(None) => {}
            Err(err) => {
                tracing::debug!("cannot decode a remote desktop frame: {err}");
                decoder.reset();
            }
        }
        if asked.is_none_or(|at| at.elapsed() >= KEYFRAME_INTERVAL) {
            sender.send(ScreenClientMsg::Keyframe);
            asked = Some(Instant::now());
        }
    }
}

/// 等主机连上（没在连时叫它连），连不上时带上给人看的原因。
async fn wait_for_host(name: &str, executor: &gpui::BackgroundExecutor) -> Result<Arc<remote::RemoteHost>, String> {
    let host = remote::remote(name).ok_or_else(|| rust_i18n::t!("screen_viewer.bad_host").into_owned())?;
    if matches!(host.status(), Status::Stopped { .. } | Status::Waiting { .. } | Status::Idle) {
        host.retry_now();
    }
    let deadline = Instant::now() + HOST_TIMEOUT;
    loop {
        let status = host.status();
        match status {
            Status::Connected(_) => return Ok(host),
            Status::Stopped { .. } => return Err(status_text(&status)),
            _ if Instant::now() >= deadline => return Err(status_text(&status)),
            _ => executor.timer(HOST_POLL).await,
        }
    }
}
