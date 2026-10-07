//! Handling OSC (Operating System Command) escape sequences.

use std::{marker::PhantomData, mem::MaybeUninit};

use crate::{
    alloc::{Allocator, Object},
    error::{Result, from_result},
    ffi,
};

/// OSC (Operating System Command) sequence parser and command handling.
///
/// The parser operates in a streaming fashion, processing input byte-by-byte
/// to handle OSC sequences that may arrive in fragments across multiple reads.
/// This interface makes it easy to integrate into most environments and avoids
/// over-allocating buffers.
#[derive(Debug)]
pub struct Parser<'alloc>(Object<'alloc, ffi::OscParserImpl>);

impl<'alloc> Parser<'alloc> {
    /// Create a new OSC parser.
    pub fn new() -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null()) }
    }

    /// Create a new OSC parser with a custom allocator.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(alloc: &'alloc Allocator<'ctx>) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw()) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator) -> Result<Self> {
        let mut raw: ffi::OscParser = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_osc_new(alloc, &raw mut raw) };
        from_result(result)?;
        Ok(Self(Object::new(raw)?))
    }

    /// Reset an OSC parser instance to its initial state.
    ///
    /// Resets the parser state, clearing any partially parsed OSC sequences
    /// and returning the parser to its initial state. This is useful for
    /// reusing a parser instance or recovering from parse errors.
    pub fn reset(&mut self) {
        unsafe { ffi::ghostty_osc_reset(self.0.as_raw()) }
    }

    /// 设置每条未实现编号的 OSC 序列最多保留多少字节。
    ///
    /// 默认值 0 丢弃这类序列，它们产生 [`CommandType::Invalid`]；非零值让它们
    /// 产生 [`CommandType::Unknown`]，携带原始内容，宿主可以自己实现这些
    /// 序列。超过上限的序列仍会报告，只是内容被截断并标记为截断。
    ///
    /// 不超过 2048 字节的上限使用解析器已有的缓冲区，不分配内存；更大的
    /// 上限会为每条未知序列从解析器的分配器分配内存。
    ///
    /// 选项在 [`Parser::reset`] 后保留。正在解析的序列可能沿用旧设置，所以
    /// 最好在解析第一条序列前设置。只有解析器不认识的编号才这样报告；已实现
    /// 的编号内容格式错误时仍是 [`CommandType::Invalid`]。
    pub fn set_unknown_max_bytes(&mut self, max: usize) -> Result<&mut Self> {
        let result = unsafe {
            ffi::ghostty_osc_set(self.0.as_raw(), ffi::OscOption::UNKNOWN_MAX_BYTES, std::ptr::from_ref(&max).cast())
        };
        from_result(result)?;
        Ok(self)
    }

    /// Parse the next byte in an OSC sequence.
    ///
    /// Processes a single byte as part of an OSC sequence. The parser maintains
    /// internal state to track the progress through the sequence. Call this
    /// function for each byte in the sequence data.
    ///
    /// When finished pumping the parser with bytes, call [`Parser::end`] to
    /// get the final result.
    pub fn next_byte(&mut self, byte: u8) {
        unsafe { ffi::ghostty_osc_next(self.0.as_raw(), byte) }
    }

    /// Finalize OSC parsing and retrieve the parsed command.
    ///
    /// Call this after feeding every byte of the sequence to
    /// [`Parser::next_byte`], except the byte that ended it. Pass that byte
    /// here as the terminator: 0x07 for BEL, 0x5C for ST, or 0x18 (CAN) or
    /// 0x1A (SUB) if it was cancelled.
    /// Call [`Parser::reset`] before parsing the next sequence.
    ///
    /// If the sequence is not a valid command, the command has type
    /// [`CommandType::Invalid`].
    ///
    /// Commands that reply to the program, such as color queries, end their
    /// reply the same way the request ended. A terminator of 0x07 (BEL) gets a
    /// BEL reply, and any other byte gets an ST reply. Commands that don't
    /// reply ignore the terminator.
    ///
    /// If the program cancelled the sequence with CAN (0x18) or SUB (0x1A),
    /// pass that byte as the terminator. The sequence is then discarded and
    /// the command has type [`CommandType::Invalid`], whatever command it
    /// contained. This matches xterm.
    ///
    /// ```rust
    /// use libghostty_vt::osc::{CommandType, Parser};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut parser = Parser::new()?;
    ///
    /// // The program sent "ESC ] 2 ; hello" to set the window title, then
    /// // sent CAN instead of a terminator.
    /// for byte in *b"2;hello" {
    ///     parser.next_byte(byte);
    /// }
    /// let command = parser.end(0x18);
    /// // So the window title does not change.
    /// assert!(matches!(command.command_type(), CommandType::Invalid));
    /// # Ok(())
    /// # }
    /// ```
    pub fn end<'p>(&'p mut self, terminator: u8) -> Command<'p, 'alloc> {
        Command {
            // NULL for an invalid or cancelled sequence.
            // `ghostty_osc_command_type` reports that as an invalid command,
            // so data is only ever read from a command of a matched type.
            inner: unsafe { ffi::ghostty_osc_end(self.0.as_raw(), terminator) },
            _parser: PhantomData,
        }
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_osc_free(self.0.as_raw()) }
    }
}

/// A parsed OSC (Operating System Command) command.
///
/// The command can be queried for its type and associated data.
#[derive(Debug)]
pub struct Command<'p, 'alloc> {
    inner: ffi::OscCommand,
    _parser: PhantomData<&'p Parser<'alloc>>,
}

impl<'p> Command<'p, '_> {
    /// Get the type of an OSC command.
    ///
    /// This can be used to determine what kind of command was parsed and
    /// what data might be available from it.
    #[must_use]
    pub fn command_type(self) -> CommandType<'p> {
        self.command_type_inner().unwrap_or(CommandType::Invalid)
    }

    fn command_type_inner(&self) -> Option<CommandType<'p>> {
        use ffi::OscCommandData as Data;
        use ffi::OscCommandType as Type;

        let raw_type = unsafe { ffi::ghostty_osc_command_type(self.inner) };
        Some(match raw_type {
            Type::CHANGE_WINDOW_TITLE => {
                // C 侧输出的是 `const char *`，以前把它当成 `&str`（胖指针）读，
                // 长度槽位是清零的 0，所以标题恒为空串，而且依赖胖指针的内存布局。
                let ptr = self.get::<*const std::ffi::c_char>(Data::CHANGE_WINDOW_TITLE_STR)?;
                if ptr.is_null() {
                    return None;
                }
                // SAFETY: 非空时指向解析器持有的、以 NUL 结尾的字符串，在下一次调用
                // 同一解析器的 ghostty_osc_* 之前有效；`'p` 借用解析器排除了这种调用。
                let title = unsafe { std::ffi::CStr::from_ptr::<'p>(ptr) };
                CommandType::ChangeWindowTitle {
                    // 标题不是合法 UTF-8 时视为无法解析，和其他取值失败一致。
                    title: title.to_str().ok()?,
                }
            }
            Type::CHANGE_WINDOW_ICON => CommandType::ChangeWindowIcon,
            Type::SEMANTIC_PROMPT => CommandType::SemanticPrompt,
            Type::CLIPBOARD_CONTENTS => CommandType::ClipboardContents,
            Type::REPORT_PWD => CommandType::ReportPwd,
            Type::MOUSE_SHAPE => CommandType::MouseShape,
            Type::COLOR_OPERATION => CommandType::ColorOperation,
            Type::KITTY_COLOR_PROTOCOL => CommandType::KittyColorProtocol,
            Type::SHOW_DESKTOP_NOTIFICATION => CommandType::ShowDesktopNotification,
            Type::HYPERLINK_START => CommandType::HyperlinkStart,
            Type::HYPERLINK_END => CommandType::HyperlinkEnd,
            Type::CONEMU_SLEEP => CommandType::ConemuSleep,
            Type::CONEMU_SHOW_MESSAGE_BOX => CommandType::ConemuShowMessageBox,
            Type::CONEMU_CHANGE_TAB_TITLE => CommandType::ConemuChangeTabTitle,
            Type::CONEMU_PROGRESS_REPORT => CommandType::ConemuProgressReport,
            Type::CONEMU_WAIT_INPUT => CommandType::ConemuWaitInput,
            Type::CONEMU_GUIMACRO => CommandType::ConemuGuiMacro,
            Type::CONEMU_RUN_PROCESS => CommandType::ConemuRunProcess,
            Type::CONEMU_OUTPUT_ENVIRONMENT_VARIABLE => CommandType::ConemuOutputEnvironmentVariable,
            Type::CONEMU_XTERM_EMULATION => CommandType::ConemuXtermEmulation,
            Type::CONEMU_COMMENT => CommandType::ConemuComment,
            Type::KITTY_TEXT_SIZING => CommandType::KittyTextSizing,
            Type::KITTY_CLIPBOARD_PROTOCOL => CommandType::KittyClipboardProtocol,
            Type::KITTY_DND_PROTOCOL => CommandType::KittyDndProtocol,
            Type::CONTEXT_SIGNAL => CommandType::ContextSignal,
            Type::KITTY_DESKTOP_NOTIFICATION => CommandType::KittyDesktopNotification,
            Type::UNKNOWN => {
                let content = self.get::<ffi::String>(Data::UNKNOWN_CONTENT)?;
                CommandType::Unknown {
                    // SAFETY: 内容由解析器持有，在下一次调用同一解析器的
                    // ghostty_osc_* 之前有效；`'p` 借用解析器排除了这种调用。
                    content: unsafe { content.to_bytes() },
                    truncated: self.get(Data::UNKNOWN_TRUNCATED)?,
                    terminator: self.get::<ffi::OscTerminator::Type>(Data::UNKNOWN_TERMINATOR)?.try_into().ok()?,
                }
            }

            _ => return None,
        })
    }

    fn get<T>(&self, tag: ffi::OscCommandData::Type) -> Option<T> {
        let mut value = MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_osc_command_data(self.inner, tag, value.as_mut_ptr().cast()) };

        if result {
            // SAFETY: Value should be initialized after successful call.
            Some(unsafe { value.assume_init() })
        } else {
            None
        }
    }
}

/// Type of an OSC command.
///
/// libghostty 会继续增加命令类型，所以这个枚举是 non-exhaustive 的。
#[derive(Debug, Clone, Default)]
#[expect(missing_docs, reason = "missing upstream docs")]
#[non_exhaustive]
pub enum CommandType<'p> {
    #[default]
    Invalid,
    ChangeWindowTitle {
        /// Window title string data.
        title: &'p str,
    },
    ChangeWindowIcon,
    SemanticPrompt,
    ClipboardContents,
    ReportPwd,
    MouseShape,
    ColorOperation,
    KittyColorProtocol,
    ShowDesktopNotification,
    HyperlinkStart,
    HyperlinkEnd,
    ConemuSleep,
    ConemuShowMessageBox,
    ConemuChangeTabTitle,
    ConemuProgressReport,
    ConemuWaitInput,
    ConemuGuiMacro,
    ConemuRunProcess,
    ConemuOutputEnvironmentVariable,
    ConemuXtermEmulation,
    ConemuComment,
    KittyTextSizing,
    KittyClipboardProtocol,
    KittyDndProtocol,
    ContextSignal,
    KittyDesktopNotification,
    /// 编号未被解析器实现的 OSC 序列，只在
    /// [`Parser::set_unknown_max_bytes`] 设了非零上限时产生。
    Unknown {
        /// 传给解析器的全部字节，包括开头的编号。例如
        /// `ESC ] 7400;status=busy BEL` 的内容是 `7400;status=busy`。
        content: &'p [u8],
        /// 序列是否超过上限（或内存不足）而被截断。
        truncated: bool,
        /// 序列的结束方式，由传给 [`Parser::end`] 的终止符决定。回复时请
        /// 用同样的方式结束。
        terminator: Terminator,
    },
}

/// OSC 序列的结束方式。
///
/// 程序可以用两种方式结束 OSC 序列。回复一条序列时，应以程序结束请求的
/// 同样方式结束回复，有些程序只认可匹配的回复。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum Terminator {
    /// 字符串终止符（ST）：ESC 后跟反斜杠（0x1B 0x5C）。
    St = ffi::OscTerminator::ST,
    /// 响铃字符 BEL（0x07）。
    Bel = ffi::OscTerminator::BEL,
}

impl Terminator {
    /// 这种结束方式在线上的字节序列。
    #[must_use]
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::St => b"\x1b\\",
            Self::Bel => b"\x07",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<'p>(parser: &'p mut Parser<'static>, body: &[u8], terminator: u8) -> CommandType<'p> {
        parser.reset();
        for &byte in body {
            parser.next_byte(byte);
        }
        parser.end(terminator).command_type()
    }

    #[test]
    fn unknown_commands_need_a_byte_limit() {
        let mut parser = Parser::new().unwrap();
        assert!(matches!(parse(&mut parser, b"7400;status=busy", 0x07), CommandType::Invalid));

        parser.set_unknown_max_bytes(64).unwrap();
        let CommandType::Unknown { content, truncated, terminator } = parse(&mut parser, b"7400;status=busy", 0x07)
        else {
            panic!("expected an unknown command");
        };
        assert_eq!(content, b"7400;status=busy");
        assert!(!truncated);
        assert_eq!(terminator, Terminator::Bel);

        // ST 结束（传给 end 的是 0x5C），超过上限被截断。
        parser.set_unknown_max_bytes(4).unwrap();
        let CommandType::Unknown { content, truncated, terminator } = parse(&mut parser, b"7400;status=busy", 0x5c)
        else {
            panic!("expected an unknown command");
        };
        assert_eq!(content, b"7400");
        assert!(truncated);
        assert_eq!(terminator, Terminator::St);

        // 已实现的编号不受影响。
        assert!(matches!(parse(&mut parser, b"2;title", 0x07), CommandType::ChangeWindowTitle { title: "title" }));
    }

    #[test]
    fn newer_command_types_are_recognized() {
        let mut parser = Parser::new().unwrap();
        assert!(matches!(
            parse(&mut parser, b"5522;type=read;dGV4dC9wbGFpbg==", 0x07),
            CommandType::KittyClipboardProtocol
        ));
        assert!(matches!(parse(&mut parser, b"99;;hello", 0x07), CommandType::KittyDesktopNotification));
    }

    #[test]
    fn change_window_title_reads_the_title() {
        let mut parser = Parser::new().unwrap();
        for (seq, expected) in
            [(&b"0;hello"[..], "hello"), (b"2;\xe4\xb8\xad\xe6\x96\x87 title", "中文 title"), (b"2;", "")]
        {
            let CommandType::ChangeWindowTitle { title } = parse(&mut parser, seq, 0x07) else {
                panic!("expected a title command for {seq:?}");
            };
            assert_eq!(title, expected);
        }
    }

    #[test]
    fn terminator_bytes() {
        assert_eq!(Terminator::St.as_bytes(), b"\x1b\\");
        assert_eq!(Terminator::Bel.as_bytes(), b"\x07");
    }
}
