//! 光标所在的输入行：单击把光标挪到点击处、删掉选中的输入，都换算成方向键和退格发给 shell。

use libghostty_vt::{
    key,
    screen::{CellSemanticContent, Screen},
    selection::Order,
    terminal::{Point, PointCoordinate, PointSpace},
};
use runode_types::{frame::Cell, grid::GridPoint};

use super::{Session, log_err};
use crate::prompt_input::wrapped_rows;

/// 光标所在的输入行，各单元格按行连成一串，下标从这一行第一格算起。
struct InputLine {
    top: u32,
    bottom: u32,
    cols: usize,
    /// 每一格是不是宽字符的占位格；方向键和退格按字符走，占位格不算一步。
    spacer: Vec<bool>,
    /// 最后一个有字的格子之后。
    end: usize,
    /// 用户输入从哪一格开始：shell 集成标出的提示符之后；没有标记时为 0。
    start: usize,
    cursor: usize,
}

impl InputLine {
    /// 活动区第 `y` 行第 `x` 列在这一串里的下标；不在这一行上时为 `None`。
    fn index(&self, x: u16, y: u32) -> Option<usize> {
        (self.top..=self.bottom).contains(&y).then(|| (y - self.top) as usize * self.cols + usize::from(x))
    }

    fn chars(&self, range: std::ops::Range<usize>) -> isize {
        let range = range.start.min(self.spacer.len())..range.end.min(self.spacer.len());
        self.spacer[range].iter().filter(|spacer| !**spacer).count() as isize
    }

    /// 光标从 `from` 走到 `to` 要按几下方向键，负数往左。
    fn steps(&self, from: usize, to: usize) -> isize {
        if to >= from { self.chars(from..to) } else { -self.chars(to..from) }
    }
}

impl Session {
    /// 单击把光标挪到点击处：按两处之间隔着的字符数发左右方向键，由 shell 自己移动光标。
    /// 点在这一行文字末尾之后时停在末尾。只在 `input_line` 能取到输入行、并且点在这一行上时
    /// 生效，返回是否发了按键。
    pub fn click_to_move(&mut self, at: GridPoint) -> bool {
        let Some(steps) = self.click_to_move_steps(at) else {
            return false;
        };
        let Some(bytes) = self.arrow_keys(steps) else {
            return false;
        };
        self.before_input();
        self.send_input(bytes);
        true
    }

    /// 从光标走到点击处要按几下方向键，负数往左；不该移动时为 `None`。
    fn click_to_move_steps(&mut self, at: GridPoint) -> Option<isize> {
        let target = self.viewport_cell(at);
        let line = log_err("input line", self.input_line()).flatten()?;
        let to = line.index(target.x, target.y)?.max(line.start).min(line.end.max(line.cursor));
        let steps = line.steps(line.cursor, to);
        (steps != 0).then_some(steps)
    }

    /// 删掉选中的文字（退格、Delete 时）：先把光标挪到选区末尾，再按选中的字符数发退格。
    /// 选区要整个落在光标所在的输入行里，`input_line` 的其他条件也一样；超出文字末尾的部分
    /// 不算。返回是否处理了；没处理时按键照常交给程序。
    pub fn delete_selection(&mut self) -> bool {
        let Some((steps, count)) = log_err("delete selection", self.delete_selection_keys()).flatten() else {
            return false;
        };
        let (Some(arrows), Some(backspace)) = (self.arrow_keys(steps), self.encode_key(key::Key::Backspace)) else {
            return false;
        };
        let mut bytes = arrows;
        bytes.extend(backspace.repeat(count));
        self.before_input();
        self.send_input(bytes);
        true
    }

    /// 删除选区要先按几下方向键（负数往左）、再按几下退格；不该处理时为 `None`。
    fn delete_selection_keys(&mut self) -> libghostty_vt::error::Result<Option<(isize, usize)>> {
        let Some(line) = self.input_line()? else {
            return Ok(None);
        };
        let range = {
            let Some(selection) = self.terminal.selection()? else {
                return Ok(None);
            };
            if selection.is_rectangle() {
                return Ok(None);
            }
            let selection = selection.to_ordered(&self.terminal, Order::Forward)?;
            let start = self.terminal.point_from_grid_ref(&selection.start(), PointSpace::Active)?;
            let end = self.terminal.point_from_grid_ref(&selection.end(), PointSpace::Active)?;
            let (Some(start), Some(end)) = (start, end) else {
                return Ok(None);
            };
            let (Some(start), Some(end)) = (line.index(start.x, start.y), line.index(end.x, end.y)) else {
                return Ok(None);
            };
            // 先夹终点再夹起点：选区整个落在提示符里时得到空区间，不能让起点越过终点。
            let end = (end + 1).min(line.end);
            start.max(line.start).min(end)..end
        };
        let count = line.chars(range.clone()) as usize;
        if count == 0 {
            return Ok(None);
        }
        Ok(Some((line.steps(line.cursor, range.end), count)))
    }

    /// 光标所在的输入行：光标那一行，连同软换行接在一起的上下几行。只在主屏、视口在底部、
    /// 光标停在 shell 提示符上时有，别的时候为 `None`。
    ///
    /// shell 集成用 OSC 133 标出了提示符时，以它为准：输入从提示符之后开始。没有这些标记时
    /// 只能看前台是不是 shell 自己，也不知道提示符在哪里结束。
    fn input_line(&mut self) -> libghostty_vt::error::Result<Option<InputLine>> {
        if self.terminal.active_screen()? == Screen::Alternate || !self.terminal.viewport_active().unwrap_or(false) {
            return Ok(None);
        }
        let rows = u32::from(self.size.get().rows);
        let cursor = (self.terminal.cursor_x()?, u32::from(self.terminal.cursor_y()?));
        let (top, bottom) = wrapped_rows(&self.terminal, cursor.1, rows)?;
        let cols = self.size.get().cols;
        let mut prompt = Vec::with_capacity((bottom - top + 1) as usize * usize::from(cols));
        for y in top..=bottom {
            for x in 0..cols {
                let cell = self.terminal.grid_ref(Point::Active(PointCoordinate { x, y }))?.cell()?;
                prompt.push(cell.semantic_content()? == CellSemanticContent::Prompt);
            }
        }
        // 没有提示符标记时只能看宿主最近一次读到的前台进程。
        let at_prompt =
            if prompt.contains(&true) { self.terminal.is_cursor_at_prompt()? } else { self.foreground_is_shell() };
        if !at_prompt {
            return Ok(None);
        }
        // 输入正好写满一行时光标停在最后一格上、等着折行（pending wrap），这时它其实在那一格后面，
        // 和 `prompt_input::read` 一样多算一格。
        let past_last = usize::from(self.terminal.is_cursor_pending_wrap()?);
        // 输入从光标前最后一个提示符单元格之后开始；光标后面的提示符单元格是画在行尾的右侧
        // 提示符（zsh、fish 的集成标出来的），不算输入，也不算输入的末尾。
        let at = ((cursor.1 - top) as usize * usize::from(cols) + usize::from(cursor.0) + past_last).min(prompt.len());
        let prompt_end = prompt[..at].iter().rposition(|p| *p).map(|i| i + 1);
        let frame = self.frame();
        // 同步输出冻着画面时 `frame` 还是冻住前的，尺寸可能已经变了：行列对不上终端时不认。
        if frame.cols != cols || u32::from(frame.rows) <= bottom {
            return Ok(None);
        }
        let cells: Vec<&Cell> = (top..=bottom).flat_map(|y| frame.row(y as u16)).collect();
        let mut line = InputLine {
            top,
            bottom,
            cols: usize::from(frame.cols),
            spacer: cells.iter().map(|c| c.spacer).collect(),
            end: cells
                .iter()
                .enumerate()
                .rposition(|(i, c)| !c.text.is_empty() && !prompt.get(i).copied().unwrap_or(false))
                .map_or(0, |i| i + 1),
            start: prompt_end.unwrap_or(0),
            cursor: 0,
        };
        line.cursor = (line.index(cursor.0, cursor.1).unwrap_or(0) + past_last).min(line.spacer.len());
        Ok(Some(line))
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;

    #[test]
    fn click_to_move_counts_characters_on_the_cursor_line() {
        let mut session = idle_session();
        // 光标停在 "$ 你好 world" 末尾（第 12 列，两个宽字符各占两列）。
        session.feed("$ 你好 world".as_bytes());
        // 点在第 7 列的 w 上：往左跨过 "world" 五个字符。
        assert_eq!(session.click_to_move_steps(at(7.5, 0.5)), Some(-5));
        // 点在「你」上：宽字符各算一步，"你好 world" 共八步。
        assert_eq!(session.click_to_move_steps(at(2.5, 0.5)), Some(-8));
        // 文字末尾之后的空白、别的行都不动。
        assert_eq!(session.click_to_move_steps(at(17.5, 0.5)), None);
        assert_eq!(session.click_to_move_steps(at(3.5, 2.5)), None);
    }

    #[test]
    fn click_to_move_follows_soft_wraps() {
        let mut session = idle_session();
        // 20 列宽：24 个字符折成两行，光标在第二行第 4 列。
        session.feed(b"abcdefghijklmnopqrstuvwx");
        assert_eq!(session.click_to_move_steps(at(2.5, 0.5)), Some(-22));
        session.feed(b"\x1b[?1049h");
        assert_eq!(session.click_to_move_steps(at(2.5, 0.5)), None);
    }

    #[test]
    fn click_to_move_counts_past_a_full_line() {
        let mut session = idle_session();
        // 20 列宽：正好写满一行，光标停在最后一格上等着折行，其实在 t 后面。
        session.feed(b"abcdefghijklmnopqrst");
        assert_eq!(session.click_to_move_steps(at(4.5, 0.5)), Some(-16));
    }

    #[test]
    fn deleting_a_selection_moves_to_its_end_and_backspaces_over_it() {
        let mut session = idle_session();
        // 光标在末尾第 12 列；选中「好 w」（第 4 到 7 列，宽字符「好」占 4、5 两列）。
        session.feed("$ 你好 world".as_bytes());
        session.select_press(at(4.2, 0.5), REPEAT);
        session.select_drag(at(7.8, 0.5), false);
        session.select_release(at(7.8, 0.5));
        assert_eq!(session.selection_text().as_deref(), Some("好 w"));
        // 从第 12 列往左走到 w 之后（"orld" 四步），再退格三个字符。
        assert_eq!(session.delete_selection_keys().unwrap(), Some((-4, 3)));
    }

    #[test]
    fn deleting_a_selection_ignores_the_blank_after_the_text_and_other_lines() {
        let mut session = idle_session();
        session.feed(b"out\r\n$ ab");
        // 只选了文字后面的空白：没有可删的字符。
        session.select_press(at(8.2, 1.5), REPEAT);
        session.select_drag(at(12.8, 1.5), false);
        session.select_release(at(12.8, 1.5));
        assert_eq!(session.delete_selection_keys().unwrap(), None);
        // 选在上面的输出行里：不在输入行上。
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(2.8, 0.5), false);
        session.select_release(at(2.8, 0.5));
        assert_eq!(session.delete_selection_keys().unwrap(), None);
    }

    #[test]
    fn marked_prompts_bound_clicks_and_deletes_to_the_input() {
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(b"hello");
        // 点在提示符上：最多退到输入开头（第 2 列），"hello" 五步。
        assert_eq!(session.click_to_move_steps(at(0.5, 0.5)), Some(-5));
        // 选区把提示符也选进去了：只删输入里的 "hel"。
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(4.8, 0.5), false);
        session.select_release(at(4.8, 0.5));
        assert_eq!(session.delete_selection_keys().unwrap(), Some((-2, 3)));
        // 选区只落在提示符里：什么都不删，退格照常交给程序。
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_release(at(0.2, 0.5));
        assert_eq!(session.delete_selection_keys().unwrap(), None);
    }

    /// 行尾画着右侧提示符（`133;P;k=r` 到 `133;B`）：它既不是输入的开头，也不算输入的末尾。
    #[test]
    fn a_right_prompt_is_not_part_of_the_input() {
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(b"\x1b[16G\x1b]133;P;k=r\x07<r>\x1b]133;B\x07\x1b[3G");
        session.feed(b"hello");
        // 点在 e 上：往左四步。
        assert_eq!(session.click_to_move_steps(at(3.5, 0.5)), Some(-4));
        // 点在右侧提示符上：停在输入末尾，不动。
        assert_eq!(session.click_to_move_steps(at(16.5, 0.5)), None);
        // 选中 "ell"：先往左走到 l 之后（一步），再退格三个字符。
        session.select_press(at(3.2, 0.5), REPEAT);
        session.select_drag(at(5.8, 0.5), false);
        session.select_release(at(5.8, 0.5));
        assert_eq!(session.delete_selection_keys().unwrap(), Some((-1, 3)));
    }

    #[test]
    fn marked_prompts_tell_when_a_command_is_running() {
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(b"sleep 9\r\n\x1b]133;C\x07");
        // 命令在跑：光标在输出区，不算停在提示符上。
        assert_eq!(session.click_to_move_steps(at(0.5, 1.5)), None);
    }
}
