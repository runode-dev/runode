//! Utilities for validating paste data safety.
//!
//! 向终端粘贴的推荐方式是 [`Terminal::paste`] 或 [`Terminal::paste_text`]：
//! 粘贴写入 pty 的内容取决于终端状态（括号粘贴模式 2004、Kitty 粘贴事件
//! 模式 5522），由终端按当前模式决定。下面的 [`is_safe`] 和 [`encode`] 是
//! 不依赖终端的基础工具。
//!
//! # Example
//!
//! ## Safety Check
//!
//! ```rust
//! use libghostty_vt::paste;
//!
//! let safe_data = "hello world";
//! let unsafe_data = "rm -rf /\n";
//!
//! if paste::is_safe(safe_data) {
//!     println!("Safe to paste");
//! }
//!
//! if !paste::is_safe(unsafe_data) {
//!     println!("Unsafe! Contains newline");
//! }
//! ```
//!
//! ## Encoding
//!
//! ```rust
//! use libghostty_vt::paste;
//!
//! let mut data = *b"hello\nworld";
//! let mut buf = [0u8; 64];
//!
//! if let Ok(len) = paste::encode(&mut data, true, &mut buf) {
//!     println!("Encoded {len} bytes: {}", buf[..len].escape_ascii());
//! }
//! ```

use std::{any::Any, io::Write, marker::PhantomData};

use crate::{
    error::{Result, from_result, from_result_with_len},
    ffi,
    terminal::{ClipboardLocation, Terminal},
};

/// Check if paste data is safe to paste into the terminal.
///
/// Data is considered unsafe if it contains:
///   * Newlines (`\n`) which can inject commands
///   * The bracketed paste end sequence (`\x1b[201~`) which can be used to exit bracketed paste
///     mode and inject commands
///
/// This check is conservative and considers data unsafe regardless of current terminal state.
#[must_use]
pub fn is_safe(data: &str) -> bool {
    unsafe { ffi::ghostty_paste_is_safe(data.as_ptr().cast(), data.len()) }
}

/// Encode paste data for writing to the terminal pty.
///
/// This function prepares paste data for terminal input by:
///
/// - Stripping unsafe control bytes (NUL, ESC, DEL, etc.) by replacing them
///   with spaces
/// - Wrapping the data in bracketed paste sequences if `bracketed` is true
/// - Replacing newlines with carriage returns if `bracketed` is false
///
/// The input `data` buffer is modified in place during encoding. The encoded
/// result (potentially with bracketed paste prefix/suffix) is written to the
/// output buffer.
///
/// If the output buffer is too small, the function returns
/// `Err(Error::OutOfSpace { required })` where `required` is the required
/// The caller can then retry with a sufficiently sized buffer.
pub fn encode(data: &mut [u8], bracketed: bool, buf: &mut [u8]) -> Result<usize> {
    let mut written = 0usize;
    let result = unsafe {
        ffi::ghostty_paste_encode(
            data.as_mut_ptr().cast(),
            data.len(),
            bracketed,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &raw mut written,
        )
    };
    from_result_with_len(result, written)
}

/// 一次粘贴的来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum PasteSource {
    /// 用户从剪贴板粘贴：快捷键、菜单、鼠标中键。
    #[default]
    Clipboard = ffi::PasteSource::CLIPBOARD,
    /// 以其他方式插入的文本：输入法提交、拖放、脚本输入。总是作为文本写入，
    /// 从不作为粘贴事件，与 kitty 一致。这不是关闭粘贴事件的手段；不想要
    /// 粘贴事件的宿主不安装 [`Terminal::on_clipboard_read`] 即可。
    Text = ffi::PasteSource::TEXT,
}

/// 一次粘贴请求，交给 [`Terminal::paste`]。
///
/// 只描述剪贴板里有哪些 MIME 表示；数据由 [`Terminal::paste`] 的读取闭包
/// 在真正需要时才产生，所以剪贴板里同时有一张大图片也不会有额外开销。
#[derive(Clone, Copy, Debug)]
pub struct PasteRequest<'a> {
    mimes: &'a [&'a str],
    location: ClipboardLocation,
    source: PasteSource,
    allow_unsafe: bool,
}

impl<'a> PasteRequest<'a> {
    /// 创建粘贴请求。`mimes` 是剪贴板现有表示的 MIME 类型，按偏好排序；
    /// 普通粘贴只需 `["text/plain"]`。
    ///
    /// 默认来自标准剪贴板、由用户发起（[`PasteSource::Clipboard`]），且不
    /// 允许可能注入命令的文本。
    #[must_use]
    pub fn new(mimes: &'a [&'a str]) -> Self {
        Self { mimes, location: ClipboardLocation::Standard, source: PasteSource::Clipboard, allow_unsafe: false }
    }

    /// 内容来自哪个剪贴板。只在发送粘贴事件时报告给程序（协议只区分两种，
    /// selection 和 primary 都报告为 primary），对文本粘贴没有影响。
    #[must_use]
    pub fn with_location(mut self, location: ClipboardLocation) -> Self {
        self.location = location;
        self
    }

    /// 这次粘贴为什么发生。
    #[must_use]
    pub fn with_source(mut self, source: PasteSource) -> Self {
        self.source = source;
        self
    }

    /// 是否写入可能注入命令的文本。通常先以 `false` 调用，遇到
    /// [`Error::Rejected`](crate::Error::Rejected) 后征得用户确认，再以
    /// `true` 重试。
    #[must_use]
    pub fn with_allow_unsafe(mut self, allow_unsafe: bool) -> Self {
        self.allow_unsafe = allow_unsafe;
        self
    }
}

/// [`Terminal::paste`] 的读取闭包用来写出一个表示数据的写入器。
///
/// 可以分任意多次写，每次写完 C 侧都不再保留这段数据，所以数据可以从任何
/// 地方流式读出。写入被拒绝时返回错误，闭包应停止并把错误返回。
#[derive(Debug)]
pub struct PasteWriter<'w> {
    raw: ffi::Writer,
    _phan: PhantomData<&'w mut ()>,
}

impl Write for PasteWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        // C 侧写入器约定 len 总大于零，空写什么也不做。
        if buf.is_empty() {
            return Ok(());
        }
        let Some(write) = self.raw.write else {
            return Err(std::io::Error::other("paste writer has no callback"));
        };
        // SAFETY: 写入器只在读取回调期间有效，`'w` 把它限制在回调内；数据只
        // 需活过这次同步调用。
        if unsafe { write(self.raw.userdata, buf.as_ptr(), buf.len()) } {
            Ok(())
        } else {
            Err(std::io::Error::other("paste writer refused the data"))
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// 粘贴相关的方法。
impl Terminal<'_, '_> {
    /// 按终端当前状态把剪贴板内容粘贴进终端。
    ///
    /// - 如果启用了 Kitty 剪贴板协议的粘贴事件（模式 5522，
    ///   [`Mode::PASTE_EVENTS`](crate::terminal::Mode::PASTE_EVENTS)）、粘贴
    ///   由用户发起（[`PasteSource::Clipboard`]）且装了
    ///   [`Terminal::on_clipboard_read`]，终端给程序发送一个列出剪贴板 MIME
    ///   类型的粘贴事件（附一次性密码）而不是数据。程序随后通过剪贴板读取
    ///   回调读它想要的内容。这种情况下不读取任何数据。
    /// - 否则写入第一个文本表示：不安全的控制字节被替换为空格；启用了模式
    ///   2004（[`Mode::BRACKETED_PASTE`](crate::terminal::Mode::BRACKETED_PASTE)）
    ///   时用括号粘贴序列包裹，否则把换行转换为回车。
    ///
    /// `reader` 只在真正粘贴某个表示时被调用，每次粘贴最多一次，参数是请求
    /// 的 MIME 类型（总是 [`PasteRequest::new`] 中的某一项）。它把该表示的
    /// 全部数据写入 [`PasteWriter`]；返回错误会让粘贴以
    /// [`Error::IoError`](crate::Error::IoError) 失败。`reader` 里的 panic
    /// 会在本函数返回前被捕获，再在返回时重新抛出。
    ///
    /// 编码后的字节通过 [`Terminal::on_pty_write`] 分块流出，单次粘贴可能
    /// 调用它多次，各块必须按顺序写入 pty。视口不会滚动，这由宿主决定。
    ///
    /// 成功时返回是否向 pty 写了任何东西；`false` 表示没有可粘贴的内容
    /// （没有非空的文本表示）。
    ///
    /// # Errors
    ///
    /// - [`Error::Rejected`](crate::Error::Rejected)：文本可能注入命令（未
    ///   括号时含换行，括号时含括号粘贴结束序列），且没有允许不安全粘贴。
    ///   此时什么也没写。
    /// - [`Error::InvalidValue`](crate::Error::InvalidValue)：没装
    ///   [`Terminal::on_pty_write`]。
    /// - [`Error::IoError`](crate::Error::IoError)：`reader` 失败，或没有
    ///   安全熵源生成粘贴事件密码。
    ///
    /// 出错时什么也不写。
    pub fn paste<R>(&mut self, request: &PasteRequest<'_>, mut reader: R) -> Result<bool>
    where
        R: FnMut(&str, &mut PasteWriter<'_>) -> std::io::Result<()>,
    {
        struct Context<'a, R> {
            mimes: &'a [&'a str],
            raw_mimes: &'a [ffi::String],
            reader: &'a mut R,
            panic: Option<Box<dyn Any + Send>>,
        }

        unsafe extern "C" fn trampoline<R>(
            userdata: *mut std::ffi::c_void,
            mime: ffi::String,
            writer: ffi::Writer,
        ) -> bool
        where
            R: FnMut(&str, &mut PasteWriter<'_>) -> std::io::Result<()>,
        {
            // SAFETY: userdata 指向下面 `paste` 栈帧上的 Context，回调在
            // `ghostty_terminal_paste` 内同步发生，期间没有别的引用访问它。
            let ctx = unsafe { &mut *userdata.cast::<Context<'_, R>>() };
            // C 侧保证传回的 MIME 就是 mimes 数组里的某一项（同一指针和长度），
            // 所以按指针找回原来的 `&str`，不必重新校验 UTF-8。
            let Some(index) = ctx.raw_mimes.iter().position(|raw| raw.ptr == mime.ptr && raw.len == mime.len) else {
                return false;
            };
            let mime = ctx.mimes[index];
            let mut writer = PasteWriter { raw: writer, _phan: PhantomData };
            // panic 不能穿过 `extern "C"` 边界，否则进程会 abort。先捕获下来，
            // 等 C 函数返回后再重新抛出。
            let reader = &mut *ctx.reader;
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader(mime, &mut writer))) {
                Ok(result) => result.is_ok(),
                Err(payload) => {
                    ctx.panic = Some(payload);
                    false
                }
            }
        }

        self.touch();
        let raw_mimes: Vec<ffi::String> = request.mimes.iter().map(|mime| ffi::String::from(*mime)).collect();
        let mut ctx = Context { mimes: request.mimes, raw_mimes: &raw_mimes, reader: &mut reader, panic: None };
        let raw = ffi::Paste {
            location: request.location.into(),
            source: request.source.into(),
            // mimes_len 为零时允许 NULL，表示没有东西可粘贴。
            mimes: if raw_mimes.is_empty() { std::ptr::null() } else { raw_mimes.as_ptr() },
            mimes_len: raw_mimes.len(),
            reader: ffi::MimeReader { read: Some(trampoline::<R>), userdata: (&raw mut ctx).cast() },
            allow_unsafe: request.allow_unsafe,
            ..ffi::sized!(ffi::Paste)
        };
        let mut written = false;
        // SAFETY: 请求、MIME 数组和 Context 都活过这次同步调用。`&mut self`
        // 保证粘贴过程中触发的 write_pty 回调可以安全地分发。
        let result = unsafe { ffi::ghostty_terminal_paste(self.inner.as_raw(), &raw const raw, &raw mut written) };
        if let Some(payload) = ctx.panic.take() {
            std::panic::resume_unwind(payload);
        }
        from_result(result)?;
        Ok(written)
    }

    /// 粘贴一段文本，是 [`Terminal::paste`] 在只有 `text/plain` 时的简写。
    ///
    /// 语义与错误同 [`Terminal::paste`]。
    pub fn paste_text(&mut self, text: &str, source: PasteSource, allow_unsafe: bool) -> Result<bool> {
        self.touch();
        let request = PasteRequest::new(&["text/plain"]).with_source(source).with_allow_unsafe(allow_unsafe);
        self.paste(&request, |_mime, writer| writer.write_all(text.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::{Error, terminal::Mode};

    /// 建一个把 pty 输出收集到 `pty` 的终端。
    fn terminal_with_pty(pty: &RefCell<Vec<u8>>) -> Terminal<'static, '_> {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.on_pty_write(|_, data| pty.borrow_mut().extend_from_slice(data)).unwrap();
        terminal
    }

    #[test]
    fn bracketed_paste_wraps_text() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        terminal.vt_write(b"\x1b[?2004h");
        assert!(terminal.mode(Mode::BRACKETED_PASTE).unwrap());
        // 括号粘贴里换行是安全的，不需要确认。
        assert!(terminal.paste_text("a\nb", PasteSource::Clipboard, false).unwrap());
        assert_eq!(pty.take(), b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn unbracketed_newline_needs_confirmation() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        assert!(matches!(terminal.paste_text("rm -rf /\n", PasteSource::Clipboard, false), Err(Error::Rejected)));
        assert!(pty.borrow().is_empty(), "a rejected paste writes nothing");

        // 用户确认后重试：换行被转换为回车，控制字节被替换为空格。
        assert!(terminal.paste_text("a\x1bb\nc", PasteSource::Clipboard, true).unwrap());
        assert_eq!(pty.take(), b"a b\rc");
    }

    #[test]
    fn paste_without_pty_writer_is_invalid() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        assert!(matches!(terminal.paste_text("hi", PasteSource::Text, false), Err(Error::InvalidValue)));
    }

    #[test]
    fn reader_only_produces_the_pasted_text_representation() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        let mut requested = Vec::new();
        let request = PasteRequest::new(&["image/png", "text/plain"]);
        let written = terminal
            .paste(&request, |mime, writer| {
                requested.push(mime.to_owned());
                // 分块写也可以。
                writer.write_all(b"hel")?;
                writer.write_all(b"")?;
                writer.write_all(b"lo")
            })
            .unwrap();
        assert!(written);
        assert_eq!(requested, ["text/plain"]);
        assert_eq!(pty.take(), b"hello");
    }

    #[test]
    fn nothing_to_paste() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        // 没有表示，或者文本表示为空，都返回 false 且不写任何东西。
        assert!(!terminal.paste(&PasteRequest::new(&[]), |_, _| unreachable!()).unwrap());
        assert!(!terminal.paste_text("", PasteSource::Clipboard, false).unwrap());
        assert!(pty.borrow().is_empty());
    }

    #[test]
    fn reader_errors_fail_the_paste() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        let result = terminal.paste(&PasteRequest::new(&["text/plain"]), |_, writer| {
            writer.write_all(b"partial")?;
            Err(std::io::Error::other("clipboard went away"))
        });
        assert!(matches!(result, Err(Error::IoError)));
        assert!(pty.borrow().is_empty(), "a failed paste writes nothing");
    }

    /// reader 里的 panic 不能穿过 `extern "C"`（会 abort），而是在 `paste`
    /// 返回时重新抛出。
    #[test]
    fn reader_panics_resume_after_the_call() {
        let pty = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            terminal.paste(&PasteRequest::new(&["text/plain"]), |_, _| {
                panic!("reader panicked");
            })
        }));
        let payload = result.expect_err("the panic must propagate");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"reader panicked"));
        // 终端仍然可用。
        assert!(terminal.paste_text("ok", PasteSource::Text, false).unwrap());
        assert_eq!(pty.take(), b"ok");
    }

    /// 模式 5522 开启、装了剪贴板读取回调、且是用户发起的粘贴时，终端发送
    /// 粘贴事件而不是文本，也不读取数据；程序随后的读取带着授权到达。
    #[test]
    fn paste_events_replace_the_text() {
        // 粘贴事件要用全局随机源生成密码，别与替换随机源的测试并行。
        let _guard = crate::sys::TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pty = RefCell::new(Vec::new());
        let granted = RefCell::new(Vec::new());
        let mut terminal = terminal_with_pty(&pty);
        terminal
            .on_clipboard_read(|_, read| {
                granted.borrow_mut().push(read.granted());
                read.reply(Err(crate::terminal::ClipboardReadError::Denied));
            })
            .unwrap();
        terminal.vt_write(b"\x1b[?5522h");
        assert!(terminal.mode(Mode::PASTE_EVENTS).unwrap());

        let request = PasteRequest::new(&["text/plain"]);
        assert!(terminal.paste(&request, |_, _| unreachable!("a paste event reads no data")).unwrap());
        let event = String::from_utf8(pty.take()).unwrap();
        assert!(event.starts_with("\x1b]5522;type=read:status=OK:pw="), "{event:?}");

        // 非用户发起的粘贴仍作为文本写入。
        assert!(terminal.paste_text("typed", PasteSource::Text, false).unwrap());
        assert_eq!(pty.take(), b"typed");

        // 程序用事件里的一次性密码读取，回调看到 granted。协议规定没有程序名
        // 的密码视同没有密码，所以必须带上 name。
        let pw = event.trim_start_matches("\x1b]5522;type=read:status=OK:pw=").split('\x1b').next().unwrap().to_owned();
        terminal.vt_write(format!("\x1b]5522;type=read:pw={pw}:name=YXBw;dGV4dC9wbGFpbg==\x1b\\").as_bytes());
        assert_eq!(*granted.borrow(), [true]);
    }
}
