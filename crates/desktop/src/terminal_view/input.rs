//! 按键、滚轮和鼠标：先给选区、补全菜单和灰字建议处理，余下的发给程序或上报给它。

use std::time::Duration;

use gpui::{
    Context, KeyDownEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, ScrollDelta, ScrollWheelEvent, Window,
};
use runode_shared_types::{
    grid::GridPoint,
    input::{self, Mods, SelectionAdjust},
};

use super::TerminalView;
use crate::keys;

/// 拖选到网格外时自动滚动的间隔，每次滚一行。
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);

impl TerminalView {
    pub(super) fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 输入法正在组字时，按键归输入法处理。
        if self.marked_text.is_some() {
            return;
        }
        // 在 shell 提示符上选中了一段命令时，退格和 Delete 删掉选中的文字。
        let keystroke = &event.keystroke;
        if matches!(keystroke.key.as_str(), "backspace" | "delete")
            && !keystroke.modifiers.modified()
            && self.session.delete_selection()
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Shift 加方向键等在有选区时用来扩展选区，没有选区时照常发给程序。
        if let Some(adjustment) = selection_adjustment(&event.keystroke)
            && self.session.adjust_selection(adjustment)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // 补全菜单开着时的上下选择、接受和关闭，以及由 runode 接管的 Tab。这些键没有默认快捷键，
        // 会走到这里。
        if self.completion_key(keystroke, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // 光标在输入末尾、后面画着建议时：→、End、Ctrl+F 接受整条，Option+→ 接受一个词。
        // 默认快捷键把 Option+→ 映射成了 ESC f，那条路在 `send_text` 里处理。
        let m = &keystroke.modifiers;
        let accept = match keystroke.key.as_str() {
            "right" | "end" if !m.modified() => Some(false),
            "f" if m.control && !m.alt && !m.shift && !m.platform => Some(false),
            "right" if m.alt && !m.control && !m.shift && !m.platform => Some(true),
            _ => None,
        };
        if let Some(word) = accept
            && self.accept_suggestion(word)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(input) = keys::translate(&event.keystroke) else {
            return;
        };
        if self.session.key(&input) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 滚在补全菜单上：上下移动选中项，不滚终端。
        if self.completion_scroll(event, cx) {
            return;
        }
        let Some(metrics) = self.metrics else {
            return;
        };
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / f32::from(metrics.cell.height),
        };
        // 滚轮增量为正表示内容向下移动，即往回滚到历史输出。程序没开鼠标上报时按像素
        // 平滑滚动回滚历史；开着时只能按整行发给程序。
        if !self.session.mouse_tracking() {
            self.scroll_remainder = 0.;
            if self.session.scroll_smoothly(lines) {
                cx.notify();
            }
            return;
        }
        self.scroll_remainder -= lines;
        let whole = self.scroll_remainder.trunc();
        self.scroll_remainder -= whole;
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        self.session.scroll(whole as isize, at, mouse_mods(&event.modifiers));
        cx.notify();
    }

    /// 窗口坐标换算成网格位置；单元格尺寸还没量出来时为 `None`。
    pub(super) fn grid_point(&self, position: Point<Pixels>) -> Option<GridPoint> {
        let metrics = self.metrics?;
        let local = position - self.grid_origin;
        Some(GridPoint {
            x: f32::from(local.x) / f32::from(metrics.cell.width),
            y: f32::from(local.y) / f32::from(metrics.cell.height),
        })
    }

    pub(super) fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        // 激活窗口的那一下只用来激活，不选择也不上报。
        if event.first_mouse {
            return;
        }
        // 点在补全菜单上：接受点到的候选，不选择也不上报。
        if self.completion_click(event, cx) {
            return;
        }
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        // 程序开了鼠标上报时按键归程序，按住 Shift 照常选择。
        if self.session.mouse_tracking() && !event.modifiers.shift {
            if let Some(button) = mouse_button(event.button) {
                let mods = mouse_mods(&event.modifiers);
                self.session.mouse_report(input::MouseAction::Press, Some(button), at, mods);
                self.reporting_press = true;
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.selecting = true;
        let plain = !event.modifiers.modified() && event.click_count == 1;
        self.click_cell = plain.then(|| grid_cell(at));
        self.session.select_press(at, double_click_interval());
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
            if self.session.select_drag(at, event.modifiers.alt) {
                self.start_autoscroll(cx);
            } else {
                self._autoscroll = None;
            }
            cx.notify();
            return;
        }
        // 没按键的移动只报给指针下的终端；按着键的拖动只报给按下时所在的终端。
        let ours = if event.pressed_button.is_some() { self.reporting_press } else { inside };
        if ours && self.session.mouse_tracking() {
            let pressed = event.pressed_button.and_then(mouse_button);
            let mods = mouse_mods(&event.modifiers);
            self.session.mouse_report(input::MouseAction::Motion, pressed, at, mods);
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
                if self.click_cell.take() == Some(grid_cell(at)) && self.session.selection_text().is_none() {
                    self.session.click_to_move(at);
                }
            }
            return;
        }
        if !std::mem::take(&mut self.reporting_press) {
            return;
        }
        if self.session.mouse_tracking()
            && let Some(button) = mouse_button(event.button)
        {
            let mods = mouse_mods(&event.modifiers);
            self.session.mouse_report(input::MouseAction::Release, Some(button), at, mods);
        }
    }

    fn finish_selecting(&mut self, at: GridPoint, cx: &mut Context<Self>) {
        self.selecting = false;
        self._autoscroll = None;
        self.session.select_release(at);
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
                    if view.session.select_autoscroll() {
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

fn mouse_mods(modifiers: &Modifiers) -> Mods {
    Mods {
        shift: modifiers.shift,
        ctrl: modifiers.control,
        alt: modifiers.alt,
        right_alt: false,
    }
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
