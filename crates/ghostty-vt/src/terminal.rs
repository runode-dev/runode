//! Types and functions around terminal state management.

use std::{io::Write, marker::PhantomData, mem::MaybeUninit, ptr::NonNull};

use crate::{
    alloc::{Allocator, Bytes, Object},
    error::{
        Error, Result, from_optional_result, from_optional_result_uninit, from_optional_result_with_len, from_result,
        from_result_with_len,
    },
    ffi::{self, TerminalData as Data, TerminalOption as Opt},
    key, mouse, osc,
    screen::{GridRef, Screen, TrackedGridRef},
    style::{self, Palette, RawPalette, RgbColor},
};

#[doc(inline)]
pub use ffi::{SizeReportSize, TerminalScrollbar as Scrollbar};

/// Complete terminal emulator state and rendering.
///
/// A terminal instance manages the full emulator state including the screen,
/// scrollback, cursor, styles, modes, and VT stream processing.
///
/// Once a terminal session is up and running, you can configure a key encoder
/// to write keyboard input via [`key::Encoder::set_options_from_terminal`].
///
/// ## Example: VT stream processing
///
/// ```
/// use libghostty_vt::Terminal;
///
/// // Create a terminal
/// let mut terminal = Terminal::new(80, 24).unwrap();
///
/// // Feed VT data into the terminal
/// terminal.vt_write(b"Hello, World!\r\n");
///
/// // ANSI color codes: ESC[1;32m = bold green, ESC[0m = reset
/// terminal.vt_write(b"\x1b[1;32mGreen Text\x1b[0m\r\n");
///
/// // Cursor positioning: ESC[1;1H = move to row 1, column 1
/// terminal.vt_write(b"\x1b[1;1HTop-left corner\r\n");
///
/// // Cursor movement: ESC[5B = move down 5 lines
/// terminal.vt_write(b"\x1b[5B");
/// terminal.vt_write(b"Moved down!\r\n");
///
/// // Erase line: ESC[2K = clear entire line
/// terminal.vt_write(b"\x1b[2K");
/// terminal.vt_write(b"New content\r\n");
///
/// // Multiple lines
/// terminal.vt_write(b"Line A\r\nLine B\r\nLine C\r\n");
/// ```
///
/// # Effects
///
/// By default, the terminal sequence processing with [`Terminal::vt_write`]
/// only process sequences that directly affect terminal state and ignores
/// sequences that have side effect behavior or require responses. These
/// sequences include things like bell characters, title changes, device
/// attributes queries, and more. To handle these sequences, the user
/// must configure "effects."
///
/// Effects are callbacks that the terminal invokes, mostly in response to VT
/// sequences processed during [`Terminal::vt_write`]. They let the embedding
/// application react to terminal-initiated events such as bell characters,
/// title changes, device status report responses, and more.
///
/// Each effect is registered with its corresponding `Terminal::on_<effect>`
/// function, which accepts a closure with access to the terminal state and
/// possibly other parameters. Some examples include [`Terminal::on_bell`]
/// and [`Terminal::on_pty_write`].
///
/// All callbacks are invoked synchronously, mostly during
/// [`Terminal::vt_write`]. A few also fire from [`Terminal::reset`] and
/// [`Terminal::resize`], such as [`Terminal::on_render_hold`] and the
/// in-band size report sent through [`Terminal::on_pty_write`].
/// Callbacks must be very careful to not block for too long or perform
/// expensive operations, since they are blocking further IO processing.
///
/// ## Shared state
///
/// **Unlike the C API**, you *cannot* specify arbitrary user data that's
/// shared between all callbacks, mainly because a safe, idiomatic Rust
/// equivalent of this pattern is very difficult to implement and use
/// due to Rust's much stricter safety guarantees. In turn, we use the
/// user data internally for callback dispatch purposes.
///
/// You should instead use types that allow safe *interior mutability*
/// (e.g. [`Cell`](std::cell::Cell) or [`RefCell`](std::cell::RefCell))
/// and pass a shared reference into each effect handler that needs to mutate
/// the shared state. Note that reference counting mechanisms like
/// [`Rc`](std::rc::Rc) and [`Arc`](std::sync::Arc) are optional.
///
/// ## Example: Registering effects and processing VT data
///
/// ```rust
/// use std::cell::Cell;
/// use libghostty_vt::Terminal;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Set up a simple bell counter.
/// //
/// // `usize` is a simple, `Copy`able type, which means `Cell`s are
/// // perfectly suitable here. More complex, non-`Copy` types should
/// // use `RefCell`s instead.
/// //
/// // This has to be done before the terminal is created, since
/// // its effect handlers will continue to refer to the bell counter
/// // during the lifetime of the terminal.
/// let bell_count = Cell::new(0usize);
///
/// let mut terminal = Terminal::new(80, 24)?;
/// terminal
///     .on_pty_write(|_term, data| {
///         println!("Replying {} bytes to the PTY", data.len());
///     })?
///    .on_bell({
///        // Explicitly borrow the bell count, or otherwise `move`
///        // will attempt to capture the entire `Cell` and cause a
///        // compiler error
///        let bell_count = &bell_count;
///        move |_term| {
///            bell_count.update(|v| v + 1);
///            println!("Bell! (count = {})", bell_count.get())
///        }
///     })?
///    .on_title_changed(|term| {
///        // Query the cursor position to confirm the terminal processed the
///        // title change (the title itself is tracked by the embedder via the
///        // OSC parser or its own state).
///        let col = term.cursor_x().unwrap();
///        println!("Title changed! (cursor at col {col})");
///    })?;
///
/// // Feed VT data that triggers effects:
/// // 1. Bell (BEL = 0x07)
/// terminal.vt_write(b"\x07");
/// // 2. Title change (OSC 2 ; <title> ST)
/// terminal.vt_write(b"\x1b]2;Hello Effects\x1b\\");
/// // 3. Device status report (DECRQM for wraparound mode ?7)
/// //    triggers write_pty with the response
/// terminal.vt_write(b"\x1B[?7$p");
/// // 4. Another bell to show the counter increments
/// terminal.vt_write(b"\x07");
///
/// assert_eq!(bell_count.get(), 2);
/// # Ok(())}
/// ```
///
/// # Color theme
///
/// The terminal maintains a set of colors used for rendering: a foreground
/// color, a background color, a cursor color, and a 256-color palette. Each
/// of these has two layers: a **default** value set by the embedder, and an
/// **override** value that programs running in the terminal can set via OSC
/// escape sequences (e.g. OSC 10/11/12 for foreground/background/cursor,
/// OSC 4 for individual palette entries).
///
/// ## Default colors
///
/// Use [`Terminal::set_default_fg_color`], [`Terminal::set_default_bg_color`],
/// [`Terminal::set_default_cursor_color`] and [`Terminal::set_default_color_palette`]
/// to configure the default colors. These represent the theme or configuration
/// chosen by the embedder. Passing `None` clears the default, leaving the color
/// unset.
///
/// For the palette, passing `None` resets to the built-in default palette.
/// The palette set operation preserves any per-index OSC overrides that programs
/// have applied; only unmodified indices are updated.
///
/// ## Reading colors
///
/// Use functions like [`Terminal::default_cursor_color`],
/// [`Terminal::bg_color`], [`Terminal::default_color_palette`], etc. to read
/// colors. There are two variants for each color: the **effective** value
/// (which returns the OSC override if one is active, otherwise the default)
/// and the **default** value (which ignores any OSC overrides).
///
/// For foreground, background, and cursor colors, the getters return `Ok(None)`
/// if no color is configured (neither a default nor an OSC override).
/// The palette getters always succeed since the palette always has a value
/// (the built-in default if nothing else is set).
///
/// ## Setting a color theme
///
/// ```
/// use libghostty_vt::{
///     style::{RgbColor, PaletteIndex},
///     Error,
///     Terminal,
/// };
///
/// fn set_color_theme(terminal: &mut Terminal<'_, '_>) -> Result<(), Error> {
///     // Set default foreground (light gray) and background (dark)
///     terminal
///         .set_default_fg_color(Some(
///             RgbColor { r: 0xDD, g: 0xDD, b: 0xDD }
///         ))?
///         .set_default_bg_color(Some(
///             RgbColor { r: 0x1E, g: 0x1E, b: 0x2E }
///         ))?
///         .set_default_cursor_color(Some(
///             RgbColor { r: 0xF5, g: 0xE0, b: 0xDC }
///         ))?;
///     
///     // Set a custom palette — start from the built-in default and override
///     // the first 8 entries with a custom dark theme.
///     let mut palette = terminal.default_color_palette()?;
///     palette.set(PaletteIndex::BLACK, RgbColor { r: 0x45, g: 0x47, b: 0x5A });
///     palette.set(PaletteIndex::RED, RgbColor { r: 0xF3, g: 0x8B, b: 0xA8 });
///     palette.set(PaletteIndex::GREEN, RgbColor { r: 0xA6, g: 0xE3, b: 0xA1 });
///     palette.set(PaletteIndex::YELLOW, RgbColor { r: 0xF9, g: 0xE2, b: 0xAF });
///     palette.set(PaletteIndex::BLUE, RgbColor { r: 0x89, g: 0xB4, b: 0xFA });
///     palette.set(PaletteIndex::MAGENTA, RgbColor { r: 0xF5, g: 0xC2, b: 0xE7 });
///     palette.set(PaletteIndex::CYAN, RgbColor { r: 0x94, g: 0xE2, b: 0xD5 });
///     palette.set(PaletteIndex::WHITE, RgbColor { r: 0xBA, g: 0xC2, b: 0xDE });
///     
///     terminal.set_default_color_palette(Some(palette))?;
///     Ok(())
/// }
/// ```
///
#[derive(Debug)]
pub struct Terminal<'alloc: 'cb, 'cb> {
    pub(crate) inner: Object<'alloc, ffi::TerminalImpl>,
    // Keep callbacks in a heap allocation so C can store a userdata pointer
    // to the VTable itself. That pointer remains stable even if Terminal moves.
    vtable: Box<VTable<'alloc, 'cb>>,
    // 存活令牌兼变更计数：只有拥有底层句柄的 Terminal 持有 `Some`。
    // [`crate::search::Search`] 等外部对象保存它的 `Weak`，借此确认绑定的终端
    // 还活着。只比较指针不够：终端释放后，新终端可能分配到同一地址，而 C 侧
    // 已解绑的搜索仍会返回指向旧终端内存的匹配结果。
    //
    // 计数在每个 `&mut self` 方法开头递增（见 `touch`）。搜索记下 feed 时的
    // 计数，读不跟踪的匹配前比对：终端变过而没有重新 feed，历史区匹配里的
    // page 指针可能已被释放或复用，必须拒绝。回调里临时构造的借用视图不拥有
    // 句柄，取 `None`。
    pub(crate) alive: Option<std::rc::Rc<std::cell::Cell<u64>>>,
}

/// Default visual style used when the cursor style is reset.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[non_exhaustive]
pub enum CursorStyle {
    /// Bar cursor (DECSCUSR 5, 6).
    Bar = ffi::TerminalCursorStyle::BAR,
    /// Block cursor (DECSCUSR 1, 2).
    Block = ffi::TerminalCursorStyle::BLOCK,
    /// Underline cursor (DECSCUSR 3, 4).
    Underline = ffi::TerminalCursorStyle::UNDERLINE,
    /// Hollow block cursor.
    BlockHollow = ffi::TerminalCursorStyle::BLOCK_HOLLOW,
}

impl<'alloc: 'cb, 'cb> Terminal<'alloc, 'cb> {
    /// Create a new terminal instance.
    ///
    /// The terminal starts with various reasonable defaults e.g. around
    /// scrollback limits. Use the `Terminal::set_*` family of methods
    /// to change any options prior to using the terminal.
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null(), cols, rows) }
    }

    /// Create a new terminal instance with a custom allocator.
    ///
    /// The terminal starts with various reasonable defaults e.g. around
    /// scrollback limits. Use the `Terminal::set_*` family of methods
    /// to change any options prior to using the terminal.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(alloc: &'alloc Allocator<'ctx>, cols: u16, rows: u16) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw(), cols, rows) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator, cols: u16, rows: u16) -> Result<Self> {
        let mut raw: ffi::Terminal = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_terminal_new(alloc, &raw mut raw, cols, rows) };
        from_result(result)?;
        unsafe { Self::from_raw(raw) }
    }

    /// 记录一次可能改变终端内容的操作，见 `alive` 字段的说明。
    ///
    /// 每个 `&mut self` 方法都在开头调用它，包括只改配置的 setter：宁可让
    /// 搜索多判一次过期，也不要漏判。
    pub(crate) fn touch(&mut self) {
        if let Some(generation) = &self.alive {
            generation.set(generation.get().wrapping_add(1));
        }
    }

    /// 当前的变更计数；借用视图没有计数，返回 `None`。
    pub(crate) fn generation(&self) -> Option<u64> {
        self.alive.as_ref().map(|generation| generation.get())
    }

    pub(crate) unsafe fn from_raw(raw: ffi::Terminal) -> Result<Self> {
        Ok(Self {
            inner: Object::new(raw)?,
            vtable: Box::new(VTable::default()),
            alive: Some(std::rc::Rc::new(std::cell::Cell::new(0))),
        })
    }

    /// Write VT-encoded data to the terminal for processing.
    ///
    /// Feeds raw bytes through the terminal's VT stream parser, updating
    /// terminal state accordingly. By default, sequences that require output
    /// (queries, device status reports) are silently ignored.
    /// Use [`Terminal::on_pty_write`] to install a callback that receives
    /// response data.
    ///
    /// This never fails. Any erroneous input or errors in processing the input
    /// are logged internally but do not cause this function to fail because
    /// this input is assumed to be untrusted and from an external source; so
    /// the primary goal is to keep the terminal state consistent and not allow
    /// malformed input to corrupt or crash.    
    pub fn vt_write(&mut self, data: &[u8]) {
        self.touch();
        unsafe { ffi::ghostty_terminal_vt_write(self.inner.as_raw(), data.as_ptr(), data.len()) }
    }

    /// Resize the terminal to the given dimensions.
    ///
    /// Changes the number of columns and rows in the terminal. The primary
    /// screen will reflow content if wraparound mode is enabled; the alternate
    /// screen does not reflow. If the dimensions are unchanged, the grid is
    /// left as is, but everything below still applies.
    ///
    /// This also updates the terminal's pixel dimensions (used for image
    /// protocols and size reports), disables synchronized output mode (allowed
    /// by the spec so that resize results are shown immediately), and sends an
    /// in-band size report if mode 2048 is enabled.
    ///
    /// If synchronized output was enabled, the [render hold](Self::on_render_hold)
    /// callback is invoked to report that the hold ended.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width_px: u32, cell_height_px: u32) -> Result<()> {
        self.touch();
        let result =
            unsafe { ffi::ghostty_terminal_resize(self.inner.as_raw(), cols, rows, cell_width_px, cell_height_px) };
        from_result(result)
    }

    /// Perform a full reset of the terminal (RIS).
    ///
    /// Resets all terminal state back to its initial configuration,
    /// including modes, scrollback, scrolling region, and screen contents.
    /// The terminal dimensions are preserved.
    ///
    /// If synchronized output was enabled, the [render hold](Self::on_render_hold)
    /// callback is invoked to report that the hold ended.
    pub fn reset(&mut self) {
        self.touch();
        unsafe { ffi::ghostty_terminal_reset(self.inner.as_raw()) }
    }

    /// Scroll the terminal viewport.
    pub fn scroll_viewport(&mut self, scroll: ScrollViewport) {
        self.touch();
        unsafe { ffi::ghostty_terminal_scroll_viewport(self.inner.as_raw(), scroll.into()) }
    }

    /// Resolve a point in the terminal grid to a grid reference.
    ///
    /// Resolves the given point (which can be in active, viewport, screen,
    /// or history coordinates) to a grid reference for that location. Use
    /// [`GridRef::cell`] and [`GridRef::row`] to extract the cell and row.
    ///
    /// Lookups in the active region and viewport are fast. Lookups in the
    /// screen and history may require traversing the full scrollback page
    /// list to resolve the y coordinate, so they can be expensive for large
    /// scrollback buffers.
    ///
    /// This function isn't meant to be used as the core of render loop. It
    /// isn't built to sustain the framerates needed for rendering large
    /// screens. Use the [render state API](crate::render::RenderState) for
    /// that. This API is instead meant for less strictly performance-sensitive
    /// use cases.
    pub fn grid_ref(&self, point: Point) -> Result<GridRef<'_>> {
        let mut grid_ref = ffi::sized!(ffi::GridRef);
        let result = unsafe { ffi::ghostty_terminal_grid_ref(self.inner.as_raw(), point.into(), &raw mut grid_ref) };
        from_result(result)?;
        Ok(unsafe { GridRef::from_raw(grid_ref) })
    }

    /// Create an owned tracked grid reference for a terminal point.
    ///
    /// This is the tracked variant of [`Terminal::grid_ref`]. The returned handle
    /// follows the referenced cell as the terminal's page list is modified:
    /// scrolling, pruning, resize/reflow, and other page-list operations update
    /// the tracked reference automatically.
    ///
    /// The reference is attached to the terminal screen/page-list that is
    /// active at creation time.
    ///
    /// If the point is outside the requested coordinate space, this returns
    /// `Err(Error::InvalidValue)`.
    ///
    /// If the tracked grid reference outlives this terminal, the handle remains
    /// valid, but it will always return `false` or `Ok(None)`.
    pub fn track_grid_ref(&self, point: Point) -> Result<TrackedGridRef> {
        let mut raw: ffi::TrackedGridRef = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_terminal_grid_ref_track(self.inner.as_raw(), point.into(), &raw mut raw) };
        from_result(result)?;

        let inner = NonNull::new(raw).ok_or(Error::InvalidValue)?;
        Ok(TrackedGridRef::new(inner, self.inner.ptr))
    }

    /// Convert a grid reference back to a point in the given coordinate system.
    ///
    /// This is the inverse of [`Terminal::grid_ref`]: given a grid reference, it
    /// returns the x/y coordinates in the requested coordinate system (active,
    /// viewport, screen, or history).
    ///
    /// The grid reference must have been obtained from the same terminal instance.
    /// Like all grid references, it is only valid until the next mutating
    /// terminal call.
    ///
    /// Not every grid reference is representable in every coordinate system.
    /// For example, a cell in scrollback history cannot be expressed in active
    /// coordinates, and a cell that has scrolled off the visible area cannot
    /// be expressed in viewport coordinates. In these cases, the function
    /// returns `Ok(None)`.
    pub fn point_from_grid_ref(&self, grid_ref: &GridRef<'_>, space: PointSpace) -> Result<Option<PointCoordinate>> {
        let mut point = MaybeUninit::<ffi::PointCoordinate>::zeroed();
        let result = unsafe {
            ffi::ghostty_terminal_point_from_grid_ref(
                self.inner.as_raw(),
                std::ptr::from_ref(&grid_ref.inner),
                space.into_raw(),
                point.as_mut_ptr(),
            )
        };

        from_optional_result_uninit(result, point).map(|value| value.map(Into::into))
    }

    /// Get the current value of a terminal mode.
    pub fn mode(&self, mode: Mode) -> Result<bool> {
        let mut mode = ffi::TerminalModeConfig { mode: mode.into(), value: false };

        let result = unsafe {
            ffi::ghostty_terminal_get(self.inner.as_raw(), Data::MODE, (&raw mut mode).cast::<std::ffi::c_void>())
        };
        from_result(result)?;
        Ok(mode.value)
    }

    /// Set the current value of a terminal mode.
    ///
    /// This does not change the value restored by a full terminal reset (RIS).
    pub fn set_mode(&mut self, mode: Mode, value: bool) -> Result<&mut Self> {
        self.touch();
        let mode = ffi::TerminalModeConfig { mode: mode.into(), value };

        let result = unsafe {
            ffi::ghostty_terminal_set(self.inner.as_raw(), Opt::MODE, (&raw const mode).cast::<std::ffi::c_void>())
        };
        from_result(result)?;
        Ok(self)
    }

    /// Set the reset default for a terminal mode.
    ///
    /// This unconditionally updates both the current value and the value
    /// restored by a full terminal reset (RIS).
    ///
    /// Some recognized modes represent transitions or mirror additional
    /// terminal state and cannot safely be configured as reset defaults.
    /// Those modes return [`Error::InvalidValue`].
    pub fn set_default_mode(&mut self, mode: Mode, value: bool) -> Result<&mut Self> {
        self.touch();
        let mode = ffi::TerminalModeConfig { mode: mode.into(), value };

        let result = unsafe {
            ffi::ghostty_terminal_set(
                self.inner.as_raw(),
                Opt::MODE_DEFAULT,
                (&raw const mode).cast::<std::ffi::c_void>(),
            )
        };
        from_result(result)?;
        Ok(self)
    }

    /// Compress eligible terminal scrollback.
    ///
    /// Incremental mode performs bounded work suitable for an idle callback.
    /// A pending result means the application should invoke another step while
    /// the terminal remains idle. A complete result means no continuation is
    /// needed until `Terminal::compression_activity` changes. Full mode
    /// performs one synchronous scan and can stall on large scrollback buffers.
    ///
    /// Compression is opportunistic. Complete means the pass has finished,
    /// not that every page was compressed: pages may be unprofitable or
    /// encounter an allocation or reclamation failure. Compression changes
    /// only the terminal's storage representation and never its logical
    /// contents or scrollback limit. Accessing compressed history restores
    /// it transparently.
    ///
    /// This function is not thread-safe with other operations on the same
    /// terminal. The caller must serialize it with writes, rendering, searches,
    /// and other terminal access.
    pub fn compress(&mut self, mode: CompressionMode) -> Result<CompressionResult> {
        self.touch();
        let mut value = ffi::TerminalCompressionResult::UNSUPPORTED;
        let result = unsafe { ffi::ghostty_terminal_compress(self.inner.as_raw(), mode.into(), &raw mut value) };
        from_result(result)?;
        value.try_into().map_err(|_| Error::InvalidValue)
    }

    /// Return the current compression activity token.
    ///
    /// The token is opaque and only equality comparisons are meaningful.
    /// An embedding application should cache it and restart its compression
    /// idle delay whenever the value changes. The value may wrap and changes
    /// in either direction have the same meaning.
    ///
    /// This function only observes terminal state.
    /// It does not perform or schedule compression.
    pub fn compression_activity(&self) -> Result<CompressionActivity> {
        let mut value = 0;
        let result = unsafe { ffi::ghostty_terminal_compression_activity(self.inner.as_raw(), &raw mut value) };
        from_result(result)?;
        Ok(CompressionActivity(value))
    }

    /// The configured maximum retained VT continuation size in bytes.
    ///
    /// A value of zero means continuation tracking is disabled. This reports
    /// the configured limit even when a current unfinished continuation is
    /// temporarily unavailable.
    pub fn continuation_max_bytes(&self) -> Result<usize> {
        self.get(Data::CONTINUATION_MAX_BYTES)
    }

    /// Set the maximum number of replay-safe VT continuation bytes retained.
    ///
    /// Continuation bytes reconstruct an escape sequence or UTF-8 codepoint
    /// which was unfinished at the end of the most recent [`Terminal::vt_write`]
    /// call. They are used automatically by terminal snapshots and may also be
    /// exported directly with the continuation APIs.
    ///
    /// Tracking is disabled by default. A nonzero value enables tracking and
    /// sets its byte limit. Passing zero disables tracking. Lowering the limit
    /// below an already-retained continuation, or enabling tracking while the
    /// parser is already unfinished, makes the current continuation unavailable
    /// because earlier bytes cannot be reconstructed. Tracking recovers
    /// automatically after a later write reaches the ground state or contains
    /// a fresh replay start.
    pub fn set_continuation_max_bytes(&mut self, v: usize) -> Result<&mut Self> {
        self.touch();
        self.set(Opt::CONTINUATION_MAX_BYTES, &v)?;
        Ok(self)
    }

    /// Write the terminal's replay-safe VT continuation to a callback writer.
    ///
    /// The continuation is the exact byte suffix needed to reconstruct
    /// unfinished VT parser or UTF-8 decoder state in an equivalent terminal.
    /// It is empty when the stream is at ground. The callback is invoked
    /// synchronously and may be called more than once. It must not call
    /// terminal APIs with the same terminal handle.
    ///
    /// Continuation tracking must have been enabled by calling
    /// [`Terminal::set_continuation_max_bytes`] with a nonzero value before
    /// the input that produced the continuation was written.    
    ///
    /// # Errors
    ///
    /// This function returns [`Error::IoError`] if the callback rejects a
    /// write, [`Error::LimitExceeded`] if output accounting overflows, or
    /// [`Error::InvalidValue`] if an argument is invalid, tracking is disabled,
    /// or the current continuation is unavailable.
    pub fn continuation_write<W: Write>(&mut self, writer: &mut W) -> Result<()> {
        self.touch();
        let writer = crate::io::to_writer(writer);
        let result = unsafe { ffi::ghostty_terminal_continuation_write(self.inner.as_raw(), writer) };
        from_result(result)
    }

    /// Return an allocated copy of the terminal's replay-safe VT continuation.
    ///
    /// The returned bytes are allocated with allocator, or the default allocator
    /// when allocator is `None`. An empty continuation is a successful result
    /// with empty [`Bytes`]; libghostty does not allocate for it.
    /// Continuation tracking must have been enabled by calling
    /// [`Terminal::set_continuation_max_bytes`] to a nonzero value before the
    /// input that produced the continuation was written.
    ///
    /// The caller must serialize this operation with all other access to the same
    /// terminal.
    ///
    /// # Errors
    ///
    /// This function returns [`Error::OutOfMemory`] on allocation failure, or
    /// [`Error::InvalidValue`] if an argument is invalid, tracking is disabled,
    /// or the current continuation is unavailable.
    pub fn continuation_alloc<'a, 'ctx: 'a>(&self, alloc: Option<&'a Allocator<'ctx>>) -> Result<Option<Bytes<'a>>> {
        let mut out = std::ptr::null_mut();
        let mut out_len = 0usize;
        let alloc = alloc.map_or(std::ptr::null(), super::alloc::Allocator::to_raw);

        let result = unsafe {
            ffi::ghostty_terminal_continuation_alloc(self.inner.as_raw(), alloc, &raw mut out, &raw mut out_len)
        };

        let out = from_optional_result(result, out)?;
        // SAFETY: On success, libghostty hands over `out_len` bytes allocated
        // with `alloc`, or NULL for empty output.
        Ok(out.map(|ptr| unsafe { Bytes::from_raw_parts(ptr, out_len, alloc) }))
    }

    /// Copy the terminal's replay-safe VT continuation into a caller buffer.
    ///
    /// Pass an empty `buf` to query the required size. A size query returns
    /// [`Error::OutOfSpace`] with the required size, including zero when the
    /// stream is at ground. If a non-empty buffer is too small, the function
    /// has the same result and reports the full required size.
    ///
    /// Continuation tracking must have been enabled by calling
    /// [`Terminal::set_continuation_max_bytes`] to a nonzero value before the
    /// input that produced the continuation was written.
    ///
    /// The caller must serialize this operation with all other access to the same
    /// terminal.
    ///
    /// # Errors
    ///
    /// This function returns [`Error::OutOfSpace`] for a size query or
    /// insufficient buffer, or [`Error::InvalidValue`] if an argument is invalid,
    /// tracking is disabled, or the current continuation is unavailable.
    pub fn continuation_buf(&self, buf: &mut [u8]) -> Result<Option<usize>> {
        let mut written = 0usize;
        // The C API uses a NULL pointer to distinguish an explicit size query
        // from a zero-capacity destination. Rust empty slices have a non-NULL
        // dangling pointer, so translate that representation at this boundary.
        let buf_ptr = if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr() };

        let result = unsafe {
            ffi::ghostty_terminal_continuation_buf(self.inner.as_raw(), buf_ptr, buf.len(), &raw mut written)
        };

        from_optional_result_with_len(result, written)
    }

    pub(crate) fn get<T>(&self, tag: ffi::TerminalData::Type) -> Result<T> {
        let mut value = MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_terminal_get(self.inner.as_raw(), tag, value.as_mut_ptr().cast()) };
        from_result(result)?;
        // SAFETY: Value should be initialized after successful call.
        Ok(unsafe { value.assume_init() })
    }
    pub(crate) fn get_optional<T>(&self, tag: ffi::TerminalData::Type) -> Result<Option<T>> {
        let mut value = MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_terminal_get(self.inner.as_raw(), tag, value.as_mut_ptr().cast()) };
        from_optional_result_uninit(result, value)
    }
    pub(crate) fn set<T>(&self, tag: ffi::TerminalOption::Type, v: &T) -> Result<()> {
        let result = unsafe { ffi::ghostty_terminal_set(self.inner.as_raw(), tag, std::ptr::from_ref(v).cast()) };
        from_result(result)
    }
    /// Set an option whose ABI expects the pointer value itself, not a pointer
    /// to Rust storage containing that value.
    pub(crate) fn set_ptr(&self, tag: ffi::TerminalOption::Type, ptr: *const std::ffi::c_void) -> Result<()> {
        let result = unsafe { ffi::ghostty_terminal_set(self.inner.as_raw(), tag, ptr) };
        from_result(result)
    }
    pub(crate) fn set_optional<T>(&self, tag: ffi::TerminalOption::Type, v: Option<&T>) -> Result<()> {
        let ptr = if let Some(v) = v { std::ptr::from_ref(v) } else { std::ptr::null() };

        let result = unsafe { ffi::ghostty_terminal_set(self.inner.as_raw(), tag, ptr.cast()) };
        from_result(result)
    }

    /// Get the terminal width in cells.
    pub fn cols(&self) -> Result<u16> {
        self.get(Data::COLS)
    }
    /// Get the terminal height in cells.
    pub fn rows(&self) -> Result<u16> {
        self.get(Data::ROWS)
    }
    /// Get the total width of the terminal in pixels.
    ///
    /// This is `cols * cell_width_px` as set by [`Terminal::resize`].
    pub fn width_px(&self) -> Result<u32> {
        self.get(Data::WIDTH_PX)
    }
    /// Get the total height of the terminal in pixels.
    ///
    /// This is `rows * cell_height_px` as set by [`Terminal::resize`].
    pub fn height_px(&self) -> Result<u32> {
        self.get(Data::HEIGHT_PX)
    }

    /// The configured maximum scrollback allocation in bytes.
    ///
    /// This always reports the primary screen's configured value, including
    /// while an alternate screen is active.
    ///
    /// Returns `None` when the configured byte limit is unlimited.
    pub fn scrollback_max_bytes(&self) -> Result<Option<usize>> {
        self.get_optional(Data::SCROLLBACK_MAX_BYTES)
    }

    /// Set the maximum scrollback allocation in bytes.
    ///
    /// This is an estimate. Internally, libghostty only prunes bytes up
    /// to a "page"-granularity. A page is the minimum allocated unit of
    /// grid space within Ghostty. A page at the time of writing these docs
    /// is about 400KB, so the byte limit will be within this delta.
    ///
    /// This works alongside the line limit configuration. If both are set,
    /// the first-reached limit is used first. Both limits are dependent
    /// on external state (byte limit can be reached with less lines if
    /// more styles are used for example, line limit can be reached with
    /// a narrower terminal viewport). So, they are useful together.
    ///
    /// Lowering the limit immediately removes eligible complete historical
    /// pages. A value of zero disables scrollback and erases retained history.
    /// A `None` value removes the byte limit.
    pub fn set_scrollback_max_bytes(&mut self, v: Option<usize>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::SCROLLBACK_MAX_BYTES, v.as_ref())?;
        Ok(self)
    }

    /// The configured maximum number of physical scrollback lines.
    ///
    /// This always reports the primary screen's configured value, including
    /// while an alternate screen is active.
    ///
    /// Returns `None` when the configured line limit is unlimited.
    pub fn scrollback_max_lines(&self) -> Result<Option<usize>> {
        self.get_optional(Data::SCROLLBACK_MAX_LINES)
    }

    /// Set the maximum number of physical lines retained in scrollback.
    ///
    /// This is an estimate. Internally, libghostty only prunes lines up
    /// to a "page"-granularity. A page is the minimum allocated unit of
    /// grid space within Ghostty. As a result, the actual available scrollback
    /// lines will almost always be higher than configured. The magnitude
    /// of the difference depends on the number of used styles, graphemes, etc.
    /// since the row-count in a page is dynamic based on that. In general,
    /// it ranges from dozens to a hundred or so lines.
    ///
    /// This works alongside the byte limit configuration. If both are set,
    /// the first-reached limit is used first. Both limits are dependent
    /// on external state (byte limit can be reached with less lines if
    /// more styles are used for example, line limit can be reached with
    /// a narrower terminal viewport). So, they are useful together.
    ///
    /// Lowering the limit immediately removes eligible complete historical
    /// pages. A `None` value pointer removes the line limit.
    pub fn set_scrollback_max_lines(&mut self, v: Option<usize>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::SCROLLBACK_MAX_LINES, v.as_ref())?;
        Ok(self)
    }

    /// Get the cursor column position (0-indexed).
    pub fn cursor_x(&self) -> Result<u16> {
        self.get(Data::CURSOR_X)
    }
    /// Get the cursor row position within the active area (0-indexed).
    pub fn cursor_y(&self) -> Result<u16> {
        self.get(Data::CURSOR_Y)
    }
    /// Get whether the cursor has a pending wrap (next print will soft-wrap).
    pub fn is_cursor_pending_wrap(&self) -> Result<bool> {
        self.get(Data::CURSOR_PENDING_WRAP)
    }
    /// Get whether the cursor is visible (DEC mode 25).
    pub fn is_cursor_visible(&self) -> Result<bool> {
        self.get(Data::CURSOR_VISIBLE)
    }
    /// Get the current SGR style of the cursor.
    ///
    /// This is the style that will be applied to newly printed characters.
    pub fn cursor_style(&self) -> Result<style::Style> {
        self.get::<ffi::Style>(Data::CURSOR_STYLE).and_then(std::convert::TryInto::try_into)
    }
    /// The mouse pointer shape requested by the application through OSC 22.
    ///
    /// Initially [`mouse::Shape::Text`]. An empty OSC 22 resets it to that,
    /// and a name libghostty doesn't know leaves it unchanged. Excludes host
    /// hover overrides.
    pub fn mouse_shape(&self) -> Result<mouse::Shape> {
        self.get::<ffi::MouseShape::Type>(Data::MOUSE_SHAPE)?.try_into().map_err(|_| Error::InvalidValue)
    }
    /// Get the current Kitty keyboard protocol flags.
    pub fn kitty_keyboard_flags(&self) -> Result<key::KittyKeyFlags> {
        self.get::<ffi::KittyKeyFlags>(Data::KITTY_KEYBOARD_FLAGS).map(key::KittyKeyFlags::from_bits_retain)
    }

    /// Get the scrollbar state for the terminal viewport.
    ///
    /// This is amortized `O(1)`: the total is maintained incrementally as
    /// the terminal is modified and the viewport offset is cached. The
    /// first read after the viewport moves to an arbitrary position that
    /// isn't an absolute row (e.g. scrolling to a selection) may cost
    /// `O(pages)` to compute the offset, after which it is cached again.
    ///
    /// There is intentionally no change notification for scroll state.
    /// Callers building scrollbars should poll this once per frame or
    /// per write batch and diff the result to detect changes; this is
    /// what Ghostty's own renderer does.
    pub fn scrollbar(&self) -> Result<Scrollbar> {
        self.get(Data::SCROLLBAR)
    }
    /// Get the currently active screen.
    pub fn active_screen(&self) -> Result<Screen> {
        self.get::<ffi::TerminalScreen::Type>(Data::ACTIVE_SCREEN)
            .and_then(|v| v.try_into().map_err(|_| Error::InvalidValue))
    }
    /// Whether the viewport is currently pinned to the active area.
    ///
    /// This is true when the viewport is following the active terminal area,
    /// and false when the user has scrolled into history.
    pub fn viewport_active(&self) -> Result<bool> {
        self.get(Data::VIEWPORT_ACTIVE)
    }
    /// Get whether any mouse tracking mode is active.
    ///
    /// Returns true if any of the mouse tracking modes (X10, normal, button,
    /// or any-event) are enabled.
    pub fn is_mouse_tracking(&self) -> Result<bool> {
        self.get(Data::MOUSE_TRACKING)
    }
    /// Whether VT processing encountered a non-gracefully handled error that
    /// may have prevented a terminal-owned semantic update.
    ///
    /// Processing remains best-effort, and [`Terminal::reset`] does not clear
    /// this flag; it is purely informational. Gracefully handled protocol
    /// failures, configured limits, malformed or unsupported input, and
    /// failures limited to external effects or query responses do not set it.
    pub fn vt_processing_error(&self) -> Result<bool> {
        self.get(Data::VT_PROCESSING_ERROR)
    }
    /// Get the terminal title as set by escape sequences (e.g. OSC 0/2).
    ///
    /// Returns a borrowed string, valid until the next mutating terminal call.
    /// An empty string is returned when no title has been set.
    pub fn title(&self) -> Result<&str> {
        let str = self.get::<ffi::String>(Data::TITLE)?;
        // SAFETY: We trust libghostty to return a valid borrowed string,
        // while we uphold that no mutation could happen during its lifetime.
        let str = unsafe { str.to_bytes() };
        std::str::from_utf8(str).map_err(|_| Error::InvalidValue)
    }

    /// Get the current working directory as set by escape sequences (e.g. OSC 7).
    ///
    /// Returns a borrowed string, valid until the next mutating terminal call.
    /// An empty string is returned when no pwd has been set.
    pub fn pwd(&self) -> Result<&str> {
        let str = self.get::<ffi::String>(Data::PWD)?;
        // SAFETY: We trust libghostty to return a valid borrowed string,
        // while we uphold that no mutation could happen during its lifetime.
        let str = unsafe { str.to_bytes() };
        std::str::from_utf8(str).map_err(|_| Error::InvalidValue)
    }
    /// The total number of rows in the active screen including scrollback.
    pub fn total_rows(&self) -> Result<usize> {
        self.get(Data::TOTAL_ROWS)
    }
    ///  The number of scrollback rows (total rows minus viewport rows).
    pub fn scrollback_rows(&self) -> Result<usize> {
        self.get(Data::SCROLLBACK_ROWS)
    }

    /// The effective foreground color (override or default).
    pub fn fg_color(&self) -> Result<Option<RgbColor>> {
        self.get_optional::<ffi::ColorRgb>(Data::COLOR_FOREGROUND).map(|v| v.map(Into::into))
    }
    /// The default foreground color (ignoring any OSC override).
    pub fn default_fg_color(&self) -> Result<Option<RgbColor>> {
        self.get_optional::<ffi::ColorRgb>(Data::COLOR_FOREGROUND_DEFAULT).map(|v| v.map(Into::into))
    }
    /// Set the default foreground color.
    pub fn set_default_fg_color(&mut self, v: Option<RgbColor>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::COLOR_FOREGROUND, v.map(ffi::ColorRgb::from).as_ref())?;
        Ok(self)
    }

    /// The effective background color (override or default).
    pub fn bg_color(&self) -> Result<Option<RgbColor>> {
        self.get_optional::<ffi::ColorRgb>(Data::COLOR_BACKGROUND).map(|v| v.map(Into::into))
    }
    /// The default background color (ignoring any OSC override).
    pub fn default_bg_color(&self) -> Result<Option<RgbColor>> {
        self.get_optional::<ffi::ColorRgb>(Data::COLOR_BACKGROUND_DEFAULT).map(|v| v.map(Into::into))
    }
    /// Set the default background color.
    pub fn set_default_bg_color(&mut self, v: Option<RgbColor>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::COLOR_BACKGROUND, v.map(ffi::ColorRgb::from).as_ref())?;
        Ok(self)
    }

    /// The effective cursor color (override or default).
    pub fn cursor_color(&self) -> Result<Option<RgbColor>> {
        self.get_optional::<ffi::ColorRgb>(Data::COLOR_CURSOR).map(|v| v.map(Into::into))
    }
    /// The default cursor color (ignoring any OSC override).
    pub fn default_cursor_color(&self) -> Result<Option<RgbColor>> {
        self.get_optional::<ffi::ColorRgb>(Data::COLOR_CURSOR_DEFAULT).map(|v| v.map(Into::into))
    }
    /// Set the default cursor color.
    pub fn set_default_cursor_color(&mut self, v: Option<RgbColor>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::COLOR_CURSOR, v.map(ffi::ColorRgb::from).as_ref())?;
        Ok(self)
    }

    /// Set the default cursor style used by DECSCUSR reset (CSI 0 q).
    ///
    /// Passing `None` resets to libghostty's built-in block cursor default.
    pub fn set_default_cursor_style(&mut self, v: Option<CursorStyle>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::DEFAULT_CURSOR_STYLE, v.as_ref())?;
        Ok(self)
    }

    /// Set whether the default cursor blinks when reset by DECSCUSR (CSI 0 q).
    ///
    /// Passing `None` resets to libghostty's built-in non-blinking default.
    pub fn set_default_cursor_blink(&mut self, v: Option<bool>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::DEFAULT_CURSOR_BLINK, v.as_ref())?;
        Ok(self)
    }

    /// Set whether a resize may pull rows out of scrollback back into the
    /// active area.
    ///
    /// When true, growing rows reveals scrollback if the cursor is on the
    /// bottom row, and a column reflow that needs fewer rows reveals
    /// scrollback as well. When false, growing rows always appends blank rows
    /// at the bottom and a column reflow keeps the top of the active area on
    /// the same content, so a line that is fully in scrollback stays there. A
    /// soft-wrapped line with at least one row still in the active area may
    /// still unwrap back into view.
    ///
    /// Set this to false when the pty keeps its own screen buffer without
    /// scrollback, since it cannot pull rows back and will otherwise disagree
    /// with the terminal about the screen contents after a resize. Windows
    /// `ConPTY` is the motivating case.
    ///
    /// This is preserved across a full reset (RIS).
    ///
    /// Passing `None` resets to the built-in default of `true`.
    pub fn set_resize_pull_scrollback(&mut self, v: Option<bool>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::RESIZE_PULL_SCROLLBACK, v.as_ref())?;
        Ok(self)
    }

    /// The current 256-color palette.
    pub fn color_palette(&self) -> Result<Palette> {
        self.get::<RawPalette>(Data::COLOR_PALETTE).map(Palette::from)
    }
    /// The default 256-color palette (ignoring any OSC overrides).
    pub fn default_color_palette(&self) -> Result<Palette> {
        self.get::<RawPalette>(Data::COLOR_PALETTE_DEFAULT).map(Palette::from)
    }
    /// Set the default 256-color palette.
    pub fn set_default_color_palette(&mut self, v: Option<Palette>) -> Result<&mut Self> {
        self.touch();
        self.set_optional::<RawPalette>(Opt::COLOR_PALETTE, v.map(std::convert::Into::into).as_ref())?;
        Ok(self)
    }

    /// Set the maximum bytes the APC handler will buffer for all protocols.
    ///
    /// This prevents malicious input from causing unbounded memory allocation.
    /// A `None` value removes all overrides, reverting to the built-in defaults.
    pub fn set_apc_max_bytes(&mut self, max: Option<usize>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::APC_MAX_BYTES, max.as_ref())?;
        Ok(self)
    }

    /// Enable or disable Glyph Protocol APC handling.
    ///
    /// Disabling the protocol makes the terminal ignore Glyph Protocol APC
    /// sequences and clears the session's glyph glossary.
    pub fn set_glyph_protocol_enabled(&mut self, enabled: bool) -> Result<&mut Self> {
        self.touch();
        self.set(Opt::GLYPH_PROTOCOL, &enabled)?;
        Ok(self)
    }

    /// Enable window title reports in response to `CSI 21 t`.
    ///
    /// This is disabled by default because a running program can set a title
    /// and query it back into the pty input stream, potentially injecting
    /// commands that execute after user interaction.
    ///
    /// Passing `false` disables title reporting.
    pub fn set_title_report_enabled(&mut self, enabled: bool) -> Result<&mut Self> {
        self.touch();
        self.set(Opt::TITLE_REPORT, &enabled)?;
        Ok(self)
    }

    /// 设置一个以 [`ffi::String`] 为输入的字符串选项，`None` 传 NULL。
    ///
    /// C 侧会复制字符串，所以 `value` 只需活过这次调用。
    fn set_string(&mut self, tag: ffi::TerminalOption::Type, value: Option<&str>) -> Result<()> {
        let raw = value.map(ffi::String::from);
        self.set_optional(tag, raw.as_ref())
    }

    /// 手动设置终端标题，效果与程序发送 OSC 0/2 相同，但不会触发
    /// [`on_title_changed`](Self::on_title_changed)。
    ///
    /// 字符串会被复制进终端。`None` 清空标题，等同于设为空字符串。
    pub fn set_title(&mut self, title: Option<&str>) -> Result<&mut Self> {
        self.touch();
        self.set_string(Opt::TITLE, title)?;
        Ok(self)
    }

    /// 手动设置终端的工作目录，效果与程序发送 OSC 7 相同，但不会触发
    /// [`on_pwd_changed`](Self::on_pwd_changed)。
    ///
    /// 字符串会被复制进终端。`None` 清空工作目录，等同于设为空字符串。
    pub fn set_pwd(&mut self, pwd: Option<&str>) -> Result<&mut Self> {
        self.touch();
        self.set_string(Opt::PWD, pwd)?;
        Ok(self)
    }

    /// 设置本终端对外声称的 terminfo 条目名（如 `xterm-256color`），用于回答
    /// XTGETTCAP 对 `TN` 的查询。
    ///
    /// 未设置时终端不回答 `TN` 查询，因为 libghostty 不知道外层终端以什么
    /// 身份运行。`None` 清空名字，等同于设为空字符串。名字超过 128 字节时
    /// 返回 [`Error::InvalidValue`]。
    pub fn set_terminfo_name(&mut self, name: Option<&str>) -> Result<&mut Self> {
        self.touch();
        self.set_string(Opt::TERMINFO_NAME, name)?;
        Ok(self)
    }

    /// 单个 Kitty 剪贴板协议（OSC 5522）写事务可累积的最大解码字节数。
    ///
    /// 见 [`Terminal::set_clipboard_write_max_bytes`]。
    pub fn clipboard_write_max_bytes(&self) -> Result<usize> {
        self.get(Data::CLIPBOARD_WRITE_MAX_BYTES)
    }

    /// 设置单个 Kitty 剪贴板协议（OSC 5522）写事务可累积的最大解码字节数。
    ///
    /// 上限在事务开始时确定，进行中的事务沿用开始时的值。超出上限会让整个
    /// 事务以 EFBIG 失败并被丢弃，之后与写相关的包都被忽略，直到新的写事务
    /// 开始，[`on_clipboard_write`](Self::on_clipboard_write) 也不会被调用。
    ///
    /// 事务缓存在内存里，所以这个上限约束了单次写能让终端分配多少内存。
    /// 传 `Some(usize::MAX)` 取消上限，`None` 恢复内置默认值 64 MiB（协议
    /// 要求的最小值）。OSC 52 写不受此限制，它受转义序列最大长度约束。
    pub fn set_clipboard_write_max_bytes(&mut self, max: Option<usize>) -> Result<&mut Self> {
        self.touch();
        self.set_optional(Opt::CLIPBOARD_WRITE_MAX_BYTES, max.as_ref())?;
        Ok(self)
    }

    /// 设置每条未实现的转义序列最多保留多少字节交给
    /// [`on_unknown_sequence`](Self::on_unknown_sequence)。APC 与 OSC 共用这个上限。
    ///
    /// 默认值 0 关闭未知序列上报，此时即使装了回调也不会被调用，也不分配
    /// 内存。超过上限的序列仍会上报，只是内容被截断到上限，并标记为截断。
    ///
    /// 不超过 2048 字节的上限对 OSC 不额外分配内存（复用终端已有的缓冲区），
    /// 更大的上限会为每条未知 OSC 分配内存；未知 APC 总是存放在分配的内存里。
    pub fn set_unknown_sequence_max_bytes(&mut self, max: usize) -> Result<&mut Self> {
        self.touch();
        self.set(Opt::UNKNOWN_MAX_BYTES, &max)?;
        Ok(self)
    }

    /// 开启或关闭对 DECRQCRA（`CSI Pi ; Pg ; Pt ; Pl ; Pb ; Pr * y`）的校验和
    /// 回报。
    ///
    /// 默认关闭：运行中的程序可以逐格计算屏幕校验和，从而读回屏幕上的全部
    /// 内容，包括其他程序的输出。关闭时 XTCHECKSUM（`CSI Ps # y`）也一并被
    /// 忽略。
    pub fn set_xt_checksum_report_enabled(&mut self, enabled: bool) -> Result<&mut Self> {
        self.touch();
        self.set(Opt::XT_CHECKSUM_REPORT, &enabled)?;
        Ok(self)
    }

    /// 设置全量重置（RIS）后 DECRQCRA 校验和的计算方式，同时修改当前的
    /// 计算方式。
    ///
    /// 取值与 XTCHECKSUM（`CSI Ps # y`）及 xterm 的 `checksumExtension` 资源
    /// 相同；运行中的程序在下次重置前仍可用 XTCHECKSUM 修改。`None` 等同于
    /// 空集合，即真实 DEC 终端的算法。
    pub fn set_xt_checksum_extension(&mut self, extension: Option<ChecksumExtension>) -> Result<&mut Self> {
        self.touch();
        let bits = extension.map(|e| e.bits());
        self.set_optional(Opt::XT_CHECKSUM_EXTENSION, bits.as_ref())?;
        Ok(self)
    }

    /// VT 流当前是否处于 ground 状态。
    ///
    /// ground 指流不处于任何序列（UTF-8、ESC、CSI、OSC 等）的中间，是流的
    /// 无状态点，适合插入带外的 VT 序列。例如从 pty 读数据时，可以先等输入
    /// 到达 ground，再写入自己的序列。另见 [`Terminal::vt_write_until_ground`]。
    pub fn is_vt_ground(&self) -> Result<bool> {
        self.get(Data::VT_GROUND)
    }

    /// 光标当前是否位于 shell 提示符或输入区。
    ///
    /// 依赖 OSC 133 等语义提示标记。缺少语义提示信息或处于备用屏时返回
    /// `false`。
    pub fn is_cursor_at_prompt(&self) -> Result<bool> {
        self.get(Data::CURSOR_AT_PROMPT)
    }

    /// 终端当前占用的内存，详见 [`MemoryUsage`]。
    ///
    /// 不会解压回滚区，但会遍历每个 page，所以不要在每次写入后都读取。
    pub fn memory_usage(&self) -> Result<MemoryUsage> {
        let mut raw = ffi::sized!(ffi::TerminalMemoryUsage);
        let result =
            unsafe { ffi::ghostty_terminal_get(self.inner.as_raw(), Data::MEMORY_USAGE, (&raw mut raw).cast()) };
        from_result(result)?;
        Ok(MemoryUsage::from(raw))
    }
    /// 写入 VT 数据，但只写到流第一次回到 ground 为止的最短前缀。
    ///
    /// ground 的含义见 [`Terminal::is_vt_ground`]。流已经处于 ground 时不消耗
    /// 任何字节，返回 `Ok(Some(0))`。到达 ground 时返回
    /// `Ok(Some(consumed))`，`consumed` 包含让流回到 ground 的那个字节；把
    /// `data` 全部写完仍未到达 ground 时返回 `Ok(None)`。
    ///
    /// 与 [`Terminal::vt_write`] 一样会同步触发各类回调，所以需要 `&mut self`。
    pub fn vt_write_until_ground(&mut self, data: &[u8]) -> Result<Option<usize>> {
        self.touch();
        let mut consumed = 0usize;
        let result = unsafe {
            ffi::ghostty_terminal_vt_write_until_ground(
                self.inner.as_raw(),
                data.as_ptr(),
                data.len(),
                &raw mut consumed,
            )
        };
        from_optional_result(result, consumed)
    }
}

/// 一个终端占用的内存，由 [`Terminal::memory_usage`] 返回。
///
/// 终端的大部分内存用于屏幕内容和回滚区，它们存放在固定大小的块（page）
/// 里。常驻字节（resident）是 page 当前实际占用的物理内存，做内存预算时应
/// 以它为准；虚拟字节（virtual）是为 page 保留的地址空间。压缩回滚区会降低
/// 常驻字节，但不降低虚拟字节，因为每个 page 的空间仍为解压保留着。
///
/// 每块屏幕各有一组字段：主屏保存 shell 输出和全部回滚区；备用屏供编辑器
/// 等全屏程序使用，在程序第一次切过去之前全为零。两组相加即终端总量。
///
/// 终端显示的一切（包括颜色、样式、超链接）都存放在 page 里，已计入 page
/// 的数字；图片单独存放，有自己的字段。page 之外的小结构（如窗口标题和
/// 内部簿记）不计入。
///
/// macOS 会延迟回收压缩释放的内存，所以在系统回收之前，进程的 RSS 可能
/// 高于这里的常驻字节。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryUsage {
    /// 在当前平台上压缩回滚区能否释放内存。为 `false` 时
    /// [`Terminal::compress`] 返回 [`CompressionResult::Unsupported`]，
    /// 压缩相关字段恒为零。
    pub compression_supported: bool,
    /// 主屏 page 数，包括已压缩的 page。
    pub primary_pages: u64,
    /// 为主屏 page 保留的地址空间字节数，包括已压缩的 page 和备用的空闲
    /// page。总是不小于 `primary_resident_bytes`。
    pub primary_virtual_bytes: u64,
    /// 主屏 page 占用的物理内存字节数，已压缩的 page 只按压缩后大小计。
    /// 做内存预算时用这个数。
    pub primary_resident_bytes: u64,
    /// 主屏中已压缩的 page 数。
    pub primary_compressed_pages: u64,
    /// 主屏压缩数据的字节数，已包含在 `primary_resident_bytes` 中。
    pub primary_compressed_bytes: u64,
    /// 主屏通过 Kitty 图形协议存放的图片字节数，不包含在
    /// `primary_resident_bytes` 中。未编译 Kitty 图形支持时恒为零。
    pub primary_image_bytes: u64,
    /// 同 `primary_pages`，对应备用屏。
    pub alternate_pages: u64,
    /// 同 `primary_virtual_bytes`，对应备用屏。
    pub alternate_virtual_bytes: u64,
    /// 同 `primary_resident_bytes`，对应备用屏。
    pub alternate_resident_bytes: u64,
    /// 同 `primary_compressed_pages`，对应备用屏。
    pub alternate_compressed_pages: u64,
    /// 同 `primary_compressed_bytes`，对应备用屏。
    pub alternate_compressed_bytes: u64,
    /// 同 `primary_image_bytes`，对应备用屏。
    pub alternate_image_bytes: u64,
}

impl From<ffi::TerminalMemoryUsage> for MemoryUsage {
    fn from(raw: ffi::TerminalMemoryUsage) -> Self {
        Self {
            compression_supported: raw.compression_supported,
            primary_pages: raw.primary_pages,
            primary_virtual_bytes: raw.primary_virtual_bytes,
            primary_resident_bytes: raw.primary_resident_bytes,
            primary_compressed_pages: raw.primary_compressed_pages,
            primary_compressed_bytes: raw.primary_compressed_bytes,
            primary_image_bytes: raw.primary_image_bytes,
            alternate_pages: raw.alternate_pages,
            alternate_virtual_bytes: raw.alternate_virtual_bytes,
            alternate_resident_bytes: raw.alternate_resident_bytes,
            alternate_compressed_pages: raw.alternate_compressed_pages,
            alternate_compressed_bytes: raw.alternate_compressed_bytes,
            alternate_image_bytes: raw.alternate_image_bytes,
        }
    }
}

bitflags::bitflags! {
    /// DECRQCRA 校验和计算方式的修改位，含义与 XTCHECKSUM（`CSI Ps # y`）
    /// 及 xterm 的 `checksumExtension` 资源相同。空集合是真实 DEC 终端的算法。
    ///
    /// 见 [`Terminal::set_xt_checksum_extension`]。
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct ChecksumExtension: u8 {
        /// 不对结果取负。
        const NO_NEGATE = 1;
        /// 不累加每个单元格的视频属性。
        const NO_ATTRIBUTES = 2;
        /// 不忽略空白单元格。
        const KEEP_BLANKS = 4;
        /// 把从未写入过的单元格按空格计算。
        const UNWRITTEN_AS_SPACES = 8;
        /// 使用完整码点，而不是 DEC 8 位取值。
        const FULL_CODEPOINTS = 16;
    }
}
/// 批量读取。
impl Terminal<'_, '_> {
    /// 一次 FFI 调用读取多个终端数据字段，key 见 [`query`]。
    ///
    /// 结果与逐个调用对应的 getter 一致，借用型的结果（如
    /// [`query::Title`]）借用终端。详见 [`crate::multi`]。
    pub fn get_multi<K: crate::multi::Keys<crate::multi::domain::Terminal>>(&self, keys: K) -> Result<K::Output<'_>> {
        let terminal = self.inner.as_raw();
        // SAFETY: 参数原样交给 `ghostty_terminal_get_multi`；输出借用 `self`。
        unsafe {
            keys.read(&mut |tags, values, written| {
                ffi::ghostty_terminal_get_multi(terminal, tags.len(), tags.as_ptr(), values.as_mut_ptr(), written)
            })
        }
    }
}

/// [`Terminal::get_multi`] 的 key。
///
/// 每个 key 的输出与同名 getter 相同。需要输入参数的 [`Terminal::mode`]、
/// 返回句柄的 [`Terminal::kitty_graphics`] 和返回路径的
/// `Terminal::kitty_image_temp_file_dir` 不在其列。
pub mod query {
    use crate::{
        ffi,
        multi::{domain::Terminal as D, multi_key_full, multi_keys},
    };

    multi_keys! {
        domain = D, tags = ffi::TerminalData;
        /// [`Terminal::cols`](crate::Terminal::cols)
        Cols = COLS: copy u16;
        /// [`Terminal::rows`](crate::Terminal::rows)
        Rows = ROWS: copy u16;
        /// [`Terminal::cursor_x`](crate::Terminal::cursor_x)
        CursorX = CURSOR_X: copy u16;
        /// [`Terminal::cursor_y`](crate::Terminal::cursor_y)
        CursorY = CURSOR_Y: copy u16;
        /// [`Terminal::is_cursor_pending_wrap`](crate::Terminal::is_cursor_pending_wrap)
        CursorPendingWrap = CURSOR_PENDING_WRAP: copy bool;
        /// [`Terminal::active_screen`](crate::Terminal::active_screen)
        ActiveScreen = ACTIVE_SCREEN: try ffi::TerminalScreen::Type => crate::screen::Screen;
        /// [`Terminal::is_cursor_visible`](crate::Terminal::is_cursor_visible)
        CursorVisible = CURSOR_VISIBLE: copy bool;
        /// [`Terminal::scrollbar`](crate::Terminal::scrollbar)
        Scrollbar = SCROLLBAR: copy ffi::TerminalScrollbar;
        /// [`Terminal::cursor_style`](crate::Terminal::cursor_style)
        CursorStyle = CURSOR_STYLE: sized ffi::Style => crate::style::Style;
        /// [`Terminal::is_mouse_tracking`](crate::Terminal::is_mouse_tracking)
        MouseTracking = MOUSE_TRACKING: copy bool;
        /// [`Terminal::total_rows`](crate::Terminal::total_rows)
        TotalRows = TOTAL_ROWS: copy usize;
        /// [`Terminal::scrollback_rows`](crate::Terminal::scrollback_rows)
        ScrollbackRows = SCROLLBACK_ROWS: copy usize;
        /// [`Terminal::width_px`](crate::Terminal::width_px)
        WidthPx = WIDTH_PX: copy u32;
        /// [`Terminal::height_px`](crate::Terminal::height_px)
        HeightPx = HEIGHT_PX: copy u32;
        /// [`Terminal::fg_color`](crate::Terminal::fg_color)
        FgColor = COLOR_FOREGROUND: opt[NO_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`Terminal::bg_color`](crate::Terminal::bg_color)
        BgColor = COLOR_BACKGROUND: opt[NO_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`Terminal::cursor_color`](crate::Terminal::cursor_color)
        CursorColor = COLOR_CURSOR: opt[NO_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`Terminal::default_fg_color`](crate::Terminal::default_fg_color)
        DefaultFgColor = COLOR_FOREGROUND_DEFAULT: opt[NO_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`Terminal::default_bg_color`](crate::Terminal::default_bg_color)
        DefaultBgColor = COLOR_BACKGROUND_DEFAULT: opt[NO_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`Terminal::default_cursor_color`](crate::Terminal::default_cursor_color)
        DefaultCursorColor = COLOR_CURSOR_DEFAULT: opt[NO_VALUE] ffi::ColorRgb => crate::style::RgbColor;
        /// [`Terminal::color_palette`](crate::Terminal::color_palette)
        ColorPalette = COLOR_PALETTE: try crate::style::RawPalette => crate::style::Palette;
        /// [`Terminal::default_color_palette`](crate::Terminal::default_color_palette)
        DefaultColorPalette = COLOR_PALETTE_DEFAULT: try crate::style::RawPalette => crate::style::Palette;
        /// [`Terminal::viewport_active`](crate::Terminal::viewport_active)
        ViewportActive = VIEWPORT_ACTIVE: copy bool;
        /// [`Terminal::vt_processing_error`](crate::Terminal::vt_processing_error)
        VtProcessingError = VT_PROCESSING_ERROR: copy bool;
        /// [`Terminal::scrollback_max_bytes`](crate::Terminal::scrollback_max_bytes)
        ScrollbackMaxBytes = SCROLLBACK_MAX_BYTES: opt[NO_VALUE] usize => usize;
        /// [`Terminal::scrollback_max_lines`](crate::Terminal::scrollback_max_lines)
        ScrollbackMaxLines = SCROLLBACK_MAX_LINES: opt[NO_VALUE] usize => usize;
        /// [`Terminal::continuation_max_bytes`](crate::Terminal::continuation_max_bytes)
        ContinuationMaxBytes = CONTINUATION_MAX_BYTES: copy usize;
        /// [`Terminal::is_vt_ground`](crate::Terminal::is_vt_ground)
        VtGround = VT_GROUND: copy bool;
        /// [`Terminal::is_cursor_at_prompt`](crate::Terminal::is_cursor_at_prompt)
        CursorAtPrompt = CURSOR_AT_PROMPT: copy bool;
        /// [`Terminal::clipboard_write_max_bytes`](crate::Terminal::clipboard_write_max_bytes)
        ClipboardWriteMaxBytes = CLIPBOARD_WRITE_MAX_BYTES: copy usize;
        /// [`Terminal::mouse_shape`](crate::Terminal::mouse_shape)
        MouseShape = MOUSE_SHAPE: try ffi::MouseShape::Type => crate::mouse::Shape;
        /// [`Terminal::memory_usage`](crate::Terminal::memory_usage)
        MemoryUsage = MEMORY_USAGE: sized ffi::TerminalMemoryUsage => super::MemoryUsage;
    }

    multi_key_full! {
        /// [`Terminal::kitty_keyboard_flags`](crate::Terminal::kitty_keyboard_flags)
        KittyKeyboardFlags in D = ffi::TerminalData::KITTY_KEYBOARD_FLAGS;
        raw ffi::KittyKeyFlags = 0;
        out<'a> crate::key::KittyKeyFlags;
        absent [];
        convert |raw, present| Ok(crate::key::KittyKeyFlags::from_bits_retain(raw))
    }

    multi_key_full! {
        /// [`Terminal::title`](crate::Terminal::title)，借用终端。
        Title in D = ffi::TerminalData::TITLE;
        raw ffi::String = ffi::String { ptr: std::ptr::null(), len: 0 };
        out<'a> &'a str;
        absent [];
        // SAFETY: 字符串借用终端，`'a` 由 `Terminal::get_multi` 绑定到终端借用上。
        convert |raw, present| std::str::from_utf8(unsafe { raw.to_bytes() })
            .map_err(|_| crate::Error::InvalidValue)
    }

    multi_key_full! {
        /// [`Terminal::pwd`](crate::Terminal::pwd)，借用终端。
        Pwd in D = ffi::TerminalData::PWD;
        raw ffi::String = ffi::String { ptr: std::ptr::null(), len: 0 };
        out<'a> &'a str;
        absent [];
        // SAFETY: 同 `Title`。
        convert |raw, present| std::str::from_utf8(unsafe { raw.to_bytes() })
            .map_err(|_| crate::Error::InvalidValue)
    }

    multi_key_full! {
        /// [`Terminal::selection`](crate::Terminal::selection)，借用终端。
        Selection in D = ffi::TerminalData::SELECTION;
        raw ffi::Selection = ffi::sized!(ffi::Selection);
        out<'a> Option<crate::selection::Selection<'a>>;
        absent [ffi::Result::NO_VALUE];
        // SAFETY: 选区快照来自这个终端，`'a` 绑定到终端借用上。
        convert |raw, present| Ok(present.then(|| unsafe { crate::selection::Selection::from_raw(raw) }))
    }

    #[cfg(feature = "kitty-graphics")]
    mod kitty {
        use crate::{
            ffi,
            multi::{domain::Terminal as D, multi_keys},
        };

        multi_keys! {
            domain = D, tags = ffi::TerminalData;
            /// [`Terminal::kitty_image_storage_limit`](crate::Terminal::kitty_image_storage_limit)
            KittyImageStorageLimit = KITTY_IMAGE_STORAGE_LIMIT: copy u64;
            /// [`Terminal::is_kitty_image_from_file_allowed`](crate::Terminal::is_kitty_image_from_file_allowed)
            KittyImageFromFileAllowed = KITTY_IMAGE_MEDIUM_FILE: copy bool;
            /// [`Terminal::is_kitty_image_from_shared_mem_allowed`](crate::Terminal::is_kitty_image_from_shared_mem_allowed)
            KittyImageFromSharedMemAllowed = KITTY_IMAGE_MEDIUM_SHARED_MEM: copy bool;
        }
    }
    #[cfg(feature = "kitty-graphics")]
    pub use kitty::*;
}

impl Drop for Terminal<'_, '_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_terminal_free(self.inner.as_raw()) }
    }
}

/// A point in the terminal grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// Active area where the cursor can move.
    Active(PointCoordinate),
    /// Visible viewport (changes when scrolled).
    Viewport(PointCoordinate),
    /// Full screen including scrollback.
    Screen(PointCoordinate),
    /// Scrollback history only (before active area).
    History(PointCoordinate),
}

impl From<Point> for ffi::Point {
    fn from(value: Point) -> Self {
        match value {
            Point::Active(coord) => {
                Self { tag: ffi::PointTag::ACTIVE, value: ffi::PointValue { coordinate: coord.into() } }
            }
            Point::Viewport(coord) => {
                Self { tag: ffi::PointTag::VIEWPORT, value: ffi::PointValue { coordinate: coord.into() } }
            }
            Point::Screen(coord) => {
                Self { tag: ffi::PointTag::SCREEN, value: ffi::PointValue { coordinate: coord.into() } }
            }
            Point::History(coord) => {
                Self { tag: ffi::PointTag::HISTORY, value: ffi::PointValue { coordinate: coord.into() } }
            }
        }
    }
}

/// A coordinate space for converting grid references back to points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointSpace {
    /// Active area where the cursor can move.
    Active,
    /// Visible viewport, which changes when scrolled.
    Viewport,
    /// Full screen including scrollback.
    Screen,
    /// Scrollback history only, before the active area.
    History,
}

impl PointSpace {
    pub(crate) fn into_raw(self) -> ffi::PointTag::Type {
        match self {
            Self::Active => ffi::PointTag::ACTIVE,
            Self::Viewport => ffi::PointTag::VIEWPORT,
            Self::Screen => ffi::PointTag::SCREEN,
            Self::History => ffi::PointTag::HISTORY,
        }
    }
}

/// A coordinate in the terminal grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointCoordinate {
    /// Column (0-indexed).
    pub x: u16,
    /// Row (0-indexed). May exceed page size for screen/history tags.
    pub y: u32,
}
impl From<PointCoordinate> for ffi::PointCoordinate {
    fn from(value: PointCoordinate) -> Self {
        let PointCoordinate { x, y } = value;
        Self { x, y }
    }
}
impl From<ffi::PointCoordinate> for PointCoordinate {
    fn from(value: ffi::PointCoordinate) -> Self {
        let ffi::PointCoordinate { x, y } = value;
        Self { x, y }
    }
}

/// Scroll viewport behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollViewport {
    /// Scroll to the top of the scrollback.
    Top,
    /// Scroll to the bottom (active area).
    Bottom,
    /// Scroll by a delta amount (up is negative).
    Delta(isize),
    /// Scroll to an absolute row offset from the top of the scrollback.
    Row(usize),
}
impl From<ScrollViewport> for ffi::TerminalScrollViewport {
    fn from(value: ScrollViewport) -> Self {
        match value {
            ScrollViewport::Top => {
                Self { tag: ffi::TerminalScrollViewportTag::TOP, value: ffi::TerminalScrollViewportValue::default() }
            }
            ScrollViewport::Bottom => {
                Self { tag: ffi::TerminalScrollViewportTag::BOTTOM, value: ffi::TerminalScrollViewportValue::default() }
            }
            ScrollViewport::Delta(delta) => Self {
                tag: ffi::TerminalScrollViewportTag::DELTA,
                value: {
                    let mut v = ffi::TerminalScrollViewportValue::default();
                    v.delta = delta;
                    v
                },
            },
            ScrollViewport::Row(row) => Self {
                tag: ffi::TerminalScrollViewportTag::ROW,
                value: {
                    let mut v = ffi::TerminalScrollViewportValue::default();
                    v.row = row;
                    v
                },
            },
        }
    }
}

/// A terminal mode consisting of its value and its kind (DEC/ANSI).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Mode(pub ffi::Mode);

impl Mode {
    #![expect(missing_docs, reason = "no upstream documentation provided")]
    const ANSI_BIT: u16 = 1 << 15;

    /// Create a new mode from its numeric value and its kind.
    #[must_use]
    pub const fn new(v: u16, kind: ModeKind) -> Self {
        match kind {
            ModeKind::Ansi => Self(v | Self::ANSI_BIT),
            ModeKind::Dec => Self(v),
        }
    }

    /// The numeric value of the mode.
    #[must_use]
    pub const fn value(self) -> u16 {
        (self.0) & 0x7fff
    }

    /// The kind of the mode (DEC/ANSI).
    #[must_use]
    pub const fn kind(self) -> ModeKind {
        if (self.0) & Self::ANSI_BIT > 0 { ModeKind::Ansi } else { ModeKind::Dec }
    }

    pub const KAM: Self = Self::new(2, ModeKind::Ansi);
    pub const INSERT: Self = Self::new(4, ModeKind::Ansi);
    pub const SRM: Self = Self::new(12, ModeKind::Ansi);
    pub const LINEFEED: Self = Self::new(20, ModeKind::Ansi);

    pub const DECCKM: Self = Self::new(1, ModeKind::Dec);
    pub const _132_COLUMN: Self = Self::new(3, ModeKind::Dec);
    pub const SLOW_SCROLL: Self = Self::new(4, ModeKind::Dec);
    pub const REVERSE_COLORS: Self = Self::new(5, ModeKind::Dec);
    pub const ORIGIN: Self = Self::new(6, ModeKind::Dec);
    pub const WRAPAROUND: Self = Self::new(7, ModeKind::Dec);
    pub const AUTOREPEAT: Self = Self::new(8, ModeKind::Dec);
    pub const X10_MOUSE: Self = Self::new(9, ModeKind::Dec);
    pub const CURSOR_BLINKING: Self = Self::new(12, ModeKind::Dec);
    pub const CURSOR_VISIBLE: Self = Self::new(25, ModeKind::Dec);
    pub const ENABLE_MODE3: Self = Self::new(40, ModeKind::Dec);
    pub const REVERSE_WRAP: Self = Self::new(45, ModeKind::Dec);
    pub const ALT_SCREEN_LEGACY: Self = Self::new(47, ModeKind::Dec);
    pub const KEYPAD_KEYS: Self = Self::new(66, ModeKind::Dec);
    /// 退格键模式（DECBKM）。
    pub const BACKARROW_KEY_MODE: Self = Self::new(67, ModeKind::Dec);
    pub const LEFT_RIGHT_MARGIN: Self = Self::new(69, ModeKind::Dec);
    pub const NORMAL_MOUSE: Self = Self::new(1000, ModeKind::Dec);
    pub const BUTTON_MOUSE: Self = Self::new(1002, ModeKind::Dec);
    pub const ANY_MOUSE: Self = Self::new(1003, ModeKind::Dec);
    pub const FOCUS_EVENT: Self = Self::new(1004, ModeKind::Dec);
    pub const UTF8_MOUSE: Self = Self::new(1005, ModeKind::Dec);
    pub const SGR_MOUSE: Self = Self::new(1006, ModeKind::Dec);
    pub const ALT_SCROLL: Self = Self::new(1007, ModeKind::Dec);
    pub const URXVT_MOUSE: Self = Self::new(1015, ModeKind::Dec);
    pub const SGR_PIXELS_MOUSE: Self = Self::new(1016, ModeKind::Dec);
    pub const NUMLOCK_KEYPAD: Self = Self::new(1035, ModeKind::Dec);
    pub const ALT_ESC_PREFIX: Self = Self::new(1036, ModeKind::Dec);
    pub const ALT_SENDS_ESC: Self = Self::new(1039, ModeKind::Dec);
    pub const REVERSE_WRAP_EXT: Self = Self::new(1045, ModeKind::Dec);
    pub const ALT_SCREEN: Self = Self::new(1047, ModeKind::Dec);
    pub const SAVE_CURSOR: Self = Self::new(1048, ModeKind::Dec);
    pub const ALT_SCREEN_SAVE: Self = Self::new(1049, ModeKind::Dec);
    pub const BRACKETED_PASTE: Self = Self::new(2004, ModeKind::Dec);
    pub const SYNC_OUTPUT: Self = Self::new(2026, ModeKind::Dec);
    pub const GRAPHEME_CLUSTER: Self = Self::new(2027, ModeKind::Dec);
    pub const COLOR_SCHEME_REPORT: Self = Self::new(2031, ModeKind::Dec);
    pub const VISIBILITY_REPORT: Self = Self::new(2033, ModeKind::Dec);
    pub const IN_BAND_RESIZE: Self = Self::new(2048, ModeKind::Dec);
    /// Kitty 剪贴板协议的粘贴事件，见 [`Terminal::paste`]。
    pub const PASTE_EVENTS: Self = Self::new(5522, ModeKind::Dec);
}

impl Mode {
    /// 编码一条 DECRPM（DEC 私有模式报告）回复序列，写入 `buf`，返回写入的
    /// 字节数。
    ///
    /// DEC 私有模式生成 `CSI ? Ps1 ; Ps2 $ y`，ANSI 模式生成
    /// `CSI Ps1 ; Ps2 $ y`。终端自己回答 DECRQM 时不需要它；它用于宿主自行
    /// 处理模式查询的场景。
    ///
    /// 缓冲区太小时返回 [`Error::OutOfSpace`]，`required` 为所需大小，可以
    /// 换足够大的缓冲区重试。
    pub fn encode_report(self, state: ModeReportState, buf: &mut [u8]) -> Result<usize> {
        let mut written = 0;
        let result = unsafe {
            ffi::ghostty_mode_report_encode(self.0, state.into(), buf.as_mut_ptr().cast(), buf.len(), &raw mut written)
        };
        from_result_with_len(result, written)
    }
}

/// DECRPM 报告中的模式状态，即回复 `CSI ? Ps1 ; Ps2 $ y` 里的 `Ps2`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum ModeReportState {
    /// 不认识这个模式。
    NotRecognized = ffi::ModeReportState::NOT_RECOGNIZED,
    /// 模式已设置（启用）。
    Set = ffi::ModeReportState::SET,
    /// 模式已重置（关闭）。
    Reset = ffi::ModeReportState::RESET,
    /// 模式永久设置。
    PermanentlySet = ffi::ModeReportState::PERMANENTLY_SET,
    /// 模式永久重置。
    PermanentlyReset = ffi::ModeReportState::PERMANENTLY_RESET,
}

/// The kind of a terminal mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ModeKind {
    /// DEC terminal mode.
    Dec,
    /// ANSI terminal mode.
    Ansi,
}

impl From<Mode> for ffi::Mode {
    fn from(value: Mode) -> Self {
        value.0
    }
}

/// Device attributes response data for all three DA levels.
/// Filled by the [`Terminal::on_device_attributes`] callback in response
/// to CSI c, CSI > c, or CSI = c queries. The terminal uses whichever
/// sub-struct matches the request type.
#[derive(Debug, Clone, Copy)]
pub struct DeviceAttributes {
    /// Primary device attributes (DA1).
    pub primary: PrimaryDeviceAttributes,
    /// Secondary device attributes (DA2).
    pub secondary: SecondaryDeviceAttributes,
    /// Tertiary device attributes (DA3).
    pub tertiary: TertiaryDeviceAttributes,
}

impl From<DeviceAttributes> for ffi::DeviceAttributes {
    fn from(value: DeviceAttributes) -> Self {
        Self { primary: value.primary.into(), secondary: value.secondary.into(), tertiary: value.tertiary.into() }
    }
}

/// Primary device attributes (DA1) response data.
///
/// Returned as part of [`DeviceAttributes`] in response to a CSI c query.
#[derive(Debug, Clone, Copy)]
pub struct PrimaryDeviceAttributes(ffi::DeviceAttributesPrimary);

impl PrimaryDeviceAttributes {
    /// Construct primary device attributes from a conformance level
    /// and an array of device attribute features.
    ///
    /// Prefer defining primary device attributes as a `const` when the feature
    /// list is statically known. That makes the 64-feature limit fail during
    /// compilation instead of panicking at runtime.
    ///
    /// # Panics
    ///
    /// **Panics** when more than 64 features are given.
    #[must_use]
    pub const fn new(conformance_level: ConformanceLevel, features: &[DeviceAttributeFeature]) -> Self {
        assert!(features.len() <= 64);

        let mut f = [0u16; 64];
        let mut i = 0;
        while i < features.len() {
            f[i] = features[i].0;
            i += 1;
        }

        Self(ffi::DeviceAttributesPrimary {
            conformance_level: conformance_level.0,
            features: f,
            num_features: features.len(),
        })
    }
}

impl From<PrimaryDeviceAttributes> for ffi::DeviceAttributesPrimary {
    fn from(value: PrimaryDeviceAttributes) -> Self {
        value.0
    }
}

/// The level of conformance to the behavior of a specific or a family of
/// physical terminal models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConformanceLevel(pub u16);

impl ConformanceLevel {
    #![expect(clippy::doc_markdown, reason = "false positive")]
    #![expect(missing_docs, reason = "self-explanatory")]
    pub const VT100: Self = Self(ffi::DA_CONFORMANCE_VT100);
    pub const VT101: Self = Self(ffi::DA_CONFORMANCE_VT101);
    pub const VT102: Self = Self(ffi::DA_CONFORMANCE_VT102);
    pub const VT125: Self = Self(ffi::DA_CONFORMANCE_VT125);
    pub const VT131: Self = Self(ffi::DA_CONFORMANCE_VT131);
    pub const VT132: Self = Self(ffi::DA_CONFORMANCE_VT132);
    pub const VT220: Self = Self(ffi::DA_CONFORMANCE_VT220);
    pub const VT240: Self = Self(ffi::DA_CONFORMANCE_VT240);
    pub const VT320: Self = Self(ffi::DA_CONFORMANCE_VT320);
    pub const VT340: Self = Self(ffi::DA_CONFORMANCE_VT340);
    pub const VT420: Self = Self(ffi::DA_CONFORMANCE_VT420);
    pub const VT510: Self = Self(ffi::DA_CONFORMANCE_VT510);
    pub const VT520: Self = Self(ffi::DA_CONFORMANCE_VT520);
    pub const VT525: Self = Self(ffi::DA_CONFORMANCE_VT525);
    /// Equivalent to a VT2xx terminal.
    pub const LEVEL_2: Self = Self(ffi::DA_CONFORMANCE_LEVEL_2);
    /// Equivalent to a VT3xx terminal.
    pub const LEVEL_3: Self = Self(ffi::DA_CONFORMANCE_LEVEL_3);
    /// Equivalent to a VT4xx terminal.
    pub const LEVEL_4: Self = Self(ffi::DA_CONFORMANCE_LEVEL_4);
    /// Equivalent to a VT5xx terminal.
    pub const LEVEL_5: Self = Self(ffi::DA_CONFORMANCE_LEVEL_5);
}

/// A feature that a terminal can report to support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceAttributeFeature(pub u16);

impl DeviceAttributeFeature {
    #![expect(missing_docs, reason = "no upstream documentation provided")]
    pub const COLUMNS_132: Self = Self(ffi::DA_FEATURE_COLUMNS_132);
    pub const PRINTER: Self = Self(ffi::DA_FEATURE_PRINTER);
    pub const REGIS: Self = Self(ffi::DA_FEATURE_REGIS);
    pub const SIXEL: Self = Self(ffi::DA_FEATURE_SIXEL);
    pub const SELECTIVE_ERASE: Self = Self(ffi::DA_FEATURE_SELECTIVE_ERASE);
    pub const USER_DEFINED_KEYS: Self = Self(ffi::DA_FEATURE_USER_DEFINED_KEYS);
    pub const NATIONAL_REPLACEMENT: Self = Self(ffi::DA_FEATURE_NATIONAL_REPLACEMENT);
    pub const TECHNICAL_CHARACTERS: Self = Self(ffi::DA_FEATURE_TECHNICAL_CHARACTERS);
    pub const LOCATOR: Self = Self(ffi::DA_FEATURE_LOCATOR);
    pub const TERMINAL_STATE: Self = Self(ffi::DA_FEATURE_TERMINAL_STATE);
    pub const WINDOWING: Self = Self(ffi::DA_FEATURE_WINDOWING);
    pub const HORIZONTAL_SCROLLING: Self = Self(ffi::DA_FEATURE_HORIZONTAL_SCROLLING);
    pub const ANSI_COLOR: Self = Self(ffi::DA_FEATURE_ANSI_COLOR);
    pub const RECTANGULAR_EDITING: Self = Self(ffi::DA_FEATURE_RECTANGULAR_EDITING);
    pub const ANSI_TEXT_LOCATOR: Self = Self(ffi::DA_FEATURE_ANSI_TEXT_LOCATOR);
    pub const CLIPBOARD: Self = Self(ffi::DA_FEATURE_CLIPBOARD);
}

/// Secondary device attributes (DA2) response data.
///
/// Returned as part of [`DeviceAttributes`] in response to a CSI > c query.
/// Response format: CSI > Pp ; Pv ; Pc c
#[derive(Debug, Copy, Clone)]
pub struct SecondaryDeviceAttributes {
    /// Terminal type identifier (Pp).
    pub device_type: DeviceType,
    /// Firmware/patch version number (Pv).
    pub firmware_version: u16,
    /// ROM cartridge registration number (Pc). Always 0 for emulators.
    pub rom_cartridge: u16,
}

impl From<SecondaryDeviceAttributes> for ffi::DeviceAttributesSecondary {
    fn from(value: SecondaryDeviceAttributes) -> Self {
        Self {
            device_type: value.device_type.0,
            firmware_version: value.firmware_version,
            rom_cartridge: value.rom_cartridge,
        }
    }
}

/// The type of terminal device being emulated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceType(pub u16);

impl DeviceType {
    #![expect(missing_docs, reason = "self-explanatory")]
    pub const VT100: Self = Self(ffi::DA_DEVICE_TYPE_VT100);
    pub const VT220: Self = Self(ffi::DA_DEVICE_TYPE_VT220);
    pub const VT240: Self = Self(ffi::DA_DEVICE_TYPE_VT240);
    pub const VT330: Self = Self(ffi::DA_DEVICE_TYPE_VT330);
    pub const VT340: Self = Self(ffi::DA_DEVICE_TYPE_VT340);
    pub const VT320: Self = Self(ffi::DA_DEVICE_TYPE_VT320);
    pub const VT382: Self = Self(ffi::DA_DEVICE_TYPE_VT382);
    pub const VT420: Self = Self(ffi::DA_DEVICE_TYPE_VT420);
    pub const VT510: Self = Self(ffi::DA_DEVICE_TYPE_VT510);
    pub const VT520: Self = Self(ffi::DA_DEVICE_TYPE_VT520);
    pub const VT525: Self = Self(ffi::DA_DEVICE_TYPE_VT525);
}

/// Tertiary device attributes (DA3) response data.
///
/// Returned as part of [`DeviceAttributes`] in response to a CSI = c query.
/// Response format: DCS ! | D...D ST (DECRPTUI).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TertiaryDeviceAttributes {
    /// Unit ID encoded as 8 uppercase hex digits in the response.
    pub unit_id: u32,
}

impl From<TertiaryDeviceAttributes> for ffi::DeviceAttributesTertiary {
    fn from(value: TertiaryDeviceAttributes) -> Self {
        Self { unit_id: value.unit_id }
    }
}

/// Color scheme reported in response to a CSI ? 996 n query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
#[expect(missing_docs, reason = "self-explanatory")]
pub enum ColorScheme {
    Light = ffi::ColorScheme::LIGHT,
    Dark = ffi::ColorScheme::DARK,
}

impl ColorScheme {
    /// Encode a color scheme report into an escape sequence.
    ///
    /// Encodes a color scheme report into the provided buffer. Dark color
    /// schemes emit `ESC [ ? 997 ; 1 n`, and light color schemes emit
    /// `ESC [ ? 997 ; 2 n`. The encoded bytes are identical to the terminal's
    /// internal `CSI ? 996 n` query response.
    ///
    /// Hosts should gate unsolicited sends on mode 2031 being set, which can
    /// be checked via the mode getters.
    ///
    /// If the buffer is too small, returns [`Error::OutOfSpace`] with the
    /// required buffer size. The caller can then retry with a sufficiently
    /// sized buffer.
    pub fn encode_report(self, buf: &mut [u8]) -> Result<usize> {
        let mut written = 0;
        let result = unsafe {
            ffi::ghostty_color_scheme_report_encode(self.into(), buf.as_mut_ptr().cast(), buf.len(), &raw mut written)
        };
        from_result_with_len(result, written)
    }
}

impl From<ColorScheme> for ffi::ColorScheme::Type {
    fn from(value: ColorScheme) -> Self {
        match value {
            ColorScheme::Light => ffi::ColorScheme::LIGHT,
            ColorScheme::Dark => ffi::ColorScheme::DARK,
        }
    }
}

/// 尺寸报告的格式，见 [`SizeReportStyle::encode_report`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum SizeReportStyle {
    /// 带内尺寸报告（模式 2048）：`ESC [ 48 ; rows ; cols ; height ; width t`。
    Mode2048 = ffi::SizeReportStyle::MODE_2048,
    /// XTWINOPS 文本区像素尺寸：`ESC [ 4 ; height ; width t`。
    Csi14T = ffi::SizeReportStyle::CSI_14_T,
    /// XTWINOPS 单元格像素尺寸：`ESC [ 6 ; height ; width t`。
    Csi16T = ffi::SizeReportStyle::CSI_16_T,
    /// XTWINOPS 文本区字符尺寸：`ESC [ 8 ; rows ; cols t`。
    Csi18T = ffi::SizeReportStyle::CSI_18_T,
}

impl SizeReportStyle {
    /// 按这种格式把终端尺寸编码为转义序列，写入 `buf`，返回写入的字节数。
    ///
    /// 编码结果与终端通过 [`Terminal::on_size`] 自行回答查询时相同；宿主要
    /// 自己发送尺寸报告（例如模式 2048 下的主动报告）时使用。
    ///
    /// 缓冲区太小时返回 [`Error::OutOfSpace`]，`required` 为所需大小，可以
    /// 换足够大的缓冲区重试。
    pub fn encode_report(self, size: SizeReportSize, buf: &mut [u8]) -> Result<usize> {
        let mut written = 0;
        let result = unsafe {
            ffi::ghostty_size_report_encode(self.into(), size, buf.as_mut_ptr().cast(), buf.len(), &raw mut written)
        };
        from_result_with_len(result, written)
    }
}

/// Amount of compression work to perform before returning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
pub enum CompressionMode {
    /// Perform one bounded compression step suitable for idle scheduling.
    Incremental = ffi::TerminalCompressionMode::INCREMENTAL,
    /// Synchronously inspect every currently eligible page.
    Full = ffi::TerminalCompressionMode::FULL,
}

/// Scheduling result from terminal compression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
pub enum CompressionResult {
    /// Retained-mapping reclamation is unavailable on this target.
    Unsupported = ffi::TerminalCompressionResult::UNSUPPORTED,
    /// More incremental compression work remains.
    Pending = ffi::TerminalCompressionResult::PENDING,
    /// The pass has no continuation to schedule.
    Complete = ffi::TerminalCompressionResult::COMPLETE,
}

/// Opaque token representing a terminal's current compression activity.
///
/// The token is opaque and only equality comparisons are meaningful.
/// An embedding application should cache it and restart its compression idle
/// delay whenever the value changes. The value may wrap and changes in either
/// direction have the same meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressionActivity(u64);

/// A synchronous request to write clipboard contents.
///
/// The request, contents array, MIME strings, and data strings are all
/// borrowed and valid only for the callback duration.
///
/// All entries in [`contents`](Self::contents) are representations of the
/// same logical value and must be committed atomically. An empty `contents`
/// requests that the destination be cleared. This is distinct from a content
/// entry whose data has zero length.
///
/// The write is answered by calling [`reply`](Self::reply). This must happen
/// within the clipboard write request callback. Returning without replying
/// denies the write.
///
/// Fields a linked libghostty is too old to send read as empty or false,
/// and [`reply`](Self::reply) then does nothing, which denies the write.
#[derive(Debug)]
pub struct ClipboardWrite<'t> {
    ptr: *const ffi::ClipboardWrite,
    _phan: PhantomData<&'t ()>,
}

impl<'t> ClipboardWrite<'t> {
    /// Name of the writing program for permission prompts, if the protocol
    /// carries one. Empty otherwise.
    #[must_use]
    pub fn name(&self) -> &'t [u8] {
        // SAFETY: The request and its strings live for the callback duration.
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardWrite, name).map_or(&[], |n| n.to_bytes()) }
    }

    /// True if the terminal already holds a session grant for this request.
    /// The embedder should skip any permission prompt and perform the write.
    #[must_use]
    pub fn granted(&self) -> bool {
        // SAFETY: The request lives for the callback duration.
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardWrite, granted) }.unwrap_or(false)
    }

    /// True if the program supplied a session password, so the embedder may
    /// offer to remember the user's decision through the `remember` argument
    /// of [`reply`](Self::reply). When false, `remember` is ignored.
    #[must_use]
    pub fn can_remember(&self) -> bool {
        // SAFETY: The request lives for the callback duration.
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardWrite, can_remember) }.unwrap_or(false)
    }

    /// Answer the write.
    ///
    /// The result answers the program with the matching protocol status for
    /// protocols with a write acknowledgement (OSC 5522: DONE, EPERM, ENOSYS,
    /// EBUSY, EINVAL, EIO); protocols without one (OSC 52, OSC 1337 Copy)
    /// discard the reply.
    ///
    /// `remember` records a session grant so future requests from the same
    /// program skip the permission prompt. It is only honored on success when
    /// [`can_remember`](Self::can_remember) is set.
    pub fn reply(self, result: std::result::Result<(), ClipboardWriteError>, remember: bool) {
        let reply = ffi::ClipboardWriteReply {
            result: result.map_or_else(Into::into, |()| ffi::ClipboardWriteResult::SUCCESS),
            remember,
            ..ffi::sized!(ffi::ClipboardWriteReply)
        };
        // SAFETY: The request lives for the callback duration.
        if let Some(callback) = unsafe { crate::sized_field!(self.ptr, ffi::ClipboardWrite, reply) }.flatten() {
            // SAFETY: The reply only needs to outlive this synchronous call.
            unsafe { callback(self.ptr, &raw const reply) };
        }
    }

    /// # Safety
    ///
    /// Caller must ensure that the given pointer has the correct lifetime.
    unsafe fn from_raw(ptr: *const ffi::ClipboardWrite) -> Self {
        Self { ptr, _phan: PhantomData }
    }

    /// Get the clipboard's destination.
    #[must_use]
    pub fn location(&self) -> ClipboardLocation {
        // SAFETY: We trust libghostty to give us a valid pointer
        // within the lifetime of the callback.
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardWrite, location) }
            .and_then(|location| location.try_into().ok())
            .unwrap_or(ClipboardLocation::Standard)
    }
    /// Get an iterator into a borrowed array of MIME representations.
    ///
    /// The iterator is empty for a write carrying no representations, which
    /// requests that the destination be cleared (e.g. OSC 52 with an empty
    /// payload).
    #[must_use]
    pub fn contents(&self) -> ClipboardContents<'t> {
        // SAFETY: We trust libghostty to give us a valid pointer
        // within the lifetime of the callback.
        let (ptr, len) = unsafe {
            (
                crate::sized_field!(self.ptr, ffi::ClipboardWrite, contents),
                crate::sized_field!(self.ptr, ffi::ClipboardWrite, contents_len),
            )
        };
        // The C API declares `contents` optional and sends null for a write
        // carrying no representations (the "clear the clipboard" shape);
        // `from_raw_parts` requires a non-null pointer even at length zero.
        let contents: &'t [ffi::ClipboardContent] = match (ptr, len) {
            (Some(ptr), Some(len)) if !ptr.is_null() => {
                // SAFETY: We trust libghostty to give us a valid pointer and
                // length within the lifetime of the callback.
                unsafe { std::slice::from_raw_parts(ptr, len) }
            }
            _ => &[],
        };
        ClipboardContents(contents.iter())
    }
}

/// An iterator into a borrowed array of MIME representations.
#[derive(Clone, Debug)]
pub struct ClipboardContents<'t>(std::slice::Iter<'t, ffi::ClipboardContent>);

impl<'t> Iterator for ClipboardContents<'t> {
    type Item = ClipboardContent<'t>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|v| unsafe { ClipboardContent::from_raw(v) })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl DoubleEndedIterator for ClipboardContents<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|v| unsafe { ClipboardContent::from_raw(v) })
    }
}
impl ExactSizeIterator for ClipboardContents<'_> {}
impl std::iter::FusedIterator for ClipboardContents<'_> {}

/// One MIME representation in a clipboard write.
///
/// The data is binary-safe and has already been decoded from any protocol-level
/// encoding. A zero-length data string is an explicit empty representation; it
/// does not clear the clipboard.
#[derive(Clone, Copy, Debug)]
pub struct ClipboardContent<'t> {
    /// MIME type of the representation.
    pub mime: &'t str,
    /// Decoded, binary-safe representation data.
    pub data: &'t [u8],
}
impl ClipboardContent<'_> {
    /// # Safety
    ///
    /// Caller must guarantee that the given raw value is valid within
    /// the given lifetime.
    unsafe fn from_raw(value: &ffi::ClipboardContent) -> Self {
        // SAFETY: Upheld by caller
        unsafe {
            Self {
                // Ghostty currently only emits ASCII mime types, but the C
                // API does not guarantee UTF-8, so validate rather than
                // trust; fall back to the opaque-bytes mime type.
                mime: std::str::from_utf8(value.mime.to_bytes()).unwrap_or("application/octet-stream"),
                // The data is binary-safe per the C API (e.g. an image/png
                // representation), so it must not be exposed as `str`.
                data: value.data.to_bytes(),
            }
        }
    }
}

/// Clipboard destination for a clipboard write.
///
/// Protocol-specific destination identifiers are normalized to these values
/// before the clipboard write callback is invoked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
pub enum ClipboardLocation {
    /// The standard system clipboard.
    Standard = ffi::ClipboardLocation::STANDARD,
    /// The selection clipboard.
    Selection = ffi::ClipboardLocation::SELECTION,
    /// The primary selection clipboard.
    Primary = ffi::ClipboardLocation::PRIMARY,
}

/// Result of a clipboard write reply.
///
/// Protocols with a write acknowledgement (OSC 5522) answer the program with
/// the matching status; protocols without one (OSC 52, OSC 1337 Copy) discard
/// the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
pub enum ClipboardWriteError {
    /// The clipboard write was denied by policy or the user.
    Denied = ffi::ClipboardWriteResult::DENIED,
    /// The destination or one or more representations are unsupported.
    Unsupported = ffi::ClipboardWriteResult::UNSUPPORTED,
    /// The clipboard is temporarily unavailable.
    Busy = ffi::ClipboardWriteResult::BUSY,
    /// One or more representations contain invalid data.
    InvalidData = ffi::ClipboardWriteResult::INVALID_DATA,
    /// The clipboard write failed due to an I/O error.
    IoError = ffi::ClipboardWriteResult::IO_ERROR,
}

/// A request to show a desktop notification.
#[derive(Debug, Copy, Clone)]
pub struct DesktopNotification<'t> {
    ptr: *const ffi::TerminalDesktopNotification,
    _phan: PhantomData<&'t ()>,
}

impl<'t> DesktopNotification<'t> {
    unsafe fn from_raw(raw: *const ffi::TerminalDesktopNotification) -> Self {
        Self { ptr: raw, _phan: PhantomData }
    }

    /// Get the notification title, or an empty string when the protocol omits it.
    #[must_use]
    pub fn title(self) -> &'t str {
        // SAFETY: We trust libghostty to give us a valid underlying ptr
        // AND that the title contains to a valid UTF-8 string.
        unsafe { (*self.ptr).title.to_str() }
    }
    /// Notification body.
    #[must_use]
    pub fn body(self) -> &'t str {
        // SAFETY: We trust libghostty to give us a valid underlying ptr
        // AND that the title contains to a valid UTF-8 string.
        unsafe { (*self.ptr).body.to_str() }
    }
}

/// A progress report emitted by the running program.
#[derive(Debug, Copy, Clone)]
pub struct ProgressReport<'t> {
    ptr: *const ffi::TerminalProgressReport,
    _phan: PhantomData<&'t ()>,
}

impl ProgressReport<'_> {
    unsafe fn from_raw(raw: *const ffi::TerminalProgressReport) -> Self {
        Self { ptr: raw, _phan: PhantomData }
    }

    /// Literal progress state reported by the running program.
    pub fn state(self) -> Result<ProgressState> {
        // SAFETY: We trust libghostty to give us a valid underlying ptr
        unsafe { *self.ptr }.state.try_into().map_err(|_| Error::InvalidValue)
    }

    /// Progress percentage from 0 through 100, or `None` when omitted.
    #[must_use]
    pub fn progress(self) -> Option<u8> {
        // SAFETY: We trust libghostty to give us a valid underlying ptr
        match unsafe { *self.ptr }.progress {
            ..=-1 => None,
            v => Some(v as u8),
        }
    }
}

/// State of a terminal progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum ProgressState {
    /// Remove any visible progress indication.
    Remove = ffi::TerminalProgressState::REMOVE,
    /// Show determinate progress.
    Set = ffi::TerminalProgressState::SET,
    /// Show a failed progress state.
    Error = ffi::TerminalProgressState::ERROR,
    /// Show indeterminate progress.
    Indeterminate = ffi::TerminalProgressState::INDETERMINATE,
    /// Show paused progress.
    Pause = ffi::TerminalProgressState::PAUSE,
}

/// 一次同步的剪贴板读取请求，由 [`Terminal::on_clipboard_read`] 的回调收到。
///
/// 请求及其中的字符串都是借用的，只在回调期间有效。
///
/// 读取通过 [`reply`](Self::reply) 应答，而且必须在回调返回之前应答；不应答
/// 就返回，等同于给程序一个空剪贴板（OSC 52）或 EPERM（OSC 5522）。
///
/// 链接的 libghostty 太旧而没有发送的字段读作空或 `false`，此时
/// [`reply`](Self::reply) 什么也不做，效果同不应答。
#[derive(Debug)]
pub struct ClipboardRead<'t> {
    ptr: *const ffi::ClipboardRead,
    _phan: PhantomData<&'t ()>,
}

impl<'t> ClipboardRead<'t> {
    /// # Safety
    ///
    /// 调用方必须保证指针在 `'t` 内有效。
    unsafe fn from_raw(ptr: *const ffi::ClipboardRead) -> Self {
        Self { ptr, _phan: PhantomData }
    }

    /// 要读取的剪贴板。
    #[must_use]
    pub fn location(&self) -> ClipboardLocation {
        // SAFETY: 请求在回调期间有效。
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardRead, location) }
            .and_then(|location| location.try_into().ok())
            .unwrap_or(ClipboardLocation::Standard)
    }

    /// 程序想要的 MIME 类型，按偏好排序。只承载文本的协议（OSC 52）请求
    /// `text/plain`。
    ///
    /// 只用于列出可用类型的请求（见 [`wants_list`](Self::wants_list)）可能
    /// 一个都没有。协议里的 MIME 类型都是 ASCII；不是合法 UTF-8 的条目会被
    /// 跳过，因为没有能回答它的 MIME 字符串。
    pub fn mimes(&self) -> impl Iterator<Item = &'t str> + use<'t> {
        // SAFETY: 请求在回调期间有效。
        let (ptr, len) = unsafe {
            (
                crate::sized_field!(self.ptr, ffi::ClipboardRead, mimes),
                crate::sized_field!(self.ptr, ffi::ClipboardRead, mimes_len),
            )
        };
        // mimes_len 为零时 C 侧传 NULL，而 `from_raw_parts` 即使长度为零也
        // 要求非空指针。
        let mimes: &'t [ffi::String] = match (ptr, len) {
            (Some(ptr), Some(len)) if !ptr.is_null() && len > 0 => {
                // SAFETY: libghostty 保证指针和长度在回调期间有效。
                unsafe { std::slice::from_raw_parts(ptr, len) }
            }
            _ => &[],
        };
        mimes.iter().filter_map(|mime| {
            // SAFETY: 字符串在回调期间有效。
            std::str::from_utf8(unsafe { mime.to_bytes() }).ok()
        })
    }

    /// 程序是否还想要剪贴板上所有可用 MIME 类型的列表，通过
    /// [`ClipboardReadReply::available`] 交付。
    ///
    /// Kitty 只为“仅列出类型”的请求（列表、无 MIME）直接应答而不弹出确认，
    /// 宿主也应如此；终端从不为这类请求查询授权。
    #[must_use]
    pub fn wants_list(&self) -> bool {
        // SAFETY: 请求在回调期间有效。
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardRead, list) }.unwrap_or(false)
    }

    /// 发起请求的程序名，用于权限提示；协议不携带时为空。
    #[must_use]
    pub fn name(&self) -> &'t [u8] {
        // SAFETY: 请求及其字符串在回调期间有效。
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardRead, name).map_or(&[], |n| n.to_bytes()) }
    }

    /// 终端是否已持有本次请求的会话授权（Kitty 剪贴板协议的密码）。为
    /// `true` 时宿主应跳过权限提示直接读取。
    ///
    /// 没有请求任何 MIME 时恒为 `false`：这类请求不经确认即可应答，终端
    /// 不为它查询授权，一次性密码会留给后续真正读数据的请求。粘贴事件之后
    /// 程序的后续读取会带着 `granted`，因为用户已经粘贴过了。
    #[must_use]
    pub fn granted(&self) -> bool {
        // SAFETY: 请求在回调期间有效。
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardRead, granted) }.unwrap_or(false)
    }

    /// 程序是否提供了会话密码。为 `true` 时宿主可以通过
    /// [`ClipboardReadReply::remember`] 记住用户的决定；为 `false` 时
    /// `remember` 被忽略。
    #[must_use]
    pub fn can_remember(&self) -> bool {
        // SAFETY: 请求在回调期间有效。
        unsafe { crate::sized_field!(self.ptr, ffi::ClipboardRead, can_remember) }.unwrap_or(false)
    }

    /// 应答这次读取。
    ///
    /// `Ok` 携带剪贴板内容：每个被请求且剪贴板里有的 MIME 类型给一份表示，
    /// 未请求的表示会被忽略；只承载单个文本值的协议（OSC 52）取第一个文本
    /// 类 MIME（如 `text/plain`）的条目。`Err` 给 OSC 52 回一个空剪贴板，给
    /// OSC 5522 回对应的协议状态（EPERM、ENOSYS、EBUSY、EIO）。
    ///
    /// 应答中的数组和字符串只需活过这次调用。重复应答会被忽略，而这里按值
    /// 消耗 `self`，所以本来也只能应答一次。
    pub fn reply(self, result: std::result::Result<ClipboardReadReply<'_>, ClipboardReadError>) {
        // SAFETY: 请求在回调期间有效。
        let Some(callback) = (unsafe { crate::sized_field!(self.ptr, ffi::ClipboardRead, reply) }).flatten() else {
            return;
        };

        // 回调只借用这两个数组到返回为止，所以放在这里的栈帧上即可。
        let (contents, available): (Vec<ffi::ClipboardContent>, Vec<ffi::String>) = match &result {
            Ok(reply) => (
                reply
                    .contents
                    .iter()
                    .map(|content| ffi::ClipboardContent {
                        mime: ffi::String::from(content.mime),
                        data: ffi::String { ptr: content.data.as_ptr(), len: content.data.len() },
                    })
                    .collect(),
                reply.available.iter().map(|mime| ffi::String::from(*mime)).collect(),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };
        let raw = ffi::ClipboardReadReply {
            result: match result {
                Ok(_) => ffi::ClipboardReadResult::SUCCESS,
                Err(err) => err.into(),
            },
            // 空数组传 NULL：C 侧把这两个字段声明为可空，长度为零时不读指针。
            contents: if contents.is_empty() { std::ptr::null() } else { contents.as_ptr() },
            contents_len: contents.len(),
            available: if available.is_empty() { std::ptr::null() } else { available.as_ptr() },
            available_len: available.len(),
            remember: result.as_ref().is_ok_and(|reply| reply.remember),
            ..ffi::sized!(ffi::ClipboardReadReply)
        };
        // SAFETY: 请求在回调期间有效；应答及其数组活过这次同步调用。
        unsafe { callback(self.ptr, &raw const raw) };
    }
}

/// 剪贴板读取成功时的应答内容，见 [`ClipboardRead::reply`]。
#[derive(Clone, Copy, Debug, Default)]
pub struct ClipboardReadReply<'a> {
    /// 剪贴板内容的各个 MIME 表示。
    pub contents: &'a [ClipboardContent<'a>],
    /// 剪贴板上所有可用的 MIME 类型。只在 [`ClipboardRead::wants_list`] 为
    /// `true` 时使用，否则可以留空。
    pub available: &'a [&'a str],
    /// 记录会话授权，让同一程序之后的请求跳过权限提示。只在
    /// [`ClipboardRead::can_remember`] 为 `true` 时生效。
    pub remember: bool,
}

/// 剪贴板读取失败的原因，见 [`ClipboardRead::reply`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
pub enum ClipboardReadError {
    /// 读取被策略或用户拒绝。
    Denied = ffi::ClipboardReadResult::DENIED,
    /// 宿主无法读取这个剪贴板。
    Unsupported = ffi::ClipboardReadResult::UNSUPPORTED,
    /// 剪贴板暂时不可用。
    Busy = ffi::ClipboardReadResult::BUSY,
    /// 读取剪贴板时发生 I/O 错误。
    IoError = ffi::ClipboardReadResult::IO_ERROR,
}

/// libghostty 未实现的一条转义序列，由
/// [`Terminal::on_unknown_sequence`] 的回调收到。
///
/// 内容是序列引导符与终止符之间的字节，可能包含任意二进制数据，只在回调
/// 期间有效，需要保留时请复制。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnknownSequence<'t> {
    /// 应用程序命令（APC）。
    Apc {
        /// `ESC _` 与终止符之间的内容。
        content: &'t [u8],
        /// 内容是否因字节上限或内存不足被截断。
        truncated: bool,
    },
    /// 编号未被实现的操作系统命令（OSC）。
    ///
    /// 例如程序写入 `ESC ] 7400;status=busy BEL` 时，`content` 是
    /// `7400;status=busy`，`terminator` 是 [`osc::Terminator::Bel`]。匹配时
    /// 连同 `;` 一起比较编号，避免 `7400;` 误匹配 `74000;`。
    Osc {
        /// `ESC ]` 与终止符之间的全部内容，包括开头的编号。
        content: &'t [u8],
        /// 内容是否因字节上限或内存不足被截断。
        truncated: bool,
        /// 程序如何结束这条序列。回复时请用同样的方式结束。
        terminator: osc::Terminator,
    },
}

impl UnknownSequence<'_> {
    /// 返回 `None` 表示链接的 libghostty 上报了本封装还不认识的序列种类；
    /// C 头文件要求回调忽略不认识的种类。
    ///
    /// # Safety
    ///
    /// 调用方必须保证指针及其中的字符串在 `'t` 内有效。
    unsafe fn from_raw(raw: *const ffi::TerminalUnknownSequence) -> Option<Self> {
        // SAFETY: 由调用方保证；联合体按 tag 读取对应的成员。
        unsafe {
            match (*raw).tag {
                ffi::TerminalUnknownSequenceTag::APC => {
                    let apc = (*raw).value.apc;
                    Some(Self::Apc { content: apc.content.to_bytes(), truncated: apc.truncated })
                }
                ffi::TerminalUnknownSequenceTag::OSC => {
                    let seq = (*raw).value.osc;
                    Some(Self::Osc {
                        content: seq.content.to_bytes(),
                        truncated: seq.truncated,
                        terminator: seq.terminator.try_into().ok()?,
                    })
                }
                _ => None,
            }
        }
    }
}

/// shell 集成上报的命令步骤，由 [`Terminal::on_semantic_prompt`] 的回调收到。
///
/// 每条命令依次经过四步：提示符开始、输入开始、输出开始、命令结束，然后
/// 下一个提示符开始。shell 的上报各不相同：很多不发送命令行或退出码，有的
/// 会跳过步骤，重绘时还可能重复开始同一个提示符，所以应逐个处理事件，不要
/// 假定严格的顺序。
///
/// 字符串只在回调期间有效，需要保留时请复制。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SemanticPrompt<'t> {
    /// shell 开始绘制提示符（OSC 133;A 等）。
    PromptStart {
        /// 开始的是哪种提示符。
        kind: PromptKind,
    },
    /// 提示符已画完，用户可以开始输入命令（OSC 133;B）。
    InputStart,
    /// 用户提交了命令，命令开始运行；此后终端收到的都是命令输出
    /// （OSC 133;C）。
    OutputStart {
        /// 即将运行的命令行（shell 编码发送，这里是解码后的文本）。shell
        /// 没发送或无法解码时为空。
        command: &'t [u8],
    },
    /// 命令运行结束（OSC 133;D）。
    CommandEnd {
        /// shell 上报的退出码；没上报时为 `None`。Windows 等平台上退出码
        /// 可能为负数。
        exit_code: Option<i32>,
        /// shell 发送的错误描述，没有时为空。很少有 shell 发送它，判断命令
        /// 是否失败通常看退出码。
        error: &'t [u8],
    },
}

impl SemanticPrompt<'_> {
    /// 返回 `None` 表示链接的 libghostty 上报了本封装还不认识的事件种类；
    /// C 头文件要求回调忽略不认识的种类。
    ///
    /// # Safety
    ///
    /// 调用方必须保证指针及其中的字符串在 `'t` 内有效。
    unsafe fn from_raw(raw: *const ffi::TerminalSemanticPrompt) -> Option<Self> {
        // SAFETY: 由调用方保证。下面读取的字段从这个结构体引入时就存在，
        // C 头文件说明只有之后新增的字段才需要先检查 size。
        let raw = unsafe { *raw };
        Some(match raw.kind {
            ffi::SemanticPromptKind::PROMPT_START => {
                Self::PromptStart { kind: raw.prompt_kind.try_into().unwrap_or(PromptKind::Primary) }
            }
            ffi::SemanticPromptKind::INPUT_START => Self::InputStart,
            ffi::SemanticPromptKind::OUTPUT_START => Self::OutputStart {
                // SAFETY: 由调用方保证。
                command: unsafe { raw.command.to_bytes() },
            },
            ffi::SemanticPromptKind::COMMAND_END => Self::CommandEnd {
                exit_code: raw.has_exit_code.then_some(raw.exit_code),
                // SAFETY: 由调用方保证。
                error: unsafe { raw.error.to_bytes() },
            },
            _ => return None,
        })
    }
}

/// [`SemanticPrompt::PromptStart`] 开始的提示符种类。
///
/// 大多数 shell 只画主提示符；有的还会在行右侧画提示符，或在多行命令的
/// 后续行开头画提示符。
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum PromptKind {
    /// 每条命令前显示的主提示符；shell 没说明是哪种时也用它。
    Primary = ffi::SemanticPromptPromptKind::PRIMARY,
    /// 画在行右侧的提示符，例如 zsh 的 RPROMPT。
    Right = ffi::SemanticPromptPromptKind::RIGHT,
    /// 多行命令后续行开头的提示符。
    Continuation = ffi::SemanticPromptPromptKind::CONTINUATION,
    /// 另一种后续输入行的提示符，例如 bash 的 PS2。shell 对后续行用
    /// continuation 还是 secondary 并不统一，大多数应用应同等对待两者。
    Secondary = ffi::SemanticPromptPromptKind::SECONDARY,
}

//---------------------------------------
// Callbacks
//---------------------------------------

/// You might be wondering just what the heck this is.
///
/// Truth to be told, you don't need to understand how it works
/// in order to use it. It does a bunch of voodoo behind the scenes
/// that make sure all the invariants of the C API are upheld, while
/// providing a convenient API for Rust users.
///
/// Each handler is defined in this following format:
/// ```ignore
/// pub fn on_foobar(
///     &mut self,
///     // The corresponding GhosttyTerminalOption
///     tag = FOOBAR,
///
///     // The name of the original function type in C,
///     // along with the extra C parameters and the expected C return type
///     from = TerminalFoobarFn(foo: *const u8, bar: usize) -> bool,
///
///     // The name of mapped Rust function type,
///     // along with the Rust parameters and return type.
///     //
///     // `<'t>` is used to tie the return value to the lifetime of the
///     // terminal. The name is arbitrary - any lifetime marker will do.
///     to = <'t>FoobarFn(&'t [u8]) -> bool,
/// ) |term, func| {
///     // `term` is the terminal and `func` is the Rust callback.
///     // Both names are arbitrary.
///
///     // Convert the raw parameters into Rust types.
///     // This is just to illustrate how.
///     let slice = unsafe { std::slice::from_raw_parts(foo, bar) };
///
///     // Call into user logic and return.
///     func(&terminal, slice)
/// }
/// ```
macro_rules! handlers {
    {
        $(
            $(#[$fmeta:meta])*
            $vis:vis fn $name:ident(
                &mut self,
                tag = $tag:ident,
                from = $rawfnty:ident( $($rfname:ident: $rfty:ty),*$(,)? ) $(-> $rawrty:ty)?,
                $(#[$tmeta:meta])*
                to = $(<$lf:lifetime>)? $fnty:ident( $($fty:ty),*$(,)? ) $(-> $rty:ty)?,
            ) |$t:ident, $func:ident| $block:block
        )*
    } => {
        /// Methods for registering [effect handlers](#effects).
        impl<'alloc, 'cb> $crate::terminal::Terminal<'alloc, 'cb> {$(
            $(#[$fmeta])*
            ///
            /// See [#Effects](Terminal#effects) for more details.
            $vis fn $name(&mut self, f: impl $fnty<'alloc, 'cb>) -> $crate::error::Result<&mut Self> {
                unsafe extern "C" fn callback(
                    t: $crate::ffi::Terminal,
                    ud: *mut std::ffi::c_void,
                    $($rfname: $rfty),*
                ) $(-> $rawrty)? {
                    // SAFETY: USERDATA is set to the boxed VTable pointee
                    // (derived from a mutable reference for write provenance)
                    // before the callback is registered. ghostty invokes
                    // callbacks synchronously from vt_write, reset and
                    // resize. All three take `&mut self`, so the VTable
                    // outlives this call and nothing else touches it
                    // meanwhile. Callbacks only get a `&Terminal`, so they
                    // can't reach any of those entry points, and dispatch
                    // never nests: at most one `&mut VTable` exists at a
                    // time.
                    let vtable = unsafe { &mut *ud.cast::<VTable<'_, '_>>() };

                    let obj = $crate::alloc::Object::new(t).expect("received null terminal ptr in callback - this is a bug!");
                    // Build a temporary borrowed Terminal view for the callback
                    // without taking ownership of the underlying ghostty terminal.
                    let mut term = ::core::mem::ManuallyDrop::new($crate::terminal::Terminal::<'_, '_> {
                        inner: obj,
                        vtable: ::core::default::Default::default(),
                        alive: None,
                    });
                    let $t: &$crate::terminal::Terminal = &term;
                    let $func = vtable.$name.as_deref_mut()
                        .expect("no handler set but callback is still called - this is a bug!");
                    let ret = $block;

                    // SAFETY: The temporary vtable was allocated solely to satisfy
                    // the Terminal layout expected by the callback signature. Drop
                    // it explicitly while intentionally leaving the borrowed
                    // terminal handle itself untouched.
                    unsafe { ::core::ptr::drop_in_place(&mut term.vtable) };

                    ret
                }

                self.vtable.$name = Some(::std::boxed::Box::new(f));

                // USERDATA is a raw pointer option: pass the heap allocation
                // itself, not the address of the Box smart pointer field stored
                // inline in Terminal.
                //
                // Derive the pointer from a mutable reference so it carries
                // write provenance – the callback later reborrows it as &mut.
                let userdata = std::ptr::from_mut::<VTable<'alloc, 'cb>>(self.vtable.as_mut())
                    as *const ::std::ffi::c_void;
                self.set_ptr($crate::ffi::TerminalOption::USERDATA, userdata)?;

                // The callback must be coerced into a function *pointer*
                // and not a function *item* (which is a ZST whose address is meaningless).
                // :)
                // Type-check against the generated C callback alias so ABI changes
                // cannot silently pass through the type-erased option setter.
                let _: $crate::ffi::$rawfnty = Some(callback);

                let callback_ptr: unsafe extern "C" fn(
                    $crate::ffi::Terminal,
                    *mut ::std::ffi::c_void,
                    $($rfty),*
                ) $(-> $rawrty)? = callback;

                let result = unsafe {
                    $crate::ffi::ghostty_terminal_set(
                        self.inner.as_raw(),
                        $crate::ffi::TerminalOption::$tag,
                        callback_ptr as *const ::std::ffi::c_void
                    )
                };
                $crate::error::from_result(result)?;
                Ok(self)
            }
        )*}
        $(
            #[doc = concat!(
                "[Effect](Terminal#effects) callback type for [`Terminal::",
                stringify!($name),
                "`](Terminal::",
                stringify!($name),
                ").\n"
            )]
            $(#[$tmeta])*
            pub trait $fnty<'alloc, 'cb>:
                $(for<$lf>)? FnMut(
                    &$($lf)? $crate::terminal::Terminal<'alloc, 'cb>,
                    $($fty),*
                ) $(-> $rty)? + 'cb {}

            impl<'alloc, 'cb, F> $fnty<'alloc, 'cb> for F
            where
                F: $(for<$lf>)? FnMut(
                    &$($lf)? $crate::terminal::Terminal<'alloc, 'cb>,
                    $($fty),*
                ) $(-> $rty)? + 'cb
            {}
        )*

        struct VTable<'alloc, 'cb> {
            $($name: Option<::std::boxed::Box<dyn $fnty<'alloc, 'cb>>>),*
        }

        impl ::core::fmt::Debug for VTable<'_, '_> {
            fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
                f.write_str("VTable {..}")
            }
        }

        impl ::core::default::Default for VTable<'_, '_> {
            fn default() -> Self {
                Self {
                    $($name: None),*
                }
            }
        }
    };
}

handlers! {
    /// Call the given function when the terminal needs to write data back
    /// to the pty (e.g. in response to a DECRQM query, device status report,
    /// or VT-driven mode 2048 enable).
    pub fn on_pty_write(
        &mut self,
        tag = WRITE_PTY,
        from = TerminalWritePtyFn(ptr: *const u8, len: usize),
        to = <'t>PtyWriteFn(&'t [u8]),
    ) |term, func| {
        // SAFETY: We trust libghostty to return valid memory given we
        // uphold all lifetime invariants (e.g. no `vt_write` calls
        // during this callback, which is guaranteed via the mutable reference).
        let data = unsafe { std::slice::from_raw_parts(ptr, len) };
        func(term, data);
    }

    /// Call the given function when the terminal receives
    /// a BEL character (0x07).
    pub fn on_bell(
        &mut self,
        tag = BELL,
        from = TerminalBellFn(),
        to = BellFn(),
    ) |term, func| {
        func(term);
    }

    /// Call the given function when the running program asks the terminal to
    /// stop updating the screen, and again when it lets the screen update
    /// again. We call the time in between a "render hold".
    ///
    /// Programs use a hold to avoid flicker. A full-screen program usually
    /// redraws in several steps: clear, draw the text, move the cursor. If
    /// the screen is drawn halfway through, the user sees a broken frame. To
    /// prevent that, the program starts a hold, draws everything, and then
    /// releases the hold. The screen should keep showing the last finished
    /// frame the whole time and then switch to the new one all at once.
    ///
    /// Today the only way a program can start a hold is synchronized output
    /// ([`Mode::SYNC_OUTPUT`], DEC private mode 2026). The callback is named
    /// for what the embedder should do rather than for that mode so that
    /// other sources of holds can be added later.
    ///
    /// # When it is called
    ///
    /// With `held` set to `true` when the program sets mode 2026.
    ///
    /// With `held` set to `false` when the hold ends, which happens when:
    ///
    /// - the program resets mode 2026
    /// - the terminal is fully reset, by the program (RIS) or by
    ///   [`Terminal::reset`]
    /// - the terminal is resized with [`Terminal::resize`]
    ///
    /// The two calls always come in pairs. Setting the mode while a hold is
    /// already active does nothing, and neither does resetting it when there
    /// is no hold. Changing the mode yourself with [`Terminal::set_mode`]
    /// never invokes the callback.
    ///
    /// # What to do
    ///
    /// When a hold begins, the terminal contains exactly the frame the
    /// program wants left on screen. Nothing after the start of the hold has
    /// been processed yet, even if more bytes follow in the same
    /// [`Terminal::vt_write`] call. Capture that frame by calling
    /// [`RenderState::update`](crate::RenderState::update) from within the
    /// callback, then stop updating the render state until the hold ends. You
    /// can keep drawing the render state in the meantime. It won't change.
    ///
    /// <div class="warning">
    ///
    /// The callback runs inside an `extern "C"` function, so a panic in it
    /// aborts the process. If the callback updates the render state, don't
    /// keep the render state borrowed (e.g. through a
    /// [`Snapshot`](crate::render::Snapshot) or a
    /// [`RefMut`](std::cell::RefMut)) across [`Terminal::vt_write`],
    /// [`Terminal::reset`] or [`Terminal::resize`], since any of them can
    /// invoke the callback. Use a non-panicking borrow such as
    /// [`RefCell::try_borrow_mut`](std::cell::RefCell::try_borrow_mut)
    /// inside the callback.
    ///
    /// </div>
    ///
    /// ```rust
    /// use std::cell::{Cell, RefCell};
    /// use std::time::{Duration, Instant};
    /// use libghostty_vt::{RenderState, Terminal, terminal::Mode};
    ///
    /// struct Renderer {
    ///     render_state: RefCell<RenderState<'static>>,
    ///     held: Cell<bool>,
    ///     hold_started: Cell<Instant>,
    /// }
    ///
    /// fn draw(r: &Renderer, terminal: &mut Terminal<'static, '_>) -> libghostty_vt::error::Result<()> {
    ///     // Give up on a program that holds the screen for too long.
    ///     if r.held.get() && r.hold_started.get().elapsed() >= Duration::from_secs(1) {
    ///         terminal.set_mode(Mode::SYNC_OUTPUT, false)?;
    ///         r.held.set(false);
    ///     }
    ///
    ///     // During a hold, skip the update and draw the captured frame.
    ///     if !r.held.get() {
    ///         r.render_state.borrow_mut().update(terminal)?;
    ///     }
    ///     // draw_render_state(&r.render_state);
    ///     Ok(())
    /// }
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let renderer = Renderer {
    ///     render_state: RefCell::new(RenderState::new()?),
    ///     held: Cell::new(false),
    ///     hold_started: Cell::new(Instant::now()),
    /// };
    ///
    /// let mut terminal = Terminal::new(80, 24)?;
    /// terminal.on_render_hold(|term, held| {
    ///     if held {
    ///         // Capture the frame the program wants left on screen. Don't
    ///         // panic if that fails, e.g. because the render state is
    ///         // borrowed elsewhere; keep drawing the previous frame instead.
    ///         let _ = renderer
    ///             .render_state
    ///             .try_borrow_mut()
    ///             .map(|mut state| state.update(term).map(|_| ()));
    ///         renderer.hold_started.set(Instant::now());
    ///     }
    ///     renderer.held.set(held);
    /// })?;
    ///
    /// terminal.vt_write(b"\x1b[?2026h");
    /// assert!(renderer.held.get());
    /// draw(&renderer, &mut terminal)?;
    ///
    /// terminal.vt_write(b"\x1b[?2026l");
    /// assert!(!renderer.held.get());
    /// draw(&renderer, &mut terminal)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Timeouts
    ///
    /// The terminal has no clock, so it never ends a hold on its own. A
    /// program that crashes or forgets to release its hold would freeze the
    /// screen forever, so you need a timeout like the one above. One second
    /// is a common choice. When it expires, reset the mode yourself and go
    /// back to updating normally. Because setting the mode again during a
    /// hold does nothing, a program can't keep pushing your deadline back.
    ///
    /// # Why a callback
    ///
    /// You could instead check [`Mode::SYNC_OUTPUT`] before each draw and
    /// skip the update when it is set. That is simpler, but it has two
    /// problems. First, the frame left on screen is whatever you happened to
    /// draw last, which can be older than what the program intended or even
    /// a half-drawn frame. Second, if the program releases a hold and starts
    /// the next one between two of your draws, you never see the mode turn
    /// off and the finished frame in between is lost. A program that draws
    /// continuously can then appear frozen. Capturing the frame when each
    /// hold begins avoids both.
    ///
    /// # Other notes
    ///
    /// You are free to ignore a hold whenever showing live content matters
    /// more, such as when the user scrolls or starts a selection.
    ///
    /// Like every callback, this runs on the thread that called
    /// [`Terminal::vt_write`], [`Terminal::reset`] or [`Terminal::resize`].
    /// Since neither a terminal nor a render state can be sent to another
    /// thread, the update in the callback needs no locking.
    pub fn on_render_hold(
        &mut self,
        tag = RENDER_HOLD,
        from = TerminalRenderHoldFn(held: bool),
        to = RenderHoldFn(bool),
    ) |term, func| {
        func(term, held);
    }

    /// Call the given function when the terminal receives
    /// an ENQ character (0x05).
    pub fn on_enquiry(
        &mut self,
        tag = ENQUIRY,
        from = TerminalEnquiryFn() -> ffi::String,
        to = <'t>EnquiryFn() -> Option<&'t str>,
    ) |term, func| {
        func(term).unwrap_or("").into()
    }

    /// Call the given function when the terminal receives an XTVERSION
    /// query (CSI > q), and respond with the resulting version string
    /// (e.g. "myterm 1.0").
    pub fn on_xtversion(
        &mut self,
        tag = XTVERSION,
        from = TerminalXtversionFn() -> ffi::String,
        to = <'t>XtversionFn() -> Option<&'t str>,
    ) |term, func| {
        func(term).unwrap_or("").into()
    }

    /// Call the given function when the terminal title changes
    /// via escape sequences (e.g. OSC 0 or OSC 2).
    ///
    /// The new title can be queried from the terminal after
    /// the callback returns.
    pub fn on_title_changed(
        &mut self,
        tag = TITLE_CHANGED,
        from = TerminalTitleChangedFn(),
        to = TitleChangedFn(),
    ) |term, func| {
        func(term);
    }

    /// Call the given function when the terminal current working directory
    /// changes via escape sequences (e.g. OSC 7, OSC 9, or OSC 1337).
    ///
    /// The new working directory can be queried from the terminal after
    /// the callback returns.
    pub fn on_pwd_changed(
        &mut self,
        tag = PWD_CHANGED,
        from = TerminalPwdChangedFn(),
        to = PwdChangedFn(),
    ) |term, func| {
        func(term);
    }

    /// Call the given function after the running program performs a full
    /// reset (RIS, `ESC c`).
    ///
    /// A full reset clears the screen and scrollback, returns modes to their
    /// defaults, and clears the title and working directory. The terminal has
    /// already reset itself when this is called.
    ///
    /// [`on_title_changed`](Self::on_title_changed) and
    /// [`on_pwd_changed`](Self::on_pwd_changed) are *not* called for the
    /// cleared title and working directory, so anything shown for them must
    /// be updated here instead. A full reset also removes any progress
    /// report; [`on_progress_report`](Self::on_progress_report) is called
    /// before this one.
    ///
    /// Only a reset from the VT stream reports here. [`Terminal::reset`] and
    /// a soft reset (DECSTR, `CSI ! p`) do not.
    pub fn on_reset(
        &mut self,
        tag = RESET,
        from = TerminalResetFn(),
        to = ResetFn(),
    ) |term, func| {
        func(term);
    }

    /// Call the given function in response to XTWINOPS size queries
    /// (CSI 14/16/18 t) and when VT input enables in-band size reports (mode
    /// 2048). Return the current terminal geometry, or `None` to suppress the
    /// report.
    pub fn on_size(
        &mut self,
        tag = SIZE,
        from = TerminalSizeFn(out: *mut ffi::SizeReportSize) -> bool,
        to = SizeFn() -> Option<SizeReportSize>,
    ) |term, func| {
        if let Some(size) = func(term) {
            // SAFETY: Out pointer is assumed to be valid.
            unsafe { *out = size };
            true
        } else {
            false
        }
    }

    /// Call the given function in response to a color scheme
    /// device status report query (CSI ? 996 n).
    ///
    /// Return `Some` to report the current color scheme,
    /// or return `None` to silently ignore.
    pub fn on_color_scheme(
        &mut self,
        tag = COLOR_SCHEME,
        from = TerminalColorSchemeFn(out: *mut ffi::ColorScheme::Type) -> bool,
        to = ColorSchemeFn() -> Option<ColorScheme>,
    ) |term, func| {
        if let Some(size) = func(term) {
            // SAFETY: Out pointer is assumed to be valid.
            unsafe { *out = size.into() };
            true
        } else {
            false
        }
    }

    /// Call the given function in response to a device attributes query
    /// (CSI c, CSI > c, or CSI = c).
    ///
    /// Return `Some` with the response data,
    /// or return `None` to silently ignore.
    pub fn on_device_attributes(
        &mut self,
        tag = DEVICE_ATTRIBUTES,
        from = TerminalDeviceAttributesFn(out: *mut ffi::DeviceAttributes) -> bool,
        to = DeviceAttributesFn() -> Option<DeviceAttributes>,
    ) |term, func| {
        if let Some(size) = func(term) {
            // SAFETY: Out pointer is assumed to be valid.
            unsafe { *out = size.into() };
            true
        } else {
            false
        }
    }

    /// Call the given function when the running program performs a clipboard write.
    ///
    /// Protocol details such as OSC 52 selectors, base64 encoding, multipart
    /// chunks, aliases, and terminators are normalized before this callback is
    /// invoked. OSC 52, iTerm2 OSC 1337 Copy, and Kitty clipboard (OSC 5522)
    /// writes therefore use the same callback shape. Without this callback,
    /// clipboard writes are ignored and Kitty clipboard writes are refused
    /// with ENOSYS.
    ///
    /// The embedder may ask for permission to write or perform the write
    /// async, but the callback itself is synchronous and
    /// [`ClipboardWrite::reply`] must be called before it returns. While this
    /// callback is active the VT stream is paused. Returning without a reply
    /// denies the write.
    ///
    /// The request may carry an optional program name requesting the write
    /// and the state of prior permission granted. If
    /// [`ClipboardWrite::can_remember`] is set the reply may set `remember`,
    /// and future requests from this same program will be
    /// [granted](ClipboardWrite::granted) so the embedder can skip permission
    /// requests.
    pub fn on_clipboard_write(
        &mut self,
        tag = CLIPBOARD_WRITE,
        from = TerminalClipboardWriteFn(
            write: *const ffi::ClipboardWrite
        ),
        to = <'t>ClipboardWriteFn(ClipboardWrite<'t>),
    ) |term, func| {
        // SAFETY: The request is only borrowed for the callback duration,
        // which `ClipboardWrite`'s lifetime enforces.
        func(term, unsafe { ClipboardWrite::from_raw(write) });
    }

    /// Callback invoked when the running program requests a desktop
    /// notification via OSC 9 or OSC 777.
    pub fn on_desktop_notification(
        &mut self,
        tag = DESKTOP_NOTIFICATION,
        from = TerminalDesktopNotificationFn(
            notif: *const ffi::TerminalDesktopNotification
        ),
        to = <'t>DesktopNotificationFn(DesktopNotification<'t>),
    ) |term, func| {
        func(term, unsafe { DesktopNotification::from_raw(notif) });
    }

    /// Call the given function when the running program reports progress
    /// via OSC 9;4.
    pub fn on_progress_report(
        &mut self,
        tag = PROGRESS_REPORT,
        from = TerminalProgressReportFn(
            progress: *const ffi::TerminalProgressReport
        ),
        to = <'t>ProgressReportFn(ProgressReport<'t>),
    ) |term, func| {
        func(term, unsafe { ProgressReport::from_raw(progress) });
    }

    /// 运行中的程序请求读取剪贴板时调用给定函数：OSC 52 的 `?` 负载，或
    /// Kitty 剪贴板协议（OSC 5522）的读取。
    ///
    /// 应答等于让程序读到用户的剪贴板，所以宿主应当负责征求同意。读取是
    /// 同步的：需要询问用户的宿主必须阻塞（例如运行模态对话框）直到得到
    /// 答案，期间 VT 流暂停。必须在回调返回前调用 [`ClipboardRead::reply`]。
    /// 没装这个回调时，OSC 52 读取被忽略，OSC 5522 读取以 EPERM 拒绝。
    ///
    /// OSC 5522 请求携带程序的 MIME 列表、程序名和密码授权状态；应答时设置
    /// [`remember`](ClipboardReadReply::remember) 会记录会话授权，之后带同一
    /// 密码的请求会以 [`granted`](ClipboardRead::granted) 到达。
    ///
    /// 装上这个回调还会启用 Kitty 粘贴事件（模式 5522）：
    /// [`Terminal::paste`] 会给程序发送事件而不是文本，程序随后的读取以
    /// `granted` 到达这里，因为用户已经粘贴过了。
    ///
    /// <div class="warning">
    ///
    /// 回调运行在 `extern "C"` 函数里，其中的 panic 会让进程 abort。
    ///
    /// </div>
    pub fn on_clipboard_read(
        &mut self,
        tag = CLIPBOARD_READ,
        from = TerminalClipboardReadFn(
            read: *const ffi::ClipboardRead
        ),
        to = <'t>ClipboardReadFn(ClipboardRead<'t>),
    ) |term, func| {
        // SAFETY: 请求只在回调期间被借用，`ClipboardRead` 的生命周期保证了这一点。
        func(term, unsafe { ClipboardRead::from_raw(read) });
    }

    /// 收到 libghostty 未实现的 APC 或 OSC 序列时调用给定函数，让宿主自己
    /// 实现这些协议（例如应用私有的 OSC 7400）。
    ///
    /// 还必须用 [`Terminal::set_unknown_sequence_max_bytes`] 设置非零上限，
    /// 否则回调永远不会被调用；只装回调既不保留数据也不分配内存。
    ///
    /// 不会上报：程序用 CAN 或 SUB 中途取消的序列；libghostty 已实现的序列
    /// （即使内容格式错误）；宿主关闭了的已支持协议。以后可能上报更多种类
    /// 的序列，本封装遇到不认识的种类时不调用回调。
    ///
    /// 回调在 [`Terminal::vt_write`] 期间运行，可以直接向 pty 写回复，回复与
    /// 终端自身的回复保持顺序。回复时请使用请求的终止符
    /// （[`UnknownSequence::Osc::terminator`](UnknownSequence::Osc)）。
    ///
    /// <div class="warning">
    ///
    /// 回调运行在 `extern "C"` 函数里，其中的 panic 会让进程 abort。
    ///
    /// </div>
    ///
    /// ```rust
    /// use std::cell::RefCell;
    /// use libghostty_vt::{Terminal, terminal::UnknownSequence};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let status = RefCell::new(Vec::new());
    /// let mut terminal = Terminal::new(80, 24)?;
    /// terminal
    ///     .on_unknown_sequence(|_term, seq| {
    ///         // 只处理完整的 OSC 7400，其余忽略。
    ///         if let UnknownSequence::Osc { content, truncated: false, .. } = seq {
    ///             if let Some(rest) = content.strip_prefix(b"7400;") {
    ///                 status.borrow_mut().extend_from_slice(rest);
    ///             }
    ///         }
    ///     })?
    ///     // 每条未知序列最多保留 4 KiB。
    ///     .set_unknown_sequence_max_bytes(4096)?;
    ///
    /// terminal.vt_write(b"\x1b]7400;status=busy\x07");
    /// assert_eq!(*status.borrow(), b"status=busy");
    /// # Ok(())
    /// # }
    /// ```
    pub fn on_unknown_sequence(
        &mut self,
        tag = UNKNOWN_SEQUENCE,
        from = TerminalUnknownSequenceFn(
            sequence: *const ffi::TerminalUnknownSequence
        ),
        to = <'t>UnknownSequenceFn(UnknownSequence<'t>),
    ) |term, func| {
        // SAFETY: 序列只在回调期间被借用，`UnknownSequence` 的生命周期保证了这一点。
        if let Some(sequence) = unsafe { UnknownSequence::from_raw(sequence) } {
            func(term, sequence);
        }
    }

    /// shell 通过 OSC 133 上报命令的某一步时调用给定函数：提示符开始、输入
    /// 开始、输出开始或命令结束。
    ///
    /// 调用时终端已经更新了屏幕。被终端判为格式错误的序列不会上报。事件
    /// 的语义见 [`SemanticPrompt`]。
    ///
    /// <div class="warning">
    ///
    /// 回调运行在 `extern "C"` 函数里，其中的 panic 会让进程 abort。
    ///
    /// </div>
    pub fn on_semantic_prompt(
        &mut self,
        tag = SEMANTIC_PROMPT,
        from = TerminalSemanticPromptFn(
            event: *const ffi::TerminalSemanticPrompt
        ),
        to = <'t>SemanticPromptFn(SemanticPrompt<'t>),
    ) |term, func| {
        // SAFETY: 事件只在回调期间被借用，`SemanticPrompt` 的生命周期保证了这一点。
        if let Some(event) = unsafe { SemanticPrompt::from_raw(event) } {
            func(term, event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RenderState;
    use crate::render::CursorVisualStyle;
    use std::cell::{Cell, RefCell};
    use std::mem::ManuallyDrop;

    #[test]
    fn resize_pull_scrollback_controls_growing_rows() {
        // Apply `configure`, fill a 5-row terminal past its height so rows
        // land in scrollback and the cursor sits on the bottom row, then grow
        // it to 8 rows and report where the cursor ended up.
        fn cursor_row_after_growing(configure: impl FnOnce(&mut Terminal<'static, 'static>)) -> u16 {
            let mut terminal = Terminal::new(10, 5).expect("terminal should initialize");
            configure(&mut terminal);
            terminal.vt_write(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
            assert_eq!(terminal.cursor_y().unwrap(), 4);
            terminal.resize(10, 8, 8, 16).expect("resize should succeed");
            terminal.cursor_y().unwrap()
        }
        fn set(terminal: &mut Terminal<'static, 'static>, pull: Option<bool>) {
            terminal.set_resize_pull_scrollback(pull).expect("option should be settable");
        }

        // Pulling scrollback back in moves the cursor's line down with it.
        // That is the default, both when never set and when set explicitly.
        assert_eq!(cursor_row_after_growing(|_| {}), 7);
        assert_eq!(cursor_row_after_growing(|t| set(t, Some(true))), 7);
        // Otherwise blank rows are appended below and the cursor stays put.
        assert_eq!(cursor_row_after_growing(|t| set(t, Some(false))), 4);
        // `None` has to actively restore the default, not just leave the
        // previous value in place.
        assert_eq!(
            cursor_row_after_growing(|t| {
                set(t, Some(false));
                set(t, None);
            }),
            7
        );
        // The setting survives a full reset, whether the program sends RIS
        // or the embedder resets the terminal.
        assert_eq!(
            cursor_row_after_growing(|t| {
                set(t, Some(false));
                t.vt_write(b"\x1bc");
            }),
            4
        );
        assert_eq!(
            cursor_row_after_growing(|t| {
                set(t, Some(false));
                t.reset();
            }),
            4
        );
    }

    #[test]
    fn render_hold_reports_start_and_end_in_pairs() {
        // Mirrors upstream's "set render_hold callback" test in
        // src/terminal/c/terminal.zig.
        let events = RefCell::new(Vec::new());
        let take = || std::mem::take(&mut *events.borrow_mut());
        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");
        terminal.on_render_hold(|_term, held| events.borrow_mut().push(held)).expect("callback should register");

        // A set during a hold and a reset without a hold are ignored.
        terminal.vt_write(b"\x1b[?2026h\x1b[?2026hA\x1b[?2026l\x1b[?2026l");
        assert_eq!(take(), [true, false]);

        // Neither a reset nor a resize reports anything without a hold.
        terminal.reset();
        terminal.resize(100, 30, 8, 16).expect("resize should succeed");
        assert_eq!(take(), []);

        // Reset and resize end an active hold, and so does a resize that
        // keeps the dimensions: upstream turns synchronized output off
        // before it checks whether the grid size changed.
        terminal.vt_write(b"\x1b[?2026h");
        terminal.reset();
        terminal.reset();
        terminal.vt_write(b"\x1b[?2026h");
        terminal.resize(80, 24, 8, 16).expect("resize should succeed");
        terminal.vt_write(b"\x1b[?2026h");
        terminal.resize(80, 24, 8, 16).expect("resize should succeed");
        assert_eq!(take(), [true, false, true, false, true, false]);
        assert!(!terminal.mode(Mode::SYNC_OUTPUT).unwrap());

        // A resize that fails leaves the mode, and so the hold, in place.
        terminal.vt_write(b"\x1b[?2026h");
        assert!(terminal.resize(0, 24, 8, 16).is_err());
        assert!(terminal.mode(Mode::SYNC_OUTPUT).unwrap());
        terminal.vt_write(b"\x1b[?2026l");
        assert_eq!(take(), [true, false]);

        // Changing the mode ourselves, e.g. when a hold times out, is never
        // reported in either direction. The hold is simply over, so the
        // callback sees `true` without a matching `false`.
        terminal.vt_write(b"\x1b[?2026h");
        terminal.set_mode(Mode::SYNC_OUTPUT, false).expect("mode should be settable");
        assert!(!terminal.mode(Mode::SYNC_OUTPUT).unwrap());
        terminal
            .set_mode(Mode::SYNC_OUTPUT, true)
            .expect("mode should be settable")
            .set_mode(Mode::SYNC_OUTPUT, false)
            .expect("mode should be settable");
        assert_eq!(take(), [true]);
        // ...and the program can start a new hold afterwards.
        terminal.vt_write(b"\x1b[?2026h");
        assert_eq!(take(), [true]);
    }

    /// Read the text of the first row of a render state snapshot.
    fn first_row_text(snapshot: &crate::render::Snapshot<'_, '_>) -> Result<String> {
        let mut rows = crate::render::RowIterator::new()?;
        let mut cells = crate::render::CellIterator::new()?;
        let mut row_iter = rows.update(snapshot)?;
        let Some(row) = row_iter.next() else {
            return Ok(String::new());
        };
        let mut cell_iter = cells.update(row)?;
        let mut text = String::new();
        while let Some(cell) = cell_iter.next() {
            text.extend(cell.graphemes()?);
        }
        Ok(text)
    }

    #[test]
    fn render_hold_captures_frame_before_hold() {
        // A renderer that refreshes its render state whenever a hold starts
        // or ends, recording the first row it captured each time. Failures
        // are recorded as `None` instead of panicking, since a panic here
        // would abort the whole test binary.
        let render_state = RefCell::new(RenderState::new().expect("render state should initialize"));
        let frames = RefCell::new(Vec::new());
        let take = || std::mem::take(&mut *frames.borrow_mut());
        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");
        terminal
            .on_render_hold(|term, held| {
                let text = render_state.try_borrow_mut().ok().and_then(|mut state| {
                    let snapshot = state.update(term).ok()?;
                    first_row_text(&snapshot).ok()
                });
                frames.borrow_mut().push((held, text));
            })
            .expect("callback should register");

        // The hold starts before `B` is processed, even though it's in the
        // same write, so the captured frame only has `A`.
        terminal.vt_write(b"A\x1b[?2026hB");
        assert_eq!(take(), [(true, Some("A".to_owned()))]);
        // The terminal itself has moved on, though.
        {
            let mut state = render_state.borrow_mut();
            let snapshot = state.update(&terminal).expect("render state should update");
            assert_eq!(first_row_text(&snapshot).unwrap(), "AB");
        }
        terminal.vt_write(b"\x1b[?2026l");
        assert_eq!(take(), [(false, Some("AB".to_owned()))]);

        // Updating the render state also works when the hold ends from
        // `resize` and `reset`, which invoke the callback while the outer
        // call holds `&mut Terminal`.
        terminal.vt_write(b"\x1b[?2026hC");
        terminal.resize(100, 30, 8, 16).expect("resize should succeed");
        assert_eq!(take(), [(true, Some("AB".to_owned())), (false, Some("ABC".to_owned()))]);
        terminal.vt_write(b"\x1b[?2026hD");
        terminal.reset();
        assert_eq!(take(), [(true, Some("ABC".to_owned())), (false, Some(String::new()))]);
    }

    #[inline(never)]
    fn build_terminal(callback_count: &RefCell<usize>) -> Terminal<'static, '_> {
        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");

        terminal
            .on_device_attributes(move |_term| {
                *callback_count.borrow_mut() += 1;
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[DeviceAttributeFeature::ANSI_COLOR],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: TertiaryDeviceAttributes { unit_id: 0 },
                })
            })
            .expect("callback should register");

        terminal
    }

    /// Move a value into distinct heap storage with an explicit byte-for-byte
    /// relocation so the test does not rely on optimizer or allocator behavior.
    fn relocate_into_new_box<T>(value: T) -> (Box<T>, usize, usize) {
        // Keep the source allocation alive without running T's destructor.
        // We need the bytes to remain initialized until after the copy.
        let src = Box::new(ManuallyDrop::new(value));
        let src_addr = std::ptr::from_ref(&**src).cast::<T>() as usize;

        unsafe {
            let dst_layout = std::alloc::Layout::new::<T>();
            let dst_ptr = std::alloc::alloc(dst_layout).cast::<T>();
            if dst_ptr.is_null() {
                std::alloc::handle_alloc_error(dst_layout);
            }

            let dst_addr = dst_ptr as usize;
            assert_ne!(src_addr, dst_addr, "test setup failed: source and destination storage unexpectedly match");

            // SAFETY: src points to a fully initialized T wrapped in
            // ManuallyDrop, dst points to distinct uninitialized storage for
            // exactly one T, and the regions do not overlap.
            std::ptr::copy_nonoverlapping(std::ptr::from_ref(&**src).cast::<T>(), dst_ptr, 1);

            // SAFETY: src was allocated as Box<ManuallyDrop<T>> and must be
            // freed without dropping T because ownership was transferred by
            // the raw byte copy above.
            std::alloc::dealloc(Box::into_raw(src).cast::<u8>(), std::alloc::Layout::new::<ManuallyDrop<T>>());

            // SAFETY: We just initialized dst_ptr by copying a valid T into it,
            // so it now owns exactly one initialized T allocation.
            (Box::from_raw(dst_ptr), src_addr, dst_addr)
        }
    }

    /// Send an OSC 2 title sequence, then verify `term.title()` returns the
    /// correct value inside the `on_title_changed` callback.
    #[test]
    fn title_changed_callback_returns_correct_title() {
        // The callback bound on `on_title_changed` is `'cb`, not `'static`,
        // so the closure can borrow stack locals directly – no Rc needed.
        let captured_title: RefCell<String> = RefCell::new(String::new());
        let callback_count: Cell<usize> = Cell::new(0);

        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");

        terminal
            .on_title_changed(|term| {
                callback_count.set(callback_count.get() + 1);
                let title = term.title().expect("title() should succeed inside callback");
                *captured_title.borrow_mut() = title.to_owned();
            })
            .expect("callback should register");

        // OSC 2 (set title) should invoke on_title_changed.
        terminal.vt_write(b"\x1b]2;Hello Effects\x1b\\");
        assert_eq!(callback_count.get(), 1);
        assert_eq!(*captured_title.borrow(), "Hello Effects");

        // A second title change should fire the callback again.
        terminal.vt_write(b"\x1b]2;Second Title\x1b\\");
        assert_eq!(callback_count.get(), 2);
        assert_eq!(*captured_title.borrow(), "Second Title");
    }

    /// RIS clears the title without calling `on_title_changed`, so
    /// `on_reset` is the only signal an embedder showing the title gets.
    /// The API reset is the embedder's own doing and is not reported.
    #[test]
    fn reset_callback_reports_ris_but_not_api_reset() {
        let resets: Cell<usize> = Cell::new(0);
        let title_changes: Cell<usize> = Cell::new(0);
        let title_seen_in_reset: RefCell<Option<String>> = RefCell::new(None);

        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");
        terminal
            .on_title_changed(|_| title_changes.set(title_changes.get() + 1))
            .expect("title callback should register")
            .on_reset(|term| {
                resets.set(resets.get() + 1);
                *title_seen_in_reset.borrow_mut() = Some(term.title().expect("title() inside callback").to_owned());
            })
            .expect("reset callback should register");

        terminal.vt_write(b"\x1b]2;before reset\x1b\\");
        assert_eq!(title_changes.get(), 1);

        terminal.vt_write(b"\x1bc");
        assert_eq!(resets.get(), 1);
        // The terminal has already reset when the callback runs.
        assert_eq!(title_seen_in_reset.borrow().as_deref(), Some(""));
        assert_eq!(title_changes.get(), 1, "RIS must not report a title change");

        // A soft reset (DECSTR) and the API reset are not reported.
        terminal.vt_write(b"\x1b[!p");
        terminal.reset();
        assert_eq!(resets.get(), 1);
    }

    /// Send an OSC 7 current-directory sequence, then verify `term.pwd()`
    /// returns the correct value inside the `on_pwd_changed` callback.
    #[test]
    fn pwd_changed_callback_returns_correct_pwd() {
        let captured_pwd: RefCell<String> = RefCell::new(String::new());
        let callback_count: Cell<usize> = Cell::new(0);

        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");

        terminal
            .on_pwd_changed(|term| {
                callback_count.set(callback_count.get() + 1);
                let pwd = term.pwd().expect("pwd() should succeed inside callback");
                *captured_pwd.borrow_mut() = pwd.to_owned();
            })
            .expect("callback should register");

        terminal.vt_write(b"\x1b]7;file://localhost/tmp/project\x1b\\");
        assert_eq!(callback_count.get(), 1);
        assert_eq!(*captured_pwd.borrow(), "file://localhost/tmp/project");

        terminal.vt_write(b"\x1b]7;file://localhost/tmp/other\x1b\\");
        assert_eq!(callback_count.get(), 2);
        assert_eq!(*captured_pwd.borrow(), "file://localhost/tmp/other");
    }

    #[test]
    fn default_cursor_reset_uses_configured_style_and_blink() {
        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");
        let mut render_state = RenderState::new().expect("render state should initialize");

        terminal
            .set_default_cursor_style(Some(CursorStyle::Underline))
            .expect("default cursor style should update")
            .set_default_cursor_blink(Some(true))
            .expect("default cursor blink should update");

        terminal.vt_write(b"\x1b[0 q");
        let snapshot = render_state.update(&terminal).expect("render state should update");

        assert_eq!(
            snapshot.cursor_visual_style().expect("cursor style should be readable"),
            CursorVisualStyle::Underline
        );
        assert!(snapshot.cursor_blinking().expect("cursor blink should be readable"));
    }

    #[test]
    fn glyph_protocol_enabled_setting_updates() {
        let mut terminal = Terminal::new(80, 24).expect("terminal should initialize");

        terminal
            .set_glyph_protocol_enabled(false)
            .expect("glyph protocol should disable")
            .set_glyph_protocol_enabled(true)
            .expect("glyph protocol should enable");
    }

    /// Explicitly relocate the Terminal into distinct storage, then verify the
    /// callback still fires through the stable `VTable` userdata pointer.
    #[test]
    fn callbacks_survive_explicit_relocation() {
        let callback_count = RefCell::new(0usize);
        let terminal = build_terminal(&callback_count);
        let (mut terminal, addr_before, addr_after) = relocate_into_new_box(terminal);
        assert_ne!(addr_before, addr_after);

        // Primary DA request (CSI c) should invoke on_device_attributes.
        terminal.vt_write(b"\x1b[c");
        assert_eq!(*callback_count.borrow(), 1);
    }

    // The next two tests reach simdutf, which libghostty-vt DLLs on Windows
    // used to crash in, since their C++ global constructors never ran. Pure
    // ASCII never reaches it.

    #[test]
    fn clipboard_write_decodes_base64() {
        let written = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(8, 3).unwrap();
        terminal
            .on_clipboard_write(|_, request| {
                // Record rather than assert here: a panic in a callback
                // aborts the whole test binary.
                for content in request.contents() {
                    written.borrow_mut().push(content.data.to_vec());
                }
                request.reply(Ok(()), false);
            })
            .unwrap();

        terminal.vt_write(b"\x1b]52;c;//4=\x1b\\");
        assert_eq!(*written.borrow(), [vec![0xff, 0xfe]]);
    }

    #[test]
    fn multibyte_utf8_is_decoded() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        // "é" is 0xC3 0xA9. Both bytes must arrive in one write: a character
        // split across writes is finished by the scalar decoder, and never
        // reaches simdutf.
        terminal.vt_write(b"\xc3\xa9");
        let codepoint = terminal
            .grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))
            .unwrap()
            .cell()
            .unwrap()
            .codepoint()
            .unwrap();
        assert_eq!(codepoint, 0xe9);
    }

    #[test]
    fn mouse_shape_follows_osc_22() {
        let mut terminal = Terminal::new(8, 3).unwrap();
        assert_eq!(terminal.mouse_shape().unwrap(), mouse::Shape::Text);
        // OSC 22 names the shape with its W3C cursor name.
        terminal.vt_write(b"\x1b]22;pointer\x07");
        assert_eq!(terminal.mouse_shape().unwrap(), mouse::Shape::Pointer);
        terminal.vt_write(b"\x1b]22;nwse-resize\x1b\\");
        assert_eq!(terminal.mouse_shape().unwrap(), mouse::Shape::NwseResize);
        // A name libghostty doesn't know leaves the shape alone.
        terminal.vt_write(b"\x1b]22;not-a-shape\x07");
        assert_eq!(terminal.mouse_shape().unwrap(), mouse::Shape::NwseResize);
        // An empty name gives the pointer back.
        terminal.vt_write(b"\x1b]22;\x1b\\");
        assert_eq!(terminal.mouse_shape().unwrap(), mouse::Shape::Text);
    }

    fn tiny_terminal() -> Terminal<'static, 'static> {
        Terminal::new(8, 3).expect("terminal should initialize")
    }

    fn codepoint_at_tracked_ref(terminal: &Terminal<'_, '_>, tracked: &TrackedGridRef) -> u32 {
        let snapshot = tracked
            .snapshot(terminal)
            .expect("tracked snapshot should not fail")
            .expect("tracked ref should have a value");
        snapshot
            .cell()
            .expect("tracked snapshot should resolve to a cell")
            .codepoint()
            .expect("tracked snapshot cell should expose a codepoint")
    }

    #[test]
    fn tracked_grid_ref_follows_scroll() {
        let mut terminal = tiny_terminal();
        terminal.vt_write(b"alpha\r\nbravo\r\ncharlie");

        let tracked = terminal
            .track_grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))
            .expect("tracked grid ref should initialize");

        terminal.vt_write(b"\r\ndelta");

        assert!(tracked.has_value());
        assert_eq!(codepoint_at_tracked_ref(&terminal, &tracked), u32::from('a'));
        assert_eq!(
            tracked
                .point(PointSpace::Screen)
                .expect("tracked point should resolve")
                .expect("tracked point should have a value")
                .x,
            0
        );
    }

    #[test]
    fn tracked_grid_ref_reports_loss_and_can_set_point() {
        let mut terminal = tiny_terminal();
        terminal.vt_write(b"alpha\r\nbravo\r\ncharlie");

        let mut tracked = terminal
            .track_grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))
            .expect("tracked grid ref should initialize");

        terminal.reset();

        assert!(!tracked.has_value());
        assert!(tracked.snapshot(&terminal).expect("missing tracked snapshot should not fail").is_none());
        assert!(tracked.point(PointSpace::Screen).expect("missing tracked point should not fail").is_none());

        terminal.vt_write(b"echo");
        tracked
            .set(&mut terminal, Point::Active(PointCoordinate { x: 0, y: 0 }))
            .expect("tracked grid ref should set to a new point");

        assert!(tracked.has_value());
        assert_eq!(codepoint_at_tracked_ref(&terminal, &tracked), u32::from('e'));
    }

    #[test]
    fn tracked_grid_ref_survives_terminal_drop() {
        let tracked = {
            let mut terminal = tiny_terminal();
            terminal.vt_write(b"alpha");
            terminal
                .track_grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))
                .expect("tracked grid ref should initialize")
        };

        assert!(!tracked.has_value());
        assert!(tracked.point(PointSpace::Screen).expect("detached tracked point should not fail").is_none());
    }

    #[test]
    fn tracked_grid_ref_rejects_different_terminal() {
        let mut first = tiny_terminal();
        first.vt_write(b"alpha");
        let mut second = tiny_terminal();
        second.vt_write(b"bravo");

        let mut tracked = first
            .track_grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }))
            .expect("tracked grid ref should initialize");

        assert!(matches!(tracked.snapshot(&second), Err(Error::InvalidValue)));
        assert!(matches!(
            tracked.set(&mut second, Point::Active(PointCoordinate { x: 0, y: 0 })),
            Err(Error::InvalidValue)
        ));
    }

    #[test]
    fn grid_ref_converts_back_to_point() {
        let mut terminal = tiny_terminal();
        terminal.vt_write(b"alpha");

        let original = PointCoordinate { x: 1, y: 0 };
        let grid_ref = terminal.grid_ref(Point::Active(original)).expect("grid ref should resolve");

        assert_eq!(
            terminal
                .point_from_grid_ref(&grid_ref, PointSpace::Active)
                .expect("grid ref point conversion should not fail")
                .expect("grid ref should be representable in active space"),
            original
        );
    }

    /// An older libghostty sends a smaller request that ends before `name`.
    /// The fields past its `size` must not be read, and `reply` must not
    /// call whatever lies where the reply function would be.
    #[test]
    fn clipboard_write_from_an_older_libghostty() {
        unsafe extern "C" fn reply(_: *const ffi::ClipboardWrite, _: *const ffi::ClipboardWriteReply) {
            panic!("a reply function past the request's size was called");
        }
        let data = b"hi";
        let content = ffi::ClipboardContent {
            mime: ffi::String::from("text/plain"),
            data: ffi::String { ptr: data.as_ptr(), len: data.len() },
        };
        let raw = ffi::ClipboardWrite {
            size: std::mem::offset_of!(ffi::ClipboardWrite, name),
            location: ffi::ClipboardLocation::PRIMARY,
            contents: &raw const content,
            contents_len: 1,
            name: ffi::String::from("program"),
            granted: true,
            can_remember: true,
            reply: Some(reply),
            ..ffi::sized!(ffi::ClipboardWrite)
        };
        // SAFETY: `raw` outlives the borrow, matching the callback contract.
        let write = unsafe { ClipboardWrite::from_raw(&raw const raw) };
        assert_eq!(write.location(), ClipboardLocation::Primary);
        assert_eq!(write.contents().next().unwrap().data, b"hi");
        assert_eq!(write.name(), []);
        assert!(!write.granted());
        assert!(!write.can_remember());
        write.reply(Ok(()), false);
    }

    /// 一次剪贴板读取请求里回调看到的内容。
    #[derive(Debug, Default, PartialEq)]
    struct SeenRead {
        location: Option<ClipboardLocation>,
        mimes: Vec<String>,
        wants_list: bool,
        granted: bool,
    }

    /// OSC 52 的 `?` 是读取请求：回调拿到 `text/plain`，应答的内容以 base64
    /// 回给程序，结尾沿用请求的终止符。
    #[test]
    fn clipboard_read_answers_osc52_query() {
        let pty = RefCell::new(Vec::new());
        let seen = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal
            .on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data))
            .unwrap()
            .on_clipboard_read(|_, read| {
                // 回调里只记录不断言：panic 会让整个测试进程 abort。
                seen.borrow_mut().push(SeenRead {
                    location: Some(read.location()),
                    mimes: read.mimes().map(str::to_owned).collect(),
                    wants_list: read.wants_list(),
                    granted: read.granted(),
                });
                let contents = [ClipboardContent { mime: "text/plain", data: b"hello" }];
                read.reply(Ok(ClipboardReadReply { contents: &contents, ..ClipboardReadReply::default() }));
            })
            .unwrap();

        terminal.vt_write(b"\x1b]52;c;?\x07");
        assert_eq!(
            *seen.borrow(),
            [SeenRead {
                location: Some(ClipboardLocation::Standard),
                mimes: vec!["text/plain".to_owned()],
                ..SeenRead::default()
            }]
        );
        assert_eq!(pty.take(), b"\x1b]52;c;aGVsbG8=\x07");

        // `p` 选择 primary 剪贴板，ST 结束的请求以 ST 应答。
        terminal.vt_write(b"\x1b]52;p;?\x1b\\");
        assert_eq!(seen.borrow()[1].location, Some(ClipboardLocation::Primary));
        assert_eq!(pty.take(), b"\x1b]52;p;aGVsbG8=\x1b\\");
    }

    /// 拒绝读取时，OSC 52 收到一个空剪贴板。
    #[test]
    fn clipboard_read_denied_answers_empty() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal
            .on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data))
            .unwrap()
            .on_clipboard_read(|_, read| read.reply(Err(ClipboardReadError::Denied)))
            .unwrap();
        terminal.vt_write(b"\x1b]52;c;?\x07");
        assert_eq!(pty.take(), b"\x1b]52;c;\x07");
    }

    /// Kitty 剪贴板协议（OSC 5522）的读取带着 MIME 列表到达；拒绝时程序收到
    /// EPERM，宿主可以把可用类型列表交给只想列类型的请求。
    #[test]
    fn clipboard_read_kitty_protocol() {
        let pty = RefCell::new(Vec::new());
        let seen = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal
            .on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data))
            .unwrap()
            .on_clipboard_read(|_, read| {
                seen.borrow_mut().push(SeenRead {
                    location: Some(read.location()),
                    mimes: read.mimes().map(str::to_owned).collect(),
                    wants_list: read.wants_list(),
                    granted: read.granted(),
                });
                if read.wants_list() {
                    read.reply(Ok(ClipboardReadReply {
                        available: &["text/plain", "image/png"],
                        ..ClipboardReadReply::default()
                    }));
                } else {
                    read.reply(Err(ClipboardReadError::Denied));
                }
            })
            .unwrap();

        // 读 text/plain：MIME 在协议里以 base64 编码，"dGV4dC9wbGFpbg==" 即 text/plain。
        terminal.vt_write(b"\x1b]5522;type=read;dGV4dC9wbGFpbg==\x1b\\");
        let seen_first = seen.borrow()[0].mimes.clone();
        assert_eq!(seen_first, ["text/plain"]);
        let reply = String::from_utf8(pty.take()).unwrap();
        assert!(reply.contains("status=EPERM"), "{reply:?}");

        // 只列类型的请求（`.` 表示列出可用类型）。
        terminal.vt_write(b"\x1b]5522;type=read;Lg==\x1b\\");
        assert!(seen.borrow()[1].wants_list, "{:?}", seen.borrow());
        assert!(!seen.borrow()[1].granted);
        let reply = String::from_utf8(pty.take()).unwrap();
        // 类型列表 "text/plain image/png\n" 以 base64 编码出现在应答的 DATA 包里。
        assert!(reply.contains("status=OK"), "{reply:?}");
        assert!(reply.contains("status=DATA:mime=Lg==;dGV4dC9wbGFpbiBpbWFnZS9wbmcK"), "{reply:?}");
    }

    #[test]
    fn clipboard_write_max_bytes_round_trips() {
        let mut terminal = tiny_terminal();
        // 内置默认值是协议要求的最小值 64 MiB。
        assert_eq!(terminal.clipboard_write_max_bytes().unwrap(), 64 << 20);
        terminal.set_clipboard_write_max_bytes(Some(1024)).unwrap();
        assert_eq!(terminal.clipboard_write_max_bytes().unwrap(), 1024);
        terminal.set_clipboard_write_max_bytes(Some(usize::MAX)).unwrap();
        assert_eq!(terminal.clipboard_write_max_bytes().unwrap(), usize::MAX);
        terminal.set_clipboard_write_max_bytes(None).unwrap();
        assert_eq!(terminal.clipboard_write_max_bytes().unwrap(), 64 << 20);
    }

    /// 一条未知序列的自有拷贝，便于在回调外断言。
    #[derive(Debug, PartialEq)]
    enum OwnedUnknown {
        Apc(Vec<u8>, bool),
        Osc(Vec<u8>, bool, osc::Terminator),
    }

    #[test]
    fn unknown_sequences_are_reported_once_enabled() {
        let seen = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal
            .on_unknown_sequence(|_, seq| {
                seen.borrow_mut().push(match seq {
                    UnknownSequence::Apc { content, truncated } => OwnedUnknown::Apc(content.to_vec(), truncated),
                    UnknownSequence::Osc { content, truncated, terminator } => {
                        OwnedUnknown::Osc(content.to_vec(), truncated, terminator)
                    }
                });
            })
            .unwrap();

        // 只装回调、不设上限时什么也不报告。
        terminal.vt_write(b"\x1b]7400;status=busy\x07");
        assert!(seen.borrow().is_empty());

        terminal.set_unknown_sequence_max_bytes(64).unwrap();
        terminal.vt_write(b"\x1b]7400;status=busy\x07");
        terminal.vt_write(b"\x1b]7400;status=idle\x1b\\");
        // 自定义 APC（不是 Kitty 图形协议的 `G`）。
        terminal.vt_write(b"\x1b_Xhello\x1b\\");
        // 已实现的 OSC（窗口标题）不报告。
        terminal.vt_write(b"\x1b]2;title\x07");
        // 用 CAN 取消的序列不报告。
        terminal.vt_write(b"\x1b]7400;cancelled\x18");
        assert_eq!(
            seen.take(),
            [
                OwnedUnknown::Osc(b"7400;status=busy".to_vec(), false, osc::Terminator::Bel),
                OwnedUnknown::Osc(b"7400;status=idle".to_vec(), false, osc::Terminator::St),
                OwnedUnknown::Apc(b"Xhello".to_vec(), false),
            ]
        );

        // 超过上限的序列仍会报告，但内容被截断。
        terminal.set_unknown_sequence_max_bytes(6).unwrap();
        terminal.vt_write(b"\x1b]7400;status=busy\x07");
        assert_eq!(seen.take(), [OwnedUnknown::Osc(b"7400;s".to_vec(), true, osc::Terminator::Bel)]);

        // 上限设回 0 关闭上报。
        terminal.set_unknown_sequence_max_bytes(0).unwrap();
        terminal.vt_write(b"\x1b]7400;status=busy\x07");
        assert!(seen.borrow().is_empty());
    }

    /// XTGETTCAP 查询 `TN`（十六进制 544E）时，终端以设置的 terminfo 名回答。
    #[test]
    fn terminfo_name_answers_xtgettcap() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data)).unwrap();

        terminal.vt_write(b"\x1bP+q544E\x1b\\");
        let unset = pty.take();
        // 未设置时不回答名字。
        assert!(!String::from_utf8_lossy(&unset).contains("544E="), "{unset:?}");

        terminal.set_terminfo_name(Some("xterm-ghostty")).unwrap();
        terminal.vt_write(b"\x1bP+q544E\x1b\\");
        // "xterm-ghostty" 的十六进制编码。
        assert_eq!(String::from_utf8(pty.take()).unwrap(), "\x1bP1+r544E=787465726D2D67686F73747479\x1b\\");

        // 名字最长 128 字节。
        assert!(matches!(terminal.set_terminfo_name(Some(&"x".repeat(129))), Err(Error::InvalidValue)));
        terminal.set_terminfo_name(None).unwrap();
    }

    #[test]
    fn title_and_pwd_can_be_set_directly() {
        let changes = Cell::new(0);
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.on_title_changed(|_| changes.set(changes.get() + 1)).unwrap();
        terminal.set_title(Some("hello")).unwrap();
        terminal.set_pwd(Some("file:///tmp")).unwrap();
        assert_eq!(terminal.title().unwrap(), "hello");
        assert_eq!(terminal.pwd().unwrap(), "file:///tmp");
        terminal.set_title(None).unwrap().set_pwd(None).unwrap();
        assert_eq!(terminal.title().unwrap(), "");
        assert_eq!(terminal.pwd().unwrap(), "");
        assert_eq!(changes.get(), 0, "setting the title is not a title change");
    }

    /// 语义提示事件的自有拷贝。
    #[derive(Debug, PartialEq)]
    enum OwnedPrompt {
        PromptStart(PromptKind),
        InputStart,
        OutputStart(Vec<u8>),
        CommandEnd(Option<i32>, Vec<u8>),
    }

    #[test]
    fn semantic_prompt_reports_osc_133_steps() {
        let events = RefCell::new(Vec::new());
        let at_prompt = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(40, 5).unwrap();
        terminal
            .on_semantic_prompt(|term, event| {
                at_prompt.borrow_mut().push(term.is_cursor_at_prompt().ok());
                events.borrow_mut().push(match event {
                    SemanticPrompt::PromptStart { kind } => OwnedPrompt::PromptStart(kind),
                    SemanticPrompt::InputStart => OwnedPrompt::InputStart,
                    SemanticPrompt::OutputStart { command } => OwnedPrompt::OutputStart(command.to_vec()),
                    SemanticPrompt::CommandEnd { exit_code, error } => {
                        OwnedPrompt::CommandEnd(exit_code, error.to_vec())
                    }
                });
            })
            .unwrap();

        assert!(!terminal.is_cursor_at_prompt().unwrap());
        terminal.vt_write(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        assert!(terminal.is_cursor_at_prompt().unwrap());
        terminal.vt_write(b"ls\r\n\x1b]133;C\x07");
        assert!(!terminal.is_cursor_at_prompt().unwrap());
        terminal.vt_write(b"file\r\n\x1b]133;D;2\x07");
        // 右侧提示符（k=r），以及没有退出码的命令结束。
        terminal.vt_write(b"\x1b]133;A;k=r\x07\x1b]133;D\x07");

        assert_eq!(
            events.take(),
            [
                OwnedPrompt::PromptStart(PromptKind::Primary),
                OwnedPrompt::InputStart,
                OwnedPrompt::OutputStart(Vec::new()),
                OwnedPrompt::CommandEnd(Some(2), Vec::new()),
                OwnedPrompt::PromptStart(PromptKind::Right),
                OwnedPrompt::CommandEnd(None, Vec::new()),
            ]
        );
        // 回调运行时终端已经更新：输入开始时光标在提示符里，输出开始后不在。
        assert_eq!(at_prompt.borrow()[1], Some(true));
        assert_eq!(at_prompt.borrow()[2], Some(false));
    }

    #[test]
    fn vt_ground_tracks_unfinished_sequences() {
        let mut terminal = tiny_terminal();
        assert!(terminal.is_vt_ground().unwrap());
        terminal.vt_write(b"\x1b[3");
        assert!(!terminal.is_vt_ground().unwrap());
        terminal.vt_write(b"1m");
        assert!(terminal.is_vt_ground().unwrap());
        // 未写完的 UTF-8 也不算 ground。
        terminal.vt_write(b"\xe4\xb8");
        assert!(!terminal.is_vt_ground().unwrap());
        terminal.vt_write(b"\xad");
        assert!(terminal.is_vt_ground().unwrap());
    }

    #[test]
    fn memory_usage_reports_primary_pages() {
        let mut terminal = Terminal::new(80, 24).unwrap();
        let before = terminal.memory_usage().unwrap();
        assert!(before.primary_pages >= 1);
        assert!(before.primary_resident_bytes > 0);
        assert!(before.primary_virtual_bytes >= before.primary_resident_bytes);
        // 还没切到过备用屏，备用屏的字段全为零。
        assert_eq!(before.alternate_pages, 0);

        // 产生足够多的回滚区，page 数随之增长。
        for i in 0..5_000 {
            terminal.vt_write(format!("line {i}\r\n").as_bytes());
        }
        let after = terminal.memory_usage().unwrap();
        assert!(after.primary_pages > before.primary_pages);
        terminal.vt_write(b"\x1b[?1049h");
        assert!(terminal.memory_usage().unwrap().alternate_pages >= 1);
    }

    #[test]
    fn vt_write_until_ground_stops_at_ground() {
        let mut terminal = tiny_terminal();
        // 已经在 ground：什么也不消耗。
        assert_eq!(terminal.vt_write_until_ground(b"abc").unwrap(), Some(0));
        assert_eq!(terminal.cursor_x().unwrap(), 0);
        // 从序列中间开始：写到让序列结束的那个字节为止，后面的 "XY" 没有写入。
        terminal.vt_write(b"\x1b[3");
        assert!(!terminal.is_vt_ground().unwrap());
        assert_eq!(terminal.vt_write_until_ground(b"1mXY").unwrap(), Some(2));
        assert!(terminal.is_vt_ground().unwrap());
        assert_eq!(terminal.cursor_x().unwrap(), 0);
        // 整段写完仍在序列中间。
        terminal.vt_write(b"\x1b]2;ti");
        assert_eq!(terminal.vt_write_until_ground(b"tle").unwrap(), None);
        assert!(!terminal.is_vt_ground().unwrap());
        assert_eq!(terminal.vt_write_until_ground(b"\x07rest").unwrap(), Some(1));
        assert_eq!(terminal.title().unwrap(), "title");
        assert_ne!(terminal.cursor_style().unwrap().fg_color, crate::style::StyleColor::None);
    }

    #[test]
    fn xt_checksum_reports_are_opt_in() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = Terminal::new(10, 3).unwrap();
        terminal.on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data)).unwrap();
        terminal.vt_write(b"A");
        let query = b"\x1b[7;1;1;1;1;1*y";

        // 默认关闭：不回答。
        terminal.vt_write(query);
        assert_eq!(pty.take(), [] as [u8; 0]);

        terminal.set_xt_checksum_report_enabled(true).unwrap();
        terminal.vt_write(query);
        let negated = String::from_utf8(pty.take()).unwrap();
        // DECRPCRA：DCS Pi ! ~ 四位十六进制校验和 ST。
        assert!(negated.starts_with("\x1bP7!~"), "{negated:?}");
        assert!(negated.ends_with("\x1b\\"), "{negated:?}");

        // 不取负时结果不同。
        terminal.set_xt_checksum_extension(Some(ChecksumExtension::NO_NEGATE)).unwrap();
        terminal.vt_write(query);
        let plain = String::from_utf8(pty.take()).unwrap();
        assert_ne!(plain, negated);

        // None 恢复 DEC 的算法。
        terminal.set_xt_checksum_extension(None).unwrap();
        terminal.vt_write(query);
        assert_eq!(String::from_utf8(pty.take()).unwrap(), negated);

        // 超出 5 个定义位的值被拒绝。
        assert!(matches!(
            terminal.set_xt_checksum_extension(Some(ChecksumExtension::from_bits_retain(32))),
            Err(Error::InvalidValue)
        ));

        terminal.set_xt_checksum_report_enabled(false).unwrap();
        terminal.vt_write(query);
        assert_eq!(pty.take(), [] as [u8; 0]);
    }

    #[test]
    fn mode_reports_encode_decrpm() {
        let mut buf = [0u8; 32];
        let len = Mode::BRACKETED_PASTE.encode_report(ModeReportState::Set, &mut buf).unwrap();
        assert_eq!(&buf[..len], b"\x1b[?2004;1$y");
        let len = Mode::INSERT.encode_report(ModeReportState::Reset, &mut buf).unwrap();
        assert_eq!(&buf[..len], b"\x1b[4;2$y");
        assert!(matches!(
            Mode::new(9999, ModeKind::Dec).encode_report(ModeReportState::NotRecognized, &mut buf[..4]),
            Err(Error::OutOfSpace { .. })
        ));
    }

    #[test]
    fn size_reports_encode_each_style() {
        let size = SizeReportSize { rows: 24, columns: 80, cell_width: 9, cell_height: 18 };
        let mut buf = [0u8; 64];
        let encode = |style: SizeReportStyle, buf: &mut [u8]| {
            let len = style.encode_report(size, buf).unwrap();
            String::from_utf8(buf[..len].to_vec()).unwrap()
        };
        assert_eq!(encode(SizeReportStyle::Mode2048, &mut buf), "\x1b[48;24;80;432;720t");
        assert_eq!(encode(SizeReportStyle::Csi14T, &mut buf), "\x1b[4;432;720t");
        assert_eq!(encode(SizeReportStyle::Csi16T, &mut buf), "\x1b[6;18;9t");
        assert_eq!(encode(SizeReportStyle::Csi18T, &mut buf), "\x1b[8;24;80t");
        assert!(matches!(SizeReportStyle::Csi18T.encode_report(size, &mut buf[..2]), Err(Error::OutOfSpace { .. })));
    }
}

/// Soundness regression tests for
/// <https://github.com/Uzaaft/libghostty-rs/issues/74>.
///
/// These tests are gated on `cfg(miri)` because they construct the exact
/// shapes the C API produces and feed them into the safe wrappers, which was
/// UB before the wrappers stopped building slices and `&str` from unvalidated
/// FFI input. Run with:
///
/// ```sh
/// cargo +nightly miri test -p libghostty-vt miri_soundness
/// ```
#[cfg(all(test, miri))]
mod miri_soundness {
    use super::*;

    /// The C trampoline declares `contents: ?[*]const ClipboardContent` and
    /// sends `contents = NULL, contents_len = 0` for a write carrying no
    /// representations (e.g. OSC 52 with an empty payload, the documented
    /// "clear the clipboard" shape). `slice::from_raw_parts` requires a
    /// non-null pointer even at length zero, so `contents()` used to be UB
    /// here; it must yield an empty iterator so hosts can observe "clear".
    #[test]
    fn clipboard_write_with_no_representations() {
        let raw = ffi::ClipboardWrite {
            location: ffi::ClipboardLocation::STANDARD,
            contents: std::ptr::null(),
            contents_len: 0,
            ..ffi::sized!(ffi::ClipboardWrite)
        };
        // SAFETY: `raw` outlives the borrow, matching the callback contract.
        let write = unsafe { ClipboardWrite::from_raw(&raw) };
        assert_eq!(write.contents().count(), 0);
    }

    /// OSC 52 payloads are base64-decoded arbitrary bytes ("binary-safe" per
    /// the C header), but `ClipboardContent` used to expose them as `&str`
    /// built with `str::from_utf8_unchecked` in the sys crate, so decoding
    /// the invalid `&str` entered unreachable code in std's UTF-8 decoder.
    /// The data is exposed as `&[u8]` now; check it round-trips verbatim.
    #[test]
    fn clipboard_content_with_non_utf8_data() {
        // OSC 52 payload "//4=" base64-decodes to FF FE, which is not UTF-8.
        let data = [0xFF_u8, 0xFE];
        let raw = ffi::ClipboardContent {
            mime: ffi::String::from("text/plain"),
            data: ffi::String { ptr: data.as_ptr(), len: data.len() },
        };
        // SAFETY: `data` outlives the borrow, matching the callback contract.
        let content = unsafe { ClipboardContent::from_raw(&raw) };
        assert_eq!(content.mime, "text/plain");
        assert_eq!(content.data, &data);
    }

    /// `mime` stays `&str`, so it must be validated rather than trusted:
    /// a non-UTF-8 mime string falls back to the opaque-bytes mime type
    /// instead of producing an invalid `&str`.
    #[test]
    fn clipboard_content_with_non_utf8_mime() {
        let mime = [0xFF_u8, 0xFE];
        let data = *b"hello";
        let raw = ffi::ClipboardContent {
            mime: ffi::String { ptr: mime.as_ptr(), len: mime.len() },
            data: ffi::String { ptr: data.as_ptr(), len: data.len() },
        };
        // SAFETY: `mime` and `data` outlive the borrow, matching the
        // callback contract.
        let content = unsafe { ClipboardContent::from_raw(&raw) };
        assert_eq!(content.mime, "application/octet-stream");
        assert_eq!(content.data, b"hello");
    }
}
