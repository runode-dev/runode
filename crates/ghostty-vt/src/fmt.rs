//! Format terminal content as plain text, VT sequences, or HTML.
//!
//! A formatter captures a reference to a terminal and formatting options.
//! It can be used repeatedly to produce output that reflects the current
//! terminal state at the time of each format call.
use std::marker::PhantomData;

use crate::{
    alloc::{Allocator, Bytes, Object},
    error::{Error, Result, from_result},
    ffi,
    selection::Selection,
    terminal::Terminal,
};

/// Formatter that formats terminal content.
#[derive(Debug)]
pub struct Formatter<'t, 'alloc: 'cb, 'cb: 't> {
    inner: Object<'alloc, ffi::FormatterImpl>,
    _terminal: PhantomData<&'t Terminal<'alloc, 'cb>>,
}

/// Options for [creating a terminal formatter](Formatter::new).
#[derive(Debug)]
pub struct FormatterOptions<'t, 's> {
    inner: ffi::FormatterTerminalOptions,
    _phan: PhantomData<&'s Selection<'t>>,
}
impl<'t, 's> FormatterOptions<'t, 's> {
    /// Create a new set of options for [creating a terminal formatter](Formatter::new).
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: ffi::FormatterTerminalOptions {
                extra: ffi::FormatterTerminalExtra {
                    screen: ffi::FormatterScreenExtra { ..ffi::sized!(ffi::FormatterScreenExtra) },
                    ..ffi::sized!(ffi::FormatterTerminalExtra)
                },
                ..ffi::sized!(ffi::FormatterTerminalOptions)
            },
            _phan: PhantomData,
        }
    }
    /// Specify the output format to emit.
    #[must_use]
    pub fn with_format(mut self, value: Format) -> Self {
        self.inner.emit = value.into();
        self
    }
    /// Specify whether to unwrap soft-wrapped lines.
    #[must_use]
    pub fn with_unwrap(mut self, value: bool) -> Self {
        self.inner.unwrap = value;
        self
    }
    /// Specify whether to trim trailing whitespace on non-blank lines.
    #[must_use]
    pub fn with_trim(mut self, value: bool) -> Self {
        self.inner.trim = value;
        self
    }
    /// Specify the selection to restrict output to a range.
    ///
    /// If a selection is not given, the formatter defaults to formatting
    /// the entire screen.
    #[must_use]
    pub fn with_selection(mut self, value: &'s Selection<'t>) -> Self {
        self.inner.selection = &raw const value.inner;
        self
    }

    // --- Extra settings --- //

    /// Specify whether to emit the palette using OSC 4 sequences.
    #[must_use]
    pub fn with_palette(mut self, value: bool) -> Self {
        self.inner.extra.palette = value;
        self
    }
    /// Specify terminal modes that differ from their defaults using CSI h/l.
    #[must_use]
    pub fn with_modes(mut self, value: bool) -> Self {
        self.inner.extra.modes = value;
        self
    }
    /// Specify whether to emit scrolling region state using DECSTBM and DECSLRM sequences.
    #[must_use]
    pub fn with_scrolling_region(mut self, value: bool) -> Self {
        self.inner.extra.scrolling_region = value;
        self
    }
    /// Specify tabstop positions by clearing all tabs and setting each one.
    #[must_use]
    pub fn with_tabstops(mut self, value: bool) -> Self {
        self.inner.extra.tabstops = value;
        self
    }
    /// Specify the present working directory using OSC 7.
    #[must_use]
    pub fn with_pwd(mut self, value: bool) -> Self {
        self.inner.extra.pwd = value;
        self
    }
    /// Specify keyboard modes such as `ModifyOtherKeys`.
    #[must_use]
    pub fn with_keyboard(mut self, value: bool) -> Self {
        self.inner.extra.keyboard = value;
        self
    }

    // --- Screen settings --- //

    /// Specify whether to emit cursor position using CUP (CSI H).
    #[must_use]
    pub fn with_cursor(mut self, value: bool) -> Self {
        self.inner.extra.screen.cursor = value;
        self
    }
    /// Emit current SGR style state based on the cursor's active `style_id`.
    #[must_use]
    pub fn with_style(mut self, value: bool) -> Self {
        self.inner.extra.screen.style = value;
        self
    }
    /// Emit current hyperlink state using OSC 8 sequences.
    #[must_use]
    pub fn with_hyperlink(mut self, value: bool) -> Self {
        self.inner.extra.screen.hyperlink = value;
        self
    }
    /// Emit character protection mode using DECSCA.
    #[must_use]
    pub fn with_protection(mut self, value: bool) -> Self {
        self.inner.extra.screen.protection = value;
        self
    }
    /// Emit Kitty keyboard protocol state using CSI > u and CSI = sequences.
    #[must_use]
    pub fn with_kitty_keyboard(mut self, value: bool) -> Self {
        self.inner.extra.screen.kitty_keyboard = value;
        self
    }
    /// Emit character set designations and invocations.
    #[must_use]
    pub fn with_charsets(mut self, value: bool) -> Self {
        self.inner.extra.screen.charsets = value;
        self
    }
}

impl<'t, 'alloc: 'cb, 'cb: 't> Formatter<'t, 'alloc, 'cb> {
    /// Create a formatter for a terminal's active screen.
    pub fn new(terminal: &'t Terminal<'alloc, 'cb>, opts: FormatterOptions<'t, '_>) -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null(), terminal, opts) }
    }

    /// Create a formatter for a terminal's active screen.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(
        alloc: &'alloc Allocator<'ctx>,
        terminal: &'t Terminal<'alloc, 'cb>,
        opts: FormatterOptions,
    ) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw(), terminal, opts) }
    }

    unsafe fn new_inner(
        alloc: *const ffi::Allocator,
        terminal: &'t Terminal<'alloc, 'cb>,
        opts: FormatterOptions,
    ) -> Result<Self> {
        let mut raw: ffi::Formatter = std::ptr::null_mut();

        let result =
            unsafe { ffi::ghostty_formatter_terminal_new(alloc, &raw mut raw, terminal.inner.as_raw(), opts.inner) };
        from_result(result)?;

        Ok(Self { inner: Object::new(raw)?, _terminal: PhantomData })
    }

    /// Run the formatter and return an allocated buffer with the output.
    ///
    /// Each call formats the current terminal state. The buffer is allocated
    /// using the provided allocator (or the default allocator if `None`).
    /// Empty output returns an empty buffer.
    pub fn format_alloc<'a, 'ctx: 'a>(&mut self, alloc: Option<&'a Allocator<'ctx>>) -> Result<Bytes<'a>> {
        let alloc = if let Some(alloc) = alloc { alloc.to_raw() } else { std::ptr::null() };

        let mut bytes = std::ptr::null_mut();
        let mut len = 0usize;
        let result = unsafe {
            ffi::ghostty_formatter_format_alloc(
                self.inner.as_raw(),
                alloc,
                std::ptr::from_mut(&mut bytes),
                std::ptr::from_mut(&mut len),
            )
        };
        from_result(result)?;

        // SAFETY: On success, libghostty hands over `len` bytes allocated
        // with `alloc`, or NULL for empty output.
        Ok(unsafe { Bytes::from_raw_parts(bytes, len, alloc) })
    }

    /// Run the formatter and produce output into the caller-provided buffer.
    ///
    /// Each call formats the current terminal state. If the buffer is too small,
    /// returns `Err(Error::OutOfSpace { required })` where `required` is the
    /// required size. The caller can then retry with a larger buffer.
    pub fn format_buf(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut len = 0usize;
        let result = unsafe {
            ffi::ghostty_formatter_format_buf(
                self.inner.as_raw(),
                std::ptr::from_mut(buf).cast(),
                buf.len(),
                std::ptr::from_mut(&mut len),
            )
        };
        from_result(result)?;
        Ok(len)
    }

    /// 运行格式化器，把输出追加到 `out` 末尾，返回追加的字节数。
    ///
    /// 与 [`Formatter::format_alloc`] 输出相同，但直接写进调用方的 `Vec`，
    /// 可以每次复用同一块分配；也不必像 [`Formatter::format_buf`] 那样先查询
    /// 大小。出错时 `out` 可能已追加了部分输出。
    pub fn format_into(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let start = out.len();
        // SAFETY: `Vec<u8>` 的写入只操作它自己的内存，不会调用任何格式化器或
        // 终端 API。
        unsafe { self.format_write(out) }?;
        Ok(out.len() - start)
    }

    /// 运行格式化器，把输出流式写入 `writer`。
    ///
    /// 每次调用都格式化终端的当前状态，输出产生时同步写入 `writer`，可能分
    /// 多次写。出错时 `writer` 可能已收到部分输出，且无法从中断处继续。不会
    /// flush `writer`。
    ///
    /// 与 [`Formatter::format_buf`]/[`Formatter::format_alloc`] 的区别是不需要
    /// 一块容纳全部输出的缓冲区：大的回滚区可以边格式化边写出。只需要一块
    /// 可复用的内存时，用安全的 [`Formatter::format_into`]。
    ///
    /// # Errors
    ///
    /// `writer` 拒绝写入时返回 [`Error::IoError`]，输出计数溢出时返回
    /// [`Error::LimitExceeded`]。
    ///
    /// # Safety
    ///
    /// `writer` 在格式化进行中被调用，此时不得调用这个格式化器或它所格式化
    /// 的终端上的任何 API（C 头文件的要求）。格式化器只持有终端的共享借用，
    /// 类型系统无法阻止 `writer` 也持有这个终端（例如经由线程局部变量），
    /// 而有些读取 API 会在内部改动终端存储（如访问被压缩的回滚区时解压），
    /// 可能让进行中的格式化失效，所以这条约束只能由调用方保证。
    pub unsafe fn format_write<W: std::io::Write>(&mut self, writer: &mut W) -> Result<()> {
        let writer = crate::io::to_writer(writer);
        let result = unsafe { ffi::ghostty_formatter_format(self.inner.as_raw(), writer) };
        from_result(result)
    }

    /// Query the required buffer size for the formatted output.
    ///
    /// The result can be used to create a sufficiently large buffer
    /// for [`Formatter::format_buf`].
    pub fn format_len(&mut self) -> Result<usize> {
        let mut len = 0usize;
        let result = unsafe {
            ffi::ghostty_formatter_format_buf(
                self.inner.as_raw(),
                std::ptr::null_mut(),
                0,
                std::ptr::from_mut(&mut len),
            )
        };
        // This should always fail with OutOfSpace.
        match from_result(result) {
            Err(Error::OutOfSpace { .. }) => Ok(len),
            Err(e) => Err(e),
            Ok(()) => Err(Error::InvalidValue),
        }
    }
}

impl Drop for Formatter<'_, '_, '_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_formatter_free(self.inner.as_raw()) }
    }
}

/// Output format.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, int_enum::IntEnum)]
pub enum Format {
    /// Plain text (no escape sequences).
    Plain = ffi::FormatterFormat::PLAIN,
    /// VT sequences preserving colors, styles, URLs, etc.
    Vt = ffi::FormatterFormat::VT,
    /// HTML with inline styles.
    Html = ffi::FormatterFormat::HTML,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_into_matches_format_alloc() {
        let mut terminal = Terminal::new(20, 4).unwrap();
        terminal.vt_write(b"hello\r\n\x1b[1mworld\x1b[0m");
        for format in [Format::Plain, Format::Vt, Format::Html] {
            let mut formatter = Formatter::new(&terminal, FormatterOptions::new().with_format(format)).unwrap();
            let expected = formatter.format_alloc(None).unwrap().to_vec();
            // 追加到已有内容之后，返回追加的字节数。
            let mut out = b"prefix".to_vec();
            let written = formatter.format_into(&mut out).unwrap();
            assert_eq!(written, expected.len());
            assert_eq!(&out[6..], expected);
        }
    }

    #[test]
    fn format_write_reports_writer_errors() {
        struct Refuse;
        impl std::io::Write for Refuse {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("refused"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut terminal = Terminal::new(20, 4).unwrap();
        terminal.vt_write(b"hello");
        let mut formatter = Formatter::new(&terminal, FormatterOptions::new()).unwrap();
        // SAFETY: 写入器不调用任何格式化器或终端 API。
        let result = unsafe { formatter.format_write(&mut Refuse) };
        assert!(matches!(result, Err(Error::IoError)));
    }
}
