//! 搜索：搜索栏打开期间的 libghostty 搜索，以及把视口里的匹配换算成高亮段。

use libghostty_vt::{
    search::Search,
    terminal::{PointSpace, Terminal},
};

use super::{Session, log_err, render::Highlight};

impl Session {
    /// 搜索 `needle`，并选中最新的一个匹配（必要时滚过去）；`needle` 为空时清掉匹配。
    /// 还没在搜索时开始搜索。
    pub fn search(&mut self, needle: &str) {
        let result = self.try_search(needle);
        log_err("search", result);
    }

    fn try_search(&mut self, needle: &str) -> libghostty_vt::error::Result<()> {
        let search = match &mut self.search {
            Some(search) => search,
            None => self.search.insert(Search::new(&mut self.terminal)?),
        };
        if needle.is_empty() {
            search.clear_needle(&mut self.terminal)?;
        } else {
            search.set_needle(&mut self.terminal, needle)?;
            search.run(&mut self.terminal)?;
            search.select_next(&mut self.terminal)?;
        }
        Ok(())
    }

    /// 选中下一个（更旧的）匹配，`backward` 时选上一个（更新的）；必要时滚动视口。
    pub fn search_step(&mut self, backward: bool) {
        let Some(search) = &mut self.search else {
            return;
        };
        let result = search.run(&mut self.terminal).and_then(|()| {
            if backward {
                search.select_prev(&mut self.terminal)
            } else {
                search.select_next(&mut self.terminal)
            }
        });
        log_err("search step", result);
    }

    /// 搜索栏上显示的进度：选中的是第几个（从 0 数，没选中为 `None`）和匹配总数。
    pub fn search_status(&self) -> Option<(Option<usize>, usize)> {
        let search = self.search.as_ref()?;
        let selected = search.selected_index().ok().flatten();
        Some((selected, search.total_matches().unwrap_or(0)))
    }

    /// 关掉搜索，清掉高亮。
    pub fn end_search(&mut self) {
        self.search = None;
    }
}

/// 让搜索追上终端的最新内容，再把视口里的匹配换算成逐行的高亮段。
pub(super) fn search_highlights(
    search: &mut Search<'static>,
    terminal: &mut Terminal<'static, 'static>,
) -> libghostty_vt::error::Result<Vec<Highlight>> {
    search.feed(terminal)?;
    search.run(terminal)?;
    let terminal = &*terminal;
    let cols = terminal.cols()?;
    let rows = terminal.rows()?;
    let selected = search.selected_match(terminal)?;
    let mut highlights = Vec::new();
    for found in search.viewport_matches(terminal)? {
        let start = terminal.point_from_grid_ref(&found.start(), PointSpace::Viewport)?;
        let end = terminal.point_from_grid_ref(&found.end(), PointSpace::Viewport)?;
        // 一端在视口外的匹配按视口边缘截断；两端都在外面的不画。
        let ((x0, y0), (x1, y1)) = match (start, end) {
            (None, None) => continue,
            (start, end) => (
                start.map_or((0, 0), |p| (p.x, p.y)),
                end.map_or((cols.saturating_sub(1), u32::from(rows.saturating_sub(1))), |p| (p.x, p.y)),
            ),
        };
        let is_selected = match &selected {
            Some(selected) => selected.equals(terminal, &found)?,
            None => false,
        };
        for y in y0..=y1.min(u32::from(rows.saturating_sub(1))) {
            highlights.push(Highlight {
                y: y as u16,
                x0: if y == y0 { x0 } else { 0 },
                x1: if y == y1 { x1 } else { cols.saturating_sub(1) },
                selected: is_selected,
            });
        }
    }
    Ok(highlights)
}

#[cfg(test)]
mod tests {
    use crate::session::testing::*;
    use runode_shared_types::theme;

    #[test]
    fn search_highlights_matches_and_steps_through_them() {
        let mut session = idle_session();
        session.feed(b"error one\r\nok\r\nerror two");
        session.search("error");
        // 先选中最新的那个。
        assert_eq!(session.search_status(), Some((Some(0), 2)));
        let frame = session.frame();
        let selected = theme::SEARCH_SELECTED_BACKGROUND;
        let other = theme::SEARCH_BACKGROUND;
        assert_eq!(frame.row(2)[0].bg, Some(selected));
        assert_eq!(frame.row(2)[4].bg, Some(selected));
        assert_eq!(frame.row(2)[5].bg, None);
        assert_eq!(frame.row(0)[0].bg, Some(other));
        assert_eq!(frame.row(1)[0].bg, None);
        drop(frame);

        session.search_step(false);
        assert_eq!(session.search_status(), Some((Some(1), 2)));
        assert_eq!(session.frame().row(0)[0].bg, Some(selected));

        // 新输出里的匹配也会算进来。
        session.feed(b"\r\nerror three");
        session.frame();
        assert_eq!(session.search_status().map(|(_, total)| total), Some(3));

        session.end_search();
        assert_eq!(session.search_status(), None);
        assert_eq!(session.frame().row(0)[0].bg, None);
    }

    #[test]
    fn search_highlight_covers_both_halves_of_a_trailing_wide_char() {
        let mut session = idle_session();
        session.feed("a你好b".as_bytes());
        session.search("你好");
        let frame = session.frame();
        let selected = Some(theme::SEARCH_SELECTED_BACKGROUND);
        let colored: Vec<bool> = (0..6).map(|x| frame.row(0)[x].bg == selected).collect();
        assert_eq!(colored, [false, true, true, true, true, false]);
    }
}
