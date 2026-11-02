//! 远程桌面页：看一台 SSH 主机的屏幕，用本机的键鼠操作它。经 `host_client::remote::screen` 连上那边的
//! 采集进程，说的和手机一样（`runode_protocol::screen`）。
//!
//! 画面在读连接的线程上解好（`video`），界面只留最新的一帧，来一帧只通知这一页。要多大的画面、多高的码率由
//! 工具条上的清晰度（`Quality`）定：画面按这一页实际占的物理像素算，窗口缩放停下来以后重新要
//! （`RESIZE_DELAY`），各档再限短边和码率；自动档每 `ADAPT_INTERVAL` 按网络调一次（`quality`）。远端只在编码前
//! 把画面缩到这么大，它屏幕自己的分辨率不变；要远端换分辨率另用分辨率菜单（`SetMode`），连接断开时远端换回原来
//! 的，自己重连上以后这里再换一次。页面拿着焦点时本机按的键（快捷键也算）都交给远端，换到别处
//! （点侧栏、工具条）才回到本机。

mod input;
mod quality;
mod saved_password;
mod video;

use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use futures::{StreamExt as _, channel::mpsc::unbounded};
use gpui::{
    Action, AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollEvent, Subscription, Task, WeakEntity,
    Window, canvas, div, prelude::*, px,
};
use gpui::{Role, SharedString, WindowControlArea};
use runode_protocol::screen::{ScreenClientMsg, ScreenDisplay, ScreenHostMsg, ScreenInput, ScreenMode};
use runode_ssh::bootstrap::Family;
use runode_types::{input::MouseButton as RemoteButton, screen::Codec};

use crate::{
    host_client::remote::{
        self, Status,
        screen::{self, ScreenEvent, ScreenLink, ScreenSender},
    },
    ssh_hosts::status_text,
    ui::icons::{
        CLOSE_ICON, MAXIMIZE_ICON, MINIMIZE_ICON, MONITOR_ICON, PIN_ICON, POP_IN_ICON, POP_OUT_ICON, REFRESH_ICON,
    },
    ui::{
        a11y::{A11yPress as _, Press as _},
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        icons::CHEVRON_DOWN_ICON,
        input::{Input as TextInput, InputEvent, InputState},
        menu::{Menu, MenuHost, MenuItem},
        switch::Switch,
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
};
use quality::{Adapter, Level, Needs, Network};
pub use video::SUPPORTED;
use video::{Picture, VideoDecoder};

/// 要的帧率。
const FPS: u32 = 60;
/// 原画以外的档画面长边最多要这么多像素：再大解码和传输都吃力，看着也分不出。
const MAX_SIDE: u32 = 2560;
/// 只解得了 H.264 时原画的长边和像素数上限：多数 H.264 硬件编码器收不下更大的画面，建不起编码会话。
const H264_MAX_SIDE: u32 = 4096;
const H264_MAX_PIXELS: f32 = 4096. * 2160.;
/// 每个像素每帧最多给多少 bit 的码率，再受各档的 `Level::max_bitrate` 限制。
const BITS_PER_PIXEL: f64 = 0.08;
/// 自动档隔这么久按网络调一次。
const ADAPT_INTERVAL: Duration = Duration::from_millis(500);
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
    /// 在浮动和占满内容区之间切换。
    ToggleFloating,
    /// 浮动时在标题上按下了鼠标，从窗口里的这个位置开始拖动这一页。
    StartDrag(Point<Pixels>),
    /// 搬到单独的窗口里。
    PopOut,
    /// 在单独的窗口里时，搬回主窗口。
    PopIn,
    /// 吸附在右侧面板上时，钉住或取消钉住。
    TogglePin,
}

/// 窗口交给这一页的。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Input {
    /// 这一页浮在窗口内容上面，不占满内容区：标题拖得动，工具条上的按钮是「占满窗口」。
    pub floating: bool,
    /// 这一页在单独的窗口里，工具条兼当窗口的标题栏：拖它移动窗口，不给浮动、单独窗口和关闭的按钮，给「放回主窗口」。
    pub titlebar: Option<Titlebar>,
    /// 吸附在右侧面板上时是钉没钉住，工具条上给钉住的按钮；没吸附时为空。
    pub pinned: Option<bool>,
}

/// 工具条当标题栏时的样子。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Titlebar {
    /// 高度，和主窗口的标题栏一样。
    pub height: f32,
    /// 左右两头给窗口按钮（macOS 的红绿灯、Windows 自己画的那几个）让出的宽度。
    pub left: f32,
    pub right: f32,
    /// 窗口全屏着：工具条平时收起来，画面铺满屏幕，鼠标移到上头一段（`REVEAL_ZONE`）时才浮出来（`toolbar_shown`）。
    pub fullscreen: bool,
    /// 全屏时浮出的工具条离窗口顶多远：macOS 上鼠标到了上头，系统会把窗口的标题栏滑下来盖在最上面，工具条摆在它
    /// 下面。
    pub reveal_top: f32,
}

/// 全屏时鼠标移到工具条下沿往下这么近的范围里就浮出工具条。
const REVEAL_ZONE: f32 = 16.;

/// 清晰度。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Quality {
    /// 按网络自动挑档、调码率（`Adapter`）。
    #[default]
    Auto,
    Fixed(Level),
}

/// 清晰度菜单里点了一项。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
struct SetScreenQuality {
    quality: Quality,
}

/// 分辨率菜单里点了一项：把屏幕 `display` 换成 `mode`，为空时换回远端原来的。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
struct SetScreenMode {
    display: u32,
    mode: Option<ScreenMode>,
}

/// 工具条上的下拉菜单，同一时刻只开一个。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dropdown {
    Quality,
    Resolution,
}

/// 换不了分辨率的提示显示这么久。
const MODE_ERROR_TIMEOUT: Duration = Duration::from_secs(6);
/// 用记住的密码自动解锁以后这么久还锁着，就当密码不对，请用户重新输入。
const AUTO_UNLOCK_WAIT: Duration = Duration::from_secs(8);

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
    /// 窗口把这一页藏起来了，连接断开着（`suspend`），再显示时由窗口叫它重连（`resume`）。
    Suspended,
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
    /// 上回要的码率。
    bitrate: Option<u32>,
    quality: Quality,
    /// 读线程记下的网络情况，自动档取来调。
    network: Arc<Mutex<Network>>,
    adapter: Adapter,
    /// 自动档隔一阵调一次的计时，固定档时为空。
    adapting: Option<Task<()>>,
    input: Input,
    /// 等下一次画的时候把焦点给密码框（刚锁屏）或画面（敲完密码）：拿到焦点要 `Window`，这一页不跟哪个窗口
    /// 绑着，能搬到别的窗口里。
    focus_password: bool,
    focus_screen: bool,
    /// 全屏时工具条浮出来了。
    toolbar_shown: bool,
    /// 换下来的画面，等画的时候从窗口的图片缓存里拿掉（Windows）；显示着的那帧画过没有，没画过的不在缓存里，
    /// 直接丢（窗口最小化时不画，不然换下来的越攒越多）。
    #[cfg(windows)]
    retired: Vec<Picture>,
    #[cfg(windows)]
    painted: bool,
    /// 开着的下拉菜单和它是哪一个的。
    menu: Option<Menu>,
    menu_for: Dropdown,
    /// 选过的分辨率，按屏幕记；重连上以后照着再换一次。
    modes: HashMap<u32, ScreenMode>,
    /// 发出去、远端还没回 `Displays` 的换分辨率请求：屏幕和请求之前 `modes` 里它的值，换不了（`ModeFailed`）时
    /// 换回这个值，重连时不再拿换不了的去换。远端按顺序回，每个请求先回可能的 `ModeFailed`、再回 `Displays`。
    mode_requests: VecDeque<(u32, Option<ScreenMode>)>,
    /// 远端说换不了分辨率的原因，显示一阵（`MODE_ERROR_TIMEOUT`）。
    mode_error: Option<String>,
    mode_error_timer: Option<Task<()>>,
    picture: Option<Picture>,
    locked: bool,
    unlock_failed: bool,
    /// 记住登录密码，远端锁屏时自动解锁（`saved_password`）。
    remember: bool,
    /// 手动解锁时要记住的密码和敲它的时刻：之后 `AUTO_UNLOCK_WAIT` 内远端解开了才存。远端不回密码错，过了
    /// 这么久才解开的多半是有人在那台电脑前解的，这个可能是错的，不存。
    pending_save: Option<(String, Instant)>,
    /// 这次锁屏用记住的密码试过了；正在等它解开的计时；试了没解开；用户这次锁屏自己敲过密码。
    auto_tried: bool,
    auto_unlock: Option<Task<()>>,
    auto_failed: bool,
    typed_unlock: bool,
    /// 记住的密码解不开（远端改了密码）：重连、再锁屏都不再拿它试，免得错密码一遍遍敲进锁屏把账户锁住；用户手动
    /// 解开、换上新密码以后才再用。重连时不清。
    saved_rejected: bool,
    /// 用户在这一页关掉了「记住密码」：忘掉的那一下即使没成功也不再拿记着的试。
    remember_off: bool,
    /// 用户拨过「记住密码」的开关，后台查到的结果不再盖掉它。
    remember_touched: bool,
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
    /// 不跟哪个窗口绑着：放在哪个窗口里，焦点在这一页上时那个窗口里按的键就交给远端。
    pub fn new(host: String, cx: &mut Context<Self>) -> Self {
        let mut viewer = Self::create(host, cx);
        viewer.connect(cx);
        viewer
    }

    /// 先不连的一页，等窗口显示它时 `resume`。
    pub fn new_suspended(host: String, cx: &mut Context<Self>) -> Self {
        let mut viewer = Self::create(host, cx);
        viewer.phase = Phase::Suspended;
        viewer
    }

    fn create(host: String, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let intercept = {
            let (focus, view) = (focus.clone(), cx.entity().downgrade());
            cx.intercept_keystrokes(move |event, window, cx: &mut App| {
                if !focus.is_focused(window) {
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
        let password_events = cx.subscribe(&password, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Next) {
                this.unlock(cx);
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
            bitrate: None,
            quality: Quality::default(),
            network: Arc::new(Mutex::new(Network::new(Instant::now()))),
            adapter: Adapter::default(),
            adapting: None,
            input: Input::default(),
            focus_password: false,
            focus_screen: false,
            toolbar_shown: false,
            #[cfg(windows)]
            retired: Vec::new(),
            #[cfg(windows)]
            painted: false,
            menu: None,
            menu_for: Dropdown::Quality,
            modes: HashMap::new(),
            mode_requests: VecDeque::new(),
            mode_error: None,
            mode_error_timer: None,
            picture: None,
            locked: false,
            unlock_failed: false,
            remember: false,
            pending_save: None,
            auto_tried: false,
            auto_unlock: None,
            auto_failed: false,
            typed_unlock: false,
            saved_rejected: false,
            remember_off: false,
            remember_touched: false,
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
        viewer.adapt_later(cx);
        viewer.load_remember(cx);
        viewer
    }

    /// 在后台看记没记着这台的密码，开关照着摆。
    fn load_remember(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let saved = executor.spawn(async move { saved_password::exists(&host) }).await;
            this.update(cx, |this, cx| {
                // 用户在查的时候已经拨过开关，照用户的。
                if !this.remember_touched {
                    this.remember = saved;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 打开或关掉「记住密码」；关掉时马上忘掉记着的。
    fn toggle_remember(&mut self, cx: &mut Context<Self>) {
        self.remember = !self.remember;
        self.remember_off = !self.remember;
        self.remember_touched = true;
        if !self.remember {
            self.pending_save = None;
            let host = self.host.clone();
            cx.background_executor()
                .spawn(async move {
                    if let Err(err) = saved_password::forget(&host) {
                        tracing::warn!("cannot forget the saved password for {host}: {err}");
                    }
                })
                .detach();
        }
        cx.notify();
    }

    /// 刚锁屏：记着这台的密码时拿它解锁，一次锁屏只试一次。
    fn unlock_with_saved(&mut self, cx: &mut Context<Self>) {
        if self.auto_tried || self.saved_rejected || self.remember_off {
            return;
        }
        self.auto_tried = true;
        let host = self.host.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let saved = executor.spawn(async move { saved_password::load(&host) }).await;
            let Some(password) = saved else { return };
            this.update(cx, |this, cx| {
                // 读钥匙串可能等系统弹框，这期间远端解开了、用户自己敲了密码或关掉了「记住」，就不再敲。
                if !this.locked || this.link.is_none() || this.typed_unlock || this.remember_off {
                    return;
                }
                tracing::info!("unlocking the remote desktop of {} with the saved password", this.host);
                this.send(ScreenClientMsg::Unlock { password });
                this.auto_unlock = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(AUTO_UNLOCK_WAIT).await;
                    this.update(cx, |this, cx| {
                        this.auto_unlock = None;
                        if this.locked && !this.typed_unlock {
                            this.auto_failed = true;
                            this.saved_rejected = true;
                            this.focus_password = true;
                        }
                        cx.notify();
                    })
                    .ok();
                }));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// 焦点在这一页上：画面或解锁的密码框。
    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.is_focused(window) || self.password.focus_handle(cx).is_focused(window)
    }

    /// 换上窗口交来的 `Input`，返回变了没有，变了由窗口等这一帧画完再通知这一页重画（它是缓存的视图）。
    pub fn set_input(&mut self, input: Input) -> bool {
        let changed = self.input != input;
        self.input = input;
        if !self.autohides_toolbar() {
            self.toolbar_shown = false;
        }
        changed
    }

    /// 工具条平时收起来：在单独的窗口里、窗口全屏着。
    fn autohides_toolbar(&self) -> bool {
        self.input.titlebar.is_some_and(|bar| bar.fullscreen)
    }

    /// 全屏时浮出或收起工具条；下拉菜单开着时不收。
    fn show_toolbar(&mut self, shown: bool, cx: &mut Context<Self>) {
        let shown = shown || self.dropdown_open();
        if self.toolbar_shown != shown {
            self.toolbar_shown = shown;
            cx.notify();
        }
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
    fn connect(&mut self, cx: &mut Context<Self>) {
        self.link = None;
        self.remote = None;
        self.requested = None;
        self.bitrate = None;
        self.locked = false;
        self.unlock_failed = false;
        self.pending_save = None;
        // 拿记住的密码试了、还没等到解开就断了：当它没解开，重连后不再拿它往锁屏里敲。
        if self.auto_unlock.take().is_some() {
            self.saved_rejected = true;
        }
        self.auto_tried = false;
        self.auto_failed = false;
        self.mode_requests.clear();
        self.typed_unlock = false;
        self.pressed.clear();
        self.drop_picture();
        if !SUPPORTED {
            self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.unsupported").into_owned());
            return;
        }
        self.phase = Phase::Connecting;
        let (tx, mut rx) = unbounded::<UiEvent>();
        let latest = Arc::new(Mutex::new(None::<Picture>));
        let reader_latest = latest.clone();
        let name = self.host.clone();
        let network = self.network.clone();
        let executor = cx.background_executor().clone();
        self.connection = Some(cx.spawn(async move |this, cx| {
            let opened = async {
                let host = wait_for_host(&name, &executor).await?;
                let on_event = reader(tx, reader_latest, network);
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
                    this.update(cx, |this, cx| {
                        if this.retries > 0 {
                            tracing::info!("cannot reopen the remote desktop of {}: {err}", this.host);
                            this.reconnect_later(cx);
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
                let alive = this.update(cx, |this, cx| this.handle(event, &latest, cx));
                if alive.is_err() {
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn handle(&mut self, event: UiEvent, latest: &Mutex<Option<Picture>>, cx: &mut Context<Self>) {
        match event {
            UiEvent::Picture => {
                let Some(picture) = latest.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
                if matches!(self.phase, Phase::Stopped(_)) {
                    return;
                }
                self.phase = Phase::Live;
                self.retries = 0;
                self.signed_out = false;
                self.drop_picture();
                self.picture = Some(picture);
            }
            UiEvent::Message(ScreenHostMsg::Ready { displays, screen_capture, post_event, .. }) => {
                self.remote = Some(Remote { displays, screen_capture, post_event });
                self.phase = Phase::Opening;
                if !self.reapply_modes() {
                    self.request_size(cx);
                }
            }
            UiEvent::Message(ScreenHostMsg::Displays { displays }) => {
                self.mode_requests.pop_front();
                if let Some(remote) = &mut self.remote {
                    remote.displays = displays;
                }
                // 远端停了采集，按新的屏幕尺寸重新要。
                self.requested = None;
                self.bitrate = None;
                self.request_size(cx);
                if self.menu_open(Dropdown::Resolution) {
                    let items = self.resolution_items();
                    self.replace_menu_items(items, cx);
                }
            }
            UiEvent::Message(ScreenHostMsg::ModeFailed { reason }) => {
                tracing::info!("the remote desktop of {} cannot change its resolution: {reason}", self.host);
                match self.mode_requests.front().copied() {
                    Some((display, Some(previous))) => self.modes.insert(display, previous),
                    Some((display, None)) => self.modes.remove(&display),
                    None => None,
                };
                self.mode_error = Some(reason);
                self.mode_error_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(MODE_ERROR_TIMEOUT).await;
                    this.update(cx, |this, cx| {
                        this.mode_error = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
            UiEvent::Message(ScreenHostMsg::Incompatible { .. }) => {
                self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.incompatible").into_owned());
            }
            UiEvent::Message(ScreenHostMsg::Started { display, .. }) => self.display = Some(display),
            UiEvent::Message(ScreenHostMsg::Stopped { signed_out: true, .. }) => {
                self.signed_out = true;
                self.drop_picture();
            }
            UiEvent::Message(ScreenHostMsg::Stopped { no_encoder: true, .. }) => {
                self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.no_encoder").into_owned());
                self.drop_picture();
            }
            UiEvent::Message(ScreenHostMsg::Stopped { reason, .. }) => {
                // 缺屏幕录制授权时系统报的原因看不懂，换成去哪里打开的指引。
                let unauthorized = self.remote.as_ref().is_some_and(|remote| !remote.screen_capture);
                self.phase = Phase::Stopped(if unauthorized {
                    rust_i18n::t!("screen_viewer.no_screen_capture").into_owned()
                } else {
                    reason
                });
                self.drop_picture();
            }
            UiEvent::Message(ScreenHostMsg::Permissions { screen_capture, post_event }) => {
                if let Some(remote) = &mut self.remote {
                    remote.screen_capture = screen_capture;
                    remote.post_event = post_event;
                }
                cx.notify();
            }
            UiEvent::Message(ScreenHostMsg::Lock { locked }) => {
                // 刚锁上屏时密码框拿焦点（焦点在这一页上时），直接就能敲密码；记着密码时先拿它解锁。
                if locked && !self.locked && self.types_password() {
                    self.focus_password = true;
                    self.unlock_with_saved(cx);
                }
                if !locked && self.locked {
                    // 手动敲的密码解开了才记住，敲错的不存。
                    if let Some((password, _)) =
                        self.pending_save.take().filter(|(_, at)| at.elapsed() <= AUTO_UNLOCK_WAIT)
                    {
                        self.saved_rejected = false;
                        let host = self.host.clone();
                        cx.background_executor()
                            .spawn(async move {
                                if let Err(err) = saved_password::save(&host, &password) {
                                    tracing::warn!("cannot save the password for {host}: {err}");
                                }
                            })
                            .detach();
                    }
                    // 没人手动敲过就解开了，是记住的密码解开的，只是远端慢：它没错。
                    if self.auto_tried && !self.typed_unlock {
                        self.saved_rejected = false;
                    }
                    self.auto_tried = false;
                    self.auto_unlock = None;
                    self.auto_failed = false;
                    self.typed_unlock = false;
                }
                self.locked = locked;
                self.unlock_failed = false;
            }
            UiEvent::Message(ScreenHostMsg::UnlockFailed { reason }) => {
                tracing::info!("cannot unlock the remote screen: {reason}");
                self.unlock_failed = true;
                self.pending_save = None;
                // 记住的密码就敲不出来，以后也别再拿它试。
                if self.auto_unlock.take().is_some() {
                    self.saved_rejected = true;
                }
            }
            // 画面走哪条路由连接自己办（`screen::open`），不交到这里。
            UiEvent::Message(ScreenHostMsg::Datagram { .. } | ScreenHostMsg::DatagramLost) => {}
            UiEvent::Undecodable(reason) => {
                self.link = None;
                self.phase = Phase::Stopped(reason);
                self.drop_picture();
            }
            // 远端说了为什么停的，等用户来办；屏幕上没人登录的等有人登录，一直重连。别的断开（ssh 断了、
            // 远端进程没了）在出过画面以后自己重连；还没出过画面就断了的，多半重连也一样，停下等用户点
            // 「重新连接」。
            UiEvent::Closed => {
                self.link = None;
                self.drop_picture();
                match self.phase {
                    Phase::Stopped(_) => {}
                    Phase::Live => self.reconnect_later(cx),
                    _ if self.retries > 0 || self.signed_out => self.reconnect_later(cx),
                    _ => self.phase = Phase::Stopped(rust_i18n::t!("screen_viewer.closed").into_owned()),
                }
            }
        }
        cx.notify();
    }

    /// 过一会儿重连，等得一次比一次久（`RECONNECT_DELAYS`）。
    fn reconnect_later(&mut self, cx: &mut Context<Self>) {
        self.retries += 1;
        let attempt = self.retries;
        self.phase = Phase::Reconnecting(attempt);
        let delay = RECONNECT_DELAYS.get(attempt as usize - 1).copied().unwrap_or(RECONNECT_LAST);
        tracing::info!("reconnecting to the remote desktop of {} in {delay:?} (attempt {attempt})", self.host);
        self.retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| this.connect(cx)).ok();
        }));
    }

    /// 窗口把这一页藏起来了：断开连接，远端不再采集、编码；停着等用户点「重新连接」的不动。
    pub fn suspend(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Stopped(_) | Phase::Suspended) {
            return;
        }
        tracing::info!("suspending the hidden remote desktop of {}", self.host);
        self.connection = None;
        self.retry = None;
        self.retries = 0;
        self.link = None;
        self.drop_picture();
        self.phase = Phase::Suspended;
        cx.notify();
    }

    pub fn suspended(&self) -> bool {
        matches!(self.phase, Phase::Suspended)
    }

    /// 窗口又显示这一页了：断开着的话重连。
    pub fn resume(&mut self, cx: &mut Context<Self>) {
        if self.suspended() {
            self.connect(cx);
        }
    }

    /// 用户点了「重新连接」：不等计时，从头连。
    fn reconnect_now(&mut self, cx: &mut Context<Self>) {
        self.retry = None;
        self.retries = 0;
        self.connect(cx);
    }

    /// 这一页要离开 `window`（搬到别的窗口、关掉）：Windows 上画过的帧占着那个窗口的图片缓存，到别的窗口里画时
    /// 拿不掉，这里先从它里面拿掉。显示着的那帧留着，到了新窗口再画。
    pub fn leave_window(&mut self, window: &mut Window) {
        #[cfg(windows)]
        for image in self
            .retired
            .drain(..)
            .map(|picture| picture.image)
            .chain(self.picture.as_ref().map(|picture| picture.image.clone()))
        {
            let _ = window.drop_image(image);
        }
        #[cfg(windows)]
        {
            self.painted = false;
        }
        #[cfg(not(windows))]
        let _ = window;
    }

    /// 丢掉显示着的那帧。Windows 上它占着窗口的图片缓存，留到画的时候从窗口里拿掉。
    fn drop_picture(&mut self) {
        #[cfg(windows)]
        if std::mem::take(&mut self.painted) {
            self.retired.extend(self.picture.take());
        }
        self.picture = None;
    }

    /// 页面占的地方变了：按新的尺寸要画面，第一次立即要，之后等尺寸停下来再要。
    fn bounds_changed(&mut self, cx: &mut Context<Self>) {
        if self.requested.is_none() {
            self.request_size(cx);
        } else if self
            .wanted()
            .is_some_and(|(display, width, height, _)| Some((display, width, height)) != self.requested)
        {
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

    /// 正在看的屏幕：选过的那块，没选过时是远端报的第一块（主屏）。
    fn current_display(&self) -> Option<&ScreenDisplay> {
        let remote = self.remote.as_ref()?;
        self.display
            .and_then(|id| remote.displays.iter().find(|display| display.id == id))
            .or_else(|| remote.displays.first())
    }

    /// 现在用的档：固定档就是它，自动档是 `Adapter` 挑的。
    fn level(&self) -> Level {
        match self.quality {
            Quality::Auto => self.adapter.level(),
            Quality::Fixed(level) => level,
        }
    }

    /// 档 `level` 要的画面尺寸；还不知道远端的屏幕，或者还不知道页面多大时为空（原画不看页面）。
    fn level_size(&self, level: Level) -> Option<(u32, u32)> {
        let display = self.current_display()?;
        let pixels = (display.pixel_width, display.pixel_height);
        if level == Level::Original {
            if video::codecs().contains(&Codec::Hevc) {
                return Some(input::lines_size(pixels, u32::MAX));
            }
            let (pw, ph) = (pixels.0.max(2) as f32, pixels.1.max(2) as f32);
            let scale = (H264_MAX_PIXELS / (pw * ph)).sqrt();
            return Some(input::capture_size((pw * scale, ph * scale), pixels, H264_MAX_SIDE));
        }
        let (bounds, scale) = self.bounds.get()?;
        let available = (f32::from(bounds.size.width) * scale, f32::from(bounds.size.height) * scale);
        let fit = input::capture_size(available, pixels, MAX_SIDE);
        // 两个尺寸的宽高比相同，取小的那个。
        Some(match level.lines().map(|lines| input::lines_size(pixels, lines)) {
            Some(capped) if capped.0 < fit.0 => capped,
            _ => fit,
        })
    }

    /// 要采的屏幕、画面尺寸和码率；还不知道远端的屏幕或页面多大时为空。
    fn wanted(&self) -> Option<(u32, u32, u32, u32)> {
        let display = self.current_display()?.id;
        let level = self.level();
        let (width, height) = self.level_size(level)?;
        let bitrate = match self.quality {
            Quality::Auto => self.adapter.bitrate().min(pixel_bitrate(width, height, level)),
            Quality::Fixed(_) => pixel_bitrate(width, height, level),
        };
        Some((display, width, height, bitrate))
    }

    /// 按现在的页面尺寸和清晰度向远端要画面：尺寸和上回一样时只改码率，都一样时不再要。远端没有屏幕录制授权时
    /// 也要：采集开不起来，但那台 Mac 由此弹框、把这个程序列进系统设置，用户才有地方打开。
    fn request_size(&mut self, cx: &mut Context<Self>) {
        let Some((display, width, height, bitrate)) = self.wanted() else { return };
        if matches!(self.phase, Phase::Stopped(_) | Phase::Connecting | Phase::Suspended) {
            return;
        }
        if Some((display, width, height)) != self.requested {
            // 本机的指针就落在远端的那个位置上，画面里不再画远端那个慢半拍的指针。
            self.send(ScreenClientMsg::Start {
                display: Some(display),
                width,
                height,
                fps: FPS,
                bitrate,
                hide_cursor: true,
                codecs: video::codecs(),
            });
            self.requested = Some((display, width, height));
        } else if Some(bitrate) != self.bitrate {
            self.send(ScreenClientMsg::Bitrate { bitrate });
        } else {
            return;
        }
        self.bitrate = Some(bitrate);
        cx.notify();
    }

    /// 自动档每 `ADAPT_INTERVAL` 取一轮网络情况调一次；有画面时才调，别的时候只把这一轮丢掉。
    fn adapt_later(&mut self, cx: &mut Context<Self>) {
        if self.quality != Quality::Auto {
            self.adapting = None;
            return;
        }
        self.adapting = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ADAPT_INTERVAL).await;
                if this.update(cx, |this, cx| this.adapt(cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn adapt(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let sample = {
            let mut network = self.network.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((received, lost)) = self.link.as_ref().and_then(ScreenLink::datagram_counts) {
                network.datagrams(received, lost);
            }
            network.sample(now)
        };
        if !matches!(self.phase, Phase::Live) || self.resize.is_some() {
            return;
        }
        let need = |level: Level| {
            self.level_size(level).map_or(level.max_bitrate(), |(width, height)| pixel_bitrate(width, height, level))
        };
        let needs = Needs { ultra: need(Level::Ultra), high: need(Level::High), standard: need(Level::Standard) };
        if self.adapter.update(&sample, &needs, now) {
            tracing::debug!(
                "remote desktop of {}: {:?} at {} bit/s (queuing {:?}, receiving {} bit/s, losing {:.1}%)",
                self.host,
                self.adapter.level(),
                self.adapter.bitrate(),
                sample.queuing,
                sample.throughput,
                sample.loss * 100.,
            );
            self.request_size(cx);
            if self.menu_open(Dropdown::Quality) {
                let items = self.quality_items();
                self.replace_menu_items(items, cx);
            }
            cx.notify();
        }
    }

    fn select_display(&mut self, id: u32, cx: &mut Context<Self>) {
        self.display = Some(id);
        self.request_size(cx);
    }

    fn set_quality(&mut self, action: &SetScreenQuality, _: &mut Window, cx: &mut Context<Self>) {
        // 从固定档换回自动档时从固定档那里接着调，不从头升。
        if let (Quality::Fixed(level), Quality::Auto) = (self.quality, action.quality) {
            let bitrate = self.bitrate.unwrap_or(level.max_bitrate());
            self.adapter = Adapter::starting_at(bitrate, level);
            // 固定档时没人取样，攒下的是很长一段的平均，丢掉重新量。
            self.network.lock().unwrap_or_else(PoisonError::into_inner).reset(Instant::now());
        }
        self.quality = action.quality;
        self.resize = None;
        self.request_size(cx);
        self.adapt_later(cx);
        cx.notify();
    }

    /// 开着的下拉菜单是 `kind` 的。
    fn menu_open(&self, kind: Dropdown) -> bool {
        self.menu_for == kind && self.dropdown_open()
    }

    /// 开关下拉菜单 `kind`；辅助工具打开时选中第一项，回车就能用。
    fn toggle_menu(&mut self, kind: Dropdown, from_a11y: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu_open(kind) {
            self.close_menu(window, cx);
            return;
        }
        let items = match kind {
            Dropdown::Quality => self.quality_items(),
            Dropdown::Resolution => self.resolution_items(),
        };
        self.menu_for = kind;
        // `open_dropdown` 选中给的那一项之后能选的第一项，从最后一项往后绕回第一项。
        let select_after = from_a11y.then(|| items.len().saturating_sub(1));
        self.open_dropdown(items, self.focus.clone(), select_after, cx);
    }

    fn set_mode(&mut self, action: &SetScreenMode, _: &mut Window, cx: &mut Context<Self>) {
        let SetScreenMode { display, mode } = action.clone();
        let previous = match mode {
            Some(mode) => self.modes.insert(display, mode),
            None => self.modes.remove(&display),
        };
        self.mode_requests.push_back((display, previous));
        self.mode_error = None;
        self.send(ScreenClientMsg::SetMode { display, mode });
        cx.notify();
    }

    /// 重连上以后把选过的分辨率再换一次（断开时远端换回了原来的）；发了换的请求时返回真，画面等远端换好再要。
    fn reapply_modes(&mut self) -> bool {
        let Some(remote) = &self.remote else { return false };
        let pending: Vec<(u32, ScreenMode)> = self
            .modes
            .iter()
            .filter(|(id, mode)| {
                remote.displays.iter().any(|display| display.id == **id && display.modes.contains(mode))
            })
            .map(|(id, mode)| (*id, *mode))
            .collect();
        for (display, mode) in &pending {
            self.mode_requests.push_back((*display, None));
            self.send(ScreenClientMsg::SetMode { display: *display, mode: Some(*mode) });
        }
        !pending.is_empty()
    }

    /// 分辨率菜单：远端原来的，和它那块屏幕能换成的各种模式。
    fn resolution_items(&self) -> Vec<Option<MenuItem>> {
        let Some(display) = self.current_display() else { return Vec::new() };
        let current = display.changed.then(|| current_mode(display)).flatten();
        let id = display.id;
        let mut items = vec![
            Some(
                MenuItem::checkbox(
                    rust_i18n::t!("screen_viewer.resolution_default").into_owned(),
                    None,
                    !display.changed,
                    Box::new(SetScreenMode { display: id, mode: None }),
                )
                .with_description(rust_i18n::t!("screen_viewer.resolution_default_detail").into_owned()),
            ),
            None,
        ];
        items.extend(display.modes.iter().map(|&mode| {
            let action = Box::new(SetScreenMode { display: id, mode: Some(mode) });
            let item = MenuItem::checkbox(size_text(mode.width, mode.height), None, current == Some(mode), action);
            // 排版尺寸和像素数不同（Mac 的 HiDPI 模式）时另写像素数。
            Some(if (mode.width, mode.height) == (mode.pixel_width, mode.pixel_height) {
                item
            } else {
                let pixels = size_text(mode.pixel_width, mode.pixel_height);
                item.with_description(rust_i18n::t!("screen_viewer.resolution_hidpi", pixels = pixels).into_owned())
            })
        }));
        items
    }

    /// 清晰度菜单：自动，和从原画到流畅的各档，每项下面写要的画面尺寸和码率。
    fn quality_items(&self) -> Vec<Option<MenuItem>> {
        let item = |quality: Quality, detail: String| {
            let label = quality_label(quality);
            let action = Box::new(SetScreenQuality { quality });
            Some(MenuItem::checkbox(label, None, self.quality == quality, action).with_description(detail))
        };
        let auto_detail = match (self.quality, self.bitrate) {
            (Quality::Auto, Some(bitrate)) => rust_i18n::t!(
                "screen_viewer.quality_auto_now",
                level = level_label(self.adapter.level()),
                bitrate = bitrate_text(bitrate)
            ),
            _ => rust_i18n::t!("screen_viewer.quality_auto_detail"),
        };
        let mut items = vec![item(Quality::Auto, auto_detail.into_owned()), None];
        items.extend(Level::MENU.into_iter().map(|level| {
            let detail = match self.level_size(level) {
                Some((width, height)) => {
                    format!("{} · {}", size_text(width, height), bitrate_text(pixel_bitrate(width, height, level)))
                }
                None => bitrate_text(level.max_bitrate()),
            };
            item(Quality::Fixed(level), detail)
        }));
        items
    }

    /// 工具条上的清晰度按钮：写着现在挑的那档；自动时带上自动挑到的档，页面小到自动挑的几档画面一样大时只写自动。
    fn render_quality_button(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let same_size =
            self.level_size(Level::Ultra).is_some_and(|ultra| self.level_size(Level::Smooth) == Some(ultra));
        let current = match self.quality {
            Quality::Auto if same_size => rust_i18n::t!("screen_viewer.quality_auto").into_owned(),
            Quality::Auto => {
                rust_i18n::t!("screen_viewer.quality_auto_short", level = level_label(self.adapter.level()))
                    .into_owned()
            }
            Quality::Fixed(level) => level_label(level),
        };
        let label = rust_i18n::t!("screen_viewer.quality", current = current.as_str()).into_owned();
        let tip = rust_i18n::t!("screen_viewer.quality_tooltip").into_owned();
        self.render_dropdown_button(Dropdown::Quality, "screen-quality", label, current, tip, window, cx)
    }

    /// 工具条上的分辨率按钮：写着远端屏幕现在的分辨率，没换过时写「默认」；远端换不了模式时为空。
    fn render_resolution_button(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let display = self.current_display().filter(|display| !display.modes.is_empty())?;
        let current = match display.changed.then(|| current_mode(display)).flatten() {
            Some(mode) => size_text(mode.width, mode.height),
            None => rust_i18n::t!("screen_viewer.resolution_default").into_owned(),
        };
        let label = rust_i18n::t!("screen_viewer.resolution", current = current.as_str()).into_owned();
        let tip = rust_i18n::t!("screen_viewer.resolution_tooltip").into_owned();
        Some(self.render_dropdown_button(Dropdown::Resolution, "screen-resolution", label, current, tip, window, cx))
    }

    /// 工具条上按下弹出菜单 `kind` 的按钮：写着 `current`，辅助工具读到的名字是 `label`。
    #[allow(clippy::too_many_arguments)]
    fn render_dropdown_button(
        &self,
        kind: Dropdown,
        id: &'static str,
        label: String,
        current: String,
        tip: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.menu_open(kind);
        let fg = cx.theme().foreground;
        // 菜单挂在按钮下面，按钮下沿就是画面区域的上沿。
        let bottom = self.bounds.get().map_or(0., |(bounds, _)| f32::from(bounds.origin.y));
        let view = cx.entity().downgrade();
        Button::new(id, label)
            .with_size(ButtonSize::Sm)
            .variant(ButtonVariant::Ghost)
            .aria_expanded(open)
            .tooltip(tooltip(tip, None))
            .child(current)
            .child(svg_icon(CHEVRON_DOWN_ICON, fg.opacity(0.6)))
            // 菜单开着时在捕获阶段就关掉、不再往下传：菜单自己的「点到外面就关」和下面再打开的都不跑。
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.menu_open(kind) {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.toggle_menu(kind, false, window, cx);
                }),
            )
            .on_a11y_press(view, move |this, window, cx| this.toggle_menu(kind, true, window, cx))
            .when(open, |button| button.children(self.render_dropdown_menu(bottom, window, cx)))
            .into_any_element()
    }

    fn unlock(&mut self, cx: &mut Context<Self>) {
        let password = self.password.read(cx).query().to_owned();
        if password.is_empty() {
            return;
        }
        self.password.update(cx, |field, cx| field.set_query(String::new(), cx));
        self.unlock_failed = false;
        self.auto_failed = false;
        self.auto_unlock = None;
        self.typed_unlock = true;
        self.pending_save = self.remember.then(|| (password.clone(), Instant::now()));
        self.send(ScreenClientMsg::Unlock { password });
        self.focus_screen = true;
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

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 全屏时鼠标移到上头浮出工具条，移回画面上收起。
        if let Some(bar) = self.input.titlebar.filter(|bar| bar.fullscreen) {
            let zone = bar.reveal_top + bar.height + REVEAL_ZONE;
            self.show_toolbar(f32::from(event.position.y) < zone, cx);
        }
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

    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let (border, fg) = (cx.theme().border, cx.theme().foreground);
        let host = crate::ssh_hosts::display_name(&self.host, cx);
        let title = rust_i18n::t!("screen_viewer.title", host = host).into_owned();
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
                .on_press(cx, |this, _, cx| this.reconnect_now(cx))
        });
        let close = rust_i18n::t!("screen_viewer.close").into_owned();
        let mut icon_button =
            |id: &'static str, icon: &'static str, label: String, event: fn() -> ScreenViewerEvent| {
                Button::new(id, label.clone())
                    .icon(icon)
                    .with_size(ButtonSize::IconSm)
                    .tooltip(tooltip(label, None))
                    .on_press(cx, move |_, _, cx| cx.emit(event()))
            };
        // 在单独的窗口里只给「放回主窗口」；在主窗口里给浮动和占满之间的切换、搬到单独的窗口。
        let window_buttons: Vec<Button> = if self.input.titlebar.is_some() {
            let label = rust_i18n::t!("screen_viewer.pop_in").into_owned();
            vec![icon_button("screen-pop-in", POP_IN_ICON, label, || ScreenViewerEvent::PopIn)]
        } else {
            let (icon, label) = if self.input.floating {
                (MAXIMIZE_ICON, rust_i18n::t!("screen_viewer.dock").into_owned())
            } else {
                (MINIMIZE_ICON, rust_i18n::t!("screen_viewer.float").into_owned())
            };
            let pop_out = rust_i18n::t!("screen_viewer.pop_out").into_owned();
            let pin = self.input.pinned.map(|pinned| {
                let key = if pinned { "screen_viewer.unpin" } else { "screen_viewer.pin" };
                icon_button("screen-pin", PIN_ICON, rust_i18n::t!(key).into_owned(), || ScreenViewerEvent::TogglePin)
                    .toggled(Some(pinned))
            });
            pin.into_iter()
                .chain([
                    icon_button("screen-float", icon, label, || ScreenViewerEvent::ToggleFloating),
                    icon_button("screen-pop-out", POP_OUT_ICON, pop_out, || ScreenViewerEvent::PopOut),
                ])
                .collect()
        };
        // 标题这一段：浮动时按下就开始拖动这一页，当标题栏时拖动窗口；按钮在它外面，按按钮不会拖。
        let (floating, titlebar) = (self.input.floating, self.input.titlebar);
        let heading = div()
            .id("screen-heading")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(svg_icon(MONITOR_ICON, fg.opacity(0.6)))
            .child(div().min_w_0().truncate().child(title))
            .when(titlebar.is_some(), |heading| heading.window_control_area(WindowControlArea::Drag))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    if titlebar.is_some() {
                        cx.stop_propagation();
                        crate::ui::drag_window(event, window, cx);
                    } else if floating {
                        cx.stop_propagation();
                        cx.emit(ScreenViewerEvent::StartDrag(event.position));
                    }
                }),
            );
        let toolbar = match titlebar {
            Some(bar) => div().h(px(bar.height)).pl(px(bar.left.max(10.))).pr(px(bar.right + 10.)),
            None => div().h(scaled(32.)).px(px(10.)),
        };
        toolbar
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .border_b_1()
            .border_color(border)
            .text_color(fg)
            .text_size(FontSize::Body)
            .child(heading)
            .children(displays)
            .children(self.render_resolution_button(window, cx))
            .when(self.remote.is_some(), |toolbar| toolbar.child(self.render_quality_button(window, cx)))
            .children(reconnect)
            .children(window_buttons)
            // 在单独的窗口里用窗口自己的关闭按钮。
            .when(titlebar.is_none(), |toolbar| {
                toolbar.child(
                    Button::new("screen-close", close.clone())
                        .icon(CLOSE_ICON)
                        .with_size(ButtonSize::IconSm)
                        .tooltip(tooltip(close, None))
                        .on_press(cx, |_, _, cx| cx.emit(ScreenViewerEvent::Close)),
                )
            })
            .into_any_element()
    }

    /// 盖在画面上的提示：连接中、开不起来、缺授权；没有要提示的为空。
    fn notice(&self, cx: &App) -> Option<String> {
        let notice = match &self.phase {
            Phase::Connecting | Phase::Suspended => {
                rust_i18n::t!("screen_viewer.connecting", host = crate::ssh_hosts::display_name(&self.host, cx))
            }
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

    /// 远端锁屏时能代敲密码：Windows 的锁屏在安全桌面上，注入不了按键。
    fn types_password(&self) -> bool {
        !remote::find(&self.host).is_some_and(|host| host.family() == Family::Windows)
    }

    /// 锁屏时盖在画面中间的一块：能代敲密码的远端给密码框和「解锁」；Windows 的锁屏在安全桌面上，注入不了按键，
    /// 只说要在那台电脑前解锁。
    fn render_lock(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let unlock = rust_i18n::t!("screen_viewer.unlock").into_owned();
        let typed = self.types_password();
        let label = if !typed {
            rust_i18n::t!("screen_viewer.locked_elsewhere")
        } else if self.unlock_failed {
            rust_i18n::t!("screen_viewer.unlock_failed")
        } else if self.auto_unlock.is_some() {
            rust_i18n::t!("screen_viewer.auto_unlocking")
        } else if self.auto_failed {
            rust_i18n::t!("screen_viewer.auto_unlock_failed")
        } else {
            rust_i18n::t!("screen_viewer.locked")
        };
        let remember = rust_i18n::t!("screen_viewer.remember_password").into_owned();
        let form = typed.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(TextInput::new(&self.password))
                .child(
                    Switch::new("screen-remember-password", remember.clone(), self.remember)
                        .label(remember)
                        .text_size(FontSize::Small)
                        .text_color(theme.muted_foreground)
                        .on_press(cx, |this, _, cx| this.toggle_remember(cx)),
                )
                .child(
                    Button::new("screen-unlock", unlock)
                        .variant(ButtonVariant::Default)
                        .on_press(cx, |this, _, cx| this.unlock(cx)),
                )
        });
        let panel = div()
            .id("screen-lock")
            .role(Role::Group)
            // 点在这块上不往下传给画面：画面按下时把焦点拿回自己，密码框就收不到字了。
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.stop_propagation()))
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

impl MenuHost for ScreenViewer {
    fn menu_slot(&mut self) -> &mut Option<Menu> {
        &mut self.menu
    }

    fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
}

impl Render for ScreenViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        #[cfg(windows)]
        {
            for picture in self.retired.drain(..) {
                let _ = window.drop_image(picture.image);
            }
            self.painted |= self.picture.is_some();
        }
        // 只在焦点本来就在这一页上时挪：这一页浮在终端上面、或者同时开着几台时，用户在别处敲的字不能被挪进密码框。
        let password = self.password.focus_handle(cx);
        let ours = self.focused(window, cx);
        if std::mem::take(&mut self.focus_password) && ours {
            window.focus(&password, cx);
        }
        if std::mem::take(&mut self.focus_screen) && ours {
            window.focus(&self.focus, cx);
        }
        let theme = cx.theme().clone();
        let label =
            rust_i18n::t!("screen_viewer.screen", host = crate::ssh_hosts::display_name(&self.host, cx)).into_owned();
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
        let notice = self.notice(cx).map(|notice| {
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
        let banner_text = if readonly {
            Some(rust_i18n::t!("screen_viewer.no_post_event").into_owned())
        } else {
            self.mode_error
                .as_ref()
                .map(|reason| rust_i18n::t!("screen_viewer.mode_failed", reason = reason).into_owned())
        };
        let banner = banner_text.map(|text| {
            div()
                .id("screen-banner")
                .role(Role::Label)
                .aria_label(text.clone())
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
                .child(text)
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
        // 全屏时工具条不占地方，浮出来时盖在画面上面（`mouse_move`）。辅助工具开着时一直显示，收起来的按钮它按
        // 不到。
        let autohide = self.autohides_toolbar() && !window.is_a11y_active();
        let toolbar = (!autohide).then(|| self.render_toolbar(window, cx));
        let reveal_top = self.input.titlebar.map_or(0., |bar| bar.reveal_top);
        let floating_toolbar = (autohide && self.toolbar_shown).then(|| {
            div()
                .id("screen-toolbar-shown")
                .absolute()
                .top(px(reveal_top))
                .left_0()
                .right_0()
                .occlude()
                .bg(theme.background)
                .child(self.render_toolbar(window, cx))
        });
        div()
            .id("screen-viewer")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .on_action(cx.listener(Self::set_quality))
            .on_action(cx.listener(Self::set_mode))
            .children(toolbar)
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
            .children(floating_toolbar)
    }
}

/// 屏幕 `display` 现在的显示模式：像素数相同的模式里排版尺寸最接近的那个（Windows 报的点数按缩放比例折过）。
fn current_mode(display: &ScreenDisplay) -> Option<ScreenMode> {
    display
        .modes
        .iter()
        .filter(|mode| (mode.pixel_width, mode.pixel_height) == (display.pixel_width, display.pixel_height))
        .min_by_key(|mode| mode.width.abs_diff(display.width))
        .copied()
}

/// 清晰度菜单里写的名字。
fn quality_label(quality: Quality) -> String {
    match quality {
        Quality::Auto => rust_i18n::t!("screen_viewer.quality_auto").into_owned(),
        Quality::Fixed(level) => level_label(level),
    }
}

fn level_label(level: Level) -> String {
    match level {
        Level::Original => rust_i18n::t!("screen_viewer.level_original"),
        Level::Ultra => rust_i18n::t!("screen_viewer.level_ultra"),
        Level::High => rust_i18n::t!("screen_viewer.level_high"),
        Level::Standard => rust_i18n::t!("screen_viewer.level_standard"),
        Level::Smooth => rust_i18n::t!("screen_viewer.level_smooth"),
    }
    .into_owned()
}

/// `width`×`height` 的画面在档 `level` 下的码率：按像素数算，不超过这一档的上限。
fn pixel_bitrate(width: u32, height: u32, level: Level) -> u32 {
    ((f64::from(width) * f64::from(height) * f64::from(FPS) * BITS_PER_PIXEL) as u32).min(level.max_bitrate())
}

fn size_text(width: u32, height: u32) -> String {
    format!("{width} × {height}")
}

fn bitrate_text(bitrate: u32) -> String {
    let mbps = f64::from(bitrate) / 1e6;
    if mbps >= 10. { format!("{mbps:.0} Mbps") } else { format!("{mbps:.1} Mbps") }
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

/// 读线程上处理远端交来的：每帧到的时刻和大小记进 `network`，画面就地解好，只把最新的一帧留在 `latest` 里，
/// 界面取走以前再来的顶掉它；解不了时向远端要关键帧（最多 `KEYFRAME_INTERVAL` 一次），本机没有这种编码的解码器
/// （`ErrorKind::Unsupported`）时报 `UiEvent::Undecodable`、之后的画面都不解。解码器在读线程上建：Windows 上它是
/// COM 对象，要在用它的线程上建。
fn reader(
    tx: futures::channel::mpsc::UnboundedSender<UiEvent>,
    latest: Arc<Mutex<Option<Picture>>>,
    network: Arc<Mutex<Network>>,
) -> impl FnMut(ScreenEvent, &ScreenSender) + Send + 'static {
    let mut decoder: Option<VideoDecoder> = None;
    let mut asked: Option<Instant> = None;
    let mut undecodable = false;
    let tx_undecodable = tx.clone();
    let give_up = move |err: std::io::Error, undecodable: &mut bool| {
        tracing::warn!("cannot decode the remote desktop: {err}");
        *undecodable = true;
        let reason = rust_i18n::t!("screen_viewer.no_decoder", error = err.to_string()).into_owned();
        let _ = tx_undecodable.unbounded_send(UiEvent::Undecodable(reason));
    };
    move |event, sender| {
        let frame = match event {
            ScreenEvent::Message(message) => {
                // 界面收到 `Ready` 就要按解得了的编码要画面；Windows 上查解码器要建 COM 对象，先在这个线程上查好。
                if matches!(message, ScreenHostMsg::Ready { .. }) {
                    let _ = video::codecs();
                }
                // 重新开始采集，之后的时间戳换了起点。
                if matches!(message, ScreenHostMsg::Started { .. }) {
                    network.lock().unwrap_or_else(PoisonError::into_inner).reset(Instant::now());
                }
                let _ = tx.unbounded_send(UiEvent::Message(message));
                return;
            }
            ScreenEvent::Closed => {
                let _ = tx.unbounded_send(UiEvent::Closed);
                return;
            }
            ScreenEvent::Video(_) if undecodable => return,
            ScreenEvent::Video(frame) => frame,
        };
        let bytes = frame.data.len() + frame.parameter_sets.iter().map(Vec::len).sum::<usize>();
        network.lock().unwrap_or_else(PoisonError::into_inner).frame(
            Instant::now(),
            frame.pts_us,
            bytes,
            frame.keyframe,
        );
        if decoder.is_none() {
            match VideoDecoder::new() {
                Ok(created) => decoder = Some(created),
                Err(err) => return give_up(err, &mut undecodable),
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
            Err(err) if err.kind() == std::io::ErrorKind::Unsupported => return give_up(err, &mut undecodable),
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
