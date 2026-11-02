//! 把远程桌面页（`screen_viewer::ScreenViewer`）接到窗口上。从菜单的「远程桌面」或标题栏右上角的远程桌面按钮
//! 打开当前 workspace 所在的 SSH 主机，或从设置页的 SSH 主机列表打开那一台（`show_remote_desktop`）。
//!
//! 一个窗口里可以同时开几台主机（`ScreenViewers`），每台一种摆法：占满内容区时盖住标签和分屏、留着侧栏，切到
//! 任何一个标签或 workspace、打开别的整页时收起，同一时刻最多一台占满；浮动时是浮在窗口内容上面的一块，拖标题
//! 移动、拖四边和四角改大小，点哪块哪块到最上面，底下的终端照常看得见、点得到。浮动的拖到右侧面板那一栏上
//! 松开，就吸附在面板的上方或下方（`snap`），一栏里可以吸附好几台；拖它的标题又浮起来。吸附着的只在右侧
//! 面板显示、又是吸附时的 workspace 时看得见，钉住后换成那台主机上的各个 workspace，切到那台主机上面板收着的
//! workspace 时打开面板；切到别的页面、别的 workspace，收起面板时跟着藏起来，连接不断。浮动的拖到窗口外面
//! 松开，或按页面上的按钮，就搬进单独的窗口（`popout`），连接不断；在那里按「放回主窗口」再搬回来。只有点页面上的
//! 关闭（或关掉单独的窗口）才收起，收起就断开。

mod popout;
mod snap;

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, CursorStyle, Div, Entity, EntityId, Focusable as _, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ResizeEdge, Size, StyleRefinement, Subscription, Task,
    Window, div, point, prelude::*, px, size,
};

use super::{
    ToggleRemoteDesktop, WindowView,
    project::{TOGGLE_HEIGHT, TOGGLE_WIDTH},
};
use crate::host_client::hosts::HostId;
use crate::screen_viewer::{Input, SUPPORTED, ScreenViewer, ScreenViewerEvent};
use crate::ui::icons::MONITOR_ICON;
use crate::ui::{a11y::PressDown as _, button::Toggle, theme::ActiveTheme as _, tooltip::tooltip};
use snap::{ShownSnap, Snap, SnapTarget};

/// 浮动的远程桌面最小这么大。
const MIN_FLOAT: Size<Pixels> = Size { width: px(360.), height: px(240.) };
/// 浮动的远程桌面四角拖着改大小的那块的边长。
const RESIZE_GRIP: f32 = 14.;
/// 浮动的远程桌面四边拖着改大小的那条的宽度。
const RESIZE_EDGE: f32 = 6.;
/// 再开一块浮动的远程桌面时比上一块往右下错开这么多。
const CASCADE: f32 = 32.;
/// 搬进单独的窗口时，原来占满内容区的那块用这个大小。
const POPOUT_SIZE: Size<Pixels> = Size { width: px(960.), height: px(620.) };
/// 拖吸附着的远程桌面的标题，鼠标挪过这么远才浮起来，只是点一下标题不动它。
const UNSNAP_DISTANCE: f32 = 4.;
/// 从吸附处浮起来时，鼠标落在浮动那块标题上离顶边这么远。
const UNSNAP_GRAB_Y: f32 = 16.;

/// 远程桌面在窗口里怎么摆。
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// 占满内容区。
    Docked,
    /// 浮在窗口内容上面。
    Floating,
    /// 吸附在右侧面板上；这一帧没画右侧面板，或者当前 workspace 不该显示它时藏着。
    Snapped(Snap),
}

/// 窗口里开着的一台主机的远程桌面。
pub(super) struct OpenScreenViewer {
    pub(super) view: Entity<ScreenViewer>,
    placement: Placement,
    /// 浮动时在窗口里的位置和大小；吸附着时记着上回浮动的，浮起来时用它的大小。
    frame: Bounds<Pixels>,
    /// 吸附着时上一帧画在窗口里的位置，prepaint 时记下。
    snap_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 吸附着而藏起来了：过 `SUSPEND_DELAY` 还藏着就断开连接。
    suspend_timer: Option<Task<()>>,
    _events: Subscription,
}

impl OpenScreenViewer {
    /// 不占满内容区：浮着或吸附着，页面上的样子一样。
    fn floating(&self) -> bool {
        self.placement != Placement::Docked
    }

    /// 交给页面的 `Input`：吸附着时给钉住的按钮。
    fn input(&self) -> Input {
        let pinned = match self.placement {
            Placement::Snapped(snap) => Some(snap.pinned),
            _ => None,
        };
        Input { floating: self.floating(), titlebar: None, pinned }
    }
}

/// 窗口里开着的远程桌面，从下到上排，最上面的在最后。
#[derive(Default)]
pub(super) struct ScreenViewers {
    open: Vec<OpenScreenViewer>,
    /// 上回选的摆法：窗口里没有别的远程桌面时照它开，已经有了时新开的都浮动。
    prefer_floating: bool,
    /// 上回浮动的位置和大小，窗口里没有别的浮动的时新开的照它摆。
    last_frame: Option<Bounds<Pixels>>,
    drag: Option<FloatDrag>,
    /// 吸附着的远程桌面在右侧那一栏里从上到下的先后；不再吸附着的由用到的地方滤掉。
    column: Vec<EntityId>,
    /// 这一帧画了右侧面板那一栏（`render_right_column`），吸附到那里的落点就在它上面。
    column_shown: bool,
    /// 这一帧画进右侧那一栏的几台，从上到下。
    shown_snaps: Vec<ShownSnap>,
    /// 右侧面板那一栏上一帧在窗口里的位置，prepaint 时记下。
    column_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// 正在拖动浮动的远程桌面 `id`：从窗口里的 `from` 按下，那时它在 `frame`；`resize` 有值时拖的是那条边或那个角。
struct FloatDrag {
    id: EntityId,
    resize: Option<ResizeEdge>,
    from: Point<Pixels>,
    frame: Bounds<Pixels>,
    /// 拖的是吸附着的那台，鼠标挪过 `UNSNAP_DISTANCE` 才浮起来。
    unsnap: bool,
    /// 拖起来之前它吸附着的样子，再吸附上时照旧钉没钉住、照旧那么高。
    was: Option<Snap>,
    /// 松开就插到右侧那一栏的这里。
    target: Option<SnapTarget>,
}

impl ScreenViewers {
    pub(super) fn views(&self) -> impl Iterator<Item = &Entity<ScreenViewer>> {
        self.open.iter().map(|open| &open.view)
    }

    fn position(&self, id: EntityId) -> Option<usize> {
        self.open.iter().position(|open| open.view.entity_id() == id)
    }

    fn docked(&self) -> Option<&OpenScreenViewer> {
        self.open.iter().find(|open| open.placement == Placement::Docked)
    }

    /// 吸附着的远程桌面能画进去的右侧面板那一栏在窗口里的位置；这一帧没画那一栏时为空。
    fn snap_column(&self) -> Option<Bounds<Pixels>> {
        self.column_shown.then(|| self.column_bounds.get()).flatten()
    }

    /// 新开一块浮动的放在哪：窗口里没有浮动的时照上回的，有了就比最上面那块往右下错开。
    fn next_frame(&self, viewport: Size<Pixels>) -> Bounds<Pixels> {
        let top = self.open.iter().rev().find(|open| open.placement == Placement::Floating).map(|open| open.frame);
        let frame = match top {
            Some(top) => Bounds { origin: top.origin + point(px(CASCADE), px(CASCADE)), size: top.size },
            None => self.last_frame.unwrap_or_else(|| default_frame(viewport)),
        };
        clamp_frame(frame, viewport)
    }
}

/// 在最前面的窗口里打开主机 `host` 的远程桌面；一个窗口都没有时先开一个。等这一轮派发结束再开：点按钮时
/// GPUI 正借着那个窗口派发事件，当场 `update` 它会失败。
pub(crate) fn show_remote_desktop(host: HostId, cx: &mut App) {
    cx.defer(move |cx| {
        with_front_window(cx, |view, window, cx| {
            view.close_settings(window, cx);
            view.show_screen_viewer(host, window, cx);
        });
    });
}

/// 把远程桌面页 `viewer`（从单独的窗口里搬回来的）放进最前面的窗口；一个窗口都没有时先开一个。
fn put_back(viewer: Entity<ScreenViewer>, cx: &mut App) {
    with_front_window(cx, |view, window, cx| view.adopt_screen_viewer(viewer, window, cx));
}

fn with_front_window(cx: &mut App, f: impl FnOnce(&mut WindowView, &mut Window, &mut Context<WindowView>)) {
    if super::remote::windows(cx).is_empty() {
        super::open_window(cx, None);
    }
    let Some(handle) = super::remote::front_window(cx) else { return };
    if let Err(err) = handle.update(cx, |view, window, cx| {
        window.activate_window();
        f(view, window, cx);
    }) {
        tracing::warn!("failed to open the remote desktop: {err:#}");
    }
}

impl WindowView {
    /// 打开或收起当前 workspace 所在主机的远程桌面；workspace 在本机或那台主机看不了远程桌面时不理。
    pub(super) fn toggle_remote_desktop(
        &mut self,
        _: &ToggleRemoteDesktop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_desktop_button_shown(cx) {
            return;
        }
        let host = self.workspace().host.clone();
        match self.screen_of(&host, cx) {
            Some(id) if !self.screen_hidden(id) => self.close_screen_viewer(id, window, cx),
            _ => self.show_screen_viewer(host, window, cx),
        }
    }

    /// 主机 `host` 的远程桌面在这个窗口里开着时它的编号。
    fn screen_of(&self, host: &HostId, cx: &App) -> Option<EntityId> {
        self.screen_viewers.views().find(|view| view.read(cx).host() == host).map(|view| view.entity_id())
    }

    /// 标题栏右上角有远程桌面按钮：当前 workspace 在 SSH 主机上，那台主机能采集画面，本机也能解远程桌面的画面。
    pub(super) fn remote_desktop_button_shown(&self, cx: &App) -> bool {
        SUPPORTED && crate::remote_hosts::screen_capturable(&self.workspace().host, cx)
    }

    /// 标题栏右上角、命令按钮左边的远程桌面按钮，打开当前 workspace 所在主机的远程桌面；它开着、看得见时按下着。
    pub(super) fn render_remote_desktop_button(&self, cx: &mut Context<Self>) -> Toggle {
        let text = rust_i18n::t!("tooltip.remote_desktop");
        let host = &self.workspace().host;
        let open = self.screen_of(host, cx).is_some_and(|id| !self.screen_hidden(id));
        Toggle::new("remote-desktop", MONITOR_ICON, text.clone().into_owned())
            .pressed(open)
            .w(px(TOGGLE_WIDTH))
            .h(px(TOGGLE_HEIGHT))
            .tooltip(tooltip(text, Some(&ToggleRemoteDesktop)))
            .on_press_down(cx, |this, window, cx| this.toggle_remote_desktop(&ToggleRemoteDesktop, window, cx))
    }

    /// 打开主机 `host` 的远程桌面，焦点给它；已经开着时把它提到最上面。吸附着而藏着的，有一台占满内容区、或者
    /// 钉在别的主机上时改成浮动，否则归到当前 workspace（钉住的不用）、打开右侧面板露出它。
    pub(super) fn show_screen_viewer(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reveal_content(window, cx) {
            return;
        }
        let existing = self.screen_viewers.views().find(|view| *view.read(cx).host() == host).cloned();
        match existing {
            Some(view) => {
                let id = view.entity_id();
                if self.screen_hidden(id)
                    && let Some(ix) = self.screen_viewers.position(id)
                {
                    let docked = self.screen_viewers.docked().is_some();
                    if !docked {
                        self.claim_snap(id);
                    }
                    let open = &self.screen_viewers.open[ix];
                    let belongs = match open.placement {
                        Placement::Snapped(snap) => self.snap_belongs_here(open, snap, cx),
                        _ => true,
                    };
                    let float = docked || !belongs;
                    if float {
                        self.screen_viewers.open[ix].placement = Placement::Floating;
                    }
                    let open = &self.screen_viewers.open[ix];
                    set_input(&open.view, open.input(), cx);
                    if !float && self.workspace().panel.is_none() {
                        self.set_panel(Some(self.workspace().last_panel), window, cx);
                    }
                    self.save(cx);
                }
                self.raise_screen(id);
                window.focus(&view.focus_handle(cx), cx);
                cx.notify();
            }
            // 搬进了单独的窗口就去那里，不再开一份连同一台。
            None if popout::activate(&host, cx) => {}
            None => {
                let view = cx.new(|cx| ScreenViewer::new(host, cx));
                self.adopt_screen_viewer(view, window, cx);
            }
        }
    }

    /// 把远程桌面页 `view` 放进这个窗口，焦点给它：窗口里没有别的远程桌面时照上回选的摆法，有了就浮动。同一台
    /// 主机已经开着时换下旧的。
    fn adopt_screen_viewer(&mut self, view: Entity<ScreenViewer>, window: &mut Window, cx: &mut Context<Self>) {
        let host = view.read(cx).host().clone();
        self.drop_screens(window, cx, |open, cx| *open.view.read(cx).host() == host);
        // 占满时要看得见：盖着的页面收不起来（看板里有对话框开着）就浮着放。
        let floating = self.screen_viewers.prefer_floating
            || !self.screen_viewers.open.is_empty()
            || !self.reveal_content(window, cx);
        let mut open = self.new_screen(view, window, cx);
        if !floating {
            open.placement = Placement::Docked;
        }
        set_input(&open.view, open.input(), cx);
        window.focus(&open.view.focus_handle(cx), cx);
        self.screen_viewers.open.push(open);
        cx.notify();
    }

    /// 窗口里新开的一台，浮动着放在下一个位置，接住它要窗口办的事；由调用方放进 `ScreenViewers::open`。
    fn new_screen(
        &mut self,
        view: Entity<ScreenViewer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> OpenScreenViewer {
        let frame = self.screen_viewers.next_frame(window.viewport_size());
        let events = self.screen_events(&view, window, cx);
        OpenScreenViewer {
            view,
            placement: Placement::Floating,
            frame,
            snap_bounds: Rc::default(),
            suspend_timer: None,
            _events: events,
        }
    }

    /// 接住远程桌面页 `view` 要窗口办的事。
    fn screen_events(
        &mut self,
        view: &Entity<ScreenViewer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(view, window, |this, view, event: &ScreenViewerEvent, window, cx| {
            let id = view.entity_id();
            match event {
                ScreenViewerEvent::Close => this.close_screen_viewer(id, window, cx),
                ScreenViewerEvent::ToggleFloating => this.toggle_screen_floating(id, window, cx),
                ScreenViewerEvent::StartDrag(from) => this.start_screen_drag(id, *from, None, cx),
                ScreenViewerEvent::PopOut => this.pop_out_screen(id, None, window, cx),
                ScreenViewerEvent::PopIn => {}
                ScreenViewerEvent::TogglePin => this.toggle_screen_pin(id, cx),
            }
        })
    }

    pub(super) fn close_screen_viewer(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.screen_viewers.position(id) else { return };
        let open = self.screen_viewers.open.remove(ix);
        open.view.update(cx, |viewer, _| viewer.leave_window(window));
        if self.screen_viewers.drag.as_ref().is_some_and(|drag| drag.id == id) {
            self.screen_viewers.drag = None;
        }
        window.focus(&self.focus_handle(cx), cx);
        self.save(cx);
        cx.notify();
    }

    /// 关分屏的动作落到远程桌面上时收起它：焦点所在的那台，焦点不在哪台上时是占满内容区的那台；都没有时返回假。
    pub(super) fn close_screen_for_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let focused = self.screen_viewers.views().find(|view| view.read(cx).focused(window, cx));
        let Some(id) =
            focused.map(Entity::entity_id).or_else(|| self.screen_viewers.docked().map(|open| open.view.entity_id()))
        else {
            return false;
        };
        self.close_screen_viewer(id, window, cx);
        true
    }

    /// 有一台远程桌面占满内容区。
    pub(super) fn screen_docked(&self) -> bool {
        self.screen_viewers.docked().is_some()
    }

    /// 占满内容区的那台远程桌面的焦点。
    pub(super) fn docked_screen_focus(&self, cx: &App) -> Option<gpui::FocusHandle> {
        self.screen_viewers.docked().map(|open| open.view.focus_handle(cx))
    }

    /// 要露出标签和分屏（切了标签、开了别的整页）：占满内容区的远程桌面收起，浮着的留着。
    pub(super) fn leave_screen_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_screens(window, cx, |open, _| !open.floating());
    }

    /// 拿掉 `gone` 选中的远程桌面，先让它们离开这个窗口（`ScreenViewer::leave_window`）。
    fn drop_screens(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        gone: impl Fn(&OpenScreenViewer, &App) -> bool,
    ) {
        let (dropped, kept) =
            std::mem::take(&mut self.screen_viewers.open).into_iter().partition(|open| gone(open, cx));
        self.screen_viewers.open = kept;
        if dropped.iter().any(|open| matches!(open.placement, Placement::Snapped(_))) {
            self.save(cx);
        }
        for open in dropped {
            open.view.update(cx, |viewer, _| viewer.leave_window(window));
        }
    }

    /// 收起盖在内容区上的页面（设置、手机、看板），占满的远程桌面才看得见；看板里有对话框开着时不收，返回假。
    fn reveal_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.leave_todo_page(window, cx) {
            return false;
        }
        self.drop_settings(cx);
        self.leave_mobile(cx);
        self.drop_kanban();
        true
    }

    fn raise_screen(&mut self, id: EntityId) {
        if let Some(ix) = self.screen_viewers.position(id)
            && ix + 1 != self.screen_viewers.open.len()
        {
            let open = self.screen_viewers.open.remove(ix);
            self.screen_viewers.open.push(open);
        }
    }

    /// 远程桌面 `id` 在浮动（吸附着的也算）和占满内容区之间切换，焦点给它；换成占满时原来占满的那台改成浮动。
    fn toggle_screen_floating(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.screen_viewers.position(id) else { return };
        let floating = !self.screen_viewers.open[ix].floating();
        // 占满时盖着的页面要先收起来，不然它看不见却拿着焦点，敲的字悄悄发给了远端。
        if !floating && !self.reveal_content(window, cx) {
            return;
        }
        self.screen_viewers.drag = None;
        self.screen_viewers.prefer_floating = floating;
        let viewport = window.viewport_size();
        if !floating && let Some(other) = self.screen_viewers.open.iter().position(|open| !open.floating()) {
            let frame = self.screen_viewers.next_frame(viewport);
            let other = &mut self.screen_viewers.open[other];
            other.placement = Placement::Floating;
            other.frame = frame;
            set_input(&other.view, other.input(), cx);
        }
        let open = &mut self.screen_viewers.open[ix];
        open.placement = if floating { Placement::Floating } else { Placement::Docked };
        set_input(&open.view, open.input(), cx);
        window.focus(&open.view.focus_handle(cx), cx);
        self.raise_screen(id);
        self.save(cx);
        cx.notify();
    }

    /// 把远程桌面 `id` 搬进单独的窗口：给了 `origin`（相对这个窗口所在显示器的位置）时窗口尽量放在那里，否则放在
    /// 它原来的地方。
    fn pop_out_screen(
        &mut self,
        id: EntityId,
        origin: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.screen_viewers.position(id) else { return };
        let open = self.screen_viewers.open.remove(ix);
        open.view.update(cx, |viewer, _| viewer.leave_window(window));
        self.screen_viewers.drag = None;
        let screen = window.bounds().origin;
        let bounds = if open.floating() {
            Bounds { origin: origin.unwrap_or(screen + open.frame.origin), size: open.frame.size }
        } else {
            let origin = origin.unwrap_or(screen + point(px(CASCADE), px(CASCADE)));
            Bounds { origin, size: POPOUT_SIZE }
        };
        // 放在这个窗口所在的那块显示器上、整块都在它里面：GPUI 给的各块显示器的范围都从原点算，看不出它们怎么
        // 排着，挪到别的显示器上交给系统的标题栏。
        let display = window.display(cx);
        let bounds = match &display {
            Some(display) => clamp_frame(bounds, display.bounds().size),
            None => bounds,
        };
        let (view, display_id) = (open.view, display.map(|display| display.id()));
        cx.defer(move |cx| popout::open(view, bounds, display_id, cx));
        window.focus(&self.focus_handle(cx), cx);
        self.save(cx);
        cx.notify();
    }

    /// 从窗口里的 `from` 开始拖动浮动的远程桌面 `id`，`resize` 有值时拖的是那条边或那个角；它提到最上面。拖的是
    /// 吸附在右侧面板上的那台时，浮起来的那块标题落在鼠标底下。
    fn start_screen_drag(
        &mut self,
        id: EntityId,
        from: Point<Pixels>,
        resize: Option<ResizeEdge>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.screen_viewers.position(id) else { return };
        let open = &self.screen_viewers.open[ix];
        let was = match open.placement {
            Placement::Snapped(snap) => Some(snap),
            _ => None,
        };
        let mut frame = open.frame;
        if was.is_some() && !self.screen_hidden(id) {
            frame.origin = from - point(frame.size.width / 2., px(UNSNAP_GRAB_Y));
        }
        let unsnap = was.is_some();
        self.screen_viewers.drag = Some(FloatDrag { id, resize, from, frame, unsnap, was, target: None });
        self.raise_screen(id);
        cx.notify();
    }

    /// 占满内容区时窗口的内容：侧栏加右边整块的远程桌面页（`render_page_body`）。
    pub(super) fn render_screen_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(view) = self.screen_viewers.docked().map(|open| open.view.clone()) else { return Vec::new() };
        let page = cached_page(view, window);
        self.render_page_body(page, false, window, cx)
    }

    /// 浮着的远程桌面：窗口内容上面的几块，盖住的终端收不到鼠标；按下哪块哪块提到最上面，四边和四角拖着改大小。
    /// 要在画右侧面板那一栏（`render_right_column`）之后调。
    pub(super) fn render_floating_screens(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let viewport = window.viewport_size();
        let (border, background) = (cx.theme().border, cx.theme().background);
        let mut screens = Vec::new();
        // 藏起来的那台还拿着焦点的话，按的键会悄悄发给远端：等这一帧画完交回窗口。
        let hidden_focused = self
            .screen_viewers
            .open
            .iter()
            .any(|open| self.screen_hidden(open.view.entity_id()) && open.view.read(cx).focused(window, cx));
        if hidden_focused {
            cx.defer_in(window, |this, window, cx| window.focus(&this.focus_handle(cx), cx));
        }
        self.sync_hidden_screens(cx);
        for open in self.screen_viewers.open.iter_mut().filter(|open| open.placement == Placement::Floating) {
            open.frame = clamp_frame(open.frame, viewport);
            let (id, frame) = (open.view.entity_id(), open.frame);
            let grip = |name: &'static str, edge: ResizeEdge, cx: &mut Context<Self>| {
                div().id((name, id.as_u64() as usize)).absolute().cursor(resize_cursor(edge)).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.start_screen_drag(id, event.position, Some(edge), cx);
                    }),
                )
            };
            let (edge, corner) = (px(RESIZE_EDGE), px(RESIZE_GRIP));
            // 四角排在四边后面，叠在上面先接住按下。
            let grips = [
                grip("screen-float-resize-top", ResizeEdge::Top, cx).top_0().left_0().w_full().h(edge),
                grip("screen-float-resize-bottom", ResizeEdge::Bottom, cx).bottom_0().left_0().w_full().h(edge),
                grip("screen-float-resize-left", ResizeEdge::Left, cx).top_0().left_0().h_full().w(edge),
                grip("screen-float-resize-right", ResizeEdge::Right, cx).top_0().right_0().h_full().w(edge),
                grip("screen-float-resize-top-left", ResizeEdge::TopLeft, cx).top_0().left_0().size(corner),
                grip("screen-float-resize-top-right", ResizeEdge::TopRight, cx).top_0().right_0().size(corner),
                grip("screen-float-resize-bottom-left", ResizeEdge::BottomLeft, cx).bottom_0().left_0().size(corner),
                grip("screen-float-resize-bottom-right", ResizeEdge::BottomRight, cx).bottom_0().right_0().size(corner),
            ];
            let screen = div()
                .id(("screen-float", id.as_u64() as usize))
                .absolute()
                .left(frame.origin.x)
                .top(frame.origin.y)
                .w(frame.size.width)
                .h(frame.size.height)
                .occlude()
                .rounded_t(px(8.))
                .overflow_hidden()
                .border_1()
                .border_color(border)
                .bg(background)
                .shadow_lg()
                // 按下时先提到最上面，按下的事照常交给页面。
                .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                    if this.screen_viewers.open.last().is_none_or(|top| top.view.entity_id() != id) {
                        this.raise_screen(id);
                        cx.notify();
                    }
                }))
                .child(cached_page(open.view.clone(), window))
                .children(grips);
            screens.push(screen.into_any_element());
        }
        let top = self.screen_viewers.open.iter().rev().find(|open| open.placement == Placement::Floating);
        if let Some(frame) = top.map(|open| open.frame) {
            self.screen_viewers.last_frame = Some(frame);
        }
        screens
    }

    /// 拖动浮动的远程桌面时盖住整个窗口、接住鼠标移动和松开的一层；没在拖时为空。挪动时落在右侧面板上就标出
    /// 松开后吸附到哪，在窗口外松开就搬进单独的窗口，放在松开的地方。
    pub(super) fn render_screen_drag(&self, cx: &mut Context<Self>) -> Option<Div> {
        let drag = self.screen_viewers.drag.as_ref()?;
        let resize = drag.resize;
        let target = drag.target.and_then(|target| self.render_snap_target(target, cx));
        Some(
            div()
                .absolute()
                .size_full()
                .occlude()
                .cursor(resize.map_or(CursorStyle::ClosedHand, resize_cursor))
                .children(target)
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    cx.stop_propagation();
                    let target = this.snap_target(event.position);
                    let Some(drag) = &mut this.screen_viewers.drag else { return };
                    // 在窗口外松开时可能收不到松开事件，回来时按键已经没按着了。
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.screen_viewers.drag = None;
                        cx.notify();
                        return;
                    }
                    let delta = event.position - drag.from;
                    if drag.unsnap {
                        if f32::from(delta.x).hypot(f32::from(delta.y)) < UNSNAP_DISTANCE {
                            return;
                        }
                        drag.unsnap = false;
                        let id = drag.id;
                        if let Some(ix) = this.screen_viewers.position(id) {
                            let open = &mut this.screen_viewers.open[ix];
                            open.placement = Placement::Floating;
                            set_input(&open.view, open.input(), cx);
                            this.save(cx);
                        }
                    }
                    let Some(drag) = &mut this.screen_viewers.drag else { return };
                    drag.target = target.filter(|_| drag.resize.is_none());
                    let viewport = window.viewport_size();
                    let frame = match drag.resize {
                        Some(edge) => resize_frame(drag.frame, edge, delta, viewport),
                        None => Bounds { origin: drag.frame.origin + delta, size: drag.frame.size },
                    };
                    let (id, frame) = (drag.id, clamp_frame(frame, viewport));
                    if let Some(ix) = this.screen_viewers.position(id) {
                        this.screen_viewers.open[ix].frame = frame;
                    }
                    cx.notify();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        let Some(drag) = this.screen_viewers.drag.take() else { return };
                        if let Some(target) = drag.target {
                            this.snap_screen(drag.id, target, drag.was, window, cx);
                        }
                        cx.notify();
                    }),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, window, cx| {
                        let Some(drag) = this.screen_viewers.drag.take() else { return };
                        cx.notify();
                        // 松在窗口里（落在盖在上面的窗口按钮、拉伸边上时也算「外面」）只是拖完了。
                        let inside = Bounds::new(Point::default(), window.viewport_size()).contains(&event.position);
                        if drag.resize.is_some() || inside {
                            return;
                        }
                        // 窗口在它那块显示器上的位置，加上松开的地方，减去按下时鼠标在这一块里的位置。
                        let grab = drag.from - drag.frame.origin;
                        let origin = window.bounds().origin + event.position - grab;
                        this.pop_out_screen(drag.id, Some(origin), window, cx);
                    }),
                ),
        )
    }
}

/// 远程桌面页：缓存，来一帧画面只通知这一页，窗口跟着终端输出重画时照搬上一帧。辅助工具开着时不缓存：照搬上一帧
/// 时跳过 prepaint，报给辅助工具的节点就丢了。画面铺到页面底边，视频帧又画不出圆角，放它的那块底下两个角要是直的。
fn cached_page(view: Entity<ScreenViewer>, window: &Window) -> AnyElement {
    if window.is_a11y_active() {
        view.into_any_element()
    } else {
        view.cached(StyleRefinement::default().size_full()).into_any_element()
    }
}

/// 换上远程桌面页的 `Input`，变了时叫它重画（它是缓存的视图）。
fn set_input(view: &Entity<ScreenViewer>, input: Input, cx: &mut App) {
    view.update(cx, |viewer, cx| {
        if viewer.set_input(input) {
            cx.notify();
        }
    });
}

/// 第一次浮动时的位置和大小：窗口宽度的四成多，贴着右下角。
fn default_frame(viewport: Size<Pixels>) -> Bounds<Pixels> {
    let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
    let w = (width * 0.45).clamp(f32::from(MIN_FLOAT.width), 960.);
    let h = (w * 0.62).max(f32::from(MIN_FLOAT.height));
    Bounds { origin: point(px(width - w - 24.), px(height - h - 48.)), size: size(px(w), px(h)) }
}

/// 拖边或角 `edge` 挪了 `delta` 后浮动的远程桌面在哪：对面的边不动，拖的边不越出窗口，也不缩到比 `MIN_FLOAT` 小。
fn resize_frame(
    frame: Bounds<Pixels>,
    edge: ResizeEdge,
    delta: Point<Pixels>,
    viewport: Size<Pixels>,
) -> Bounds<Pixels> {
    use ResizeEdge::*;
    let (mut left, mut top) = (f32::from(frame.origin.x), f32::from(frame.origin.y));
    let (mut right, mut bottom) = (left + f32::from(frame.size.width), top + f32::from(frame.size.height));
    let (min_w, min_h) = (f32::from(MIN_FLOAT.width), f32::from(MIN_FLOAT.height));
    let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
    if matches!(edge, Left | TopLeft | BottomLeft) {
        left = (left + dx).min(right - min_w).max(0.);
    }
    if matches!(edge, Right | TopRight | BottomRight) {
        right = (right + dx).max(left + min_w).min(f32::from(viewport.width));
    }
    if matches!(edge, Top | TopLeft | TopRight) {
        top = (top + dy).min(bottom - min_h).max(0.);
    }
    if matches!(edge, Bottom | BottomLeft | BottomRight) {
        bottom = (bottom + dy).max(top + min_h).min(f32::from(viewport.height));
    }
    Bounds { origin: point(px(left), px(top)), size: size(px(right - left), px(bottom - top)) }
}

fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// 把浮动的远程桌面限在窗口里：不比窗口大，整块都在窗口里。
fn clamp_frame(frame: Bounds<Pixels>, viewport: Size<Pixels>) -> Bounds<Pixels> {
    let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
    let w = f32::from(frame.size.width).min(vw).max(f32::from(MIN_FLOAT.width).min(vw));
    let h = f32::from(frame.size.height).min(vh).max(f32::from(MIN_FLOAT.height).min(vh));
    let x = f32::from(frame.origin.x).clamp(0., (vw - w).max(0.));
    let y = f32::from(frame.origin.y).clamp(0., (vh - h).max(0.));
    Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_frame_stays_inside_the_window() {
        let viewport = size(px(1200.), px(800.));
        let outside = Bounds { origin: point(px(1100.), px(-50.)), size: size(px(500.), px(300.)) };
        let frame = clamp_frame(outside, viewport);
        assert_eq!(frame.origin, point(px(700.), px(0.)));
        assert_eq!(frame.size, size(px(500.), px(300.)));
        let huge = Bounds { origin: point(px(0.), px(0.)), size: size(px(5000.), px(5000.)) };
        assert_eq!(clamp_frame(huge, viewport).size, viewport);
        let tiny = Bounds { origin: point(px(10.), px(10.)), size: size(px(10.), px(10.)) };
        assert_eq!(clamp_frame(tiny, viewport).size, MIN_FLOAT);
        let first = default_frame(viewport);
        assert_eq!(clamp_frame(first, viewport), first);
    }

    #[test]
    fn resizing_keeps_the_opposite_edges() {
        let viewport = size(px(1200.), px(800.));
        let frame = Bounds { origin: point(px(100.), px(100.)), size: size(px(500.), px(300.)) };
        let grown = resize_frame(frame, ResizeEdge::TopLeft, point(px(-50.), px(-30.)), viewport);
        assert_eq!(grown, Bounds { origin: point(px(50.), px(70.)), size: size(px(550.), px(330.)) });
        let past_window = resize_frame(frame, ResizeEdge::Left, point(px(-500.), px(0.)), viewport);
        assert_eq!(past_window, Bounds { origin: point(px(0.), px(100.)), size: size(px(600.), px(300.)) });
        let shrunk = resize_frame(frame, ResizeEdge::Top, point(px(0.), px(500.)), viewport);
        assert_eq!(shrunk, Bounds { origin: point(px(100.), px(160.)), size: size(px(500.), MIN_FLOAT.height) });
        let wider = resize_frame(frame, ResizeEdge::Right, point(px(900.), px(0.)), viewport);
        assert_eq!(wider.origin, frame.origin);
        assert_eq!(wider.size, size(px(1100.), px(300.)));
    }
}
