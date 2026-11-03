//! 搜索：搜索栏打开期间的 libghostty 搜索，以及把视口里的匹配换算成高亮段。

use libghostty_vt::{
    search::{MatchBuffer, Scroll, Search},
    terminal::{Point, PointCoordinate, PointSpace, Terminal},
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
        // 选中匹配后视口由 `reveal_selected` 来滚，libghostty 自己滚只滚到刚好露出。
        let search = match &mut self.search {
            Some(search) => search,
            None => {
                let mut search = Search::new(&mut self.terminal)?;
                search.set_scroll(Scroll::None)?;
                self.search.insert(search)
            }
        };
        // 空的 needle 清掉搜索，回到空闲。
        if needle.is_empty() {
            search.set_needle(&mut self.terminal, b"")?;
        } else {
            search.set_needle(&mut self.terminal, needle.as_bytes())?;
            search.run(&mut self.terminal)?;
            search.select_next(&mut self.terminal)?;
            // 选中的匹配可能在视口最后一行：平滑滚动错开的半行会把它挡掉一截。
            self.scroll_offset = 0.;
            self.reveal_selected()?;
        }
        Ok(())
    }

    /// 选中的匹配不在视口里时滚到让它落在中间，前后几行都看得到；已经看得到时不动。
    fn reveal_selected(&mut self) -> libghostty_vt::error::Result<()> {
        let Some(search) = &mut self.search else { return Ok(()) };
        if let Some(row) = hidden_selected_row(search, &mut self.terminal)? {
            self.scroll_to_row(row);
        }
        Ok(())
    }

    /// 选中下一个（更旧的）匹配，`backward` 时选上一个（更新的）；必要时滚动视口。
    pub fn search_step(&mut self, backward: bool) {
        let Some(search) = &mut self.search else {
            return;
        };
        let result = search.run(&mut self.terminal).and_then(|()| {
            if backward { search.select_prev(&mut self.terminal) } else { search.select_next(&mut self.terminal) }
        });
        // 同 `try_search`，不留平滑滚动错开的半行。
        self.scroll_offset = 0.;
        log_err("search step", result.and_then(|_| self.reveal_selected()));
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

/// 选中的匹配开头在视口外时，它在整个屏幕（含回滚历史）中的行号；没选中或已经看得到时为 `None`。
fn hidden_selected_row(
    search: &mut Search<'static, 'static>,
    terminal: &mut Terminal<'static, 'static>,
) -> libghostty_vt::error::Result<Option<u32>> {
    let snapshot = search.snapshot(terminal)?;
    let terminal = snapshot.terminal();
    let Some(selected) = snapshot.selected_match()? else { return Ok(None) };
    if terminal.point_from_grid_ref(&selected.start(), PointSpace::Viewport)?.is_some() {
        return Ok(None);
    }
    Ok(terminal.point_from_grid_ref(&selected.start(), PointSpace::Screen)?.map(|point| point.y))
}

/// 让搜索追上终端的最新内容，再把视口里的匹配换算成逐行的高亮段。
pub(super) fn search_highlights(
    search: &mut Search<'static, 'static>,
    terminal: &mut Terminal<'static, 'static>,
) -> libghostty_vt::error::Result<Vec<Highlight>> {
    search.run(terminal)?;
    let snapshot = search.snapshot(terminal)?;
    let terminal = snapshot.terminal();
    let cols = terminal.cols()?;
    let rows = terminal.rows()?;
    let selected = snapshot.selected_match()?;
    let mut highlights = Vec::new();
    let mut matches = MatchBuffer::new();
    for found in snapshot.viewport_matches(&mut matches)? {
        let start = terminal.point_from_grid_ref(&found.start(), PointSpace::Viewport)?;
        let end = terminal.point_from_grid_ref(&found.end(), PointSpace::Viewport)?;
        // 一端在视口外的匹配按视口边缘截断。两端都在外面的，列出来可能只因为和视口在同一页上，
        // 整个在视口上面或下面，不画；也可能比视口还长、盖住了整个视口，整个视口都画。
        let ((x0, y0), (x1, y1)) = match (start, end) {
            (None, None) => {
                let screen = |grid_ref| terminal.point_from_grid_ref(grid_ref, PointSpace::Screen);
                let top = terminal.grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))?;
                match (screen(&found.start())?, screen(&found.end())?, screen(&top)?) {
                    (Some(start), Some(end), Some(top)) if start.y < top.y && end.y > top.y => {
                        ((0, 0), (cols.saturating_sub(1), u32::from(rows.saturating_sub(1))))
                    }
                    _ => continue,
                }
            }
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
