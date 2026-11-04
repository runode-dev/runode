//! X11 的远程桌面：RandR 列屏幕，MIT-SHM 把一块屏幕的画面读进和 X 服务器共享的内存（没有 MIT-SHM 时退到普通
//! 的 `GetImage`），XFixES 取指针的图像画上去，XTEST 注入键鼠。不用授权，X 服务器认得这个用户就行。
//!
//! X 不告诉屏幕变了没有，`Source::wait` 每次都读一整张，和上一张比过才算变了；一块 1080p 的屏幕读加比较远不到
//! 一帧的时长。

use std::{
    io,
    os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
    ptr::NonNull,
    thread,
    time::Duration,
};

use runode_types::{input::MouseButton, screen::ScreenInput};
use x11rb::{
    connection::{Connection as _, RequestConnection as _},
    protocol::{
        randr::ConnectionExt as _,
        shm::{self, ConnectionExt as _},
        xfixes::{self, ConnectionExt as _},
        xproto::{self, ConnectionExt as _, ImageFormat},
        xtest::{self, ConnectionExt as _},
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

use super::{capture::Picture, keysym};
use crate::Display;

/// XTEST 假输入的事件类型。
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;
/// 临时改键位注入字符后等这么久，各程序收到改键位的通知、按新键位读这个键。
const REMAP_SETTLE: Duration = Duration::from_millis(20);

fn connect() -> io::Result<(RustConnection, xproto::Window)> {
    let (conn, screen) = x11rb::connect(None).map_err(|err| io::Error::other(format!("cannot connect to X: {err}")))?;
    let root = conn.setup().roots[screen].root;
    Ok((conn, root))
}

fn other(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

/// 一块屏幕在根窗口里的位置，像素。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

/// 开着的各块屏幕，主屏排第一；编号是 RandR 给屏幕起的名字（atom）。X 没有缩放的概念，点数就是像素数。没有
/// RandR 1.5 时整个根窗口算一块屏幕，编号是 0。
pub(crate) fn displays() -> io::Result<Vec<Display>> {
    let (conn, root) = connect()?;
    Ok(monitors(&conn, root)
        .into_iter()
        .map(|(id, rect)| {
            let (width, height) = (u32::from(rect.width), u32::from(rect.height));
            Display { id, width, height, pixel_width: width, pixel_height: height }
        })
        .collect())
}

fn monitors(conn: &RustConnection, root: xproto::Window) -> Vec<(u32, Rect)> {
    let listed = conn
        .randr_get_monitors(root, true)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| {
            let mut monitors = reply.monitors;
            monitors.sort_by_key(|monitor| !monitor.primary);
            monitors
                .into_iter()
                .filter(|monitor| monitor.width > 0 && monitor.height > 0)
                .map(|monitor| {
                    let rect = Rect { x: monitor.x, y: monitor.y, width: monitor.width, height: monitor.height };
                    (monitor.name, rect)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !listed.is_empty() {
        return listed;
    }
    let Some(screen) = root_screen(conn, root) else { return Vec::new() };
    vec![(0, Rect { x: 0, y: 0, width: screen.width_in_pixels, height: screen.height_in_pixels })]
}

/// 屏幕 `id` 的位置；找不到时是主屏。
fn find(conn: &RustConnection, root: xproto::Window, id: u32) -> Rect {
    let monitors = monitors(conn, root);
    monitors.iter().find(|(monitor, _)| *monitor == id).or(monitors.first()).map(|(_, rect)| *rect).unwrap_or(Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    })
}

/// 一块屏幕的画面。
pub(crate) struct Source {
    conn: RustConnection,
    root: xproto::Window,
    rect: Rect,
    grab: Grab,
    /// 最近一张交出去的画面，画上了指针，行与行紧挨着。
    frame: Vec<u8>,
    have_frame: bool,
    /// 读下一张用的，和 `frame` 比过后两边对调。
    scratch: Vec<u8>,
    cursor: Option<Cursor>,
}

/// 怎么读屏幕的像素。
enum Grab {
    Shm(ShmSegment),
    /// 没有 MIT-SHM（比如连的是别的机器上的 X）时用普通的 `GetImage`，慢一些。
    Plain,
}

/// 和 X 服务器共享的一段内存。
struct ShmSegment {
    seg: shm::Seg,
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: 映射的内存只有采集线程读，X 服务器只在 `GetImage` 的应答回来之前写。
unsafe impl Send for ShmSegment {}

impl Drop for ShmSegment {
    fn drop(&mut self) {
        // SAFETY: 映射是 `ShmSegment::new` 建的，长度如实给出，只放一次。
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

impl ShmSegment {
    /// 建一段 `len` 字节的共享内存交给 X 服务器（MIT-SHM 1.2 的按文件描述符共享）。
    fn new(conn: &RustConnection, len: usize) -> io::Result<Self> {
        let version = conn.shm_query_version().map_err(other)?.reply().map_err(other)?;
        if (version.major_version, version.minor_version) < (1, 2) {
            return Err(io::Error::other("MIT-SHM is older than 1.2"));
        }
        // SAFETY: 名字是以 NUL 结尾的字面量。
        let fd = unsafe { libc::memfd_create(c"runode-screen".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` 是刚建的、只归这里。
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: 对自己的 memfd 设长度。
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 映射整个 memfd，只读；失败时返回 `MAP_FAILED`。
        let ptr =
            unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr = NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::other("mmap gave a null pointer"))?;
        let segment = Self { seg: conn.generate_id().map_err(other)?, ptr, len };
        conn.shm_attach_fd(segment.seg, fd, false).map_err(other)?.check().map_err(other)?;
        Ok(segment)
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: 映射有 `len` 字节，活到 `self` 丢掉。
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

/// XFixES 交来的指针图像。
struct Cursor {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    xhot: u16,
    yhot: u16,
    serial: u32,
    /// 预乘过透明度的 ARGB，一个像素一个 `u32`。
    pixels: Vec<u32>,
}

impl Source {
    pub(crate) fn open(display: u32, cursor: bool) -> io::Result<Self> {
        let (conn, root) = connect()?;
        check_format(&conn, root)?;
        let rect = find(&conn, root, display);
        let len = usize::from(rect.width) * usize::from(rect.height) * 4;
        let grab = match ShmSegment::new(&conn, len) {
            Ok(segment) => Grab::Shm(segment),
            Err(err) => {
                tracing::info!("reading the X screen without shared memory: {err}");
                Grab::Plain
            }
        };
        let cursor =
            cursor
                && conn.extension_information(xfixes::X11_EXTENSION_NAME).ok().flatten().is_some_and(|_| {
                    conn.xfixes_query_version(4, 0).ok().and_then(|cookie| cookie.reply().ok()).is_some()
                });
        Ok(Self {
            conn,
            root,
            rect,
            grab,
            frame: vec![0; len],
            have_frame: false,
            scratch: Vec::with_capacity(len),
            cursor: cursor.then(|| Cursor {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                xhot: 0,
                yhot: 0,
                serial: 0,
                pixels: Vec::new(),
            }),
        })
    }

    /// 读一整张屏幕进 `into`。
    fn read(&mut self, into: &mut Vec<u8>) -> io::Result<()> {
        let Rect { x, y, width, height } = self.rect;
        match &self.grab {
            Grab::Shm(segment) => {
                self.conn
                    .shm_get_image(self.root, x, y, width, height, !0, ImageFormat::Z_PIXMAP.into(), segment.seg, 0)
                    .map_err(other)?
                    .reply()
                    .map_err(other)?;
                into.clear();
                into.extend_from_slice(segment.bytes());
            }
            Grab::Plain => {
                let reply = self
                    .conn
                    .get_image(ImageFormat::Z_PIXMAP, self.root, x, y, width, height, !0)
                    .map_err(other)?
                    .reply()
                    .map_err(other)?;
                *into = reply.data;
            }
        }
        Ok(())
    }

    fn refresh_cursor(&mut self) {
        let Some(cursor) = &mut self.cursor else { return };
        let Ok(reply) = self.conn.xfixes_get_cursor_image().map_err(other).and_then(|c| c.reply().map_err(other))
        else {
            return;
        };
        cursor.x = reply.x;
        cursor.y = reply.y;
        if reply.cursor_serial != cursor.serial || cursor.pixels.is_empty() {
            cursor.width = reply.width;
            cursor.height = reply.height;
            cursor.xhot = reply.xhot;
            cursor.yhot = reply.yhot;
            cursor.serial = reply.cursor_serial;
            cursor.pixels = reply.cursor_image;
        }
    }
}

impl super::capture::Source for Source {
    /// X 不报屏幕变了没有，不等：每次读一整张、画上指针，和上一张比。
    fn wait(&mut self, _timeout: Duration) -> io::Result<bool> {
        // 没订事件也会收到 `MappingNotify` 这类，不取就在连接里越堆越多。
        while self.conn.poll_for_event().map_err(other)?.is_some() {}
        let mut scratch = std::mem::take(&mut self.scratch);
        self.read(&mut scratch)?;
        self.refresh_cursor();
        if let Some(cursor) = &self.cursor {
            draw_cursor(&mut scratch, self.rect, cursor);
        }
        let changed = !self.have_frame || scratch != self.frame;
        if changed {
            std::mem::swap(&mut self.frame, &mut scratch);
            self.have_frame = true;
        }
        self.scratch = scratch;
        Ok(changed)
    }

    fn picture(&self) -> Option<Picture<'_>> {
        self.have_frame.then(|| Picture {
            data: &self.frame,
            stride: usize::from(self.rect.width) * 4,
            size: (u32::from(self.rect.width), u32::from(self.rect.height)),
        })
    }
}

/// 根窗口是 `root` 的那个 X 屏幕：`DISPLAY` 里写了屏幕号（`:0.1`）时不是第一个。
fn root_screen(conn: &RustConnection, root: xproto::Window) -> Option<&xproto::Screen> {
    conn.setup().roots.iter().find(|screen| screen.root == root)
}

/// 根窗口要是每像素 32 位、红绿蓝各占一个字节的 TrueColor，内存里才是 BGRx。
fn check_format(conn: &RustConnection, root: xproto::Window) -> io::Result<()> {
    let setup = conn.setup();
    let screen = root_screen(conn, root).ok_or_else(|| io::Error::other("the X server lists no such screen"))?;
    let bits = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == screen.root_depth)
        .map(|format| format.bits_per_pixel);
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| &depth.visuals)
        .find(|visual| visual.visual_id == screen.root_visual);
    let bgrx = visual
        .is_some_and(|visual| visual.red_mask == 0xff0000 && visual.green_mask == 0xff00 && visual.blue_mask == 0xff);
    if bits != Some(32) || !bgrx || setup.image_byte_order != xproto::ImageOrder::LSB_FIRST {
        return Err(io::Error::other(format!(
            "the X screen is {}-bit deep with {:?} bits per pixel, not 32-bit BGRx",
            screen.root_depth, bits
        )));
    }
    Ok(())
}

/// 把指针按预乘的透明度叠到画面上。
fn draw_cursor(frame: &mut [u8], rect: Rect, cursor: &Cursor) {
    let left = i32::from(cursor.x) - i32::from(cursor.xhot) - i32::from(rect.x);
    let top = i32::from(cursor.y) - i32::from(cursor.yhot) - i32::from(rect.y);
    let (width, height) = (i32::from(rect.width), i32::from(rect.height));
    for row in 0..i32::from(cursor.height) {
        let y = top + row;
        if !(0..height).contains(&y) {
            continue;
        }
        for column in 0..i32::from(cursor.width) {
            let x = left + column;
            if !(0..width).contains(&x) {
                continue;
            }
            let Some(&argb) = cursor.pixels.get((row * i32::from(cursor.width) + column) as usize) else { continue };
            let alpha = argb >> 24;
            if alpha == 0 {
                continue;
            }
            let at = ((y * width + x) * 4) as usize;
            let pixel = &mut frame[at..at + 3];
            for (channel, shift) in pixel.iter_mut().zip([0u32, 8, 16]) {
                let source = (argb >> shift) & 0xff;
                *channel = (source + u32::from(*channel) * (255 - alpha) / 255).min(255) as u8;
            }
        }
    }
}

/// 往一块屏幕上注入输入（XTEST）。
pub(crate) struct Injector {
    conn: RustConnection,
    root: xproto::Window,
    rect: Rect,
    keymap: Keymap,
}

/// 键位表：keycode 对应的 keysym，每个 keycode `per_keycode` 个（第一个不按 Shift，第二个按 Shift）。
struct Keymap {
    min: u8,
    per_keycode: u8,
    keysyms: Vec<u32>,
}

impl Keymap {
    fn read(conn: &RustConnection) -> io::Result<Self> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn.get_keyboard_mapping(min, max - min + 1).map_err(other)?.reply().map_err(other)?;
        Ok(Self { min, per_keycode: reply.keysyms_per_keycode, keysyms: reply.keysyms })
    }

    fn row(&self, index: usize) -> &[u32] {
        let per = usize::from(self.per_keycode);
        &self.keysyms[index * per..(index + 1) * per]
    }

    fn count(&self) -> usize {
        if self.per_keycode == 0 { 0 } else { self.keysyms.len() / usize::from(self.per_keycode) }
    }

    /// 打出 `keysym` 的键和要不要按 Shift。
    fn find(&self, keysym: u32) -> Option<(u8, bool)> {
        for index in 0..self.count() {
            let row = self.row(index);
            let keycode = self.min + index as u8;
            if row.first() == Some(&keysym) {
                return Some((keycode, false));
            }
            if row.get(1) == Some(&keysym) {
                return Some((keycode, true));
            }
        }
        None
    }

    /// 没有任何 keysym 的键，临时改成要打的字符用；从大往小找，不碰常用的键。
    fn spare(&self) -> Option<u8> {
        (0..self.count())
            .rev()
            .find(|&index| self.row(index).iter().all(|&keysym| keysym == 0))
            .map(|index| self.min + index as u8)
    }
}

impl Injector {
    pub(crate) fn new(display: u32) -> io::Result<Self> {
        let (conn, root) = connect()?;
        conn.extension_information(xtest::X11_EXTENSION_NAME)
            .map_err(other)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "the X server has no XTEST"))?;
        let rect = find(&conn, root, display);
        let keymap = Keymap::read(&conn)?;
        Ok(Self { conn, root, rect, keymap })
    }

    fn try_inject(&mut self, input: &ScreenInput) -> io::Result<()> {
        self.drain_events()?;
        match input {
            ScreenInput::MouseMove { x, y } => self.move_to(*x, *y)?,
            ScreenInput::MouseButton { button, down, x, y, mods } => {
                self.move_to(*x, *y)?;
                // 按下时带着的修饰键（Shift 加点击之类）只在按下那一刻按住。
                let held = if *down { self.modifier_keys(*mods) } else { Vec::new() };
                for &keycode in &held {
                    self.fake(KEY_PRESS, keycode)?;
                }
                self.fake(if *down { BUTTON_PRESS } else { BUTTON_RELEASE }, button_number(*button))?;
                for &keycode in held.iter().rev() {
                    self.fake(KEY_RELEASE, keycode)?;
                }
            }
            ScreenInput::Scroll { dx, dy } => {
                // 正的 `dy` 是内容往下走，就是往上滚（按钮 4）；正的 `dx` 是内容往右走，往左滚（按钮 6）。
                for (delta, positive, negative) in [(*dy, 4u8, 5u8), (*dx, 6, 7)] {
                    let notches = notches(delta);
                    let button = if delta > 0 { positive } else { negative };
                    for _ in 0..notches {
                        self.fake(BUTTON_PRESS, button)?;
                        self.fake(BUTTON_RELEASE, button)?;
                    }
                }
            }
            ScreenInput::Key { key, mods } => {
                let Some(keysym) = keysym::of_key(*key) else { return Ok(()) };
                let held = self.modifier_keys(*mods);
                for &keycode in &held {
                    self.fake(KEY_PRESS, keycode)?;
                }
                // 打不出来也要抬起修饰键，不然之后的输入都带着它。
                let typed = self.type_keysym(keysym);
                for &keycode in held.iter().rev() {
                    self.fake(KEY_RELEASE, keycode)?;
                }
                typed?;
            }
            ScreenInput::Text { text } => {
                for keysym in text.chars().filter_map(keysym::of_char) {
                    self.type_keysym(keysym)?;
                }
            }
        }
        self.conn.flush().map_err(other)
    }

    /// 取空连接上的事件，不然越堆越多；其中有 `MappingNotify`（换了布局，也包括 `type_keysym` 自己临时改的空键）
    /// 时重读键位表，免得按旧表找键。
    fn drain_events(&mut self) -> io::Result<()> {
        let mut remapped = false;
        while let Some(event) = self.conn.poll_for_event().map_err(other)? {
            remapped |= matches!(event, x11rb::protocol::Event::MappingNotify(_));
        }
        if remapped {
            self.keymap = Keymap::read(&self.conn)?;
        }
        Ok(())
    }

    fn move_to(&self, x: f64, y: f64) -> io::Result<()> {
        let axis = |fraction: f64, start: i16, len: u16| {
            (f64::from(start) + fraction.clamp(0.0, 1.0) * f64::from(len.saturating_sub(1))).round() as i16
        };
        let (x, y) = (axis(x, self.rect.x, self.rect.width), axis(y, self.rect.y, self.rect.height));
        self.conn.xtest_fake_input(MOTION_NOTIFY, 0, 0, self.root, x, y, 0).map_err(other)?;
        Ok(())
    }

    fn fake(&self, kind: u8, detail: u8) -> io::Result<()> {
        self.conn.xtest_fake_input(kind, detail, 0, self.root, 0, 0, 0).map_err(other)?;
        Ok(())
    }

    fn modifier_keys(&self, mods: runode_types::screen::ScreenMods) -> Vec<u8> {
        keysym::modifiers(mods)
            .into_iter()
            .filter_map(|keysym| self.keymap.find(keysym).map(|(keycode, _)| keycode))
            .collect()
    }

    /// 按一下打出 `keysym` 的键，要 Shift 时连 Shift 一起按；键位表里没有时临时把一个空键改成它。
    fn type_keysym(&mut self, keysym: u32) -> io::Result<()> {
        if let Some((keycode, shift)) = self.keymap.find(keysym) {
            let shift_key = shift.then(|| self.keymap.find(keysym::SHIFT_L).map(|(keycode, _)| keycode)).flatten();
            if let Some(shift_key) = shift_key {
                self.fake(KEY_PRESS, shift_key)?;
            }
            self.fake(KEY_PRESS, keycode)?;
            self.fake(KEY_RELEASE, keycode)?;
            if let Some(shift_key) = shift_key {
                self.fake(KEY_RELEASE, shift_key)?;
            }
            return Ok(());
        }
        let Some(spare) = self.keymap.spare() else {
            return Err(io::Error::other(format!("no free key to type keysym {keysym:#x}")));
        };
        let per = self.keymap.per_keycode.max(1);
        let mut syms = vec![0u32; usize::from(per)];
        syms[0] = keysym;
        if per > 1 {
            syms[1] = keysym;
        }
        self.conn.change_keyboard_mapping(1, spare, per, &syms).map_err(other)?.check().map_err(other)?;
        thread::sleep(REMAP_SETTLE);
        self.fake(KEY_PRESS, spare)?;
        self.fake(KEY_RELEASE, spare)?;
        self.conn.sync().map_err(other)?;
        thread::sleep(REMAP_SETTLE);
        let blank = vec![0u32; usize::from(per)];
        self.conn.change_keyboard_mapping(1, spare, per, &blank).map_err(other)?;
        Ok(())
    }
}

impl super::Inject for Injector {
    fn inject(&mut self, input: &ScreenInput) {
        if let Err(err) = self.try_inject(input) {
            tracing::debug!("cannot inject {}: {err}", super::input_kind(input));
        }
    }

    fn release(&mut self, button: MouseButton) {
        let _ = self.fake(BUTTON_RELEASE, button_number(button));
        let _ = self.conn.flush();
    }
}

fn button_number(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
    }
}

/// 滚动的点数折成滚轮的格数，至少一格（`Injector::inject` 交来的都是整格）。
fn notches(points: i32) -> u32 {
    if points == 0 { 0 } else { (points.unsigned_abs() / super::POINTS_PER_NOTCH as u32).max(1) }
}

/// 防止屏幕保护和熄屏：每隔一会儿让 X 重置屏幕保护的计时，像有人动了一下。
pub(crate) fn reset_screensaver() -> io::Result<()> {
    let (conn, _) = connect()?;
    conn.force_screen_saver(xproto::ScreenSaver::RESET).map_err(other)?;
    conn.flush().map_err(other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_keys_and_spare_keycodes() {
        // keycode 8：a/A，9：空，10：1/!
        let keymap = Keymap { min: 8, per_keycode: 2, keysyms: vec![0x61, 0x41, 0, 0, 0x31, 0x21] };
        assert_eq!(keymap.find(0x61), Some((8, false)));
        assert_eq!(keymap.find(0x21), Some((10, true)));
        assert_eq!(keymap.find(0x4e2d), None);
        assert_eq!(keymap.spare(), Some(9));
    }

    #[test]
    fn draws_the_cursor_with_premultiplied_alpha() {
        let rect = Rect { x: 0, y: 0, width: 2, height: 1 };
        let mut frame = vec![100u8; 8];
        let cursor = Cursor { x: 1, y: 0, width: 1, height: 1, xhot: 0, yhot: 0, serial: 1, pixels: vec![0xff00_00ff] };
        draw_cursor(&mut frame, rect, &cursor);
        assert_eq!(&frame[..4], &[100, 100, 100, 100]);
        assert_eq!(&frame[4..7], &[255, 0, 0]);
    }

    #[test]
    fn scrolls_at_least_one_notch() {
        assert_eq!(notches(0), 0);
        assert_eq!(notches(5), 1);
        assert_eq!(notches(-100), 2);
    }
}
