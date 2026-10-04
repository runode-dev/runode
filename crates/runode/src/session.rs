//! One terminal: the libghostty-vt state machine wired to a child shell.
//!
//! `Session` owns the VT state and lives on the UI thread, because
//! `libghostty_vt::Terminal` is single-threaded. PTY output arrives over a
//! channel (see `Pty::spawn`) and is fed in with `feed`; the renderer reads a
//! `Frame`, which `refresh` keeps current by copying only the rows libghostty
//! reports dirty.

use std::{
    cell::{Cell as StdCell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::channel::mpsc::UnboundedReceiver;
use libghostty_vt::{
    key, mouse, paste,
    render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator},
    screen::CellWide,
    style::{RgbColor, Underline},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode,
        PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes, SizeReportSize,
        Terminal,
    },
};

use crate::pty::{GridSize, Pty, PtyEvent, PtyWriter};

const SCROLLBACK_LINES: usize = 10_000;
/// How long a program may freeze the screen with synchronized output (mode
/// 2026) before we stop honouring it, as Ghostty and others do.
pub const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<RgbColor> for Rgb {
    fn from(c: RgbColor) -> Self {
        Self(c.r, c.g, c.b)
    }
}

impl Rgb {
    pub fn to_u32(self) -> u32 {
        (u32::from(self.0) << 16) | (u32::from(self.1) << 8) | u32::from(self.2)
    }
}

/// Text attributes that change how a glyph is shaped or decorated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// One grid cell, already resolved to concrete colors.
#[derive(Clone, Debug, Default)]
pub struct Cell {
    /// The grapheme cluster, empty for a blank cell.
    pub text: String,
    pub fg: Rgb,
    /// `None` paints nothing: the frame background shows through.
    pub bg: Option<Rgb>,
    pub attrs: Attrs,
    /// A wide glyph that also covers the next cell.
    pub wide: bool,
    /// The continuation of a wide glyph; draw nothing here.
    pub spacer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    BlockHollow,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub color: Rgb,
    /// The cursor sits on a wide glyph and spans two cells.
    pub wide: bool,
}

/// What the renderer draws: a copy of the viewport, independent of the
/// libghostty render state so painting never touches the VT.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// Row-major, `cols * rows` cells.
    pub cells: Vec<Cell>,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Option<Cursor>,
}

impl Frame {
    pub fn row(&self, y: u16) -> &[Cell] {
        let start = usize::from(y) * usize::from(self.cols);
        &self.cells[start..start + usize::from(self.cols)]
    }
}

/// Changes the UI cares about, accumulated by the VT callbacks.
#[derive(Default)]
struct Effects {
    title_changed: StdCell<bool>,
    bell: StdCell<bool>,
}

/// Copies libghostty's render state into a `Frame`. Shared with the
/// render-hold callback, which must capture the frame the moment a
/// synchronized update begins.
struct Renderer {
    render_state: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    frame: Frame,
}

pub struct Session {
    terminal: Terminal<'static, 'static>,
    renderer: Rc<RefCell<Renderer>>,
    /// When the running program began holding the screen (mode 2026).
    held_since: Rc<StdCell<Option<Instant>>>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    pty: Pty,
    writer: PtyWriter,
    size: Rc<StdCell<GridSize>>,
    effects: Rc<Effects>,
    /// Encoded input waiting to be written, reused across keystrokes.
    scratch: Vec<u8>,
    pub title: Option<String>,
    pub exited: bool,
}

impl Session {
    pub fn spawn(size: GridSize) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        Self::spawn_shell(size, None)
    }

    pub fn spawn_shell(
        size: GridSize,
        shell: Option<&str>,
    ) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        let (pty, rx) = Pty::spawn(size, shell, None)?;
        let writer = pty.writer.clone();

        let mut terminal = Terminal::new(size.cols, size.rows)?;
        terminal.set_scrollback_max_lines(Some(SCROLLBACK_LINES))?;
        terminal.resize(
            size.cols,
            size.rows,
            u32::from(size.cell_width_px),
            u32::from(size.cell_height_px),
        )?;

        let shared_size = Rc::new(StdCell::new(size));
        let effects = Rc::new(Effects::default());
        let renderer = Rc::new(RefCell::new(Renderer {
            render_state: RenderState::new()?,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
            frame: Frame::default(),
        }));
        let held_since = Rc::new(StdCell::new(None));

        // Query replies (DA, DECRQM, DSR...) go straight back to the child.
        // Without them programs like vim and tmux stall while probing.
        let reply = writer.clone();
        terminal
            .on_pty_write(move |_, data| reply.write(data))?
            .on_size({
                let size = shared_size.clone();
                move |_| {
                    let size = size.get();
                    Some(SizeReportSize {
                        rows: size.rows,
                        columns: size.cols,
                        cell_width: u32::from(size.cell_width_px),
                        cell_height: u32::from(size.cell_height_px),
                    })
                }
            })?
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: Default::default(),
                })
            })?
            .on_xtversion(|_| Some(concat!("runode ", env!("CARGO_PKG_VERSION"))))?
            .on_title_changed({
                let effects = effects.clone();
                move |_| effects.title_changed.set(true)
            })?
            .on_bell({
                let effects = effects.clone();
                move |_| effects.bell.set(true)
            })?
            // RIS clears the title without reporting a title change.
            .on_reset({
                let effects = effects.clone();
                move |_| effects.title_changed.set(true)
            })?
            .on_render_hold({
                let renderer = renderer.clone();
                let held_since = held_since.clone();
                move |term, held| {
                    if held {
                        // Freeze on the frame as it stood before the update
                        // began. The renderer is never borrowed across a VT
                        // write, but this runs inside an extern "C" callback,
                        // so never risk a panicking borrow here.
                        if let Ok(mut renderer) = renderer.try_borrow_mut()
                            && let Err(err) = renderer.refresh(term)
                        {
                            tracing::warn!("render hold capture failed: {err}");
                        }
                        held_since.set(Some(Instant::now()));
                    } else {
                        held_since.set(None);
                    }
                }
            })?;

        Ok((
            Self {
                terminal,
                renderer,
                held_since,
                key_encoder: key::Encoder::new()?,
                key_event: key::Event::new()?,
                mouse_encoder: mouse::Encoder::new()?,
                mouse_event: mouse::Event::new()?,
                pty,
                writer,
                size: shared_size,
                effects,
                scratch: Vec::with_capacity(64),
                title: None,
                exited: false,
            },
            rx,
        ))
    }

    /// Feeds PTY output into the VT. Returns whether the title changed.
    pub fn feed(&mut self, data: &[u8]) -> bool {
        self.terminal.vt_write(data);
        if self.effects.title_changed.take() {
            self.title = self
                .terminal
                .title()
                .ok()
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            return true;
        }
        false
    }

    pub fn take_bell(&self) -> bool {
        self.effects.bell.take()
    }

    pub fn resize(&mut self, size: GridSize) {
        if self.size.get() == size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.size.set(size);
        if let Err(err) = self.terminal.resize(
            size.cols,
            size.rows,
            u32::from(size.cell_width_px),
            u32::from(size.cell_height_px),
        ) {
            tracing::warn!("terminal resize failed: {err}");
        }
        self.pty.resize(size);
    }

    /// Encodes one key press through libghostty, which honours the modes the
    /// running program asked for (application cursor keys, Kitty keyboard
    /// protocol, modifyOtherKeys...). Returns false when the key produced no
    /// bytes, so the caller can let the platform handle it.
    pub fn key(&mut self, input: &KeyInput) -> bool {
        self.key_event
            .set_action(key::Action::Press)
            .set_key(input.key)
            .set_mods(input.mods)
            .set_consumed_mods(input.consumed_mods)
            .set_unshifted_codepoint(input.unshifted)
            .set_utf8(input.text.as_deref());
        self.scratch.clear();
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&self.key_event, &mut self.scratch);
        if let Err(err) = encoded {
            tracing::warn!("key encode failed: {err}");
            return false;
        }
        if self.scratch.is_empty() {
            return false;
        }
        self.scroll_to_bottom();
        self.writer.write(&self.scratch);
        true
    }

    /// Text committed by an input method, sent as typed.
    pub fn commit_text(&mut self, text: &str) {
        self.scroll_to_bottom();
        self.writer.write(text.as_bytes());
    }

    pub fn paste(&mut self, text: &str) {
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.as_bytes().to_vec();
        // Bracketing adds 12 bytes; unbracketed newlines become CRs in place.
        let mut buf = vec![0u8; data.len() + 16];
        match paste::encode(&mut data, bracketed, &mut buf) {
            Ok(len) => {
                self.scroll_to_bottom();
                self.writer.write(&buf[..len]);
            }
            Err(err) => tracing::warn!("paste encode failed: {err}"),
        }
    }

    /// Wheel input: reported to the program when it tracks the mouse,
    /// otherwise it scrolls the viewport through scrollback.
    pub fn scroll(&mut self, lines: isize, cell: (u16, u16), mods: key::Mods) {
        if lines == 0 {
            return;
        }
        if self.terminal.is_mouse_tracking().unwrap_or(false) {
            let size = self.size.get();
            self.mouse_encoder
                .set_options_from_terminal(&self.terminal)
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
            let button = if lines < 0 {
                mouse::Button::Four
            } else {
                mouse::Button::Five
            };
            let position = mouse::Position {
                x: f32::from(cell.0) * f32::from(size.cell_width_px),
                y: f32::from(cell.1) * f32::from(size.cell_height_px),
            };
            self.scratch.clear();
            for _ in 0..lines.unsigned_abs() {
                self.mouse_event
                    .set_mods(mods)
                    .set_position(position)
                    .set_button(Some(button))
                    .set_action(mouse::Action::Press);
                if let Err(err) = self
                    .mouse_encoder
                    .encode_to_vec(&self.mouse_event, &mut self.scratch)
                {
                    tracing::warn!("mouse encode failed: {err}");
                    return;
                }
            }
            self.writer.write(&self.scratch);
        } else {
            self.terminal.scroll_viewport(ScrollViewport::Delta(lines));
        }
    }

    fn scroll_to_bottom(&mut self) {
        if !self.terminal.viewport_active().unwrap_or(true) {
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
        }
    }

    /// Whether the running program is holding the screen for a synchronized
    /// update; the view must repaint once `SYNC_OUTPUT_TIMEOUT` passes even
    /// if no more output arrives.
    pub fn render_held(&self) -> bool {
        self.held_since.get().is_some()
    }

    /// The up-to-date frame, moved out so a painter can hold it alongside
    /// other mutable state. Hand it back with `restore_frame`.
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
            // A program that crashed or forgot to release its hold must not
            // freeze the screen forever.
            if let Err(err) = self.terminal.set_mode(Mode::SYNC_OUTPUT, false) {
                tracing::warn!("failed to end synchronized output: {err}");
            }
            self.held_since.set(None);
        }
        if let Err(err) = self.renderer.borrow_mut().refresh(&self.terminal) {
            tracing::warn!("render state update failed: {err}");
        }
    }
}

impl Renderer {
    fn refresh(&mut self, terminal: &Terminal<'static, '_>) -> libghostty_vt::error::Result<()> {
        let snapshot = self.render_state.update(terminal)?;
        let dirty = snapshot.dirty()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let colors = snapshot.colors()?;
        let background = Rgb::from(colors.background);
        let foreground = Rgb::from(colors.foreground);

        let frame = &mut self.frame;
        let reshaped = frame.cols != cols || frame.rows != rows;
        let recolored = frame.background != background || frame.foreground != foreground;
        if dirty == Dirty::Clean && !reshaped && !recolored {
            return Ok(());
        }
        if reshaped {
            frame.cols = cols;
            frame.rows = rows;
            frame.cells = vec![Cell::default(); usize::from(cols) * usize::from(rows)];
        }
        frame.background = background;
        frame.foreground = foreground;
        let full = dirty == Dirty::Full || reshaped || recolored;

        let mut row_it = self.row_it.update(&snapshot)?;
        let mut y = 0usize;
        while let Some(row) = row_it.next() {
            if y >= usize::from(rows) {
                break;
            }
            if full || row.dirty()? {
                let selection = row.selection()?;
                let mut cell_it = self.cell_it.update(row)?;
                let mut x = 0usize;
                while let Some(cell) = cell_it.next() {
                    if x >= usize::from(cols) {
                        break;
                    }
                    let out = &mut frame.cells[y * usize::from(cols) + x];
                    let wide = cell.raw_cell()?.wide()?;
                    out.wide = wide == CellWide::Wide;
                    out.spacer = matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead);
                    out.text.clear();
                    if cell.graphemes_len()? > 0 {
                        cell.graphemes_utf8(&mut out.text)?;
                    }
                    let mut fg = cell.fg_color()?.map_or(foreground, Rgb::from);
                    let mut bg = cell.bg_color()?.map(Rgb::from);
                    out.attrs = Attrs::default();
                    if cell.has_styling()? {
                        let style = cell.style()?;
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
                    // Selected cells draw in reverse video, as most terminals do.
                    let x16 = x as u16;
                    if selection.is_some_and(|s| s.start_x <= x16 && x16 <= s.end_x) {
                        let swapped = bg.unwrap_or(background);
                        bg = Some(fg);
                        fg = swapped;
                    }
                    out.fg = fg;
                    out.bg = bg;
                    x += 1;
                }
                row.set_dirty(false)?;
            }
            y += 1;
        }

        frame.cursor = None;
        if snapshot.cursor_visible()?
            && let Some(vp) = snapshot.cursor_viewport()?
        {
            let shape = match snapshot.cursor_visual_style()? {
                CursorVisualStyle::Bar => CursorShape::Bar,
                CursorVisualStyle::Underline => CursorShape::Underline,
                CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
                _ => CursorShape::Block,
            };
            let index = usize::from(vp.y) * usize::from(cols) + usize::from(vp.x);
            frame.cursor = Some(Cursor {
                x: vp.x,
                y: vp.y,
                shape,
                color: colors.cursor.map_or(foreground, Rgb::from),
                wide: frame.cells.get(index).is_some_and(|c| c.wide),
            });
        }
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(())
    }
}

/// A platform keystroke translated to libghostty's vocabulary.
pub struct KeyInput {
    pub key: key::Key,
    pub mods: key::Mods,
    /// Modifiers the platform already applied to produce `text`.
    pub consumed_mods: key::Mods,
    /// The character the key produces with no modifiers ('\0' if none).
    pub unshifted: char,
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt as _, executor::block_on};

    fn row_text(frame: &Frame, y: u16) -> String {
        frame
            .row(y)
            .iter()
            .filter(|c| !c.spacer)
            .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Drives a real shell through the PTY and libghostty-vt end to end:
    /// typed keys reach the child, its colored and wide output lands in the
    /// frame, and EOF arrives when it exits.
    #[test]
    fn shell_round_trip() {
        let size = GridSize {
            cols: 40,
            rows: 10,
            cell_width_px: 8,
            cell_height_px: 16,
        };
        let (mut session, mut rx) = Session::spawn_shell(size, Some("/bin/sh")).unwrap();
        let script = "printf '\\033[31mred\\033[0m \\344\\270\\255\\346\\226\\207 ok\\n'; exit\r";
        for c in script.chars() {
            let (key, unshifted) = if c == '\r' {
                (key::Key::Enter, '\r')
            } else {
                (key::Key::Unidentified, c)
            };
            let input = KeyInput {
                key,
                mods: key::Mods::empty(),
                consumed_mods: key::Mods::empty(),
                unshifted,
                text: (c != '\r').then(|| c.to_string()),
            };
            assert!(session.key(&input), "key {c:?} produced no bytes");
        }
        block_on(async {
            while let Some(event) = rx.next().await {
                match event {
                    PtyEvent::Output(data) => {
                        session.feed(&data);
                    }
                    PtyEvent::Exited => break,
                }
            }
        });

        let frame = session.frame().clone();
        let line = (0..frame.rows)
            .find(|&y| row_text(&frame, y).starts_with("red "))
            .unwrap_or_else(|| {
                let screen: Vec<_> = (0..frame.rows).map(|y| row_text(&frame, y)).collect();
                panic!("output line not found in {screen:#?}")
            });
        assert_eq!(row_text(&frame, line), "red 中文 ok");
        let row = frame.row(line);
        assert_ne!(row[0].fg, frame.foreground, "SGR 31 should color the text");
        assert_eq!(row[4].text, "中");
        assert!(row[4].wide && row[5].spacer);
    }

    fn idle_session() -> Session {
        let size = GridSize {
            cols: 20,
            rows: 4,
            cell_width_px: 8,
            cell_height_px: 16,
        };
        // `cat` stays quiet, so only what the test feeds reaches the VT.
        Session::spawn_shell(size, Some("/bin/cat")).unwrap().0
    }

    #[test]
    fn synchronized_output_freezes_the_frame_until_released() {
        let mut session = idle_session();
        session.feed(b"before");
        assert_eq!(row_text(&session.frame(), 0), "before");

        session.feed(b"\x1b[?2026h\r\x1b[2Kafter");
        assert!(session.render_held());
        assert_eq!(row_text(&session.frame(), 0), "before", "held frame must not change");

        session.feed(b"\x1b[?2026l");
        assert!(!session.render_held());
        assert_eq!(row_text(&session.frame(), 0), "after");
    }

    #[test]
    fn full_reset_clears_the_title() {
        let mut session = idle_session();
        assert!(session.feed(b"\x1b]2;hello\x07"));
        assert_eq!(session.title.as_deref(), Some("hello"));
        assert!(session.feed(b"\x1bc"));
        assert_eq!(session.title, None);
    }
}
