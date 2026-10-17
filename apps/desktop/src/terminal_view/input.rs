//! 按键、滚轮和鼠标：先给选区处理，余下的发给程序或上报给它。

use std::time::Duration;

use futures::{FutureExt, future::Shared};

use gpui::{
    App, Context, KeyDownEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, ScrollDelta, ScrollWheelEvent, Size, Task, Window,
};
use runode_terminal::session::{Link, LinkCandidates, LinkTarget, Session};
use runode_types::{
    grid::GridPoint,
    input::{self, Mods, SelectionAdjust},
};

use super::TerminalView;
use super::keys;

/// 按着 ⌘ 时指针下的候选链接。路径要查存在与否，路径在断开的网络卷或 autofs 下时 stat 能卡几十秒，
/// 所以放到后台去查，查完再把结果记进 `hovered_link`；指针挪到别的候选上时换掉这一份，丢掉任务，
/// 旧的结果就不会再回来。
pub(super) struct LinkLookup {
    candidates: LinkCandidates,
    /// 后台查的结果，查完就清掉；没查完时 ⌘ 点下去的也等它。
    check: Option<Shared<Task<Option<Link>>>>,
    _task: Task<()>,
}

/// 拖选到网格外时自动滚动的间隔，每次滚一行。
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);

/// 一次滚轮滚了几行（可以带小数），按像素给的按单元格高度 `cell_height` 换算。正数表示内容
/// 向下移动，即往回滚。
pub(super) fn wheel_lines(delta: ScrollDelta, cell_height: Pixels) -> f32 {
    match delta {
        ScrollDelta::Lines(delta) => delta.y,
        ScrollDelta::Pixels(delta) => f32::from(delta.y) / f32::from(cell_height),
    }
}

/// 把这次滚的 `lines` 行累加进不足一行的余量 `remainder`，取出其中的整行。方向和 `lines` 相反：
/// 正数表示往下（内容往上走）。
pub(super) fn take_whole_lines(remainder: &mut f32, lines: f32) -> isize {
    *remainder -= lines;
    let whole = remainder.trunc();
    *remainder -= whole;
    whole as isize
}

impl TerminalView {
    pub(super) fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 按键（包括输入法组字时的）算一次活动：光标亮起，停了的闪烁恢复。
        self.reset_cursor_blink(cx);
        // 输入法正在组字时，按键归输入法处理。
        if self.marked_text.is_some() {
            return;
        }
        // 在 shell 提示符上选中了一段命令时，退格和 Delete 删掉选中的文字。
        let keystroke = &event.keystroke;
        if matches!(keystroke.key.as_str(), "backspace" | "delete")
            && !keystroke.modifiers.modified()
            && self.screen.live_mut().is_some_and(Session::delete_selection)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Shift 加方向键等在有选区时用来扩展选区，没有选区时照常发给程序。
        if let Some(adjustment) = selection_adjustment(&event.keystroke)
            && self.screen.shown_mut().is_some_and(|session| session.adjust_selection(adjustment))
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(input) = keys::translate(&event.keystroke) else {
            return;
        };
        // 没在看（重新连上的过程中、和宿主断开了）时按键丢掉。
        if self.screen.live_mut().is_some_and(|session| session.key(&input)) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let lines = wheel_lines(event.delta, metrics.cell.height);
        // 滚轮增量为正表示内容向下移动，即往回滚到历史输出。滚轮不归程序管时按像素平滑滚动
        // 回滚历史；归程序管（开着鼠标上报，或备用屏上当方向键）时只能按整行发给程序。
        if !self.screen.live().is_some_and(Session::program_scrolls) {
            self.scroll_remainder = 0.;
            if self.screen.shown_mut().is_some_and(|session| session.scroll_smoothly(lines)) {
                cx.notify();
            }
            return;
        }
        let whole = take_whole_lines(&mut self.scroll_remainder, lines);
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        // 画面等程序的输出再变；这里通知重画只是白画一帧。
        if let Some(session) = self.screen.live_mut() {
            session.scroll(whole, at, mouse_mods(&event.modifiers));
        }
    }

    /// 窗口坐标换算成网格位置；单元格尺寸还没量出来时为 `None`。
    pub(super) fn grid_point(&self, position: Point<Pixels>) -> Option<GridPoint> {
        let metrics = self.metrics?;
        Some(grid_point_at(position, self.grid_origin, metrics.cell))
    }

    pub(super) fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        // 尺寸归别的前端管时，点一下就接管：已经有焦点时不会再因为获得焦点报一次，这里补上。
        if self.screen.size_owner().is_some() {
            self.report_focus(true);
        }
        // 激活窗口的那一下只用来激活，不选择也不上报。
        if event.first_mouse {
            return;
        }
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        // ⌘ 点在链接上：打开它，不选择也不上报（程序开着鼠标上报时也一样，上报本来就不带 ⌘）。
        if event.button == MouseButton::Left && event.modifiers.platform && self.click_link(at, cx) {
            return;
        }
        // 程序开了鼠标上报时按键归程序，按住 Shift 照常选择。
        if let Some(session) = self.screen.live_mut()
            && session.mouse_tracking()
            && !event.modifiers.shift
        {
            if let Some(button) = mouse_button(event.button) {
                let mods = mouse_mods(&event.modifiers);
                session.mouse_report(input::MouseAction::Press, Some(button), at, mods);
                self.reporting_press = true;
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let Some(session) = self.screen.shown_mut() else {
            return;
        };
        session.select_press(at, double_click_interval());
        self.selecting = true;
        let plain = !event.modifiers.modified() && event.click_count == 1;
        self.click_cell = plain.then(|| grid_cell(at));
        cx.notify();
    }

    pub(super) fn mouse_move(&mut self, event: &MouseMoveEvent, inside: bool, cx: &mut Context<Self>) {
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        if self.selecting {
            // 漏掉了松开事件时按松开处理，免得之后的移动还在扩展选区。
            if event.pressed_button != Some(MouseButton::Left) {
                self.finish_selecting(at, cx);
                return;
            }
            // Option 拖出矩形块。
            if self.screen.shown_mut().is_some_and(|session| session.select_drag(at, event.modifiers.alt)) {
                self.start_autoscroll(cx);
            } else {
                self._autoscroll = None;
            }
            cx.notify();
            return;
        }
        self.hover_link(inside && event.pressed_button.is_none() && event.modifiers.platform, at, cx);
        // 没按键的移动只报给指针下的终端；按着键的拖动只报给按下时所在的终端。
        let ours = if event.pressed_button.is_some() { self.reporting_press } else { inside };
        if ours
            && let Some(session) = self.screen.live_mut()
            && session.mouse_tracking()
        {
            let pressed = event.pressed_button.and_then(mouse_button);
            let mods = mouse_mods(&event.modifiers);
            session.mouse_report(input::MouseAction::Motion, pressed, at, mods);
        }
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        if self.selecting {
            if event.button == MouseButton::Left {
                self.finish_selecting(at, cx);
                // 原地单击、没选出东西：在 shell 提示符上时把光标挪到点击处。
                if self.click_cell.take() == Some(grid_cell(at))
                    && let Some(session) = self.screen.live_mut()
                    && session.selection_text().is_none()
                {
                    session.click_to_move(at);
                }
            }
            return;
        }
        if !std::mem::take(&mut self.reporting_press) {
            return;
        }
        if let Some(session) = self.screen.live_mut()
            && session.mouse_tracking()
            && let Some(button) = mouse_button(event.button)
        {
            let mods = mouse_mods(&event.modifiers);
            session.mouse_report(input::MouseAction::Release, Some(button), at, mods);
        }
    }

    /// 按下或松开了修饰键：指针在这个终端里时按 ⌘ 有没有按着重新找指针下的链接。
    pub(super) fn link_modifiers_changed(
        &mut self,
        modifiers: &Modifiers,
        pointer: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let at = pointer.and_then(|pointer| self.grid_point(pointer));
        match at {
            Some(at) => self.hover_link(modifiers.platform, at, cx),
            None => self.clear_hovered_link(cx),
        }
    }

    /// 按着 ⌘ 悬停时记下指针下的链接，画下划线、指针换成手形，网址在左下角显示真实目标；
    /// `active` 为假时清掉。
    fn hover_link(&mut self, active: bool, at: GridPoint, cx: &mut Context<Self>) {
        let candidates = if active { self.screen.shown().and_then(|session| session.links_at(at)) } else { None };
        self.look_up_link(candidates, cx);
    }

    fn clear_hovered_link(&mut self, cx: &mut Context<Self>) {
        self.look_up_link(None, cx);
    }

    /// ⌘ 点在 `at` 上：是链接就打开，还在查就等查完再打开，这两种都算点中了链接，返回真；
    /// 查过不是链接时返回假，照常选择或上报。
    fn click_link(&mut self, at: GridPoint, cx: &mut Context<Self>) -> bool {
        let candidates = self.screen.shown().and_then(|session| session.links_at(at));
        self.look_up_link(candidates, cx);
        match &self.link_lookup {
            Some(LinkLookup { check: Some(check), .. }) => {
                // 松开 ⌘、指针挪开都不撤回这一下：后台查完是链接就打开。查完不是时这一下什么都不做，
                // 选择已经错过了：只有按下 ⌘ 的同一瞬间点在不存在的路径上才会这样，不为它记下按的位置。
                let check = check.clone();
                cx.spawn(async move |_, cx| {
                    if let Some(link) = check.await {
                        cx.update(|cx| open_link(&link.target, cx));
                    }
                })
                .detach();
                true
            }
            Some(_) if let Some(link) = &self.hovered_link => {
                open_link(&link.target, cx);
                true
            }
            _ => false,
        }
    }

    /// 换成 `candidates` 这一份候选：和手上的一样时什么都不做（同一个词上挪动不重复查）；网址当场就有
    /// 结果，路径交给后台查。查的时候先不画下划线，查出存在才画。
    fn look_up_link(&mut self, candidates: Option<LinkCandidates>, cx: &mut Context<Self>) {
        if self.link_lookup.as_ref().map(|lookup| &lookup.candidates) == candidates.as_ref() {
            return;
        }
        let Some(candidates) = candidates else {
            self.link_lookup = None;
            self.set_hovered_link(None, cx);
            return;
        };
        if !candidates.checks_paths() {
            self.set_hovered_link(candidates.clone().resolve(), cx);
            self.link_lookup = Some(LinkLookup { candidates, check: None, _task: Task::ready(()) });
            return;
        }
        self.set_hovered_link(None, cx);
        let check = cx
            .background_executor()
            .spawn({
                let candidates = candidates.clone();
                async move { candidates.resolve() }
            })
            .shared();
        let task = cx.spawn({
            let check = check.clone();
            async move |this, cx| {
                let link = check.await;
                this.update(cx, |view, cx| {
                    if let Some(lookup) = &mut view.link_lookup {
                        lookup.check = None;
                    }
                    view.set_hovered_link(link, cx);
                })
                .ok();
            }
        });
        self.link_lookup = Some(LinkLookup { candidates, check: Some(check), _task: task });
    }

    fn set_hovered_link(&mut self, link: Option<Link>, cx: &mut Context<Self>) {
        if link != self.hovered_link {
            self.hovered_link = link;
            cx.notify();
        }
    }

    fn finish_selecting(&mut self, at: GridPoint, cx: &mut Context<Self>) {
        self.selecting = false;
        self._autoscroll = None;
        if let Some(session) = self.screen.shown_mut() {
            session.select_release(at);
        }
        cx.notify();
    }

    /// 拖到网格上下边以外时定时滚动视口、扩展选区，直到拖回网格里或松开左键。
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self._autoscroll.is_some() {
            return;
        }
        self._autoscroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                let updated = this.update(cx, |view, cx| {
                    if view.screen.shown_mut().is_some_and(Session::select_autoscroll) {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        }));
    }
}

/// 网址交给系统按协议打开，文件和目录用系统默认的程序打开（目录进 Finder）。
fn open_link(target: &LinkTarget, cx: &App) {
    match target {
        LinkTarget::Url(url) => cx.open_url(url),
        LinkTarget::Path(path) => cx.open_with_system(path),
    }
}

fn mouse_mods(modifiers: &Modifiers) -> Mods {
    Mods { shift: modifiers.shift, ctrl: modifiers.control, alt: modifiers.alt, right_alt: false }
}

/// 系统设置里的双击间隔，决定两次按下算不算连击。
fn double_click_interval() -> Duration {
    #[cfg(target_os = "macos")]
    return Duration::from_secs_f64(objc2_app_kit::NSEvent::doubleClickInterval());
    #[cfg(not(target_os = "macos"))]
    Duration::from_millis(500)
}

fn mouse_button(button: MouseButton) -> Option<input::MouseButton> {
    match button {
        MouseButton::Left => Some(input::MouseButton::Left),
        MouseButton::Right => Some(input::MouseButton::Right),
        MouseButton::Middle => Some(input::MouseButton::Middle),
        _ => None,
    }
}

/// 窗口坐标 `position` 换算成网格位置。`origin` 是 VT 第 0 列第 0 行在窗口里的位置：平滑滚动时跟着
/// 错开，尺寸归别的前端管、视图只画了 VT 的一块时按 `Crop::vt_origin` 挪过，所以落在视图左上角的是
/// VT 里画在那里的那一格。`cell` 是单元格尺寸。
pub(super) fn grid_point_at(position: Point<Pixels>, origin: Point<Pixels>, cell: Size<Pixels>) -> GridPoint {
    let local = position - origin;
    GridPoint { x: f32::from(local.x) / f32::from(cell.width), y: f32::from(local.y) / f32::from(cell.height) }
}

/// 网格位置所在的单元格。
fn grid_cell(at: GridPoint) -> (i32, i32) {
    (at.x.floor() as i32, at.y.floor() as i32)
}

/// 只按着 Shift 的方向、翻页、Home/End 键对应的选区调整。
fn selection_adjustment(keystroke: &Keystroke) -> Option<SelectionAdjust> {
    let mods = &keystroke.modifiers;
    if !mods.shift || mods.control || mods.alt || mods.platform {
        return None;
    }
    Some(match keystroke.key.as_str() {
        "left" => SelectionAdjust::Left,
        "right" => SelectionAdjust::Right,
        "up" => SelectionAdjust::Up,
        "down" => SelectionAdjust::Down,
        "pageup" => SelectionAdjust::PageUp,
        "pagedown" => SelectionAdjust::PageDown,
        "home" => SelectionAdjust::Home,
        "end" => SelectionAdjust::End,
        _ => return None,
    })
}
