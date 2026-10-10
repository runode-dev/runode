//! 右侧面板的模拟器页：经用户自己装的 mobilecli 列出 iOS 模拟器、Android 模拟器和连着的真机，
//! 显示选中那台的实时画面；在画面上点按、长按、滑动，在输入框里打字，按硬件键，都转成 mobilecli
//! 的命令。agent 在终端里直接用 mobilecli 操作同一台设备，这一页让人看着。
//!
//! 画面本身只是一张图，辅助工具（VoiceOver、CUA）开着时另外隔一会儿用 `dump ui` 读设备里的元素，
//! 在画面上对应的位置摆上看不见的节点报出去，辅助工具按下一个就点它的中心。
//!
//! mobilecli 不随 app 分发（它的许可不是开源许可），从访达打开的 app 自己的 PATH 里也没有 npm、
//! brew 装的目录，所以和 `ai_message` 一样用终端里 shell 报告的 PATH 找它。

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    ffi::OsString,
    io::{self, BufRead, BufReader},
    os::unix::process::CommandExt as _,
    process::{Child, Command, Stdio},
    rc::Rc,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use gpui::accesskit::ActionData;
use gpui::{
    AccessibleAction, AnyElement, App, Bounds, Context, CursorStyle, DispatchPhase, Div, Entity, ImageSource,
    MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, RenderImage, Role, SharedString, Stateful, Subscription,
    Task, Window, canvas, div, img, prelude::*, px, svg,
};
use serde::Deserialize;
use serde_json::Value;

use runode_shared_types::color::Rgb;

use super::{
    WindowView,
    project::{ADDED, panel_message, panel_shell, panel_title},
    titlebar::icon_toggle,
};
use crate::{
    assets::{
        ARROW_LEFT_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, HOME_BUTTON_ICON, POWER_ICON, REFRESH_ICON,
        VOLUME_DOWN_ICON, VOLUME_UP_ICON,
    },
    ui::{
        a11y::A11yPress,
        hsla,
        text_field::{TextField, TextFieldEvent},
        tooltip::tooltip,
    },
};

const PROGRAM: &str = "mobilecli";
/// 按下到抬起挪动不到这么多（面板上的点）算点按，否则是滑动；按住超过 `LONG_PRESS` 算长按。
const TAP_SLOP: f32 = 6.;
const LONG_PRESS: Duration = Duration::from_millis(500);
/// 一帧最大多少字节，超过的当流坏了，不照着 `Content-Length` 去分配。
const MAX_FRAME_BYTES: usize = 16 << 20;
/// 辅助工具开着时隔多久重读一次设备里的元素：画面自己会变，agent 也在操作同一台设备。
const ELEMENTS_POLL: Duration = Duration::from_secs(2);
/// 画面流断了以后隔多久再看一次设备是不是关了。
const SHUTDOWN_POLL: Duration = Duration::from_secs(2);
/// 设备四周的留白。
const SCREEN_PADDING: f32 = 12.;
/// 灵动岛的颜色，和真机的一样是黑的，深浅主题下都不变。
const ISLAND: Rgb = Rgb(0x00, 0x00, 0x00);
/// 设备下面那条工具栏的高度，和它离设备的距离。
const TOOLBAR_HEIGHT: f32 = 28.;
const TOOLBAR_GAP: f32 = 10.;

/// mobilecli 列出的一台设备。
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(super) struct Device {
    id: String,
    name: String,
    /// `ios` 或 `android`。
    platform: String,
    /// `simulator`、`emulator` 或 `real`。
    #[serde(rename = "type")]
    kind: String,
    /// `online` 或 `offline`。
    state: String,
    /// 模拟器是 CoreSimulator 的机型标识（`com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro`），
    /// 真机是硬件型号（`iPhone14,5`）；屏幕圆角多大、画不画灵动岛靠它。
    #[serde(default)]
    model: String,
}

impl Device {
    fn online(&self) -> bool {
        self.state == "online"
    }

    fn android(&self) -> bool {
        self.platform == "android"
    }
}

/// 设备屏幕的大小，单位和 `io tap` 的坐标一样（iOS 是点，Android 是像素）。
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
struct ScreenSize {
    width: f32,
    height: f32,
}

/// 设备里的一个界面元素：名字、种类和在屏幕上的位置，单位和 `ScreenSize` 一样。
#[derive(Clone, Debug, PartialEq)]
struct DeviceElement {
    label: String,
    /// 输入框里现在的文字，别的元素多半为空。
    value: String,
    /// mobilecli 报的种类，`Button`、`StaticText`、`TextField` 这些。
    kind: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl DeviceElement {
    fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2., self.y + self.height / 2.)
    }
}

/// 屏幕画成什么样：圆角是屏幕宽的几倍，顶上要不要画灵动岛。
#[derive(Debug, PartialEq)]
struct ScreenShape {
    radius: f32,
    island: bool,
}

impl Device {
    fn screen_shape(&self) -> ScreenShape {
        if self.android() {
            return ScreenShape { radius: 0.07, island: false };
        }
        if self.model.contains("iPad") || self.name.contains("iPad") {
            return ScreenShape { radius: 0.03, island: false };
        }
        ScreenShape { radius: 0.135, island: self.has_island() }
    }

    /// iPhone 14 Pro 起和 15 以后的机型有灵动岛；真机只报硬件型号，认不出来，不画。
    fn has_island(&self) -> bool {
        let Some(rest) = self.model.split("iPhone-").nth(1) else { return false };
        let generation: u32 = rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
        let variant = &rest[generation.to_string().len().min(rest.len())..];
        // 16e 是刘海屏。
        (generation >= 15 && !variant.starts_with('e')) || (generation == 14 && variant.contains("Pro"))
    }
}

/// 在画面上按下的地方和时候，抬起时据此分出点按、长按和滑动。
#[derive(Clone, Copy)]
struct Press {
    at: Point<Pixels>,
    since: Instant,
}

#[derive(Debug, PartialEq)]
enum Gesture {
    Tap(i64, i64),
    LongPress(i64, i64, u64),
    Swipe(i64, i64, i64, i64, u64),
}

/// 模拟器页的状态，整个窗口一份。
#[derive(Default)]
pub(super) struct SimulatorPage {
    /// 最近一次列出的设备；还没列过时为空。
    devices: Option<Vec<Device>>,
    selected: Option<String>,
    screen: Option<ScreenSize>,
    frame: Option<Arc<RenderImage>>,
    stream: Option<Stream>,
    /// 最近一次 mobilecli 报的错，显示在设备列表下面。
    error: Option<String>,
    /// 找不到 mobilecli。
    missing: bool,
    press: Option<Press>,
    /// 辅助工具开着时最近一次读到的设备里的元素。
    elements: Vec<DeviceElement>,
    /// 画面上一帧画在哪里，按下抬起的位置靠它换算成设备的坐标。
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 设备列表和输入框之间留给画面的地方上一帧有多大，画面按它等比缩放。
    room: Rc<Cell<Option<(f32, f32)>>>,
    text: Option<(Entity<TextField>, Subscription)>,
    /// 上次看 PATH 里有没有 mobilecli 时的 PATH 和结果，标签要不要显示靠它，PATH 没变就不再找。
    found: RefCell<Option<(Option<OsString>, bool)>>,
    loading: Option<Task<()>>,
    /// 标题行展开着设备列表；选好一台后收起，把地方让给画面。
    picking: bool,
    /// 正在启动设备或往里装 mobilecli 的 agent 时，画面的位置上显示的那句话。
    busy: Option<String>,
}

/// 画面出不来时那个按钮（启动、装 agent、重连）按下时做的事。
type PageAction = fn(&mut WindowView, &mut Context<WindowView>);
/// 那个按钮的标识、文字和按下时做的事。
type StatusAction = (&'static str, Cow<'static, str>, PageAction);

/// 正在推的画面：推流的进程和收帧的任务，丢掉时一起停。
struct Stream {
    _process: StreamProcess,
    _frames: Task<()>,
    /// 辅助工具开着时定时读设备里的元素，跟着画面一起停。
    elements: Option<Task<()>>,
}

/// 推画面的 mobilecli 进程，丢掉时连它拉起的子进程一起结束。一拉起来就包上：连接的任务被新的连接
/// 顶掉时，已经拉起的进程随任务的结果一起丢掉，也会结束。
struct StreamProcess(Option<Child>);

impl Drop for StreamProcess {
    fn drop(&mut self) {
        let Some(mut child) = self.0.take() else { return };
        // mobilecli 是 npm 装的 node 包装脚本，真正推流的二进制是它的子进程，所以杀整个进程组。
        unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGTERM) };
        thread::spawn(move || child.wait());
    }
}

impl WindowView {
    /// 从访达打开时 app 自己的 PATH 里没有 npm、brew 装的目录，用终端里 shell 报告的 PATH。
    fn simulator_path(&self, cx: &App) -> Option<OsString> {
        let path = self.focused_view().and_then(|view| view.read(cx).meta().shell_path.clone());
        path.or_else(|| std::env::var_os("PATH"))
    }

    /// PATH 里有 mobilecli 时才显示模拟器的标签；正显示着时仍留着。
    pub(super) fn simulator_tab_visible(&self, cx: &App) -> bool {
        if self.simulator_shown() {
            return true;
        }
        let path = self.simulator_path(cx);
        let mut found = self.simulator.found.borrow_mut();
        match &*found {
            Some((seen, installed)) if *seen == path => *installed,
            _ => {
                let installed = path.as_ref().is_some_and(on_path);
                *found = Some((path, installed));
                installed
            }
        }
    }

    /// 切到模拟器页时列一遍设备；切走或收起时停掉画面，放掉最后一帧：一帧是整块屏幕大小的位图，
    /// 内存里和图集里各占一份，Pro Max 上各约 15 MB，切回来时重连会推新的。
    pub(super) fn simulator_panel_changed(&mut self, cx: &mut Context<Self>) {
        if self.simulator_shown() {
            if self.simulator.devices.is_none() {
                self.list_devices(true, cx);
            } else if self.simulator.stream.is_none() {
                self.connect_device(cx);
            }
        } else {
            self.simulator.stream = None;
            if let Some(frame) = self.simulator.frame.take() {
                cx.drop_image(frame, None);
            }
        }
    }

    /// 列一遍设备；`connect` 为假时只更新设备的状态，不拉起画面，画面流断了以后用它看出设备是不是关了。
    fn list_devices(&mut self, connect: bool, cx: &mut Context<Self>) {
        let path = self.simulator_path(cx);
        let job = cx.background_spawn(async move { mobilecli(path.as_deref(), &["devices", "--include-offline"]) });
        self.simulator.loading = Some(cx.spawn(async move |this, cx| {
            let result = job.await.and_then(|data| parse_devices(&data));
            this.update(cx, |this, cx| {
                let page = &mut this.simulator;
                page.missing = matches!(&result, Err(err) if err == MISSING);
                match result {
                    Ok(devices) => {
                        // 没选过或选的那台不在了，挑第一台开着的。
                        if !page.selected.as_ref().is_some_and(|id| devices.iter().any(|device| &device.id == id)) {
                            page.selected = default_device(&devices).map(|device| device.id.clone());
                            page.screen = None;
                            page.stream = None;
                        }
                        page.devices = Some(sorted(devices));
                        // 只看状态时设备还开着，就留着流断开的原因。
                        if connect || !this.selected_device().is_some_and(Device::online) {
                            this.simulator.error = None;
                        }
                    }
                    Err(err) => {
                        page.devices.get_or_insert_default();
                        page.error = Some(err);
                    }
                }
                if connect {
                    this.connect_device(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 画面流断了以后隔一会儿列一遍设备，最多十来秒：模拟器正在关时流先断，
    /// mobilecli 要等关完、不再是 Booted 才报 offline。
    fn watch_device_shutdown(&mut self, cx: &mut Context<Self>) {
        let id = self.simulator.selected.clone();
        cx.spawn(async move |this, cx| {
            for _ in 0..6 {
                // 选了别的设备、重连了或在连、页收起了、设备已经报关了，都不用再看。
                let stalled = this.update(cx, |this, cx| {
                    let stalled = this.simulator.selected == id
                        && this.simulator.stream.is_none()
                        && this.simulator.loading_done()
                        && this.simulator_shown()
                        && this.selected_device().is_some_and(Device::online);
                    if stalled {
                        this.list_devices(false, cx);
                    }
                    stalled
                });
                if !matches!(stalled, Ok(true)) {
                    return;
                }
                cx.background_executor().timer(SHUTDOWN_POLL).await;
            }
        })
        .detach();
    }

    fn selected_device(&self) -> Option<&Device> {
        let page = &self.simulator;
        page.devices.as_ref()?.iter().find(|device| Some(&device.id) == page.selected.as_ref())
    }

    fn select_device(&mut self, id: String, cx: &mut Context<Self>) {
        self.simulator.picking = false;
        cx.notify();
        if self.simulator.selected.as_ref() == Some(&id) {
            return;
        }
        let page = &mut self.simulator;
        page.selected = Some(id);
        page.screen = None;
        page.stream = None;
        page.error = None;
        page.busy = None;
        page.elements.clear();
        if let Some(frame) = page.frame.take() {
            cx.drop_image(frame, None);
        }
        self.connect_device(cx);
        cx.notify();
    }

    /// 选中的设备开着、页显示着而画面没在推时，先问屏幕大小，再拉起推画面的进程。
    fn connect_device(&mut self, cx: &mut Context<Self>) {
        if !self.simulator_shown() || self.simulator.stream.is_some() {
            return;
        }
        let Some(device) = self.selected_device().filter(|device| device.online()) else { return };
        let id = device.id.clone();
        let path = self.simulator_path(cx);
        let svg = cx.svg_renderer();
        let job = cx.background_spawn(async move {
            let data = mobilecli(path.as_deref(), &["device", "info", "--device", &id])?;
            let screen = parse_screen(&data)?;
            let (child, frames) = start_stream(path.as_deref(), &id, svg)?;
            Ok::<_, String>((screen, StreamProcess(Some(child)), frames))
        });
        self.simulator.loading = Some(cx.spawn(async move |this, cx| {
            let started = job.await;
            // 连着的时候页收起来了：不推画面也不报错，丢掉的进程跟着结束；下次展开时会重新连。
            if !this.update(cx, |this, _| this.simulator_shown()).unwrap_or(false) {
                return;
            }
            let (screen, process, mut frames) = match started {
                Ok(started) => started,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.simulator.error = Some(err);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let pump = cx.spawn({
                let this = this.clone();
                async move |cx| {
                    while let Some(mut frame) = frames.next().await {
                        // 界面跟不上时只画最新的一帧；没画过的帧不在图集里，直接丢掉。
                        while let Ok(newer) = frames.try_recv() {
                            frame = newer;
                        }
                        let shown = this.update(cx, |this, cx| {
                            let page = &mut this.simulator;
                            match frame {
                                Ok(frame) => {
                                    if let Some(old) = page.frame.replace(frame) {
                                        cx.drop_image(old, None);
                                    }
                                    page.error = None;
                                }
                                Err(err) => page.error = Some(err),
                            }
                            cx.notify();
                        });
                        if shown.is_err() {
                            return;
                        }
                    }
                    // 流断了（设备关了、mobilecli 退了），留着最后一帧，等用户按重连；
                    // 设备关了时换成「没在运行」和启动按钮。
                    this.update(cx, |this, cx| {
                        let page = &mut this.simulator;
                        page.stream = None;
                        page.error.get_or_insert_with(|| rust_i18n::t!("simulator.stream_ended").into_owned());
                        this.watch_device_shutdown(cx);
                        cx.notify();
                    })
                    .ok();
                }
            });
            this.update(cx, |this, cx| {
                this.simulator.screen = Some(screen);
                this.simulator.stream = Some(Stream { _process: process, _frames: pump, elements: None });
                cx.notify();
            })
            .ok();
        }));
    }

    /// 在后台对选中的设备跑一条 mobilecli 命令，`args` 里不含 `--device`；做完后跑 `then`。出错时把错写在页上。
    fn device_command(
        &mut self,
        args: Vec<String>,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.simulator.selected.clone() else { return };
        let path = self.simulator_path(cx);
        let job = cx.background_spawn(async move {
            let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
            args.extend(["--device", &id]);
            mobilecli(path.as_deref(), &args)
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(_) => then(this, cx),
                    Err(err) => {
                        // 启动、装 agent 失败时也要收起「正在…」，把原因显示出来。
                        this.simulator.busy = None;
                        this.simulator.error = Some(err);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn boot_device(&mut self, cx: &mut Context<Self>) {
        self.simulator.error = None;
        self.simulator.busy = Some(rust_i18n::t!("simulator.booting").into_owned());
        self.device_command(
            vec!["device".into(), "boot".into()],
            |this, cx| {
                this.simulator.busy = None;
                this.list_devices(true, cx);
            },
            cx,
        );
        cx.notify();
    }

    /// mobilecli 第一次操作一台 iOS 设备前要往里装它的 agent，没装时 `device info` 报错提示这一步。
    fn install_agent(&mut self, cx: &mut Context<Self>) {
        self.simulator.error = None;
        self.simulator.busy = Some(rust_i18n::t!("simulator.installing_agent").into_owned());
        self.device_command(vec!["agent".into(), "install".into()], |this, cx| this.reconnect_device(cx), cx);
        cx.notify();
    }

    fn reconnect_device(&mut self, cx: &mut Context<Self>) {
        self.simulator.busy = None;
        self.simulator.stream = None;
        self.simulator.error = None;
        self.connect_device(cx);
        cx.notify();
    }

    fn press_button(&mut self, button: &'static str, cx: &mut Context<Self>) {
        self.device_command(vec!["io".into(), "button".into(), button.into()], |_, _| {}, cx);
    }

    fn send_device_text(&mut self, cx: &mut Context<Self>) {
        let Some((field, _)) = &self.simulator.text else { return };
        let text = field.read(cx).query().to_owned();
        if text.is_empty() {
            return;
        }
        field.update(cx, |field, cx| field.set_query(String::new(), cx));
        self.device_command(vec!["io".into(), "text".into(), text], |_, _| {}, cx);
    }

    fn release_screen(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(press), Some(bounds), Some(screen)) =
            (self.simulator.press.take(), self.simulator.bounds.get(), self.simulator.screen)
        else {
            return;
        };
        let to_device = |point: Point<Pixels>| {
            let x = (f32::from(point.x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0., 1.);
            let y = (f32::from(point.y - bounds.origin.y) / f32::from(bounds.size.height)).clamp(0., 1.);
            (x * screen.width, y * screen.height)
        };
        let moved = f32::from((at.x - press.at.x).abs()).max(f32::from((at.y - press.at.y).abs()));
        let gesture = gesture(to_device(press.at), to_device(at), moved, press.since.elapsed());
        self.device_command(gesture_args(&gesture), |_, _| {}, cx);
    }

    /// 辅助工具开着、画面在推时，隔 `ELEMENTS_POLL` 读一次设备里的元素；画面停了跟着停。
    fn watch_device_elements(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !window.is_a11y_active() || self.simulator.stream.as_ref().is_none_or(|stream| stream.elements.is_some()) {
            return;
        }
        let Some(id) = self.simulator.selected.clone() else { return };
        let path = self.simulator_path(cx);
        let task = cx.spawn(async move |this, cx| {
            loop {
                let (path, id) = (path.clone(), id.clone());
                let elements = cx
                    .background_spawn(async move { mobilecli(path.as_deref(), &["dump", "ui", "--device", &id]) })
                    .await
                    .map(|data| parse_elements(&data));
                // 读不出来时留着上次的，下一轮再试；出的错由点按这些操作去报。
                let alive = this.update(cx, |this, cx| {
                    if let Ok(elements) = elements
                        && this.simulator.elements != elements
                    {
                        this.simulator.elements = elements;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
                cx.background_executor().timer(ELEMENTS_POLL).await;
            }
        });
        if let Some(stream) = &mut self.simulator.stream {
            stream.elements = Some(task);
        }
    }

    /// 辅助工具给设备里的输入框设文字。
    // shortcut: mobilecli 只能在光标处打字，原来的文字不会被换掉；要整段替换得先能全选，等 mobilecli 有清空输入框的命令再改。
    fn type_into_element(&mut self, element: &DeviceElement, text: String, cx: &mut Context<Self>) {
        let (x, y) = element.center();
        let tap = gesture_args(&Gesture::Tap(x.round() as i64, y.round() as i64));
        self.device_command(
            tap,
            move |this, cx| this.device_command(vec!["io".into(), "text".into(), text], |_, _| {}, cx),
            cx,
        );
    }

    /// 辅助工具按下画面边上的滑动按钮：手指从屏幕中间朝 `(dx, dy)` 的方向划半屏，`dx`、`dy` 取 -1、0、1。
    fn swipe_screen(&mut self, (dx, dy): (f32, f32), cx: &mut Context<Self>) {
        let Some(screen) = self.simulator.screen else { return };
        let (mid_x, mid_y) = (screen.width / 2., screen.height / 2.);
        let (dx, dy) = (dx * screen.width / 4., dy * screen.height / 4.);
        let point = |x: f32, y: f32| (x.round() as i64, y.round() as i64);
        let ((x1, y1), (x2, y2)) = (point(mid_x - dx, mid_y - dy), point(mid_x + dx, mid_y + dy));
        self.device_command(gesture_args(&Gesture::Swipe(x1, y1, x2, y2, 300)), |_, _| {}, cx);
    }

    /// 辅助工具按下设备里的一个元素：点它的中心。
    fn tap_element(&mut self, element: &DeviceElement, cx: &mut Context<Self>) {
        let (x, y) = element.center();
        self.device_command(gesture_args(&Gesture::Tap(x.round() as i64, y.round() as i64)), |_, _| {}, cx);
    }

    /// 模拟器页：设备列表，选中那台的画面、输入框和硬件键。
    pub(super) fn render_simulator_panel(
        &mut self,
        width: f32,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.watch_device_elements(window, cx);
        let view = cx.entity().downgrade();
        let refresh = icon_toggle("simulator-refresh", REFRESH_ICON, 14., false, fg, bg)
            .aria_label(rust_i18n::t!("simulator.refresh").into_owned())
            .flex_none()
            .size(px(22.))
            .tooltip(tooltip(rust_i18n::t!("simulator.refresh"), None, fg, bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.list_devices(true, cx);
                }),
            )
            .on_a11y_press(view.clone(), |this, _, cx| this.list_devices(true, cx));
        let shell = panel_shell("simulator-panel", width, fg, bg, cx)
            .bg(hsla(bg.mix(fg, 0.03)))
            .text_size(px(12.))
            .child(self.render_panel_tabs(fg, bg, cx));
        if self.simulator.missing {
            return shell
                .child(panel_title().child(div().flex_1()).child(refresh))
                .child(panel_message(rust_i18n::t!("simulator.missing").into_owned(), fg));
        }
        let Some(devices) = &self.simulator.devices else {
            return shell
                .child(panel_title().child(div().flex_1()).child(refresh))
                .child(panel_message(rust_i18n::t!("simulator.loading").into_owned(), fg));
        };
        if devices.is_empty() && self.simulator.error.is_none() {
            return shell
                .child(panel_title().child(div().flex_1()).child(refresh))
                .child(panel_message(rust_i18n::t!("simulator.no_devices").into_owned(), fg));
        }
        let device = self.selected_device().cloned();
        let picking = self.simulator.picking || device.is_none();
        // 标题行就是选设备的按钮：显示选中的那台，按下展开或收起设备列表。
        let (name, detail) = match &device {
            Some(device) => (SharedString::from(device.name.clone()), device_detail(device)),
            None => (rust_i18n::t!("simulator.choose_device").into_owned().into(), String::new()),
        };
        let picker = div()
            .id("simulator-picker")
            .role(Role::Button)
            .aria_label(format!("{name} {detail}"))
            .aria_expanded(picking)
            .flex_1()
            .min_w_0()
            .h(px(24.))
            .px(px(6.))
            .ml(px(-6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .hover(|picker| picker.bg(hsla(bg.mix(fg, 0.07))))
            .child(div().flex_initial().min_w_0().truncate().text_color(hsla(fg)).child(name))
            .child(div().flex_initial().min_w_0().truncate().text_color(hsla(fg).opacity(0.5)).child(detail))
            .child(
                svg()
                    .path(if picking { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                    .flex_none()
                    .size(px(12.))
                    .text_color(hsla(fg).opacity(0.5)),
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(cx)))
            .on_a11y_press(view.clone(), |this, _, cx| this.toggle_picker(cx));
        let shell = shell.child(panel_title().child(picker).child(refresh));
        let shell = if picking {
            let rows = devices.iter().map(|device| {
                let selected = self.simulator.selected.as_ref() == Some(&device.id);
                let detail = device_detail(device);
                let id = device.id.clone();
                let offline = (!device.online()).then(|| rust_i18n::t!("simulator.offline"));
                let label = match &offline {
                    Some(offline) => format!("{} {detail} {offline}", device.name),
                    None => format!("{} {detail}", device.name),
                };
                // 没启动的那几台淡一些，一眼看出哪些能直接看画面。
                let dim = if device.online() { 1. } else { 0.55 };
                div()
                    .id(SharedString::from(format!("device-{}", device.id)))
                    .role(Role::ListBoxOption)
                    .aria_label(label)
                    .aria_selected(selected)
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(selected, |row| row.bg(hsla(bg.mix(fg, 0.10))))
                    .hover(|row| row.bg(hsla(bg.mix(fg, 0.07))))
                    .child(div().flex_none().size(px(6.)).rounded_full().bg(if device.online() {
                        hsla(ADDED)
                    } else {
                        hsla(fg).opacity(0.25)
                    }))
                    .child(
                        div()
                            .flex_initial()
                            .min_w_0()
                            .truncate()
                            .text_color(hsla(fg).opacity(dim))
                            .child(device.name.clone()),
                    )
                    .child(div().flex_1().truncate().text_color(hsla(fg).opacity(0.5 * dim)).child(detail))
                    .children(
                        offline.map(|offline| div().flex_none().text_color(hsla(fg).opacity(0.45)).child(offline)),
                    )
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.select_device(id.clone(), cx)
                    }))
                    .on_a11y_press(view.clone(), move |this, _, cx| this.select_device(id.clone(), cx))
            });
            let list = div()
                .id("simulator-devices")
                .role(Role::ListBox)
                .aria_label(rust_i18n::t!("simulator.devices").into_owned())
                .flex_none()
                .max_h(px(200.))
                .pb(px(6.))
                .overflow_y_scroll()
                .children(rows);
            shell.child(list).child(div().flex_none().h(px(1.)).bg(hsla(fg).opacity(0.08)))
        } else {
            shell
        };
        let Some(device) = device else { return shell };
        let screen = self.render_device_screen(&device, width, fg, bg, window, cx);
        let shell = shell.child(screen);
        if !device.online() || self.simulator.frame.is_none() {
            return shell;
        }
        let controls = self.render_device_controls(fg, bg, window, cx);
        shell.child(controls)
    }

    fn toggle_picker(&mut self, cx: &mut Context<Self>) {
        self.simulator.picking = !self.simulator.picking;
        cx.notify();
    }

    /// 画面还出不来时画面的位置上居中说一句为什么，下面是能接着做的那一步（启动、装 agent、重连）。
    fn render_screen_status(&self, device: &Device, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Div> {
        let page = &self.simulator;
        let error = page.error.as_deref();
        let agent_missing = error.is_some_and(|err| err.contains("agent is not installed"));
        let (text, action): (String, Option<StatusAction>) = if let Some(busy) = &page.busy {
            (busy.clone(), None)
        } else if !device.online() {
            // 启动失败时写出原因，按钮留着再试。
            let text = error.map_or_else(|| rust_i18n::t!("simulator.not_running").into_owned(), str::to_owned);
            (text, Some(("simulator-boot", rust_i18n::t!("simulator.boot"), Self::boot_device)))
        } else if agent_missing && device.kind == "real" {
            // 真机要先拿开发者的描述文件签 agent（`--provisioning-profile`），那一步留给终端。
            (rust_i18n::t!("simulator.agent_missing_real", id = device.id).into_owned(), None)
        } else if agent_missing {
            let install = rust_i18n::t!("simulator.install_agent");
            (
                rust_i18n::t!("simulator.agent_missing").into_owned(),
                Some(("simulator-agent", install, Self::install_agent)),
            )
        } else if page.frame.is_some() {
            return None;
        } else if !page.loading_done() || page.stream.is_some() {
            (rust_i18n::t!("simulator.connecting").into_owned(), None)
        } else {
            let text = error.map_or_else(|| rust_i18n::t!("simulator.stream_ended").into_owned(), str::to_owned);
            (text, Some(("simulator-reconnect", rust_i18n::t!("simulator.reconnect"), Self::reconnect_device)))
        };
        let view = cx.entity().downgrade();
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .px(px(16.))
                .child(
                    div()
                        .id("simulator-status")
                        .role(Role::Label)
                        .aria_label(text.clone())
                        .w_full()
                        .text_center()
                        .text_color(hsla(fg).opacity(0.6))
                        .child(text),
                )
                .children(action.map(|(id, text, act)| {
                    text_button(id, text.into_owned(), fg, bg)
                        .on_click(cx.listener(move |this, _, _, cx| act(this, cx)))
                        .on_a11y_press(view.clone(), move |this, _, cx| act(this, cx))
                })),
        )
    }

    /// 画面按留给它的地方等比缩放；在上面按下、抬起变成点按、长按或滑动。
    fn render_device_screen(
        &self,
        device: &Device,
        width: f32,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = self.render_screen_status(device, fg, bg, cx);
        let page = &self.simulator;
        let room = page.room.clone();
        // 量出留给画面的地方：第一帧还没量过时先按面板宽、半个窗口高。
        let measure = canvas(
            move |bounds, _, _| room.set(Some((f32::from(bounds.size.width), f32::from(bounds.size.height)))),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let area =
            div().flex_1().min_h_0().relative().overflow_hidden().flex().justify_center().items_center().child(measure);
        let frame = page.frame.clone().filter(|_| device.online());
        let (Some(frame), Some(screen), None) = (frame, page.screen, &status) else {
            return area.children(status).into_any_element();
        };
        let (room_w, room_h) = page.room.get().unwrap_or((width, f32::from(window.viewport_size().height) / 2.));
        // 画面按留给它的地方等比缩放，下面还要放工具栏。
        let shape = device.screen_shape();
        let fit = ((room_w - SCREEN_PADDING * 2.) / screen.width)
            .min((room_h - SCREEN_PADDING * 2. - TOOLBAR_HEIGHT - TOOLBAR_GAP) / screen.height)
            .max(0.05);
        let elements = self.render_device_elements(fit, window, cx);
        let bounds = page.bounds.clone();
        let weak = cx.weak_entity();
        // 抬起时鼠标可能已经滑出画面，在整个窗口上接；按下才在画面上接。
        let events = canvas(
            move |frame_bounds, _, _| bounds.set(Some(frame_bounds)),
            move |_, _, window, _| {
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        weak.update(cx, |this, cx| this.release_screen(event.position, cx)).ok();
                    }
                });
            },
        )
        .absolute()
        .size_full();
        let (w, h) = (screen.width * fit, screen.height * fit);
        let radius = w * shape.radius;
        // 灵动岛照 iPhone 的比例画在屏幕顶上；模拟器推来的画面里没有它。
        let island = shape.island.then(|| {
            div()
                .absolute()
                .top(px(w * 0.027))
                .left(px(w * (1. - 0.31) / 2.))
                .w(px(w * 0.31))
                .h(px(w * 0.092))
                .rounded_full()
                .bg(hsla(ISLAND))
        });
        let screen_view = div()
            .id("simulator-screen")
            // 图片在 macOS 的辅助功能里是叶子节点，下面摆的设备元素报不出去；有元素时报成一组。
            .role(if elements.is_empty() { Role::Image } else { Role::Group })
            .aria_label(device.name.clone())
            .flex_none()
            .relative()
            .w(px(w))
            .h(px(h))
            .rounded(px(radius))
            .overflow_hidden()
            .cursor(CursorStyle::PointingHand)
            // 流断了时留着最后一帧，淡下去表示它不再动。
            .when(page.stream.is_none(), |screen| screen.opacity(0.5))
            // overflow_hidden 只按矩形裁，圆角要画在图片自己身上。
            .child(img(ImageSource::Render(frame)).absolute().size_full().rounded(px(radius)))
            .children(island)
            .children(elements)
            .child(events)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.simulator.press = Some(Press { at: event.position, since: Instant::now() });
                }),
            );
        area.p(px(SCREEN_PADDING))
            .flex_col()
            .gap(px(TOOLBAR_GAP))
            .child(screen_view)
            .child(self.render_device_toolbar(device, fg, bg, cx))
            .into_any_element()
    }

    /// 设备下面那条胶囊形的工具栏：主屏幕键、音量键和电源键，Android 另有返回键，iOS 模拟器没有音量键。
    fn render_device_toolbar(&self, device: &Device, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let view = cx.entity().downgrade();
        // mobilecli 在 iOS 上不认 POWER，锁屏键叫 LOCK；Android 那边只认 POWER。
        let power = if device.android() { "POWER" } else { "LOCK" };
        let mut keys = vec![
            ("HOME", HOME_BUTTON_ICON, rust_i18n::t!("simulator.home")),
            ("VOLUME_DOWN", VOLUME_DOWN_ICON, rust_i18n::t!("simulator.volume_down")),
            ("VOLUME_UP", VOLUME_UP_ICON, rust_i18n::t!("simulator.volume_up")),
            (power, POWER_ICON, rust_i18n::t!("simulator.power")),
        ];
        if device.android() {
            keys.insert(0, ("BACK", ARROW_LEFT_ICON, rust_i18n::t!("simulator.back")));
        } else if device.kind != "real" {
            // XCTest 的音量键只在真机上有用，iOS 模拟器上 mobilecli 回 ok 却什么都不做。
            keys.retain(|(button, ..)| !button.starts_with("VOLUME"));
        }
        let keys = keys.into_iter().map(|(button, icon, text)| {
            icon_toggle(button, icon, 14., false, fg, bg)
                .aria_label(text.clone().into_owned())
                .w(px(28.))
                .h(px(22.))
                .rounded_full()
                .tooltip(tooltip(text, None, fg, bg))
                .on_click(cx.listener(move |this, _, _, cx| this.press_button(button, cx)))
                .on_a11y_press(view.clone(), move |this, _, cx| this.press_button(button, cx))
        });
        div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(3.))
            .flex()
            .items_center()
            .gap(px(2.))
            .rounded_full()
            .border_1()
            .border_color(hsla(fg).opacity(0.12))
            .bg(hsla(bg.mix(fg, 0.05)))
            .children(keys)
    }

    /// 设备里的元素按位置摆在画面上，看不见、不接鼠标，只报给辅助工具；辅助工具没开时不摆。
    ///
    /// 另在画面四边各摆一个滑动按钮：GPUI 在 macOS 上不转辅助工具的滚动动作，滑动只能这样给。
    fn render_device_elements(&self, fit: f32, window: &Window, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        if !window.is_a11y_active() {
            return Vec::new();
        }
        let view = cx.entity().downgrade();
        let swipes = [
            ("swipe-up", rust_i18n::t!("simulator.swipe_up"), (0., -1.)),
            ("swipe-down", rust_i18n::t!("simulator.swipe_down"), (0., 1.)),
            ("swipe-left", rust_i18n::t!("simulator.swipe_left"), (-1., 0.)),
            ("swipe-right", rust_i18n::t!("simulator.swipe_right"), (1., 0.)),
        ]
        .into_iter()
        .map(|(id, label, direction): (&'static str, Cow<str>, (f32, f32))| {
            // 往哪边划就摆在出发的那条边上：向上滑在底边，向右滑在左边。
            let strip = div().id(id).role(Role::Button).aria_label(label.into_owned()).absolute();
            let strip = match direction {
                (_, -1.) => strip.bottom_0().left_0().w_full().h(px(12.)),
                (_, 1.) => strip.top_0().left_0().w_full().h(px(12.)),
                (-1., _) => strip.right_0().top_0().h_full().w(px(12.)),
                _ => strip.left_0().top_0().h_full().w(px(12.)),
            };
            strip.on_a11y_press(view.clone(), move |this, _, cx| this.swipe_screen(direction, cx))
        })
        .collect::<Vec<_>>();
        self.simulator
            .elements
            .iter()
            .enumerate()
            .map(|(ix, element)| {
                let role = match element.kind.as_str() {
                    "StaticText" => Role::Label,
                    "TextField" | "SecureTextField" | "SearchField" => Role::TextInput,
                    "Image" => Role::Image,
                    _ => Role::Button,
                };
                let (target, typed) = (element.clone(), element.clone());
                div()
                    .id(("device-element", ix))
                    .role(role)
                    .aria_label(element.label.clone())
                    .aria_value(SharedString::from(element.value.clone()))
                    .absolute()
                    .left(px(element.x * fit))
                    .top(px(element.y * fit))
                    .w(px(element.width * fit))
                    .h(px(element.height * fit))
                    .on_a11y_press(view.clone(), move |this, _, cx| this.tap_element(&target, cx))
                    // 辅助工具往设备的输入框里写字：先点它拿到焦点，再把字打进去。
                    .on_a11y_action(AccessibleAction::SetValue, {
                        let view = view.clone();
                        move |data, _, cx| {
                            let Some(ActionData::Value(text)) = data else { return };
                            let (text, typed) = (text.to_string(), typed.clone());
                            view.update(cx, |this, cx| this.type_into_element(&typed, text, cx)).ok();
                        }
                    })
            })
            .chain(swipes)
            .collect()
    }

    /// 面板底下：画面出来以后出的错，和打字的输入框（回车发出去）。
    fn render_device_controls(&mut self, fg: Rgb, bg: Rgb, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let field = match &self.simulator.text {
            Some((field, _)) => field.clone(),
            None => {
                let placeholder = rust_i18n::t!("simulator.type_text").into_owned();
                let field = cx.new(|cx| {
                    TextField::new(String::new(), cx).with_placeholder(placeholder.clone()).with_label(placeholder)
                });
                let events = cx.subscribe_in(&field, window, |this, _, event: &TextFieldEvent, _, cx| {
                    if let TextFieldEvent::Next = event {
                        this.send_device_text(cx);
                    }
                });
                self.simulator.text = Some((field.clone(), events));
                field
            }
        };
        let input = div()
            .h(px(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .border_1()
            .border_color(hsla(fg).opacity(0.12))
            .bg(hsla(bg.mix(fg, 0.06)))
            .child(div().flex_1().min_w_0().h_full().text_color(hsla(fg)).child(field));
        div()
            .flex_none()
            .px(px(10.))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            // 画面出来以后点按、打字这些出的错，写在输入框上面一行。
            .children(self.simulator.error.clone().map(|err| {
                div()
                    .id("simulator-error")
                    .role(Role::Label)
                    .aria_label(err.clone())
                    .text_color(hsla(fg).opacity(0.6))
                    .truncate()
                    .child(err)
            }))
            .child(input)
    }
}

impl SimulatorPage {
    /// 没有在列设备、问屏幕大小或拉起画面：这时还没画面才值得给「重连」。
    fn loading_done(&self) -> bool {
        self.loading.as_ref().is_none_or(|task| task.is_ready())
    }
}

/// 设备列表里名字后面那一句：平台和是模拟器还是真机。
fn device_detail(device: &Device) -> String {
    let kind = match device.kind.as_str() {
        "real" => rust_i18n::t!("simulator.real"),
        _ => rust_i18n::t!("simulator.virtual"),
    };
    format!("{} · {kind}", if device.android() { "Android" } else { "iOS" })
}

/// 设备列表的顺序：开着的在前，同样开着的模拟器排在真机前面，其余照 mobilecli 给的顺序。
fn sorted(mut devices: Vec<Device>) -> Vec<Device> {
    devices.sort_by_key(|device| (!device.online(), device.kind == "real"));
    devices
}

/// 打开页时先选哪台：开着的模拟器优先，真机的画面要先签好 agent 才出得来。
fn default_device(devices: &[Device]) -> Option<&Device> {
    let online = || devices.iter().filter(|device| device.online());
    online().find(|device| device.kind != "real").or_else(|| online().next())
}

/// 面板里的文字按钮。
fn text_button(id: &'static str, text: String, fg: Rgb, bg: Rgb) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(text.clone())
        .h(px(22.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(hsla(fg).opacity(0.12))
        .text_color(hsla(fg).opacity(0.8))
        .hover(|button| button.bg(hsla(bg.mix(fg, 0.10))))
        .child(text)
}

/// 找不到 mobilecli 时 `mobilecli` 返回的错，页上据此显示怎么装。
const MISSING: &str = "mobilecli not found";

/// PATH 里哪个目录有 mobilecli。
fn on_path(path: &OsString) -> bool {
    std::env::split_paths(path).any(|dir| dir.join(PROGRAM).is_file())
}

/// 跑一条 mobilecli 命令，交回它 JSON 里的 `data`；出错时交回它说的原因。
fn mobilecli(path: Option<&std::ffi::OsStr>, args: &[&str]) -> Result<Value, String> {
    let output = command(path).args(args).stdin(Stdio::null()).output().map_err(spawn_error)?;
    reply(&output.stdout, &output.stderr)
}

fn command(path: Option<&std::ffi::OsStr>) -> Command {
    let mut command = Command::new(PROGRAM);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
}

fn spawn_error(err: io::Error) -> String {
    match err.kind() {
        io::ErrorKind::NotFound => MISSING.to_owned(),
        _ => format!("{PROGRAM}: {err}"),
    }
}

/// mobilecli 在 stdout 写 `{"status":"ok","data":…}` 或 `{"status":"error","error":"…"}`，读不出时用 stderr。
fn reply(stdout: &[u8], stderr: &[u8]) -> Result<Value, String> {
    let json: Option<Value> = serde_json::from_slice(stdout).ok();
    match json {
        Some(mut json) if json["status"] == "ok" => Ok(json["data"].take()),
        Some(json) if json["error"].is_string() => Err(json["error"].as_str().unwrap_or_default().to_owned()),
        _ => {
            let text = String::from_utf8_lossy(stderr);
            let line = text.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("no output");
            Err(format!("{PROGRAM}: {line}"))
        }
    }
}

fn parse_devices(data: &Value) -> Result<Vec<Device>, String> {
    Vec::<Device>::deserialize(&data["devices"]).map_err(|err| format!("{PROGRAM} devices: {err}"))
}

fn parse_screen(data: &Value) -> Result<ScreenSize, String> {
    let screen = ScreenSize::deserialize(&data["device"]["screenSize"])
        .map_err(|err| format!("{PROGRAM} device info: {err}"))?;
    if screen.width > 0. && screen.height > 0. {
        Ok(screen)
    } else {
        Err(format!("{PROGRAM} device info: empty screen size"))
    }
}

/// `dump ui` 的元素树摊平，只留有名字（label、占位文字或值）、有大小的：没名字的容器辅助工具读了也不知道是什么。
fn parse_elements(data: &Value) -> Vec<DeviceElement> {
    fn walk(nodes: &Value, out: &mut Vec<DeviceElement>) {
        for node in nodes.as_array().into_iter().flatten() {
            let text = |key: &str| node[key].as_str().map(str::trim).filter(|text| !text.is_empty());
            let rect = |key: &str| node["rect"][key].as_f64().unwrap_or(0.) as f32;
            let (width, height) = (rect("width"), rect("height"));
            // 空着的输入框没有 label，只有占位文字。
            if let Some(label) = text("label").or_else(|| text("placeholder")).or_else(|| text("value"))
                && width > 0.
                && height > 0.
            {
                out.push(DeviceElement {
                    label: label.to_owned(),
                    value: text("value").unwrap_or_default().to_owned(),
                    kind: text("type").unwrap_or_default().to_owned(),
                    x: rect("x"),
                    y: rect("y"),
                    width,
                    height,
                });
            }
            walk(&node["children"], out);
        }
    }
    let mut out = Vec::new();
    walk(&data["elements"], &mut out);
    out
}

type Frames = futures::channel::mpsc::UnboundedReceiver<Result<Arc<RenderImage>, String>>;

/// 拉起推 MJPEG 画面的 mobilecli，另起线程读帧、解码，解好的帧从交回的通道里出来；进程退出时通道关掉。
fn start_stream(path: Option<&std::ffi::OsStr>, id: &str, svg: gpui::SvgRenderer) -> Result<(Child, Frames), String> {
    let mut child = command(path)
        // 按设备的原始分辨率传：Retina 屏上面板画出来的像素比半分辨率多，缩小了会糊；一帧解码只要几毫秒。
        .args(["screencapture", "--device", id, "--format", "mjpeg"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // 只写一行「Starting video stream」，出错的原因在 stdout 的 JSON 里。
        .stderr(Stdio::null())
        // 自成一个进程组，`Stream` 丢掉时连 node 包装脚本拉起的二进制一起杀。
        .process_group(0)
        .spawn()
        .map_err(spawn_error)?;
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = futures::channel::mpsc::unbounded();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let frame = match next_frame(&mut reader) {
                Ok(Part::Frame(bytes)) => gpui::Image::from_bytes(gpui::ImageFormat::Jpeg, bytes)
                    .to_image_data(svg.clone())
                    .map_err(|err| format!("{PROGRAM} screencapture: {err}")),
                Ok(Part::Reply(text)) => Err(reply(text.as_bytes(), b"").err().unwrap_or_default()),
                Ok(Part::End) | Err(_) => return,
            };
            if tx.unbounded_send(frame).is_err() {
                return;
            }
        }
    });
    Ok((child, rx))
}

#[derive(Debug, PartialEq)]
enum Part {
    /// 一帧 JPEG。
    Frame(Vec<u8>),
    /// 推不了画面时 mobilecli 在 stdout 写的 JSON 回话。
    Reply(String),
    End,
}

/// 从 multipart 的 MJPEG 流里读下一帧：跳过分隔行，按 `Content-Length` 读出正文，不去 JPEG 里找结束标记
/// （带 EXIF 的帧里可能也有）。还没出帧时读到 `{` 开头的行，是 mobilecli 的 JSON 回话，读完整段交回。
fn next_frame(reader: &mut impl BufRead) -> io::Result<Part> {
    let mut length = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(Part::End);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.starts_with('{') && length.is_none() {
            let mut rest = String::new();
            reader.read_to_string(&mut rest)?;
            return Ok(Part::Reply(format!("{text}\n{rest}")));
        }
        if let Some((name, value)) = text.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok().filter(|&len| len <= MAX_FRAME_BYTES);
            if length.is_none() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"));
            }
        } else if text.is_empty()
            && let Some(len) = length
        {
            let mut frame = vec![0; len];
            reader.read_exact(&mut frame)?;
            return Ok(Part::Frame(frame));
        }
    }
}

/// 按下到抬起：`moved` 是在面板上挪了多远，起止点已经换算成设备的坐标。
fn gesture(from: (f32, f32), to: (f32, f32), moved: f32, held: Duration) -> Gesture {
    let point = |(x, y): (f32, f32)| (x.round() as i64, y.round() as i64);
    let (x1, y1) = point(from);
    let ms = held.as_millis() as u64;
    if moved >= TAP_SLOP {
        let (x2, y2) = point(to);
        Gesture::Swipe(x1, y1, x2, y2, ms.max(100))
    } else if held >= LONG_PRESS {
        Gesture::LongPress(x1, y1, ms)
    } else {
        Gesture::Tap(x1, y1)
    }
}

fn gesture_args(gesture: &Gesture) -> Vec<String> {
    let io = |verb: &str, rest: Vec<String>| [vec!["io".to_owned(), verb.to_owned()], rest].concat();
    match *gesture {
        Gesture::Tap(x, y) => io("tap", vec![format!("{x},{y}")]),
        Gesture::LongPress(x, y, ms) => io("longpress", vec![format!("{x},{y}"), "--duration".into(), ms.to_string()]),
        Gesture::Swipe(x1, y1, x2, y2, ms) => {
            io("swipe", vec![format!("{x1},{y1},{x2},{y2}"), "--duration".into(), ms.to_string()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multipart(frames: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for frame in frames {
            out.extend_from_slice(b"--mjpeg-frame-boundary\r\nContent-Type: image/jpeg\r\n");
            out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", frame.len()).as_bytes());
            out.extend_from_slice(frame);
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    #[test]
    fn reads_frames_by_content_length() {
        // 第二帧里夹着 JPEG 的结束标记和换行，按长度读不受影响。
        let first: &[u8] = b"\xff\xd8abc\xff\xd9";
        let second: &[u8] = b"\xff\xd8x\xff\xd9\r\n\r\ny\xff\xd9";
        let bytes = multipart(&[first, second]);
        let mut reader = BufReader::with_capacity(4, bytes.as_slice());
        assert_eq!(next_frame(&mut reader).unwrap(), Part::Frame(first.to_vec()));
        assert_eq!(next_frame(&mut reader).unwrap(), Part::Frame(second.to_vec()));
        assert_eq!(next_frame(&mut reader).unwrap(), Part::End);
    }

    #[test]
    fn json_before_any_frame_is_a_reply() {
        let bytes = b"{\n  \"status\": \"error\",\n  \"error\": \"agent is not installed\"\n}\n";
        let mut reader = BufReader::new(bytes.as_slice());
        let Part::Reply(text) = next_frame(&mut reader).unwrap() else { panic!("not a reply") };
        assert_eq!(reply(text.as_bytes(), b""), Err("agent is not installed".to_owned()));
    }

    #[test]
    fn oversized_frame_is_an_error() {
        let bytes = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
        assert!(next_frame(&mut BufReader::new(bytes.as_bytes())).is_err());
    }

    #[test]
    fn parses_devices_and_screen_size() {
        let devices = reply(
            br#"{"status":"ok","data":{"devices":[{"id":"CBC9","name":"iPhone 18 Pro","platform":"ios","type":"simulator","version":"27.0","state":"online","model":"x"},{"id":"emulator-5554","name":"Pixel","platform":"android","type":"emulator","version":"15","state":"offline"}]}}"#,
            b"",
        )
        .and_then(|data| parse_devices(&data))
        .unwrap();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].online() && !devices[0].android());
        assert!(!devices[1].online() && devices[1].android());
        let info = reply(
            br#"{"status":"ok","data":{"device":{"id":"CBC9","screenSize":{"width":402,"height":874,"scale":3}}}}"#,
            b"",
        )
        .unwrap();
        assert_eq!(parse_screen(&info), Ok(ScreenSize { width: 402., height: 874. }));
    }

    #[test]
    fn opens_a_running_simulator_before_a_real_device() {
        let device = |id: &str, kind: &str, state: &str| Device {
            id: id.into(),
            name: id.into(),
            platform: "ios".into(),
            kind: kind.into(),
            state: state.into(),
            model: String::new(),
        };
        let pick = |devices: &[Device]| default_device(devices).map(|device| device.id.clone());
        let phone = device("phone", "real", "online");
        let off = device("off", "simulator", "offline");
        let sim = device("sim", "simulator", "online");
        assert_eq!(pick(&[phone.clone(), off.clone(), sim]), Some("sim".into()));
        assert_eq!(pick(&[off.clone(), phone]), Some("phone".into()));
        assert_eq!(pick(std::slice::from_ref(&off)), None);
        let order: Vec<_> = sorted(vec![off, device("phone", "real", "online"), device("sim", "simulator", "online")])
            .into_iter()
            .map(|device| device.id)
            .collect();
        assert_eq!(order, ["sim", "phone", "off"]);
    }

    #[test]
    fn flattens_named_device_elements() {
        let data = serde_json::json!({"elements": [
            {"type": "Other", "name": "Identifier:SectionHeader", "rect": {"x": 20, "y": 62, "width": 400, "height": 38},
             "children": [{"type": "StaticText", "label": "建议", "rect": {"x": 20, "y": 74, "width": 30, "height": 18}}]},
            {"type": "Cell", "label": "设置", "rect": {"x": 0, "y": 100, "width": 440, "height": 56}},
            {"type": "TextField", "label": " ", "value": "中文", "rect": {"x": 10, "y": 600, "width": 300, "height": 40}},
            {"type": "TextField", "placeholder": "搜索", "rect": {"x": 35, "y": 871, "width": 370, "height": 48}},
            {"type": "Button", "label": "藏起来的", "rect": {"x": 0, "y": 0, "width": 0, "height": 0}}
        ]});
        let elements = parse_elements(&data);
        let labels: Vec<_> = elements.iter().map(|element| element.label.as_str()).collect();
        assert_eq!(labels, ["建议", "设置", "中文", "搜索"]);
        assert_eq!(elements[2].value, "中文");
        assert_eq!(elements[1].center(), (220., 128.));
    }

    #[test]
    fn screen_shape_follows_the_model() {
        let device = |platform: &str, name: &str, model: &str| Device {
            id: "x".into(),
            name: name.into(),
            platform: platform.into(),
            kind: "simulator".into(),
            state: "online".into(),
            model: model.into(),
        };
        let island = |model: &str| device("ios", "iPhone", model).screen_shape().island;
        assert!(island("com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro"));
        assert!(island("com.apple.CoreSimulator.SimDeviceType.iPhone-15"));
        assert!(island("com.apple.CoreSimulator.SimDeviceType.iPhone-14-Pro-Max"));
        assert!(!island("com.apple.CoreSimulator.SimDeviceType.iPhone-14"));
        assert!(!island("com.apple.CoreSimulator.SimDeviceType.iPhone-16e"));
        assert!(!island("iPhone14,5"));
        let ipad =
            device("ios", "iPad Pro 13-inch (M5)", "com.apple.CoreSimulator.SimDeviceType.iPad-Pro-13-inch-M5-12GB");
        assert_eq!(ipad.screen_shape(), ScreenShape { radius: 0.03, island: false });
        assert!(!device("android", "Pixel", "").screen_shape().island);
    }

    #[test]
    fn unreadable_reply_uses_the_last_stderr_line() {
        assert_eq!(reply(b"", b"first\nfailed to start\n\n"), Err("mobilecli: failed to start".to_owned()));
    }

    #[test]
    fn gestures_split_by_movement_and_duration() {
        let quick = Duration::from_millis(80);
        let long = Duration::from_millis(700);
        assert_eq!(gesture((10.4, 20.6), (11., 21.), 2., quick), Gesture::Tap(10, 21));
        assert_eq!(gesture((10., 20.), (10., 20.), 0., long), Gesture::LongPress(10, 20, 700));
        assert_eq!(gesture((10., 20.), (10., 300.), 90., quick), Gesture::Swipe(10, 20, 10, 300, 100));
        assert_eq!(gesture_args(&Gesture::Tap(1, 2)), ["io", "tap", "1,2"]);
        assert_eq!(gesture_args(&Gesture::Swipe(1, 2, 3, 4, 250)), ["io", "swipe", "1,2,3,4", "--duration", "250"]);
    }
}
