//! 视口滚动：按像素平滑滚动、翻页、滚到两端和在提示符之间跳转。

use libghostty_vt::{
    screen::RowSemanticPrompt,
    terminal::{Point, PointCoordinate, ScrollViewport, Terminal},
};
use runode_shared_types::grid::ViewportScroll;

use super::{Session, log_err};

impl Session {
    /// 视口停在最底部，没有翻回滚历史，也没有平滑滚动错开的半行。
    pub fn viewport_at_bottom(&self) -> bool {
        self.terminal.viewport_active().unwrap_or(false) && self.scroll_offset == 0.
    }

    /// 不开鼠标上报时按像素滚动回滚历史：`lines` 为正往回看，可以是零点几行。凑够整行的
    /// 部分挪视口，剩下的记在 `scroll_offset`，绘制时整屏往下错开这么多，露出视口上面那一行
    /// 的一部分。返回画面是否变了。
    pub fn scroll_smoothly(&mut self, lines: f32) -> bool {
        let top = |terminal: &Terminal<'static, 'static>| terminal.scrollbar().map_or(0, |s| s.offset);
        let before = (top(&self.terminal), self.scroll_offset);
        let mut offset = self.scroll_offset + lines;
        let whole = offset.floor();
        if whole != 0. {
            self.terminal.scroll_viewport(ScrollViewport::Delta(-(whole as isize)));
            // 到了历史顶上或者已经在底部时视口挪不动，按实际挪了多少扣。
            offset -= before.0 as f32 - top(&self.terminal) as f32;
        }
        // 视口上面没有行时不能往下错开；在底部继续往下滚时也不会错开成负的。
        let top = top(&self.terminal);
        self.scroll_offset = if top == 0 { 0. } else { offset.clamp(0., 0.999) };
        (top, self.scroll_offset) != before
    }

    /// 滚动视口，让屏幕第 `row` 行落在视口中间。
    pub(super) fn scroll_to_row(&mut self, row: u32) {
        self.scroll_offset = 0.;
        let half = usize::from(self.size.get().rows / 2);
        self.terminal
            .scroll_viewport(ScrollViewport::Row((row as usize).saturating_sub(half)));
    }

    /// 滚动视口；`Page(n)` 按视口高度翻 n 页，负数往回翻。
    pub fn scroll_viewport(&mut self, scroll: ViewportScroll) {
        self.scroll_offset = 0.;
        let scroll = match scroll {
            ViewportScroll::Top => ScrollViewport::Top,
            ViewportScroll::Bottom => ScrollViewport::Bottom,
            ViewportScroll::Page(pages) => {
                ScrollViewport::Delta(pages * self.size.get().rows as isize)
            }
        };
        self.terminal.scroll_viewport(scroll);
    }

    /// 视口跳到上一个（`backward`）或下一个提示符所在的行，靠 shell 集成标在提示符上的记号。
    /// 往后没有提示符时回到底部。
    pub fn jump_to_prompt(&mut self, backward: bool) {
        self.scroll_offset = 0.;
        let result = self.terminal.scrollbar().map(|scrollbar| {
            let is_prompt = |y: u64| {
                self.terminal
                    .grid_ref(Point::Screen(PointCoordinate { x: 0, y: y as u32 }))
                    .and_then(|r| r.row())
                    .and_then(|r| r.semantic_prompt())
                    .is_ok_and(|p| p == RowSemanticPrompt::Prompt)
            };
            if backward {
                (0..scrollbar.offset).rev().find(|y| is_prompt(*y))
            } else {
                (scrollbar.offset + 1..scrollbar.total).find(|y| is_prompt(*y))
            }
        });
        match log_err("jump to prompt", result) {
            Some(Some(row)) => self.terminal.scroll_viewport(ScrollViewport::Row(row as usize)),
            Some(None) if !backward => self.terminal.scroll_viewport(ScrollViewport::Bottom),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testing::*;

    #[test]
    fn scrolling_the_viewport_by_page_and_to_the_ends() {
        let mut session = idle_session();
        for n in 0..20 {
            session.feed(format!("{n}\r\n").as_bytes());
        }
        session.scroll_viewport(ViewportScroll::Top);
        assert_eq!(row_text(&session.frame(), 0), "0");
        session.scroll_viewport(ViewportScroll::Page(1));
        assert_eq!(row_text(&session.frame(), 0), "4");
        session.scroll_viewport(ViewportScroll::Bottom);
        assert_eq!(row_text(&session.frame(), 0), "17");
    }

    #[test]
    fn smooth_scrolling_shifts_by_fractions_and_stops_at_the_ends() {
        let mut session = idle_session();
        for n in 0..20 {
            session.feed(format!("{n}\r\n").as_bytes());
        }
        // 在底部往回滚半行：视口不动，整屏错开半行，露出上面那一行。
        assert!(session.scroll_smoothly(0.5));
        let frame = session.frame();
        assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("17", 0.5));
        assert_eq!(frame.above.iter().map(|c| c.text.as_str()).collect::<String>().trim_end(), "16");
        drop(frame);
        // 再滚 0.75 行：凑够一行挪视口，剩下 0.25 行。
        session.scroll_smoothly(0.75);
        let frame = session.frame();
        assert_eq!(row_text(&frame, 0), "16");
        assert!((frame.scroll_offset - 0.25).abs() < 1e-6);
        drop(frame);
        // 往底部滚过头：回到底部，不留错开。
        session.scroll_smoothly(-5.);
        let frame = session.frame();
        assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("17", 0.));
        drop(frame);
        // 滚到历史顶上以后不能再错开。
        session.scroll_smoothly(100.5);
        let frame = session.frame();
        assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("0", 0.));
        assert!(frame.above.is_empty());
    }

    #[test]
    fn jumping_between_marked_prompts() {
        let mut session = idle_session();
        for n in 0..3 {
            session.feed(PROMPT);
            session.feed(format!("cmd{n}\r\n\x1b]133;C\x07out\r\nout\r\n\x1b]133;D;0\x07").as_bytes());
        }
        session.feed(PROMPT);
        // 4 行高的视口最上面一行是 "$ cmd2"，往上跳到它之前的那个提示符。
        assert_eq!(row_text(&session.frame(), 0), "$ cmd2");
        session.jump_to_prompt(true);
        assert_eq!(row_text(&session.frame(), 0), "$ cmd1");
        session.jump_to_prompt(true);
        assert_eq!(row_text(&session.frame(), 0), "$ cmd0");
        session.jump_to_prompt(false);
        assert_eq!(row_text(&session.frame(), 0), "$ cmd1");
    }
}
