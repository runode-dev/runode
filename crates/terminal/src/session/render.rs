//! 帧：把 libghostty 的 render state 复制进 `Frame`，叠上选区、搜索高亮和光标的颜色。

use libghostty_vt::{
    render::{CellIterator, CursorVisualStyle, Dirty, Overscan, RenderState, RowIterator, Snapshot},
    screen::CellWide,
    style::Underline,
    terminal::Terminal,
};
use runode_shared_types::{
    color::{Rgb, TerminalColor},
    frame::{Attrs, Cell, Cursor, CursorShape, Frame},
    theme,
};

use super::{SYNC_OUTPUT_TIMEOUT, Session, convert::rgb, log_err, search::search_highlights};

/// 把 libghostty 的 render state 复制进 `Frame`。和 render-hold 回调共用，
/// 因为同步更新一开始，回调就要立即截下当前帧。
pub(super) struct Renderer {
    render_state: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    pub(super) frame: Frame,
    /// 配置的选区颜色；没配时选区反色显示。
    pub(super) selection_bg: Option<TerminalColor>,
    pub(super) selection_fg: Option<TerminalColor>,
    /// 配置的光标颜色。固定色已交给 VT 作默认光标色，这里只用来解析跟随单元格的两种。
    pub(super) cursor_color: Option<TerminalColor>,
    /// 配置的光标下文字颜色；没配时用背景色。
    pub(super) cursor_text: Option<TerminalColor>,
    /// 视口里的搜索匹配，按行切成段。
    highlights: Vec<Highlight>,
    /// 搜索匹配的颜色：普通匹配和选中匹配各一对（背景、文字）。
    pub(super) search_colors: [(TerminalColor, TerminalColor); 2],
    /// 高亮或颜色变了，下一次刷新不能只画脏行。
    force_full: bool,
}

/// 视口第 `y` 行从 `x0` 到 `x1`（含）的一段搜索匹配。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Highlight {
    pub(super) y: u16,
    pub(super) x0: u16,
    pub(super) x1: u16,
    pub(super) selected: bool,
}

impl Session {
    /// 终端当前的 16 个 ANSI 颜色，见 `palette`。
    pub fn ansi_colors(&self) -> [Rgb; 16] {
        let palette = self.palette();
        std::array::from_fn(|i| palette[i])
    }

    /// 终端当前的 256 色调色板，跟着主题、配置和程序用 OSC 4 改的颜色走。读不到时用默认
    /// 前景色代替。
    pub fn palette(&self) -> [Rgb; 256] {
        match self.terminal.color_palette() {
            Ok(palette) => std::array::from_fn(|i| rgb(palette.0[i])),
            Err(err) => {
                tracing::warn!("failed to read the palette: {err}");
                [self.peek_colors().0; 256]
            }
        }
    }

    /// 上一帧的默认前景色和背景色，不触发刷新；画搜索栏这类界面元素时用。
    pub fn peek_colors(&self) -> (Rgb, Rgb) {
        let renderer = self.renderer.borrow();
        (renderer.frame.foreground, renderer.frame.background)
    }

    /// 运行中的程序是否正为同步更新冻结屏幕；即使没有新输出，
    /// 过了 `SYNC_OUTPUT_TIMEOUT` 视图也必须重绘。
    pub fn render_held(&self) -> bool {
        self.held_since.get().is_some()
    }

    /// 取出最新的帧，让绘制方能同时持有其他可变状态。用完用 `restore_frame` 放回。
    pub fn take_frame(&mut self) -> Frame {
        self.sync_frame();
        std::mem::take(&mut self.renderer.borrow_mut().frame)
    }

    pub fn restore_frame(&mut self, frame: Frame) {
        self.renderer.borrow_mut().frame = frame;
    }

    pub fn frame(&mut self) -> std::cell::Ref<'_, Frame> {
        self.sync_frame();
        std::cell::Ref::map(self.renderer.borrow(), |r| &r.frame)
    }

    fn sync_frame(&mut self) {
        if let Some(since) = self.held_since.get() {
            if since.elapsed() < SYNC_OUTPUT_TIMEOUT {
                return;
            }
            // 程序崩溃或忘了释放时，不能让屏幕永远冻结：超时后照常画，这次冻结就不管了，等程序
            // 关掉再重新开始冻结（render-hold 回调）时再遵守。VT 里的 mode 2026 不动：界面这份
            // VT 只跟着字节流变，和宿主那份一样，程序查询这个模式时宿主答的也是它。
            self.held_since.set(None);
        }
        let highlights = match &mut self.search {
            Some(search) => {
                log_err("search highlights", search_highlights(search, &mut self.terminal)).unwrap_or_default()
            }
            None => Vec::new(),
        };
        let mut renderer = self.renderer.borrow_mut();
        if renderer.highlights != highlights {
            renderer.highlights = highlights;
            renderer.force_full = true;
        }
        if let Err(err) = renderer.refresh(&self.terminal) {
            tracing::warn!("render state update failed: {err}");
        }
        let frame = &mut renderer.frame;
        frame.scroll_offset = if frame.above.is_empty() { 0. } else { self.scroll_offset };
    }
}

impl Renderer {
    pub(super) fn new() -> libghostty_vt::error::Result<Self> {
        let mut render_state = RenderState::new()?;
        // 多取视口上面一行：平滑滚动时画面往下错开，顶上要露出它的一部分。
        render_state.set_overscan(Overscan { above: 1, below: 0 })?;
        Ok(Self {
            render_state,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
            frame: Frame::default(),
            selection_bg: None,
            selection_fg: None,
            cursor_color: None,
            cursor_text: None,
            highlights: Vec::new(),
            // 应用配置之前用默认配色里的搜索高亮。
            search_colors: {
                use theme::{SEARCH_BACKGROUND, SEARCH_FOREGROUND, SEARCH_SELECTED_BACKGROUND};
                [
                    (TerminalColor::Rgb(SEARCH_BACKGROUND), TerminalColor::Rgb(SEARCH_FOREGROUND)),
                    (TerminalColor::Rgb(SEARCH_SELECTED_BACKGROUND), TerminalColor::Rgb(SEARCH_FOREGROUND)),
                ]
            },
            force_full: false,
        })
    }

    pub(super) fn refresh(&mut self, terminal: &Terminal<'static, '_>) -> libghostty_vt::error::Result<()> {
        let snapshot = self.render_state.update(terminal)?;
        let dirty = snapshot.dirty()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let colors = snapshot.colors()?;
        let background = rgb(colors.background);
        let foreground = rgb(colors.foreground);

        // VT 里的光标色（配置的固定色，或程序用 OSC 12 设的）优先。
        let cursor_colors =
            (colors.cursor.map(|color| TerminalColor::Rgb(rgb(color))).or(self.cursor_color), self.cursor_text);

        let frame = &mut self.frame;
        let reshaped = frame.cols != cols || frame.rows != rows;
        let recolored = frame.background != background || frame.foreground != foreground;
        if dirty == Dirty::Clean && !reshaped && !recolored && !self.force_full {
            // 只改光标形状或闪烁（DECSCUSR、DEC 模式 12）不会让 render state 变脏，光标要每次都重读。
            frame.cursor = read_cursor(&snapshot, frame, cursor_colors)?;
            return Ok(());
        }
        if reshaped {
            frame.cols = cols;
            frame.rows = rows;
            frame.cells = vec![Cell::default(); usize::from(cols) * usize::from(rows)];
        }
        // 视口上面那一行这次有没有取到；刚出现时它的脏标记不一定反映我们这边是空的，要整行读。
        let above_len = if snapshot.overscan()?.above > 0 { usize::from(cols) } else { 0 };
        let above_fresh = frame.above.len() != above_len;
        if above_fresh {
            frame.above = vec![Cell::default(); above_len];
        }
        frame.background = background;
        frame.foreground = foreground;
        let full = dirty == Dirty::Full || reshaped || recolored || std::mem::take(&mut self.force_full);

        let mut row_it = self.row_it.update(&snapshot)?;
        while let Some(row) = row_it.next() {
            // 视口里的行从 0 数，视口上面那一行是 -1。
            let y = row.viewport_y()?;
            if y >= i32::from(rows) {
                break;
            }
            let cells = match usize::try_from(y) {
                Ok(y) => &mut frame.cells[y * usize::from(cols)..(y + 1) * usize::from(cols)],
                Err(_) => &mut frame.above[..],
            };
            if cells.is_empty() {
                continue;
            }
            if full || (y < 0 && above_fresh) || row.dirty()? {
                let selection = row.selection()?;
                let mut cell_it = self.cell_it.update(row)?;
                let mut x = 0usize;
                while let Some(cell) = cell_it.next() {
                    if x >= usize::from(cols) {
                        break;
                    }
                    let out = &mut cells[x];
                    // 一次批量读取拿到渲染所需的全部字段，比逐个 getter 少几次 FFI。
                    let read = cell.read(&colors.palette, &mut out.text)?;
                    out.wide = read.wide == CellWide::Wide;
                    out.spacer = matches!(read.wide, CellWide::SpacerTail | CellWide::SpacerHead);
                    let mut fg = read.fg_color.map_or(foreground, rgb);
                    let mut bg = read.bg_color.map(rgb);
                    out.attrs = Attrs::default();
                    if read.has_styling {
                        let style = read.style;
                        out.attrs = Attrs {
                            bold: style.bold,
                            italic: style.italic,
                            faint: style.faint,
                            underline: style.underline != Underline::None,
                            strikethrough: style.strikethrough,
                        };
                        if style.inverse {
                            let swapped = bg.unwrap_or(background);
                            bg = Some(fg);
                            fg = swapped;
                        }
                        if style.invisible {
                            out.text.clear();
                        }
                    }
                    let x16 = x as u16;
                    // 宽字符的右半格跟着左半格：匹配的终点只落在宽字符的头格上。
                    let tail = read.wide == CellWide::SpacerTail;
                    let highlight = self
                        .highlights
                        .iter()
                        .find(|h| i32::from(h.y) == y && h.x0 <= x16 && (x16 <= h.x1 || (tail && x16 == h.x1 + 1)));
                    if let Some(highlight) = highlight {
                        let cell_bg = bg.unwrap_or(background);
                        let (hl_bg, hl_fg) = self.search_colors[usize::from(highlight.selected)];
                        bg = Some(resolve(hl_bg, fg, cell_bg));
                        fg = resolve(hl_fg, fg, cell_bg);
                    }
                    // 选中的单元格用配置的选区颜色。没配底色时用统一的蓝色、文字不变：
                    // 逐格反色会让彩色文字变成一块块彩色底，看起来像高亮而不像选区。
                    out.selected = selection.is_some_and(|s| s.start_x <= x16 && x16 <= s.end_x);
                    if out.selected {
                        let cell_bg = bg.unwrap_or(background);
                        let text = fg;
                        bg = Some(match self.selection_bg {
                            Some(c) => resolve(c, text, cell_bg),
                            None => default_selection_bg(background),
                        });
                        fg = match (self.selection_fg, self.selection_bg) {
                            (Some(c), _) => resolve(c, text, cell_bg),
                            (None, Some(_)) => cell_bg,
                            (None, None) => text,
                        };
                    }
                    out.fg = fg;
                    out.bg = bg;
                    x += 1;
                }
                row.set_dirty(false)?;
            }
        }

        frame.cursor = read_cursor(&snapshot, frame, cursor_colors)?;
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(())
    }
}

/// 把配置的颜色按单元格的前景、背景色解析成具体值。
fn default_selection_bg(background: Rgb) -> Rgb {
    let Rgb(r, g, b) = background;
    let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luma < 128. { theme::SELECTION_ON_DARK } else { theme::SELECTION_ON_LIGHT }
}

fn resolve(color: TerminalColor, fg: Rgb, bg: Rgb) -> Rgb {
    match color {
        TerminalColor::Rgb(color) => color,
        TerminalColor::CellForeground => fg,
        TerminalColor::CellBackground => bg,
    }
}

/// 视口里可见的光标；`frame` 的单元格和默认颜色须已是最新，用来判断光标是否落在
/// 宽字符上，以及解析跟随单元格的颜色。第三个参数是配置的光标色和光标下文字色，
/// 没配时分别用前景色和背景色。
fn read_cursor(
    snapshot: &Snapshot<'_, '_>,
    frame: &Frame,
    (color, text): (Option<TerminalColor>, Option<TerminalColor>),
) -> libghostty_vt::error::Result<Option<Cursor>> {
    if !snapshot.cursor_visible()? {
        return Ok(None);
    }
    let Some(vp) = snapshot.cursor_viewport()? else {
        return Ok(None);
    };
    let shape = match snapshot.cursor_visual_style()? {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
        _ => CursorShape::Block,
    };
    let index = usize::from(vp.y) * usize::from(frame.cols) + usize::from(vp.x);
    let cell = frame.cells.get(index);
    let cell_fg = cell.map_or(frame.foreground, |c| c.fg);
    let cell_bg = cell.and_then(|c| c.bg).unwrap_or(frame.background);
    Ok(Some(Cursor {
        x: vp.x,
        y: vp.y,
        shape,
        color: color.map_or(frame.foreground, |c| resolve(c, cell_fg, cell_bg)),
        text: text.map_or(frame.background, |c| resolve(c, cell_fg, cell_bg)),
        wide: cell.is_some_and(|c| c.wide),
        blinking: snapshot.cursor_blinking()?,
    }))
}
