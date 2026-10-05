//! 鼠标：程序开启鼠标上报时把按键、移动和滚轮按它要求的格式发给它；没开时滚轮滚动回滚历史。

use libghostty_vt::{key, mouse, terminal::ScrollViewport};
use runode_shared_types::{
    grid::GridPoint,
    input::{Mods, MouseAction, MouseButton},
};

use super::{Session, convert::ghostty_mods};

impl Session {
    /// 滚轮输入：程序开启鼠标上报时发给程序，否则在回滚缓冲里滚动视口。
    pub fn scroll(&mut self, lines: isize, at: GridPoint, mods: Mods) {
        if lines == 0 {
            return;
        }
        if self.mouse_tracking() {
            let button = if lines < 0 { mouse::Button::Four } else { mouse::Button::Five };
            self.sync_mouse_encoder();
            self.scratch.clear();
            for _ in 0..lines.unsigned_abs() {
                if !self.encode_mouse(mouse::Action::Press, Some(button), at, ghostty_mods(mods)) {
                    return;
                }
            }
            self.send_input(self.scratch.clone());
        } else {
            self.terminal.scroll_viewport(ScrollViewport::Delta(lines));
        }
    }

    /// 运行中的程序是否开启了鼠标上报。
    pub fn mouse_tracking(&self) -> bool {
        self.terminal.is_mouse_tracking().unwrap_or(false)
    }

    /// 把鼠标按键或移动按程序要求的上报格式发给它；程序没开上报时编码结果为空，什么都不发。
    /// 移动时 `button` 是按着的键，按键拖动模式只上报这时的移动。
    pub fn mouse_report(&mut self, action: MouseAction, button: Option<MouseButton>, at: GridPoint, mods: Mods) {
        let action = match action {
            MouseAction::Press => mouse::Action::Press,
            MouseAction::Release => mouse::Action::Release,
            MouseAction::Motion => mouse::Action::Motion,
        };
        let button = button.map(|button| match button {
            MouseButton::Left => mouse::Button::Left,
            MouseButton::Right => mouse::Button::Right,
            MouseButton::Middle => mouse::Button::Middle,
        });
        let mods = ghostty_mods(mods);
        self.sync_mouse_encoder();
        self.mouse_encoder.set_any_button_pressed(action != mouse::Action::Release && button.is_some());
        self.scratch.clear();
        if self.encode_mouse(action, button, at, mods) && !self.scratch.is_empty() {
            self.send_input(self.scratch.clone());
        }
    }

    /// 编码一个鼠标事件，追加到 `scratch`；失败时记日志并返回 false。
    fn encode_mouse(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        at: GridPoint,
        mods: key::Mods,
    ) -> bool {
        let (x, y) = self.surface_position(at);
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mods)
            .set_position(mouse::Position { x: x as f32, y: y as f32 });
        match self.mouse_encoder.encode_to_vec(&self.mouse_event, &mut self.scratch) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("mouse encode failed: {err}");
                false
            }
        }
    }

    /// 让鼠标编码器跟上终端的上报模式和当前网格尺寸。
    fn sync_mouse_encoder(&mut self) {
        let size = self.size.get();
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            // 同一单元格里的移动不重复上报。
            .set_track_last_cell(true)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(size.cols) * u32::from(size.cell_width_px),
                screen_height: u32::from(size.rows) * u32::from(size.cell_height_px),
                cell_width: u32::from(size.cell_width_px),
                cell_height: u32::from(size.cell_height_px),
                padding_top: 0,
                padding_bottom: 0,
                padding_left: 0,
                padding_right: 0,
            });
    }
}
