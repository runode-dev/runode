//! Managing [render states](RenderState) of the terminal.

use std::{convert::Into, marker::PhantomData, mem::MaybeUninit};

use crate::{
    alloc::{Allocator, Object},
    error::{Error, Result, from_optional_result, from_result},
    ffi,
    screen::{Cell, CellWide, Row},
    style::{RgbColor, Style, StyleColor},
    terminal::Terminal,
};

pub use ffi::RenderStateRowSelection as RowSelection;

/// A number of rows above and below the viewport.
///
/// This is used both to [request](RenderState::set_overscan) overscan and to
/// [report](Snapshot::overscan) how many rows an update captured. See
/// [Overscan](RenderState#overscan) for how the extra rows are used.
pub use ffi::RenderStateOverscan as Overscan;

/// Represents the state required to render a visible screen (a viewport) of
/// a terminal instance.
///
/// This is stateful and optimized for repeated updates from a single terminal
/// instance and only updating dirty regions of the screen.
///
/// The key design principle of this API is that it only needs read/write
/// access to the terminal instance during the update call. This allows the
/// render state to minimally impact terminal IO performance and also allows
/// the renderer to be safely multi-threaded (as long as a lock is held
/// during the update call to ensure exclusive access to the terminal instance).
///
/// The basic usage of this API is:
///
///  1. Create an empty render state
///  2. Update it from a terminal instance whenever you need.
///  3. Read from the render state to get the data needed to draw your frame.
///
/// # Dirty Tracking
///
/// Dirty tracking is a key feature of the render state that allows renderers
/// to efficiently determine what parts of the screen have changed and only
/// redraw changed regions.
///
/// The render state API keeps track of dirty state at two independent layers:
/// a global dirty state that indicates whether the entire frame is clean,
/// partially dirty, or fully dirty, and a per-row dirty state that allows
/// tracking which rows in a partially dirty frame have changed.
///
/// The user of the render state API is expected to unset both of these.
/// The update call does not unset dirty state, it only updates it. After
/// successfully rendering a complete frame, use [`Snapshot::clean`] to unset
/// both layers in one call. The granular setters remain available for
/// callers that only consume part of a frame.
///
/// An extremely important detail: **setting one dirty state doesn't unset
/// the other.** For example, setting the global dirty state to false does
/// not reset the row-level dirty flags. So, the caller of the render state
/// API must be careful to manage both layers of dirty state correctly.
///
/// # Overscan
///
/// By default, the render state captures exactly the rows visible in the
/// viewport. That is all a renderer needs when it draws whole rows.
///
/// Some renderers draw the grid shifted by a fraction of a row, most commonly
/// to scroll smoothly. While the grid is shifted, part of a row just outside
/// the viewport becomes visible at one edge, and the renderer needs that
/// row's content to draw it. Overscan asks the render state to capture extra
/// rows above and below the viewport for this purpose.
///
/// Request overscan with [`RenderState::set_overscan`]. The request applies to
/// every update after it is set. [Row iterations](RowIteration) then visit
/// the extra rows along with the viewport, from top to bottom: the rows above
/// the viewport, the viewport rows, and then the rows below it.
/// [`RowIteration::viewport_y`] tells you where each row belongs. Rows above
/// the viewport have negative values, viewport rows are 0 through
/// [`rows`](Snapshot::rows) - 1, and rows below the viewport start at
/// [`rows`](Snapshot::rows).
///
/// Extra rows are only captured when they exist. There is nothing above the
/// first line of scrollback, and there is nothing below the viewport while it
/// is scrolled to the bottom, which is the usual case. After an update,
/// [`Snapshot::overscan`] reports how many rows were actually captured on
/// each side. Don't shift the grid toward a side where nothing was captured.
///
/// Extra rows carry the same data as viewport rows, including cells, styles,
/// dirty flags, and selection. The cursor is only reported when it is inside
/// the viewport.
///
/// The render state doesn't store the scroll position. If you need it, read
/// [`Terminal::scrollbar`](crate::Terminal::scrollbar) or
/// [`Terminal::viewport_active`](crate::Terminal::viewport_active) at the same
/// time as you call [`RenderState::begin_update`], while you have exclusive
/// access to the terminal. The values then describe the same moment as the
/// render state.
///
/// # Row identity
///
/// Every row has an [id](RowIteration::id) that stays with the row as it
/// moves. When the viewport scrolls by one row, each row shows up one
/// position higher or lower in the next update but keeps its id. Ids work
/// with or without overscan.
///
/// Ids let a renderer keep expensive per-row work, such as shaped text or a
/// prepared texture, in its own cache keyed by id. A cached entry can be
/// reused when both of these are true:
///
///  1. A row with the same id is present in the new update.
///  2. That row's [dirty flag](RowIteration::dirty) is not set.
///
/// The dirty flag is conservative. A row may be marked dirty even though its
/// content didn't change. For example, every row is currently marked dirty
/// after the viewport scrolls. Rebuilding a dirty row is always correct.
///
/// An id disappears when its row is no longer captured, is removed from
/// scrollback, or is changed in place by the terminal (for example, when a
/// program scrolls only part of the screen). Ids are never reused, so an old
/// id can never match a different row. Cache entries for ids that no longer
/// appear can be discarded.
///
/// # Examples
///
/// ## Creating and updating render state
///
/// ```rust
/// // Create a terminal and render state, then update the render state
/// // from the terminal. The render state captures a snapshot of everything
/// // needed to draw a frame.
/// use libghostty_vt::{Terminal, RenderState};
///
/// let mut terminal = Terminal::new(40, 5).unwrap();
/// let mut render_state = RenderState::new().unwrap();
///
/// // Feed some styled content into the terminal.
/// terminal.vt_write(b"Hello, \x1b[1;32mworld\x1b[0m!\r\n");
/// terminal.vt_write(b"\x1b[4munderlined\x1b[0m text\r\n");
/// terminal.vt_write(b"\x1b[38;2;255;128;0morange\x1b[0m\r\n");
///
/// assert!(render_state.update(&terminal).is_ok());
/// ```
///
/// ## Splitting an update
///
/// ```rust
/// use libghostty_vt::{RenderState, Terminal};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let terminal = Terminal::new(80, 25)?;
/// let mut render_state = RenderState::new()?;
///
/// // Use `update` unless you need to minimize how long terminal access is
/// // held. `begin_update` copies the terminal-dependent state into an update
/// // token, then `end` finishes the deferred render-state work.
/// let update = render_state.begin_update(&terminal)?;
///
/// // Terminal access is no longer needed here.
/// let snapshot = update.end()?;
///
/// // Read from the snapshot to draw the frame.
/// let _dirty = snapshot.dirty()?;
/// # Ok(())}
/// ```
///
/// ## Checking dirty state
///
/// ```rust
/// // Check the global dirty state to decide how much work the renderer
/// // needs to do. After rendering, reset it to false.
/// # use libghostty_vt::{Terminal, RenderState, render::Dirty};
/// # let terminal = Terminal::new(80, 25).unwrap();
/// # let mut render_state = RenderState::new().unwrap();
/// let snapshot = render_state.update(&terminal).unwrap();
///
/// match snapshot.dirty().unwrap() {
///     Dirty::Clean => println!("Frame is clean, nothing to draw."),
///     Dirty::Partial => println!("Partial redraw needed."),
///     Dirty::Full => println!("Full redraw needed."),
/// }
/// ```
///
/// ## Reading colors
///
/// ```rust
/// // Retrieve colors (background, foreground, palette) from the render
/// // state. These are needed to resolve palette-indexed cell colors.
/// # use libghostty_vt::{Terminal, RenderState};
/// # let terminal = Terminal::new(80, 25).unwrap();
/// # let mut render_state = RenderState::new().unwrap();
/// let snapshot = render_state.update(&terminal).unwrap();
/// let colors = snapshot.colors().unwrap();
///
/// println!(
///     "Background: {:02x}{:02x}{:02x}",
///     colors.background.r, colors.background.g, colors.background.b
/// );
/// println!(
///     "Foreground: {:02x}{:02x}{:02x}",
///     colors.background.r, colors.background.g, colors.background.b
/// );
/// ```
///
/// ## Reading cursor state
///
/// ```rust
/// // Read cursor position and visual style from the render state.
/// use libghostty_vt::render::CursorViewport;
/// # use libghostty_vt::{Terminal, RenderState};
/// # let terminal = Terminal::new(80, 25).unwrap();
/// # let mut render_state = RenderState::new().unwrap();
/// let snapshot = render_state.update(&terminal).unwrap();
///
/// if snapshot.cursor_visible().unwrap() {
///     if let Some(CursorViewport { x, y, .. }) = snapshot.cursor_viewport().unwrap() {
///         let style = snapshot.cursor_visual_style().unwrap();
///         println!("Cursor at ({x}, {y}), style {style:?}");
///     }
/// }
/// ```
///
/// ## Scrolling smoothly with overscan
///
/// ```rust
/// // Draw one frame of a smooth scroll. `offset_px` comes from the renderer's
/// // own scroll animation: how far the grid is shifted up, from zero up to
/// // but not including one row height.
/// use libghostty_vt::{RenderState, Terminal, render::{Overscan, RowIterator}};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let terminal = Terminal::new(80, 25)?;
/// # let cell_height = 16;
/// # let mut offset_px = 4;
/// # let mut draw_row = |_: &_, _: i32| {};
/// let mut render_state = RenderState::new()?;
/// let mut rows = RowIterator::new()?;
///
/// // Once, when setting up the render state. One row below the viewport is
/// // enough to draw the partially visible row at the bottom edge.
/// render_state.set_overscan(Overscan { above: 0, below: 1 })?;
///
/// // Each frame.
/// let snapshot = render_state.update(&terminal)?;
///
/// // With no row below the viewport, there is nothing to scroll into.
/// if snapshot.overscan()?.below == 0 {
///     offset_px = 0;
/// }
///
/// let mut row_iter = rows.update(&snapshot)?;
/// while let Some(row) = row_iter.next() {
///     draw_row(row, row.viewport_y()? * cell_height - offset_px);
/// }
/// # Ok(())}
/// ```
///
/// ## Caching per-row work by id
///
/// ```rust
/// use std::collections::HashMap;
/// use libghostty_vt::{RenderState, Terminal, render::{RowId, RowIterator}};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let terminal = Terminal::new(80, 25)?;
/// # let mut render_state = RenderState::new()?;
/// # let mut rows = RowIterator::new()?;
/// # let prepare = |_: &_| ();
/// // The renderer's own map from row id to prepared row.
/// let mut cache: HashMap<RowId, ()> = HashMap::new();
///
/// let snapshot = render_state.update(&terminal)?;
/// let mut row_iter = rows.update(&snapshot)?;
/// while let Some(row) = row_iter.next() {
///     let id = row.id()?;
///     if row.dirty()? || !cache.contains_key(&id) {
///         cache.insert(id, prepare(row));
///     }
/// }
/// # Ok(())}
/// ```
///
/// ## Iterating rows and cells
///
/// ```rust
/// // Iterate rows via the row iterator. For each dirty row, iterate its
/// // cells, read codepoints/graphemes and styles, and emit ANSI-colored
/// // output as a simple "renderer".
/// use libghostty_vt::{Terminal, RenderState};
/// use libghostty_vt::style::Underline;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let terminal = Terminal::new(80, 25).unwrap();
/// # let mut render_state = RenderState::new()?;
/// use libghostty_vt::render::{RowIterator, CellIterator};
///
/// // During setup:
/// let mut rows = RowIterator::new()?;
/// let mut cells = CellIterator::new()?;
///
/// // On each frame:
/// let snapshot = render_state.update(&terminal)?;
/// let colors = snapshot.colors()?;
///
/// let mut row_iter = rows.update(&snapshot)?;
/// let mut row_index = 0;
///
/// while let Some(row) = row_iter.next() {
///     // Check per-row dirty state; a real renderer would skip clean rows.
///     print!(
///         "Row {row_index} [{}]",
///         if row.dirty()? { "dirty" } else { "clean" }
///     );
///
///     // Get cells for this row (reuses the same cells handle).
///     let mut cell_iter = cells.update(&row)?;
///     while let Some(cell) = cell_iter.next() {
///         let graphemes = cell.graphemes()?;
///
///         if graphemes.is_empty() {
///             print!(" ");
///             continue;
///         }
///
///         // Resolve foreground color for this cell.
///         let fg = cell.fg_color()?.unwrap_or(colors.foreground);
///         // Emit ANSI true-color escape for the foreground.
///         print!("\x1b[38;2;{};{};{}m", fg.r, fg.g, fg.b);
///
///         // Read the style for this cell. Returns the default style for
///         // cells that have no explicit styling.
///         let style = cell.style()?;
///         if style.bold {
///             print!("\x1b[1m");
///         }
///         if style.underline != Underline::None {
///             print!("\x1b[4m");
///         }
///
///         for grapheme in graphemes {
///             print!("{}", grapheme.escape_default());
///         }
///         print!("\x1b[0m"); // Reset style after each cell.
///     }
///     println!();
///
///     // Clear per-row dirty flag after "rendering" it.
///     row.set_dirty(false);
///
///     row_index += 1;
/// }
/// # Ok(())}
/// ```
#[derive(Debug)]
pub struct RenderState<'alloc>(Object<'alloc, ffi::RenderStateImpl>);

/// A snapshot of the render state after an update.
///
/// This struct exists to guard data accessed from the render state from
/// being accidentally modified after an update. If you find yourself unable
/// to update the render state due to borrow checker errors, make sure to
/// drop the active snapshot (and data that depends on it) before updating.
#[derive(Debug)]
pub struct Snapshot<'alloc, 's>(&'s mut RenderState<'alloc>);

/// An in-progress render state update.
///
/// This token is returned by [`RenderState::begin_update`] and keeps the render
/// state borrowed until [`Self::end`] completes the deferred update work. This
/// makes it impossible to read from the render state while it is incomplete.
#[derive(Debug)]
pub struct Update<'alloc, 's> {
    state: Option<&'s mut RenderState<'alloc>>,
}

/// Opaque handle to a render-state row iterator.
///
/// The row iterator must be [updated](RowIterator::update) from a snapshot of
/// the render state in order to function, as most data is only accessible
/// per [iteration](RowIteration).
///
/// The iteration visits every row the last update captured, from top to
/// bottom. This is exactly the viewport unless
/// [overscan](RenderState#overscan) was requested.
#[derive(Debug)]
pub struct RowIterator<'alloc>(Object<'alloc, ffi::RenderStateRowIteratorImpl>);

/// An active iteration over the rows in the render state.
///
/// Row iterations are created by [updating](RowIterator::update) row iterators
/// with a snapshot of the render state. The borrow checker statically
/// guarantees that all accesses of the data do not outlive the given snapshot,
/// at the cost of added lifetime annotations.
#[derive(Debug)]
pub struct RowIteration<'alloc, 's> {
    iter: &'s mut RowIterator<'alloc>,
    // NOTE: While in theory the snapshot borrow should have its own
    // lifetime 'ss where 'rs: 'ss, but it gets very unwieldy and honestly
    // one wouldn't run into too many situations where this simpler constraint
    // isn't enough.
    _phan: PhantomData<&'s Snapshot<'alloc, 's>>,
}

/// Opaque handle to a render state cell iterator.
///
/// The cell iterator must be [updated](CellIterator::update) from a
/// [row](RowIteration) in order to function, as most data is only
/// accessible per [iteration](CellIteration).
#[derive(Debug)]
pub struct CellIterator<'alloc>(Object<'alloc, ffi::RenderStateRowCellsImpl>);

/// An active iteration over the cells on a given row
/// within the render state.
///
/// Cell iterations are created by [updating](CellIterator::update) row iterators
/// at a given [row](RowIteration). The borrow checker statically
/// guarantees that all accesses of the data do not outlive the given snapshot,
/// at the cost of added lifetime annotations.
#[derive(Debug)]
pub struct CellIteration<'alloc, 's> {
    iter: &'s mut CellIterator<'alloc>,
    _phan: PhantomData<&'s RowIteration<'alloc, 's>>,
}

//--------------------------
// Impl blocks
//--------------------------

impl<'alloc> RenderState<'alloc> {
    /// Create a new render state instance.
    pub fn new() -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null()) }
    }

    /// Create a new render state instance with a custom allocator.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(alloc: &'alloc Allocator<'ctx>) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw()) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator) -> Result<Self> {
        let mut raw: ffi::RenderState = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_render_state_new(alloc, &raw mut raw) };
        from_result(result)?;
        Ok(Self(Object::new(raw)?))
    }

    /// Update a render state instance from a terminal,
    /// returning a new [snapshot](Snapshot).
    ///
    /// This consumes terminal/screen dirty state in the same way as the
    /// internal render state update path.
    ///
    /// # Errors
    ///
    /// Returns `Err(Error::OutOfMemory)` if updating the state requires
    /// allocation and that allocation fails.
    pub fn update<'cb>(&mut self, terminal: &Terminal<'alloc, 'cb>) -> Result<Snapshot<'alloc, '_>> {
        let result = unsafe { ffi::ghostty_render_state_update(self.0.as_raw(), terminal.inner.as_raw()) };
        from_result(result)?;
        Ok(Snapshot(self))
    }

    /// Begin an update of a render state instance from a terminal.
    ///
    /// Every begin must be completed with [`Update::end`] before the render
    /// state is read.
    ///
    /// This two-phase structure exists for callers that synchronize access to
    /// the terminal state: only this function requires terminal access, so a
    /// caller can hold its lock for this call only and then call [`Update::end`]
    /// after releasing it. The end phase exclusively reads
    /// and writes memory owned by the render state, so it is safe to call while
    /// the terminal is being modified.
    ///
    /// Work that doesn't require terminal access may be deferred to the end
    /// phase to keep this call, and therefore lock hold time, as short as
    /// possible. Callers must treat the render state as incomplete until
    /// [`Update::end`] is called.
    ///
    /// This consumes terminal and screen dirty state in the same way as the
    /// internal render state update path.
    pub fn begin_update<'cb>(&mut self, terminal: &Terminal<'alloc, 'cb>) -> Result<Update<'alloc, '_>> {
        let result = unsafe { ffi::ghostty_render_state_begin_update(self.0.as_raw(), terminal.inner.as_raw()) };
        from_result(result)?;
        Ok(Update { state: Some(self) })
    }

    fn get<T>(&self, tag: ffi::RenderStateData::Type) -> Result<T> {
        let mut value = MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_render_state_get(self.0.as_raw(), tag, value.as_mut_ptr().cast()) };
        // Since we manually model every possible query, this should never fail.
        from_result(result)?;
        // SAFETY: Value should be initialized after successful call.
        Ok(unsafe { value.assume_init() })
    }

    fn set<T>(&self, tag: ffi::RenderStateOption::Type, value: &T) -> Result<()> {
        let result = unsafe { ffi::ghostty_render_state_set(self.0.as_raw(), tag, std::ptr::from_ref(value).cast()) };
        // Since we manually model every possible query, this should never fail.
        from_result(result)
    }

    /// Request [overscan](Self#overscan) rows above and below the viewport.
    ///
    /// The request takes effect on the next update and stays in effect until
    /// it is changed. Both sides are zero by default, which captures only the
    /// viewport. Expect a full redraw on the update after a change.
    pub fn set_overscan(&mut self, request: Overscan) -> Result<&mut Self> {
        self.set(ffi::RenderStateOption::OVERSCAN, &request)?;
        Ok(self)
    }

    /// The overscan request most recently set with [`Self::set_overscan`].
    ///
    /// The next update uses this request. Both sides are zero if it was never
    /// set.
    pub fn overscan_request(&self) -> Result<Overscan> {
        self.get(ffi::RenderStateData::OVERSCAN_REQUEST)
    }
}

impl Drop for RenderState<'_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_render_state_free(self.0.as_raw()) }
    }
}

impl<'alloc, 's> Update<'alloc, 's> {
    /// Complete a prior [`RenderState::begin_update`] call by performing any deferred work.
    ///
    /// This only reads and writes memory owned by the render state, so it is
    /// safe to call while the terminal is being modified. Consumes the update
    /// token and returns a snapshot that can be read to draw the frame.
    pub fn end(mut self) -> Result<Snapshot<'alloc, 's>> {
        let Some(state) = self.state.take() else {
            return Err(Error::InvalidValue);
        };
        let result = unsafe { ffi::ghostty_render_state_end_update(state.0.as_raw()) };
        from_result(result)?;
        Ok(Snapshot(state))
    }
}

impl Drop for Update<'_, '_> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            let _ = unsafe { ffi::ghostty_render_state_end_update(state.0.as_raw()) };
        }
    }
}

impl Snapshot<'_, '_> {
    fn get<T>(&self, tag: ffi::RenderStateData::Type) -> Result<T> {
        self.0.get(tag)
    }

    fn set<T>(&self, tag: ffi::RenderStateOption::Type, value: &T) -> Result<()> {
        self.0.set(tag, value)
    }

    /// Get the current dirty state.
    pub fn dirty(&self) -> Result<Dirty> {
        self.get::<ffi::RenderStateDirty::Type>(ffi::RenderStateData::DIRTY)
            .and_then(|v| v.try_into().map_err(|_| Error::InvalidValue))
    }

    /// Get the viewport width.
    pub fn cols(&self) -> Result<u16> {
        self.get(ffi::RenderStateData::COLS)
    }

    /// Get the viewport height.
    ///
    /// This does not include [overscan](RenderState#overscan) rows.
    pub fn rows(&self) -> Result<u16> {
        self.get(ffi::RenderStateData::ROWS)
    }

    /// How many [overscan](RenderState#overscan) rows this update captured on
    /// each side.
    ///
    /// This is never more than the [request](RenderState::overscan_request).
    /// It is less when those rows don't exist: `above` is smaller near the top
    /// of the scrollback, and `below` is zero while the viewport is scrolled
    /// to the bottom.
    pub fn overscan(&self) -> Result<Overscan> {
        self.get(ffi::RenderStateData::OVERSCAN)
    }

    /// Get the cursor color that may have been explicitly set by the terminal state.
    pub fn cursor_color(&self) -> Result<Option<RgbColor>> {
        let has_value = self.get(ffi::RenderStateData::COLOR_CURSOR_HAS_VALUE)?;
        if has_value {
            let color = self.get(ffi::RenderStateData::COLOR_CURSOR)?;
            Ok(Some(color))
        } else {
            Ok(None)
        }
    }

    /// Whether the cursor is currently visible based on terminal modes.
    pub fn cursor_visible(&self) -> Result<bool> {
        self.get(ffi::RenderStateData::CURSOR_VISIBLE)
    }

    /// Whether the cursor is currently blinking based on terminal modes.
    pub fn cursor_blinking(&self) -> Result<bool> {
        self.get(ffi::RenderStateData::CURSOR_BLINKING)
    }

    /// Whether the cursor is at a password input field.
    pub fn cursor_password_input(&self) -> Result<bool> {
        self.get(ffi::RenderStateData::CURSOR_PASSWORD_INPUT)
    }

    /// Get the visual style of the cursor.
    pub fn cursor_visual_style(&self) -> Result<CursorVisualStyle> {
        self.get::<ffi::RenderStateCursorVisualStyle::Type>(ffi::RenderStateData::CURSOR_VISUAL_STYLE)
            .and_then(|v| v.try_into().map_err(|_| Error::InvalidValue))
    }

    /// Get the relative position of the cursor and other information
    /// if it is currently visible within the viewport.
    pub fn cursor_viewport(&self) -> Result<Option<CursorViewport>> {
        let has_value = self.get(ffi::RenderStateData::CURSOR_VIEWPORT_HAS_VALUE)?;
        if has_value {
            let x = self.get(ffi::RenderStateData::CURSOR_VIEWPORT_X)?;
            let y = self.get(ffi::RenderStateData::CURSOR_VIEWPORT_Y)?;
            let at_wide_tail = self.get(ffi::RenderStateData::CURSOR_VIEWPORT_WIDE_TAIL)?;
            Ok(Some(CursorViewport { x, y, at_wide_tail }))
        } else {
            Ok(None)
        }
    }

    /// All cursor state in one call.
    ///
    /// This is equivalent to the individual `cursor_*` getters, but needs a
    /// single query instead of one per property.
    pub fn cursor(&self) -> Result<Cursor> {
        let mut raw = ffi::sized!(ffi::RenderStateCursor);
        from_result(unsafe {
            ffi::ghostty_render_state_get(self.0.0.as_raw(), ffi::RenderStateData::CURSOR, (&raw mut raw).cast())
        })?;
        Cursor::from_raw(raw)
    }

    /// Mark all dirty render-state data as consumed.
    ///
    /// This sets the global [dirty state](Self::dirty) to [`Dirty::Clean`] and
    /// clears every per-row dirty flag. It is idempotent and does not modify
    /// cell contents or dirty state owned by the terminal. Call this only
    /// after a complete frame has been rendered successfully; partial
    /// consumers should use [`Self::set_dirty`] and [`RowIteration::set_dirty`]
    /// instead.
    pub fn clean(&self) -> Result<()> {
        from_result(unsafe { ffi::ghostty_render_state_clean(self.0.0.as_raw()) })
    }

    /// Get the current color information from a render state.
    pub fn colors(&self) -> Result<Colors> {
        let mut colors = ffi::sized!(ffi::RenderStateColors);
        let result = unsafe {
            ffi::ghostty_render_state_get(self.0.0.as_raw(), ffi::RenderStateData::COLORS, (&raw mut colors).cast())
        };
        from_result(result)?;
        Ok(Colors::from_raw(&colors))
    }

    /// 一次 FFI 调用读取多个渲染状态字段，key 见 [`query`]。
    ///
    /// 结果与逐个调用对应的 getter 一致，详见 [`crate::multi`]。
    pub fn get_multi<K: crate::multi::Keys<crate::multi::domain::RenderState>>(
        &self,
        keys: K,
    ) -> Result<K::Output<'_>> {
        let state = self.0.0.as_raw();
        // SAFETY: 参数原样交给 `ghostty_render_state_get_multi`。
        unsafe {
            keys.read(&mut |tags, values, written| {
                ffi::ghostty_render_state_get_multi(state, tags.len(), tags.as_ptr(), values.as_mut_ptr(), written)
            })
        }
    }

    /// Set dirty state.
    pub fn set_dirty(&self, dirty: Dirty) -> Result<()> {
        self.set(ffi::RenderStateOption::DIRTY, &(dirty as ffi::RenderStateDirty::Type))
    }
}

impl<'alloc> RowIterator<'alloc> {
    /// Create a new row iterator instance.
    pub fn new() -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null()) }
    }

    /// Create a new cell iterator instance with a custom allocator.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(alloc: &'alloc Allocator<'ctx>) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw()) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator) -> Result<Self> {
        let mut raw: ffi::RenderStateRowIterator = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_render_state_row_iterator_new(alloc, &raw mut raw) };
        from_result(result)?;
        Ok(Self(Object::new(raw)?))
    }

    /// Update the row iterator for a snapshot of the render state,
    /// returning a new row iteration.
    ///
    /// The iteration borrows the snapshot, so it cannot outlive it:
    ///
    /// ```compile_fail,E0505
    /// use libghostty_vt::{Terminal, RenderState, render::RowIterator};
    /// let terminal = Terminal::new(8, 2).unwrap();
    /// let mut state = RenderState::new().unwrap();
    /// let snapshot = state.update(&terminal).unwrap();
    /// let mut rows = RowIterator::new().unwrap();
    /// let mut iteration = rows.update(&snapshot).unwrap();
    /// drop(snapshot); // Iteration still borrows its owning snapshot.
    /// iteration.next();
    /// ```
    pub fn update<'s>(&'s mut self, snapshot: &'s Snapshot<'alloc, '_>) -> Result<RowIteration<'alloc, 's>> {
        let result = unsafe {
            ffi::ghostty_render_state_get(
                snapshot.0.0.as_raw(),
                ffi::RenderStateData::ROW_ITERATOR,
                std::ptr::from_mut(&mut self.0.ptr).cast(),
            )
        };
        from_result(result)?;

        Ok(RowIteration { iter: self, _phan: PhantomData })
    }
}

impl Drop for RowIterator<'_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_render_state_row_iterator_free(self.0.as_raw()) }
    }
}

impl<'s> RowIteration<'_, 's> {
    /// The raw cell values for the current row, one per column.
    ///
    /// This is identical to querying [`CellIteration::raw_cell`] for each
    /// cell, and is the bulk alternative to iterating cells one at a time.
    ///
    /// The values are only valid as long as the underlying render state is
    /// not updated, so they borrow the snapshot rather than this row: the
    /// iteration may keep advancing while they are in use.
    ///
    /// ```compile_fail,E0505
    /// use libghostty_vt::{RenderState, Terminal, render::RowIterator};
    /// let terminal = Terminal::new(8, 2).unwrap();
    /// let mut state = RenderState::new().unwrap();
    /// let snapshot = state.update(&terminal).unwrap();
    /// let mut rows = RowIterator::new().unwrap();
    /// let mut iteration = rows.update(&snapshot).unwrap();
    /// let cells = iteration.next().unwrap().cells_raw().unwrap();
    /// drop(snapshot); // The cells still borrow the snapshot.
    /// cells.count();
    /// ```
    pub fn cells_raw(&self) -> Result<impl ExactSizeIterator<Item = Cell> + use<'s>> {
        let view: ffi::CellsView = self.get(ffi::RenderStateRowData::CELLS_RAW)?;
        // An empty row's view comes from an empty Zig slice, whose pointer
        // need not be dereferenceable or aligned, so never build a slice
        // from it.
        let cells: &'s [ffi::Cell] = if view.len == 0 {
            &[]
        } else {
            // SAFETY: libghostty keeps the view valid until the render state
            // is updated, which the snapshot borrow `'s` rules out. The only
            // writes possible meanwhile are to dirty flags (`set_dirty`,
            // `clean`), which live outside the cells. Each element is the
            // cell's `u64` value (`ffi::Cell`), which `Cell` wraps by value.
            unsafe { std::slice::from_raw_parts(view.ptr, view.len) }
        };
        Ok(cells.iter().copied().map(Cell))
    }
}

impl RowIteration<'_, '_> {
    /// Move a row iteration to the next row requiring a redraw.
    ///
    /// If the global dirty state is [`Dirty::Clean`], this returns `None`. If
    /// it is [`Dirty::Partial`], clean rows are skipped. If it is
    /// [`Dirty::Full`], every remaining row is returned regardless of its
    /// per-row dirty flag. Rows are returned in ascending viewport order,
    /// together with their position in the iteration. This does not clear any
    /// dirty state.
    ///
    /// Without [overscan](RenderState#overscan), the position is the viewport
    /// y. With overscan, it counts from the highest captured row, so use
    /// [`Self::viewport_y`] to place the row.
    pub fn next_dirty(&mut self) -> Option<(u16, &Self)> {
        let mut y = 0;
        // The receiver is evaluated before the arguments, so `y` is read only
        // after libghostty has written it.
        unsafe { ffi::ghostty_render_state_row_iterator_next_dirty(self.iter.0.as_raw(), &raw mut y) }
            .then_some((y, self))
    }

    /// Move a row iteration to the next row.
    ///
    /// Returns `Some(row)` if the iteration moved successfully and row
    /// data is available to read at the new position using `row`.
    #[expect(clippy::should_implement_trait, reason = "lending `next` cannot implement trait")]
    pub fn next(&mut self) -> Option<&Self> {
        if unsafe { ffi::ghostty_render_state_row_iterator_next(self.iter.0.as_raw()) } { Some(self) } else { None }
    }

    fn get<T>(&self, tag: ffi::RenderStateRowData::Type) -> Result<T> {
        let mut value = MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_render_state_row_get(self.iter.0.as_raw(), tag, value.as_mut_ptr().cast()) };
        // Since we manually model every possible query, this should never fail.
        from_result(result)?;
        // SAFETY: Value should be initialized after successful call.
        Ok(unsafe { value.assume_init() })
    }

    fn set<T>(&self, tag: ffi::RenderStateRowOption::Type, value: &T) -> Result<()> {
        let result =
            unsafe { ffi::ghostty_render_state_row_set(self.iter.0.as_raw(), tag, std::ptr::from_ref(value).cast()) };
        from_result(result)
    }

    /// 一次 FFI 调用读取当前行的多个字段，key 见 [`row_query`]。
    ///
    /// 结果与逐个调用对应的 getter 一致，详见 [`crate::multi`]。
    pub fn get_multi<K: crate::multi::Keys<crate::multi::domain::RenderRow>>(&self, keys: K) -> Result<K::Output<'_>> {
        let iter = self.iter.0.as_raw();
        // SAFETY: 参数原样交给 `ghostty_render_state_row_get_multi`。
        unsafe {
            keys.read(&mut |tags, values, written| {
                ffi::ghostty_render_state_row_get_multi(iter, tags.len(), tags.as_ptr(), values.as_mut_ptr(), written)
            })
        }
    }

    /// Whether the current row is dirty.
    pub fn dirty(&self) -> Result<bool> {
        self.get(ffi::RenderStateRowData::DIRTY)
    }

    /// The row's [identity](RenderState#row-identity) across updates.
    ///
    /// This works with or without [overscan](RenderState#overscan).
    pub fn id(&self) -> Result<RowId> {
        self.get::<ffi::RenderStateRowId>(ffi::RenderStateRowData::ID).map(|id| RowId(id.bits))
    }

    /// The row's position relative to the top of the viewport.
    ///
    /// Viewport rows are 0 through [`rows`](Snapshot::rows) - 1.
    /// [Overscan](RenderState#overscan) rows above the viewport are negative,
    /// and overscan rows below it start at [`rows`](Snapshot::rows). Without
    /// overscan, this equals the position reported by [`Self::next_dirty`].
    pub fn viewport_y(&self) -> Result<i32> {
        self.get(ffi::RenderStateRowData::VIEWPORT_Y)
    }

    /// The raw row value.
    pub fn raw_row(&self) -> Result<Row> {
        self.get(ffi::RenderStateRowData::RAW).map(Row)
    }

    /// Set dirty state for the current row.
    pub fn set_dirty(&self, dirty: bool) -> Result<()> {
        self.set(ffi::RenderStateRowOption::DIRTY, &dirty)
    }

    /// Row-local selected cell range.
    pub fn selection(&self) -> Result<Option<RowSelection>> {
        let mut value = ffi::sized!(RowSelection);
        let result = unsafe {
            ffi::ghostty_render_state_row_get(
                self.iter.0.as_raw(),
                ffi::RenderStateRowData::SELECTION,
                std::ptr::from_mut(&mut value).cast(),
            )
        };
        // Since we manually model every possible query, this should never fail.
        // SAFETY: Value should be initialized after successful call.
        from_optional_result(result, value)
    }
}

impl<'alloc> CellIterator<'alloc> {
    /// Create a new cell iterator instance.
    pub fn new() -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null()) }
    }

    /// Create a new cell iterator instance with a custom allocator.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(alloc: &'alloc Allocator<'ctx>) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw()) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator) -> Result<Self> {
        let mut raw: ffi::RenderStateRowCells = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_render_state_row_cells_new(alloc, &raw mut raw) };
        from_result(result)?;
        Ok(Self(Object::new(raw)?))
    }

    /// Update the cell iterator for a new row iteration,
    /// returning a new cell iteration.
    ///
    /// The iteration borrows the row, so it cannot outlive it:
    ///
    /// ```compile_fail,E0505
    /// use libghostty_vt::{
    ///     RenderState, Terminal,
    ///     render::{CellIterator, RowIterator},
    /// };
    /// let terminal = Terminal::new(8, 2).unwrap();
    /// let mut state = RenderState::new().unwrap();
    /// let snapshot = state.update(&terminal).unwrap();
    /// let mut rows = RowIterator::new().unwrap();
    /// let mut row = rows.update(&snapshot).unwrap();
    /// row.next();
    /// let mut cells = CellIterator::new().unwrap();
    /// let mut iteration = cells.update(&row).unwrap();
    /// drop(row); // Iteration still borrows its owning row.
    /// iteration.next();
    /// ```
    pub fn update<'s>(&'s mut self, row: &'s RowIteration<'alloc, '_>) -> Result<CellIteration<'alloc, 's>> {
        let result = unsafe {
            ffi::ghostty_render_state_row_get(
                row.iter.0.as_raw(),
                ffi::RenderStateRowData::CELLS,
                std::ptr::from_mut(&mut self.0.ptr).cast(),
            )
        };
        from_result(result)?;

        Ok(CellIteration { iter: self, _phan: PhantomData })
    }
}

impl Drop for CellIterator<'_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_render_state_row_cells_free(self.0.as_raw()) }
    }
}

impl CellIteration<'_, '_> {
    /// Move a cell iteration to the next cell.
    ///
    /// Returns `Some(cell)` if the iteration moved successfully and cell
    /// data is available to read at the new position using `cell`.
    #[expect(clippy::should_implement_trait, reason = "lending `next` cannot implement trait")]
    pub fn next(&mut self) -> Option<&Self> {
        if unsafe { ffi::ghostty_render_state_row_cells_next(self.iter.0.as_raw()) } { Some(self) } else { None }
    }

    /// Move a cell iteration to a specific column.
    ///
    /// Positions the iteration at the given x (column) index so that
    /// subsequent reads return data for that cell.
    pub fn select(&mut self, x: u16) -> Result<()> {
        let result = unsafe { ffi::ghostty_render_state_row_cells_select(self.iter.0.as_raw(), x) };
        from_result(result)
    }

    fn get<T>(&self, tag: ffi::RenderStateRowCellsData::Type) -> Result<T> {
        let mut value = MaybeUninit::<T>::zeroed();
        let result =
            unsafe { ffi::ghostty_render_state_row_cells_get(self.iter.0.as_raw(), tag, value.as_mut_ptr().cast()) };
        from_result(result)?;
        // SAFETY: Value should be initialized after successful call.
        Ok(unsafe { value.assume_init() })
    }

    /// 一次 FFI 调用读取当前单元格的多个字段，key 见 [`cell_query`]。
    ///
    /// 结果与逐个调用对应的 getter 一致，详见 [`crate::multi`]。渲染循环的
    /// 常见组合请直接用 [`CellIteration::read`]。
    pub fn get_multi<K: crate::multi::Keys<crate::multi::domain::RenderCell>>(&self, keys: K) -> Result<K::Output<'_>> {
        let iter = self.iter.0.as_raw();
        // SAFETY: 参数原样交给 `ghostty_render_state_row_cells_get_multi`。
        unsafe {
            keys.read(&mut |tags, values, written| {
                ffi::ghostty_render_state_row_cells_get_multi(
                    iter,
                    tags.len(),
                    tags.as_ptr(),
                    values.as_mut_ptr(),
                    written,
                )
            })
        }
    }

    /// 读出渲染当前单元格通常需要的全部数据，并把字形簇的 UTF-8 编码写入
    /// `text`（先清空）。
    ///
    /// 等价于依次调用 [`raw_cell`](Self::raw_cell)、`raw_cell().wide()`、
    /// [`graphemes_len`](Self::graphemes_len)、
    /// [`graphemes_utf8`](Self::graphemes_utf8)、
    /// [`has_styling`](Self::has_styling)、[`style`](Self::style)、
    /// [`fg_color`](Self::fg_color) 和 [`bg_color`](Self::bg_color)，但通常
    /// 只需两次 FFI 调用：一次批量读取单元格字段，一次从原始单元格值解出
    /// 宽度（纯计算，不访问渲染状态）。
    ///
    /// `palette` 必须是同一快照的调色板，即 [`Snapshot::colors`] 返回的
    /// [`Colors::palette`]，每帧取一次即可。前景色在 Rust 侧用它解析：C 侧
    /// 的前景色就是“有样式时取样式的前景色，经调色板解析”，这样可以省掉一次
    /// 缺失时会中断批量读取的 FFI 调用。传入别的调色板只会得到错误的颜色，
    /// 不会造成内存问题。
    ///
    /// `text` 的分配会被复用，只在字形簇放不下时增长，所以一帧里所有单元格
    /// 可以共用一个 `String`。字形簇超出现有容量时会多一次 FFI 调用。出错时
    /// `text` 为空。
    pub fn read(&self, palette: &[RgbColor; 256], text: &mut String) -> Result<RenderCell> {
        use ffi::RenderStateRowCellsData as D;

        // 背景色可能缺失（C 返回 INVALID_VALUE 并中断批量读取），放在最后，
        // 缺失时不必再发起调用。字形簇缓冲区可能放不下，放在背景色之前，
        // 扩容后从它继续。
        const TAGS: [ffi::RenderStateRowCellsData::Type; 6] =
            [D::RAW, D::HAS_STYLING, D::STYLE, D::GRAPHEMES_LEN, D::GRAPHEMES_UTF8, D::BG_COLOR];
        const UTF8: usize = 4;
        const BG: usize = 5;

        text.clear();
        // SAFETY: 长度只会被设为 libghostty 成功写入的字节数，而它只写入
        // 完整的 UTF-8 码点（写入前会校验每个码点），字符串始终是合法 UTF-8。
        let bytes = unsafe { text.as_mut_vec() };
        // 绝大多数单元格是一个 ASCII 字符，预留一点容量让首次调用就能成功。
        bytes.reserve(16);

        let mut raw: ffi::Cell = 0;
        let mut has_styling = false;
        let mut style = ffi::sized!(ffi::Style);
        let mut graphemes_len: u32 = 0;
        let mut buf = ffi::Buffer { ptr: bytes.as_mut_ptr(), cap: bytes.capacity(), len: 0 };
        let mut bg = ffi::ColorRgb::default();
        let mut values: [*mut std::ffi::c_void; 6] = [
            (&raw mut raw).cast(),
            (&raw mut has_styling).cast(),
            (&raw mut style).cast(),
            (&raw mut graphemes_len).cast(),
            (&raw mut buf).cast(),
            (&raw mut bg).cast(),
        ];

        let mut bg_present = true;
        let mut start = 0;
        loop {
            let mut written = 0usize;
            let result = unsafe {
                ffi::ghostty_render_state_row_cells_get_multi(
                    self.iter.0.as_raw(),
                    TAGS.len() - start,
                    TAGS[start..].as_ptr(),
                    values[start..].as_mut_ptr(),
                    &raw mut written,
                )
            };
            if result == ffi::Result::SUCCESS {
                break;
            }
            match (start + written, result) {
                // 字形簇放不下：C 把所需字节数写进了 `buf.len`，什么也没写入
                // 缓冲区。扩容后从它继续。
                (UTF8, ffi::Result::OUT_OF_SPACE) => {
                    bytes.reserve(buf.len);
                    buf.ptr = bytes.as_mut_ptr();
                    buf.cap = bytes.capacity();
                    buf.len = 0;
                    start = UTF8;
                }
                // 没有背景色，与 `bg_color` 返回 `None` 一致。
                (BG, ffi::Result::INVALID_VALUE) => {
                    bg_present = false;
                    break;
                }
                (_, code) => return Err(from_result(code).err().unwrap_or(Error::InvalidValue)),
            }
        }
        // SAFETY: 成功时 libghostty 写入了 `buf.len <= cap` 个字节。
        unsafe { bytes.set_len(buf.len) };

        let raw = Cell(raw);
        let style = Style::try_from(style)?;
        // 与 C 的 FG_COLOR 相同：没有样式就没有前景色，否则取样式的前景色并
        // 经调色板解析。
        let fg_color = if has_styling {
            match style.fg_color {
                StyleColor::None => None,
                StyleColor::Palette(index) => Some(palette[usize::from(index.0)]),
                StyleColor::Rgb(rgb) => Some(rgb),
            }
        } else {
            None
        };
        Ok(RenderCell {
            raw,
            wide: raw.wide()?,
            graphemes_len: graphemes_len as usize,
            has_styling,
            style,
            fg_color,
            bg_color: bg_present.then(|| bg.into()),
        })
    }

    /// The raw cell value.
    pub fn raw_cell(&self) -> Result<Cell> {
        self.get(ffi::RenderStateRowCellsData::RAW).map(Cell)
    }

    /// The style for the current cell.
    pub fn style(&self) -> Result<Style> {
        let mut value = ffi::sized!(ffi::Style);
        let result = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.iter.0.as_raw(),
                ffi::RenderStateRowCellsData::STYLE,
                std::ptr::from_mut(&mut value).cast(),
            )
        };
        from_result(result)?;
        Style::try_from(value)
    }

    /// The resolved foreground color of the cell.
    ///
    /// Resolves palette indices through the palette. Bold color handling
    /// is not applied; the caller should handle bold styling separately.
    ///
    /// Returns `None` if the cell has no explicit foreground color, in which
    /// case the caller should use whatever default foreground color it want
    /// (e.g. the terminal foreground).
    pub fn fg_color(&self) -> Result<Option<RgbColor>> {
        let res = self.get::<ffi::ColorRgb>(ffi::RenderStateRowCellsData::FG_COLOR);
        match res {
            Ok(o) => Ok(Some(o.into())),
            Err(Error::InvalidValue) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The resolved background color of the cell.
    ///
    /// Flattens the three possible sources: [`Cell::bg_color_rgb`],
    /// [`Cell::bg_color_palette`] (looked up in the palette), or the
    /// style's [`bg_color`][Style::bg_color].
    ///
    /// Returns `None` if the cell has no background color, in which case the
    /// caller should use whatever default background color it wants
    /// (e.g. the terminal background).
    pub fn bg_color(&self) -> Result<Option<RgbColor>> {
        let res = self.get::<ffi::ColorRgb>(ffi::RenderStateRowCellsData::BG_COLOR);
        match res {
            Ok(o) => Ok(Some(o.into())),
            Err(Error::InvalidValue) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get the grapheme codepoints.
    ///
    /// The base codepoint is placed first, followed by any extra codepoints.
    pub fn graphemes(&self) -> Result<Vec<char>> {
        let len = self.graphemes_len()?;
        let mut graphemes = vec!['\0'; len];
        self.graphemes_buf(&mut graphemes)?;
        Ok(graphemes)
    }

    /// The total number of grapheme codepoints including the base codepoint.
    ///
    /// Returns 0 if the cell has no text.
    pub fn graphemes_len(&self) -> Result<usize> {
        self.get(ffi::RenderStateRowCellsData::GRAPHEMES_LEN)
    }

    /// Write grapheme codepoints into a caller-provided buffer.
    ///
    /// The buffer must be at least [`CellIteration::graphemes_len`] elements.
    /// The base codepoint is written first, followed by any extra codepoints.
    pub fn graphemes_buf(&self, buf: &mut [char]) -> Result<()> {
        let result = unsafe {
            ffi::ghostty_render_state_row_cells_get(
                self.iter.0.as_raw(),
                ffi::RenderStateRowCellsData::GRAPHEMES_BUF,
                buf.as_mut_ptr().cast(),
            )
        };
        from_result(result)
    }

    /// Replace the contents of `buf` with the current cell's full grapheme
    /// cluster, encoded as UTF-8.
    ///
    /// The base codepoint is encoded first, followed by any extra grapheme
    /// codepoints. A cell without text leaves `buf` empty.
    ///
    /// `buf`'s allocation is reused and only grows if the cluster does not
    /// fit, so one buffer can serve every cell of a frame. On error, `buf` is
    /// left empty.
    pub fn graphemes_utf8(&self, buf: &mut String) -> Result<()> {
        // libghostty writes from the start of the buffer, so start empty.
        buf.clear();
        // SAFETY: The length only ever grows to what libghostty reports
        // having written on success, and libghostty only writes whole UTF-8
        // encoded codepoints (it validates every codepoint before writing
        // any byte), so the string stays valid UTF-8.
        let bytes = unsafe { buf.as_mut_vec() };
        loop {
            let mut cbuf = ffi::Buffer { ptr: bytes.as_mut_ptr(), cap: bytes.capacity(), len: 0 };
            let result = unsafe {
                ffi::ghostty_render_state_row_cells_get(
                    self.iter.0.as_raw(),
                    ffi::RenderStateRowCellsData::GRAPHEMES_UTF8,
                    std::ptr::from_mut(&mut cbuf).cast(),
                )
            };
            match result {
                ffi::Result::SUCCESS => {
                    // SAFETY: libghostty wrote `cbuf.len <= cap` bytes.
                    unsafe { bytes.set_len(cbuf.len) };
                    return Ok(());
                }
                // `cbuf.len` is the size needed, and `bytes` is empty.
                ffi::Result::OUT_OF_SPACE => bytes.reserve(cbuf.len),
                ffi::Result::OUT_OF_MEMORY => return Err(Error::OutOfMemory),
                _ => return Err(Error::InvalidValue),
            }
        }
    }

    /// Whether the cell is contained within the current selection.
    ///
    /// This returns true when the cell's column is within the current row's
    /// row-local selection range, and false otherwise. Rendering policy for
    /// selected cells (colors, inversion, etc.) is left to the caller.
    ///
    /// Renderers that can draw cells in spans may be more efficient calling
    /// [`RowIteration::selection`] once per row and applying that range
    /// directly, avoiding one C API call per cell for selection state.
    pub fn is_selected(&self) -> Result<bool> {
        self.get(ffi::RenderStateRowCellsData::SELECTED)
    }

    /// Whether the cell has any explicit styling.
    ///
    /// This is equivalent to querying the raw cell's [`Cell::has_styling`]
    /// value, but avoids materializing the raw [`Cell`] for renderers that
    /// only need to know whether fetching the full style is necessary.
    pub fn has_styling(&self) -> Result<bool> {
        self.get(ffi::RenderStateRowCellsData::HAS_STYLING)
    }
}

/// [`Snapshot::get_multi`] 的 key。
///
/// 每个 key 的输出与同名 getter 相同。用于填充行迭代器的
/// `ROW_ITERATOR` 不在其列，请用 [`RowIterator::update`]。
pub mod query {
    use crate::{
        ffi,
        multi::{domain::RenderState as D, multi_key_full, multi_keys},
    };

    multi_keys! {
        domain = D, tags = ffi::RenderStateData;
        /// [`Snapshot::cols`](super::Snapshot::cols)
        Cols = COLS: copy u16;
        /// [`Snapshot::rows`](super::Snapshot::rows)
        Rows = ROWS: copy u16;
        /// [`Snapshot::dirty`](super::Snapshot::dirty)
        Dirty = DIRTY: try ffi::RenderStateDirty::Type => super::Dirty;
        /// [`Colors::background`](super::Colors::background)
        ColorBackground = COLOR_BACKGROUND: try ffi::ColorRgb => crate::style::RgbColor;
        /// [`Colors::foreground`](super::Colors::foreground)
        ColorForeground = COLOR_FOREGROUND: try ffi::ColorRgb => crate::style::RgbColor;
        /// [`Snapshot::cursor_color`](super::Snapshot::cursor_color)。没有显式
        /// 光标色时 C 返回 `GHOSTTY_INVALID_VALUE`，这里记为 `None`。
        ColorCursor = COLOR_CURSOR: opt[INVALID_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// 是否设置了显式光标色。
        ColorCursorHasValue = COLOR_CURSOR_HAS_VALUE: copy bool;
        /// [`Snapshot::cursor_visual_style`](super::Snapshot::cursor_visual_style)
        CursorVisualStyle = CURSOR_VISUAL_STYLE: try ffi::RenderStateCursorVisualStyle::Type => super::CursorVisualStyle;
        /// [`Snapshot::cursor_visible`](super::Snapshot::cursor_visible)
        CursorVisible = CURSOR_VISIBLE: copy bool;
        /// [`Snapshot::cursor_blinking`](super::Snapshot::cursor_blinking)
        CursorBlinking = CURSOR_BLINKING: copy bool;
        /// [`Snapshot::cursor_password_input`](super::Snapshot::cursor_password_input)
        CursorPasswordInput = CURSOR_PASSWORD_INPUT: copy bool;
        /// 光标是否在视口内。
        CursorViewportHasValue = CURSOR_VIEWPORT_HAS_VALUE: copy bool;
        /// 光标在视口内的列；不在视口内时为 `None`。
        CursorViewportX = CURSOR_VIEWPORT_X: opt[INVALID_VALUE] u16 => u16;
        /// 光标在视口内的行；不在视口内时为 `None`。
        CursorViewportY = CURSOR_VIEWPORT_Y: opt[INVALID_VALUE] u16 => u16;
        /// 光标是否在宽字符的尾部；不在视口内时为 `None`。
        CursorViewportWideTail = CURSOR_VIEWPORT_WIDE_TAIL: opt[INVALID_VALUE] bool => bool;
        /// [`Snapshot::overscan`](super::Snapshot::overscan)
        Overscan = OVERSCAN: copy ffi::RenderStateOverscan;
        /// [`RenderState::overscan_request`](super::RenderState::overscan_request)
        OverscanRequest = OVERSCAN_REQUEST: copy ffi::RenderStateOverscan;
    }

    multi_key_full! {
        /// [`Colors::palette`](super::Colors::palette)
        ColorPalette in D = ffi::RenderStateData::COLOR_PALETTE;
        raw crate::style::RawPalette = [ffi::ColorRgb::default(); 256];
        out<'a> [crate::style::RgbColor; 256];
        absent [];
        convert |raw, present| Ok(raw.map(Into::into))
    }

    multi_key_full! {
        /// [`Snapshot::cursor`](super::Snapshot::cursor)
        Cursor in D = ffi::RenderStateData::CURSOR;
        raw ffi::RenderStateCursor = ffi::sized!(ffi::RenderStateCursor);
        out<'a> super::Cursor;
        absent [];
        convert |raw, present| super::Cursor::from_raw(raw)
    }

    multi_key_full! {
        /// [`Snapshot::colors`](super::Snapshot::colors)
        Colors in D = ffi::RenderStateData::COLORS;
        raw ffi::RenderStateColors = ffi::sized!(ffi::RenderStateColors);
        out<'a> super::Colors;
        absent [];
        convert |raw, present| Ok(super::Colors::from_raw(&raw))
    }
}

/// [`RowIteration::get_multi`] 的 key。
///
/// 每个 key 的输出与同名 getter 相同。填充单元格迭代器的 `CELLS` 和借用
/// 行内数据的 `CELLS_RAW` 不在其列，请用 [`CellIterator::update`] 和
/// [`RowIteration::cells_raw`]。
pub mod row_query {
    use crate::{
        ffi,
        multi::{domain::RenderRow as D, multi_key_full, multi_keys},
    };

    multi_keys! {
        domain = D, tags = ffi::RenderStateRowData;
        /// [`RowIteration::dirty`](super::RowIteration::dirty)
        Dirty = DIRTY: copy bool;
        /// [`RowIteration::viewport_y`](super::RowIteration::viewport_y)
        ViewportY = VIEWPORT_Y: copy i32;
    }

    multi_key_full! {
        /// [`RowIteration::raw_row`](super::RowIteration::raw_row)
        Raw in D = ffi::RenderStateRowData::RAW;
        raw ffi::Row = 0;
        out<'a> crate::screen::Row;
        absent [];
        convert |raw, present| Ok(crate::screen::Row(raw))
    }

    multi_key_full! {
        /// [`RowIteration::id`](super::RowIteration::id)
        Id in D = ffi::RenderStateRowData::ID;
        raw ffi::RenderStateRowId = ffi::RenderStateRowId::default();
        out<'a> super::RowId;
        absent [];
        convert |raw, present| Ok(super::RowId(raw.bits))
    }

    multi_key_full! {
        /// [`RowIteration::selection`](super::RowIteration::selection)
        Selection in D = ffi::RenderStateRowData::SELECTION;
        raw ffi::RenderStateRowSelection = ffi::sized!(ffi::RenderStateRowSelection);
        out<'a> Option<super::RowSelection>;
        absent [ffi::Result::NO_VALUE];
        convert |raw, present| Ok(present.then_some(raw))
    }
}

/// [`CellIteration::get_multi`] 的 key。
///
/// 每个 key 的输出与同名 getter 相同。需要调用方缓冲区的
/// `GRAPHEMES_BUF`/`GRAPHEMES_UTF8` 不在其列，请用
/// [`CellIteration::read`] 或对应的 getter。
pub mod cell_query {
    use crate::{
        ffi,
        multi::{domain::RenderCell as D, multi_key_full, multi_keys},
    };

    multi_keys! {
        domain = D, tags = ffi::RenderStateRowCellsData;
        /// [`CellIteration::style`](super::CellIteration::style)
        Style = STYLE: sized ffi::Style => crate::style::Style;
        /// [`CellIteration::bg_color`](super::CellIteration::bg_color)。没有背景色
        /// 时 C 返回 `GHOSTTY_INVALID_VALUE`，这里记为 `None`。
        BgColor = BG_COLOR: opt[INVALID_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`CellIteration::fg_color`](super::CellIteration::fg_color)。没有前景色
        /// 时 C 返回 `GHOSTTY_INVALID_VALUE`，这里记为 `None`。
        FgColor = FG_COLOR: opt[INVALID_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`CellIteration::is_selected`](super::CellIteration::is_selected)
        Selected = SELECTED: copy bool;
        /// [`CellIteration::has_styling`](super::CellIteration::has_styling)
        HasStyling = HAS_STYLING: copy bool;
    }

    multi_key_full! {
        /// [`CellIteration::raw_cell`](super::CellIteration::raw_cell)
        Raw in D = ffi::RenderStateRowCellsData::RAW;
        raw ffi::Cell = 0;
        out<'a> crate::screen::Cell;
        absent [];
        convert |raw, present| Ok(crate::screen::Cell(raw))
    }

    multi_key_full! {
        /// [`CellIteration::graphemes_len`](super::CellIteration::graphemes_len)
        GraphemesLen in D = ffi::RenderStateRowCellsData::GRAPHEMES_LEN;
        // C 的输出类型是 uint32_t。
        raw u32 = 0;
        out<'a> usize;
        absent [];
        convert |raw, present| Ok(raw as usize)
    }
}

//---------------------------
// Auxiliary types
//---------------------------

/// Cursor viewport position information.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorViewport {
    /// Cursor viewport x position in cells.
    pub x: u16,
    /// Cursor viewport y position in cells.
    pub y: u16,
    /// Whether the cursor is on the tail of a wide character.
    pub at_wide_tail: bool,
}

/// Render-state cursor information, as returned by [`Snapshot::cursor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The cursor position if the cursor is visible within the viewport.
    pub viewport: Option<CursorViewport>,
    /// Whether the cursor is visible based on terminal modes.
    pub visible: bool,
    /// Whether the cursor should blink based on terminal modes.
    pub blinking: bool,
    /// Whether the cursor is at a password input field.
    pub password_input: bool,
    /// The visual style of the cursor.
    pub visual_style: CursorVisualStyle,
}

/// The [identity](RenderState#row-identity) of a row across render state
/// updates, as returned by [`RowIteration::id`].
///
/// Treat this value as opaque: two ids are the same row when they are equal,
/// and no other comparison or interpretation is meaningful. The contents may
/// change between library versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RowId([u64; 2]);

/// Render-state color information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    /// The default/current background color for the render state.
    pub background: RgbColor,
    /// The default/current foreground color for the render state.
    pub foreground: RgbColor,
    /// The cursor color which may be explicitly set by terminal state.
    pub cursor: Option<RgbColor>,
    /// The active 256-color palette for this render state.
    pub palette: [RgbColor; 256],
}

/// 渲染一个单元格通常需要的数据，由 [`CellIteration::read`] 一次读出。
///
/// 每个字段与 [`CellIteration`] 上同名 getter 的结果一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderCell {
    /// 原始单元格值，见 [`CellIteration::raw_cell`]。
    pub raw: Cell,
    /// 单元格宽度，即 `raw.wide()`。
    pub wide: CellWide,
    /// 字形簇的码点数（含基础码点），没有文本时为 0，见
    /// [`CellIteration::graphemes_len`]。
    pub graphemes_len: usize,
    /// 是否有显式样式，见 [`CellIteration::has_styling`]。
    pub has_styling: bool,
    /// 单元格样式，没有显式样式时为默认样式，见 [`CellIteration::style`]。
    pub style: Style,
    /// 解析后的前景色，见 [`CellIteration::fg_color`]。
    pub fg_color: Option<RgbColor>,
    /// 解析后的背景色，见 [`CellIteration::bg_color`]。
    pub bg_color: Option<RgbColor>,
}

impl Cursor {
    /// 把 C 的光标结构体转换为 [`Cursor`]，供 [`Snapshot::cursor`] 和批量读取
    /// 共用。
    fn from_raw(raw: ffi::RenderStateCursor) -> Result<Self> {
        Ok(Self {
            // The viewport fields are undefined unless `viewport_has_value`
            // is set, so they must not be read otherwise.
            viewport: raw.viewport_has_value.then_some(CursorViewport {
                x: raw.viewport_x,
                y: raw.viewport_y,
                at_wide_tail: raw.wide_tail,
            }),
            visible: raw.visible,
            blinking: raw.blinking,
            password_input: raw.password_input,
            visual_style: raw.visual_style.try_into().map_err(|_| Error::InvalidValue)?,
        })
    }
}

impl Colors {
    /// 把 C 的颜色结构体转换为 [`Colors`]，供 [`Snapshot::colors`] 和批量读取
    /// 共用。
    fn from_raw(colors: &ffi::RenderStateColors) -> Self {
        Self {
            background: colors.background.into(),
            foreground: colors.foreground.into(),
            cursor: if colors.cursor_has_value { Some(colors.cursor.into()) } else { None },
            palette: colors.palette.map(Into::into),
        }
    }
}

/// Dirty state of a render state after update.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
pub enum Dirty {
    /// Not dirty at all; rendering can be skipped.
    Clean = ffi::RenderStateDirty::FALSE,
    /// Some rows changed; renderer can redraw incrementally.
    Partial = ffi::RenderStateDirty::PARTIAL,
    /// Global state changed; renderer should redraw everything.
    Full = ffi::RenderStateDirty::FULL,
}

/// Visual style of the cursor.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[non_exhaustive]
pub enum CursorVisualStyle {
    /// Bar cursor (DECSCUSR 5, 6).
    Bar = ffi::RenderStateCursorVisualStyle::BAR,
    /// Block cursor (DECSCUSR 1, 2).
    Block = ffi::RenderStateCursorVisualStyle::BLOCK,
    /// Underline cursor (DECSCUSR 3, 4).
    Underline = ffi::RenderStateCursorVisualStyle::UNDERLINE,
    /// Hollow block cursor.
    BlockHollow = ffi::RenderStateCursorVisualStyle::BLOCK_HOLLOW,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    /// Guards the `set_dirty` → `update` → `dirty()` round-trip. If
    /// `Snapshot::set(value: &T)` calls `from_ref(&value)`, the result has
    /// type `*const &T` (a pointer to the local reference), not `*const T`.
    /// C reads stack-address bytes into the dirty field, the next `update`
    /// propagates them, and `dirty()` fails enum decoding.
    #[test]
    fn dirty_decodes_after_set_dirty_then_update() {
        let terminal = Terminal::new(8, 3).unwrap();
        let mut state = RenderState::new().unwrap();

        state.update(&terminal).unwrap().set_dirty(Dirty::Clean).unwrap();

        assert!(state.update(&terminal).unwrap().dirty().is_ok());
    }

    /// Build the expected bulk cursor from the individual getters.
    fn cursor_from_getters(snapshot: &Snapshot<'_, '_>) -> Cursor {
        Cursor {
            viewport: snapshot.cursor_viewport().unwrap(),
            visible: snapshot.cursor_visible().unwrap(),
            blinking: snapshot.cursor_blinking().unwrap(),
            password_input: snapshot.cursor_password_input().unwrap(),
            visual_style: snapshot.cursor_visual_style().unwrap(),
        }
    }

    #[test]
    fn bulk_cursor_matches_individual_getters() {
        let mut terminal = Terminal::new(8, 2).unwrap();
        let mut state = RenderState::new().unwrap();

        // Default cursor after writing a narrow character.
        terminal.vt_write(b"hi");
        let snapshot = state.update(&terminal).unwrap();
        let cursor = snapshot.cursor().unwrap();
        assert_eq!(cursor, cursor_from_getters(&snapshot));
        assert_eq!(cursor.viewport, Some(CursorViewport { x: 2, y: 0, at_wide_tail: false }));

        // Hidden blinking bar cursor on the tail of a wide character.
        terminal.vt_write("\x1b[?25l\x1b[5 q\r\n中\x1b[2G".as_bytes());
        let snapshot = state.update(&terminal).unwrap();
        let cursor = snapshot.cursor().unwrap();
        assert_eq!(cursor, cursor_from_getters(&snapshot));
        assert!(!cursor.visible);
        assert_eq!(cursor.visual_style, CursorVisualStyle::Bar);
        assert_eq!(cursor.viewport, Some(CursorViewport { x: 1, y: 1, at_wide_tail: true }));

        // Scrolling the cursor out of the viewport leaves no position.
        terminal.vt_write(b"\r\n\r\n\r\n");
        terminal.scroll_viewport(crate::terminal::ScrollViewport::Top);
        let snapshot = state.update(&terminal).unwrap();
        let cursor = snapshot.cursor().unwrap();
        assert_eq!(cursor, cursor_from_getters(&snapshot));
        assert_eq!(cursor.viewport, None);
    }

    /// Collect the viewport rows returned by `next_dirty`.
    fn dirty_rows<'alloc>(rows: &mut RowIterator<'alloc>, snapshot: &Snapshot<'alloc, '_>) -> Vec<u16> {
        let mut iteration = rows.update(snapshot).unwrap();
        let mut ys = Vec::new();
        while let Some((y, _)) = iteration.next_dirty() {
            ys.push(y);
        }
        ys
    }

    #[test]
    fn next_dirty_follows_global_and_row_dirty_state() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        // Park the cursor on the row we write to below: moving the cursor
        // also dirties the row it leaves.
        terminal.vt_write(b"\x1b[2;1H");
        let mut state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();

        // The first update is fully dirty, so every row is returned.
        let snapshot = state.update(&terminal).unwrap();
        assert_eq!(snapshot.dirty().unwrap(), Dirty::Full);
        assert_eq!(dirty_rows(&mut rows, &snapshot), [0, 1, 2]);

        // Once clean, nothing is returned.
        snapshot.clean().unwrap();
        assert_eq!(snapshot.dirty().unwrap(), Dirty::Clean);
        assert_eq!(dirty_rows(&mut rows, &snapshot), [] as [u16; 0]);
        // Cleaning is idempotent.
        snapshot.clean().unwrap();

        // Changing one row only makes that row dirty.
        terminal.vt_write(b"x");
        let snapshot = state.update(&terminal).unwrap();
        assert_eq!(snapshot.dirty().unwrap(), Dirty::Partial);
        assert_eq!(dirty_rows(&mut rows, &snapshot), [1]);
    }

    /// Collect the viewport y of every row an iteration visits, along with
    /// the positions `next_dirty` reports for them.
    fn row_positions<'alloc>(rows: &mut RowIterator<'alloc>, snapshot: &Snapshot<'alloc, '_>) -> (Vec<i32>, Vec<u16>) {
        let mut viewport_ys = Vec::new();
        let mut iteration = rows.update(snapshot).unwrap();
        while let Some(row) = iteration.next() {
            viewport_ys.push(row.viewport_y().unwrap());
        }
        (viewport_ys, dirty_rows(rows, snapshot))
    }

    /// An overscan as `(above, below)`, since the FFI struct can't be compared.
    fn sides(overscan: Overscan) -> (u16, u16) {
        (overscan.above, overscan.below)
    }

    #[test]
    fn overscan_request_defaults_to_none_and_round_trips() {
        let mut state = RenderState::new().unwrap();
        assert_eq!(sides(state.overscan_request().unwrap()), (0, 0));

        let request = Overscan { above: 2, below: 1 };
        state.set_overscan(request).unwrap();
        assert_eq!(sides(state.overscan_request().unwrap()), (2, 1));
    }

    #[test]
    fn overscan_captures_only_rows_that_exist() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        for _ in 0..10 {
            terminal.vt_write(b"x\r\n");
        }
        let mut state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();

        // Without overscan, only the viewport is visited, and the position
        // from `next_dirty` is the viewport y.
        let snapshot = state.update(&terminal).unwrap();
        assert_eq!(sides(snapshot.overscan().unwrap()), (0, 0));
        assert_eq!(row_positions(&mut rows, &snapshot), (vec![0, 1, 2], vec![0, 1, 2]));

        // At the bottom, there is nothing below the viewport to capture.
        state.set_overscan(Overscan { above: 1, below: 1 }).unwrap();
        let snapshot = state.update(&terminal).unwrap();
        assert_eq!(snapshot.rows().unwrap(), 3);
        assert_eq!(sides(snapshot.overscan().unwrap()), (1, 0));
        // `next_dirty` counts from the highest captured row instead.
        assert_eq!(row_positions(&mut rows, &snapshot), (vec![-1, 0, 1, 2], vec![0, 1, 2, 3]));

        // At the top, there is nothing above it.
        terminal.scroll_viewport(crate::terminal::ScrollViewport::Top);
        let snapshot = state.update(&terminal).unwrap();
        assert_eq!(sides(snapshot.overscan().unwrap()), (0, 1));
        assert_eq!(row_positions(&mut rows, &snapshot), (vec![0, 1, 2, 3], vec![0, 1, 2, 3]));
    }

    /// Collect the id of every row an iteration visits.
    fn row_ids<'alloc>(rows: &mut RowIterator<'alloc>, snapshot: &Snapshot<'alloc, '_>) -> Vec<RowId> {
        let mut ids = Vec::new();
        let mut iteration = rows.update(snapshot).unwrap();
        while let Some(row) = iteration.next() {
            ids.push(row.id().unwrap());
        }
        ids
    }

    #[test]
    fn row_ids_follow_rows_as_the_viewport_scrolls() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        for _ in 0..10 {
            terminal.vt_write(b"x\r\n");
        }
        let mut state = RenderState::new().unwrap();
        let mut rows = RowIterator::new().unwrap();

        let before = row_ids(&mut rows, &state.update(&terminal).unwrap());
        // Every row has its own id, and it doesn't change without a reason.
        assert!(before[0] != before[1] && before[1] != before[2] && before[0] != before[2]);
        assert_eq!(row_ids(&mut rows, &state.update(&terminal).unwrap()), before);

        // Scrolling up by one moves each row one position down, keeping its id.
        terminal.scroll_viewport(crate::terminal::ScrollViewport::Delta(-1));
        let after = row_ids(&mut rows, &state.update(&terminal).unwrap());
        assert_eq!(after[1..], before[..2]);
        assert!(!before.contains(&after[0]));

        // With overscan, the row scrolled out below is still captured under
        // the same id.
        state.set_overscan(Overscan { above: 0, below: 1 }).unwrap();
        let overscanned = row_ids(&mut rows, &state.update(&terminal).unwrap());
        assert_eq!(overscanned[..3], after);
        assert_eq!(overscanned[3], before[2]);
    }

    #[test]
    fn cells_raw_matches_cell_iteration_and_outlives_the_row() {
        let mut terminal = Terminal::new(4, 2).unwrap();
        terminal.vt_write(b"ab\r\ncd");
        let mut state = RenderState::new().unwrap();
        let snapshot = state.update(&terminal).unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut iteration = rows.update(&snapshot).unwrap();

        let row = iteration.next().unwrap();
        let first_row = row.cells_raw().unwrap();
        assert_eq!(first_row.len(), 4);
        let mut expected = Vec::new();
        let mut cell_iteration = cells.update(row).unwrap();
        while let Some(cell) = cell_iteration.next() {
            expected.push(cell.raw_cell().unwrap());
        }

        // The raw cells stay usable after advancing to the next row.
        let second_row = iteration.next().unwrap().cells_raw().unwrap();
        let first_row: Vec<_> = first_row.collect();
        assert_eq!(first_row, expected);
        let codepoints = |cells: &[Cell]| -> Vec<u32> { cells.iter().map(|c| c.codepoint().unwrap()).collect() };
        assert_eq!(codepoints(&first_row), [0x61, 0x62, 0, 0]);
        assert_eq!(codepoints(&second_row.collect::<Vec<_>>()), [0x63, 0x64, 0, 0]);
    }

    #[test]
    fn graphemes_utf8_replaces_the_buffer() {
        let mut terminal = Terminal::new(4, 1).unwrap();
        // An "e" with a combining acute accent, then an empty cell.
        terminal.vt_write("e\u{301}".as_bytes());
        let mut state = RenderState::new().unwrap();
        let snapshot = state.update(&terminal).unwrap();
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut iteration = rows.update(&snapshot).unwrap();
        let mut cell_iteration = cells.update(iteration.next().unwrap()).unwrap();

        // Too small for the cluster, and holding leftovers from earlier.
        let mut text = String::from("x");
        cell_iteration.next().unwrap();
        cell_iteration.graphemes_utf8(&mut text).unwrap();
        assert_eq!(text, "e\u{301}");
        cell_iteration.next().unwrap();
        cell_iteration.graphemes_utf8(&mut text).unwrap();
        assert_eq!(text, "");
    }

    /// 一屏覆盖各种单元格：无样式文本、调色板/RGB 前景与背景、反显、宽字符、
    /// 组合字符、只有背景色的擦除单元格，以及被 OSC 4 改过的调色板项。
    fn varied_terminal() -> Terminal<'static, 'static> {
        let mut terminal = Terminal::new(12, 6).unwrap();
        terminal.vt_write(b"plain \x1b[31mred\x1b[0m\r\n");
        terminal.vt_write(b"\x1b[38;2;1;2;3mrgb\x1b[48;2;4;5;6mbg\x1b[0m\x1b[7minv\x1b[0m\r\n");
        terminal.vt_write("\x1b[1;42m中e\u{301}\x1b[0m👍🏽\r\n".as_bytes());
        // 带背景色的擦除产生只有背景色的单元格（content tag 为 bg color）。
        terminal.vt_write(b"\x1b[44m\x1b[K\x1b[0m\r\n");
        terminal.vt_write(b"\x1b[45m\x1b[K\x1b[48;5;200m\x1b[K\x1b[0m\r\n");
        // 改掉调色板第 1 项，前景色要经修改后的调色板解析。
        terminal.vt_write(b"\x1b]4;1;rgb:12/34/56\x07\x1b[31mosc4\x1b[0m");
        // 超过 `read` 预留容量（16 字节）的字形簇：x 加 10 个组合重音，共 21 字节。
        terminal.vt_write(format!(" x{}", "\u{301}".repeat(10)).as_bytes());
        terminal
    }

    /// 用逐个 getter 读出 `read` 应返回的内容。
    fn read_by_getters(cell: &CellIteration<'_, '_>) -> (RenderCell, String) {
        let raw = cell.raw_cell().unwrap();
        let mut text = String::new();
        cell.graphemes_utf8(&mut text).unwrap();
        (
            RenderCell {
                raw,
                wide: raw.wide().unwrap(),
                graphemes_len: cell.graphemes_len().unwrap(),
                has_styling: cell.has_styling().unwrap(),
                style: cell.style().unwrap(),
                fg_color: cell.fg_color().unwrap(),
                bg_color: cell.bg_color().unwrap(),
            },
            text,
        )
    }

    #[test]
    fn read_matches_individual_getters() {
        let terminal = varied_terminal();
        let mut state = RenderState::new().unwrap();
        // 选中第一行，覆盖 `Selected` key。
        let selection = terminal
            .select_line(crate::selection::SelectLineOptions::new(
                terminal
                    .grid_ref(crate::terminal::Point::Active(crate::terminal::PointCoordinate { x: 0, y: 0 }))
                    .unwrap(),
            ))
            .unwrap()
            .unwrap();
        terminal.set_selection(Some(&selection)).unwrap();

        let snapshot = state.update(&terminal).unwrap();
        let palette = snapshot.colors().unwrap().palette;
        let mut rows = RowIterator::new().unwrap();
        let mut cells = CellIterator::new().unwrap();
        let mut row_iter = rows.update(&snapshot).unwrap();
        // 每个单元格前都把缓冲区缩回零容量，让超长字形簇走扩容后继续读取的
        // 路径。
        let mut text = String::new();
        let mut seen = Vec::new();
        while let Some(row) = row_iter.next() {
            let mut cell_iter = cells.update(row).unwrap();
            while let Some(cell) = cell_iter.next() {
                text.shrink_to(0);
                let read = cell.read(&palette, &mut text).unwrap();
                let (expected, expected_text) = read_by_getters(cell);
                assert_eq!(read, expected);
                assert_eq!(text, expected_text);

                // 通用的批量读取，把可能缺失的 key 放在中间也能得到同样的结果。
                let (fg, raw, bg, style, len, styled, selected) = cell
                    .get_multi((
                        cell_query::FgColor,
                        cell_query::Raw,
                        cell_query::BgColor,
                        cell_query::Style,
                        cell_query::GraphemesLen,
                        cell_query::HasStyling,
                        cell_query::Selected,
                    ))
                    .unwrap();
                assert_eq!(
                    (fg, raw, bg, style, len, styled, selected),
                    (
                        expected.fg_color,
                        expected.raw,
                        expected.bg_color,
                        expected.style,
                        expected.graphemes_len,
                        expected.has_styling,
                        cell.is_selected().unwrap(),
                    )
                );
                seen.push((read.wide, read.fg_color, read.bg_color, text.clone()));
            }
        }

        // 确认样本确实覆盖了想测的情形。
        let rgb = |r, g, b| Some(RgbColor { r, g, b });
        assert!(seen.iter().any(|c| c.0 == CellWide::Wide && c.3 == "中"));
        assert!(seen.iter().any(|c| c.0 == CellWide::SpacerTail));
        assert!(seen.iter().any(|c| c.3 == "e\u{301}"));
        assert!(seen.iter().any(|c| c.3.len() == 21));
        // 是否把肤色修饰符并进同一簇取决于模式 2027，这里只要求表情本身在。
        assert!(seen.iter().any(|c| c.3.starts_with('👍')));
        assert!(seen.iter().any(|c| c.1 == rgb(1, 2, 3)));
        assert!(seen.iter().any(|c| c.2 == rgb(4, 5, 6)));
        assert!(seen.iter().any(|c| c.1 == rgb(0x12, 0x34, 0x56) && c.3 == "o"));
        // 只有背景色的单元格：没有文本、没有样式，却有背景色。
        assert!(seen.iter().any(|c| c.3.is_empty() && c.2 == Some(palette[4])));
        assert!(seen.iter().any(|c| c.3.is_empty() && c.2 == Some(palette[200])));
        assert!(seen.iter().any(|c| c.1.is_none() && c.2.is_none()));
    }

    #[test]
    fn snapshot_and_row_get_multi_match_getters() {
        let mut terminal = varied_terminal();
        terminal.vt_write(b"\x1b]12;rgb:aa/bb/cc\x07\x1b[5 q");
        let mut state = RenderState::new().unwrap();
        state.set_overscan(Overscan { above: 0, below: 0 }).unwrap();
        let snapshot = state.update(&terminal).unwrap();

        let colors = snapshot.colors().unwrap();
        let (cols, rows, dirty, bg, fg, cursor_color, palette, cursor, all_colors) = snapshot
            .get_multi((
                query::Cols,
                query::Rows,
                query::Dirty,
                query::ColorBackground,
                query::ColorForeground,
                query::ColorCursor,
                query::ColorPalette,
                query::Cursor,
                query::Colors,
            ))
            .unwrap();
        assert_eq!(cols, snapshot.cols().unwrap());
        assert_eq!(rows, snapshot.rows().unwrap());
        assert_eq!(dirty, snapshot.dirty().unwrap());
        assert_eq!((bg, fg), (colors.background, colors.foreground));
        assert_eq!(cursor_color, snapshot.cursor_color().unwrap());
        assert!(cursor_color.is_some());
        assert_eq!(palette, colors.palette);
        assert_eq!(cursor, snapshot.cursor().unwrap());
        assert_eq!(all_colors, colors);

        let (x, y, tail, style, visible, overscan) = snapshot
            .get_multi((
                query::CursorViewportX,
                query::CursorViewportY,
                query::CursorViewportWideTail,
                query::CursorVisualStyle,
                query::CursorVisible,
                query::Overscan,
            ))
            .unwrap();
        let viewport = snapshot.cursor_viewport().unwrap().unwrap();
        assert_eq!((x, y, tail), (Some(viewport.x), Some(viewport.y), Some(viewport.at_wide_tail)));
        assert_eq!(style, CursorVisualStyle::Bar);
        assert!(visible);
        assert_eq!((overscan.above, overscan.below), (0, 0));

        let mut rows = RowIterator::new().unwrap();
        let mut row_iter = rows.update(&snapshot).unwrap();
        while let Some(row) = row_iter.next() {
            let (selection, dirty, raw, id, viewport_y) = row
                .get_multi((
                    row_query::Selection,
                    row_query::Dirty,
                    row_query::Raw,
                    row_query::Id,
                    row_query::ViewportY,
                ))
                .unwrap();
            assert_eq!(selection.is_some(), row.selection().unwrap().is_some());
            assert_eq!(dirty, row.dirty().unwrap());
            assert_eq!(raw, row.raw_row().unwrap());
            assert_eq!(id, row.id().unwrap());
            assert_eq!(viewport_y, row.viewport_y().unwrap());
        }
    }

    /// 光标滚出视口后，视口坐标类 key 记为 `None`，同一批里其他 key 照常读出。
    #[test]
    fn missing_values_do_not_abort_the_batch() {
        let mut terminal = Terminal::new(8, 2).unwrap();
        terminal.vt_write(b"a\r\nb\r\nc\r\nd");
        terminal.scroll_viewport(crate::terminal::ScrollViewport::Top);
        let mut state = RenderState::new().unwrap();
        let snapshot = state.update(&terminal).unwrap();
        let (x, cols, y, has_value, color) = snapshot
            .get_multi((
                query::CursorViewportX,
                query::Cols,
                query::CursorViewportY,
                query::CursorViewportHasValue,
                query::ColorCursor,
            ))
            .unwrap();
        assert_eq!((x, cols, y, has_value, color), (None, 8, None, false, None));
    }
}
