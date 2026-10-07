//! 批量读取：一次 FFI 调用读取同一句柄的多个数据字段。
//!
//! C API 为每类句柄提供了 `*_get_multi`，用一组 key 和一组输出指针一次读出
//! 多个字段，省去逐个 getter 的 FFI 往返。C 侧要求每个输出指针的类型与 key
//! 文档里的输出类型一致，传错就是未定义行为。这里把每个 key 做成一个零大小
//! 的标记类型，由类型系统把 key 和它的输出类型绑在一起，调用方无从配错：
//!
//! ```rust
//! use libghostty_vt::{Terminal, terminal::query};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut terminal = Terminal::new(80, 24)?;
//! terminal.vt_write(b"hello");
//! let (cols, rows, x, title) = terminal.get_multi((
//!     query::Cols,
//!     query::Rows,
//!     query::CursorX,
//!     query::Title,
//! ))?;
//! assert_eq!((cols, rows, x, title), (80, 24, 5, ""));
//! # Ok(())
//! # }
//! ```
//!
//! key 以元组传入（1 到 16 个），结果以同样顺序的元组返回。每个 key 的输出
//! 类型与对应的单个 getter 一致：例如单个 getter 返回 `Option` 的字段，批量
//! 读取也返回 `Option`。
//!
//! # 缺失值
//!
//! C 侧遇到第一个失败的 key 就停下，并报告它的位置。有些 key 用错误码表示
//! “没有值”（例如未设置的颜色返回 `GHOSTTY_NO_VALUE`，单元格没有前景色时
//! 返回 `GHOSTTY_INVALID_VALUE`），对应的输出是 `Option`。遇到这种情况时，
//! 这里把该 key 记为 `None`，再从下一个 key 继续发起一次调用，所以结果与
//! 逐个调用 getter 一致，只是可能多花几次 FFI 调用。要让常见情况只用一次
//! 调用，把可能缺失的 key 放在元组末尾。
//!
//! 需要调用方提供缓冲区的字段（如字形簇的 UTF-8 编码、搜索的匹配列表）和
//! 需要输入参数的字段（如 [`Terminal::mode`](crate::Terminal::mode)）不在
//! 批量读取的范围内，请用对应的方法。渲染循环里逐单元格读取的常见组合见
//! [`CellIteration::read`](crate::render::CellIteration::read)。

use std::{ffi::c_void, os::raw::c_int};

use crate::{
    error::{Error, Result, from_result},
    ffi,
};

pub(crate) mod sealed {
    /// 阻止 crate 外实现 [`Key`](super::Key) 与 [`Keys`](super::Keys)：
    /// 每个 key 的输出类型必须与 C 头文件一致，只能由本 crate 保证。
    pub trait Sealed<D> {}
}

/// 区分各类句柄的 key 集合的标记类型，只出现在类型参数里。
pub mod domain {
    /// [`Terminal`](crate::Terminal) 的 key，见 [`crate::terminal::query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum Terminal {}
    /// 渲染状态快照的 key，见 [`crate::render::query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum RenderState {}
    /// 渲染状态行迭代的 key，见 [`crate::render::row_query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum RenderRow {}
    /// 渲染状态单元格迭代的 key，见 [`crate::render::cell_query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum RenderCell {}
    /// [`Row`](crate::screen::Row) 的 key，见 [`crate::screen::row_query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum Row {}
    /// [`Cell`](crate::screen::Cell) 的 key，见 [`crate::screen::cell_query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum Cell {}
    /// Kitty 图形图片的 key，见 `kitty::graphics::image_query`。
    #[derive(Clone, Copy, Debug)]
    pub enum KittyImage {}
    /// Kitty 图形放置的 key，见 `kitty::graphics::placement_query`。
    #[derive(Clone, Copy, Debug)]
    pub enum KittyPlacement {}
    /// [`Search`](crate::search::Search) 的 key，见 [`crate::search::query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum Search {}
    /// 选区手势的 key，见 [`crate::selection::gesture::query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum SelectionGesture {}
    /// 快照解码器的 key，见 [`crate::snapshot::decoder_query`]。
    #[derive(Clone, Copy, Debug)]
    pub enum SnapshotDecoder {}
}

/// 句柄类别 `D` 的一个数据字段，以及读取它得到的值的类型。
///
/// 只由本 crate 实现。
pub trait Key<D>: sealed::Sealed<D> + Copy {
    /// C 侧写入的存储类型，与 C 头文件为这个 key 规定的输出类型一致。
    #[doc(hidden)]
    type Raw;
    /// 读到的值。`'a` 是被读句柄的借用，借用型的值（如终端标题）受它约束。
    type Output<'a>;
    /// C 侧的 key 值。
    #[doc(hidden)]
    const TAG: c_int;
    /// 调用前的存储初值；sized 结构体在这里设好 `size`。
    #[doc(hidden)]
    fn raw() -> Self::Raw;
    /// 这个错误码对该 key 是否表示“没有值”。
    #[doc(hidden)]
    fn absent(code: ffi::Result::Type) -> bool;
    /// 把 C 写入的存储转换为输出。`present` 为 `false` 时 C 没有写入，
    /// `raw` 仍是 [`Key::raw`] 的初值。
    ///
    /// # Safety
    ///
    /// `raw` 必须是这个 key 成功读取（或判定缺失）后的存储；借用型输出要求
    /// 调用方把 `'a` 绑定到被读句柄的借用上。
    #[doc(hidden)]
    unsafe fn convert<'a>(raw: Self::Raw, present: bool) -> Result<Self::Output<'a>>;
}

/// 句柄类别 `D` 的一组 key，即 [`Key`] 的元组（1 到 16 个）。
///
/// 只由本 crate 实现。别的句柄的 key 无法传入，编译期就会报错：
///
/// ```compile_fail,E0277
/// use libghostty_vt::{Terminal, render::cell_query};
///
/// let terminal = Terminal::new(80, 24).unwrap();
/// // 单元格的 key 不能用来读终端。
/// terminal.get_multi((cell_query::Raw,));
/// ```
pub trait Keys<D>: sealed::Sealed<D> {
    /// 与 key 顺序一致的结果元组。
    type Output<'a>;

    /// 读取全部 key。`call` 用给定的 key 和输出指针调用一次对应的
    /// `*_get_multi`，并通过最后一个参数报告成功写入的个数。
    ///
    /// # Safety
    ///
    /// `call` 必须把参数原样交给与 `D` 对应的 `*_get_multi`；调用方负责把
    /// `'a` 绑定到被读句柄的借用上。
    #[doc(hidden)]
    unsafe fn read<'a>(self, call: &mut Call<'_>) -> Result<Self::Output<'a>>;
}

/// 对一个句柄调用一次 `*_get_multi`。
#[doc(hidden)]
pub type Call<'c> = dyn FnMut(&[c_int], &mut [*mut c_void], &mut usize) -> ffi::Result::Type + 'c;

/// 驱动一次批量读取：遇到表示“没有值”的错误码时记下缺失，从下一个 key
/// 继续；遇到其他错误就返回。
fn run<const N: usize>(
    tags: &[c_int; N],
    ptrs: &mut [*mut c_void; N],
    absent: &[fn(ffi::Result::Type) -> bool; N],
    call: &mut Call<'_>,
) -> Result<[bool; N]> {
    let mut present = [true; N];
    let mut start = 0;
    while start < N {
        let mut written = 0usize;
        let code = call(&tags[start..], &mut ptrs[start..], &mut written);
        if code == ffi::Result::SUCCESS {
            break;
        }
        // 出错时 C 侧把 `written` 设为失败 key 的下标（相对本次调用）。
        let failed = start + written;
        if failed < N && absent[failed](code) {
            present[failed] = false;
            start = failed + 1;
            continue;
        }
        return Err(from_result(code).err().unwrap_or(Error::InvalidValue));
    }
    Ok(present)
}

macro_rules! impl_keys {
    ($n:literal; $($K:ident $i:tt),+) => {
        impl<D, $($K: Key<D>),+> sealed::Sealed<D> for ($($K,)+) {}

        impl<D, $($K: Key<D>),+> Keys<D> for ($($K,)+) {
            type Output<'a> = ($($K::Output<'a>,)+);

            unsafe fn read<'a>(self, call: &mut Call<'_>) -> Result<Self::Output<'a>> {
                let mut raws = ($($K::raw(),)+);
                let tags = [$($K::TAG),+];
                let absent: [fn(ffi::Result::Type) -> bool; $n] = [$($K::absent),+];
                // 输出指针指向上面元组里各自的存储，C 按 key 写入对应类型。
                let mut ptrs: [*mut c_void; $n] = [$((&raw mut raws.$i).cast()),+];
                let present = run(&tags, &mut ptrs, &absent, call)?;
                // SAFETY: 每个存储都由对应 key 的读取写入（或判定缺失），
                // `'a` 由调用方绑定。
                Ok(($(unsafe { $K::convert(raws.$i, present[$i]) }?,)+))
            }
        }
    };
}

impl_keys!(1; A 0);
impl_keys!(2; A 0, B 1);
impl_keys!(3; A 0, B 1, C 2);
impl_keys!(4; A 0, B 1, C 2, E 3);
impl_keys!(5; A 0, B 1, C 2, E 3, F 4);
impl_keys!(6; A 0, B 1, C 2, E 3, F 4, G 5);
impl_keys!(7; A 0, B 1, C 2, E 3, F 4, G 5, H 6);
impl_keys!(8; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7);
impl_keys!(9; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8);
impl_keys!(10; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9);
impl_keys!(11; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9, L 10);
impl_keys!(12; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9, L 10, M 11);
impl_keys!(13; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9, L 10, M 11, N 12);
impl_keys!(14; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9, L 10, M 11, N 12, O 13);
impl_keys!(15; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9, L 10, M 11, N 12, O 13, P 14);
impl_keys!(16; A 0, B 1, C 2, E 3, F 4, G 5, H 6, I 7, J 8, K 9, L 10, M 11, N 12, O 13, P 14, Q 15);

/// 全零的存储初值。
///
/// 只用于 C 输出类型：整数、浮点、`bool`、C 枚举（`c_int`）以及由它们组成
/// 的 FFI 结构体和数组，全零对它们都是合法值。用它而不是 `Default`，是因为
/// 长度超过 32 的数组（如 256 色调色板）没有实现 `Default`。
///
/// # Safety
///
/// `T` 必须是全零为合法值的类型，即上述 C 输出类型。
pub(crate) unsafe fn zeroed<T>() -> T {
    // SAFETY: 由调用方保证。
    unsafe { std::mem::zeroed() }
}

/// 把 C 的存储值转换为对外的值；转换失败（例如未知的枚举值）视为
/// [`Error::InvalidValue`]，与单个 getter 的处理一致。
pub(crate) fn try_convert<R, T: TryFrom<R>>(raw: R) -> Result<T> {
    T::try_from(raw).map_err(|_| Error::InvalidValue)
}

/// 声明一个 key 标记类型并实现 [`Key`]。
///
/// 规格（`$spec`）有四种：
///
/// - `copy T`：C 写入 `T`，原样返回。
/// - `try R => T`：C 写入 `R`，经 `TryFrom` 转换为 `T`。
/// - `opt[CODE, ...] R => T`：同上，但给定错误码表示没有值，输出
///   `Option<T>`。
/// - `sized R => T`：`R` 是 sized 结构体，调用前设好 `size`，经 `TryFrom`
///   转换为 `T`。
///
/// 借用型或需要特殊转换的 key 直接调用 [`multi_key_full!`]。
macro_rules! multi_keys {
    (domain = $D:ty, tags = $($T:ident)::+;) => {};

    (domain = $D:ty, tags = $($T:ident)::+;
        $(#[$m:meta])* $name:ident = $tag:ident: copy $ty:ty; $($rest:tt)*) => {
        $crate::multi::multi_key_full! {
            $(#[$m])* $name in $D = $($T)::+::$tag;
            raw $ty = unsafe { $crate::multi::zeroed() };
            out<'a> $ty;
            absent [];
            convert |raw, _present| Ok(raw)
        }
        $crate::multi::multi_keys!(domain = $D, tags = $($T)::+; $($rest)*);
    };

    (domain = $D:ty, tags = $($T:ident)::+;
        $(#[$m:meta])* $name:ident = $tag:ident: try $raw:ty => $out:ty; $($rest:tt)*) => {
        $crate::multi::multi_key_full! {
            $(#[$m])* $name in $D = $($T)::+::$tag;
            raw $raw = unsafe { $crate::multi::zeroed() };
            out<'a> $out;
            absent [];
            convert |raw, _present| $crate::multi::try_convert(raw)
        }
        $crate::multi::multi_keys!(domain = $D, tags = $($T)::+; $($rest)*);
    };

    (domain = $D:ty, tags = $($T:ident)::+;
        $(#[$m:meta])* $name:ident = $tag:ident: opt[$($code:ident),+] $raw:ty => $out:ty; $($rest:tt)*) => {
        $crate::multi::multi_key_full! {
            $(#[$m])* $name in $D = $($T)::+::$tag;
            raw $raw = unsafe { $crate::multi::zeroed() };
            out<'a> Option<$out>;
            absent [$($crate::ffi::Result::$code),+];
            convert |raw, present| if present {
                $crate::multi::try_convert(raw).map(Some)
            } else {
                Ok(None)
            }
        }
        $crate::multi::multi_keys!(domain = $D, tags = $($T)::+; $($rest)*);
    };

    (domain = $D:ty, tags = $($T:ident)::+;
        $(#[$m:meta])* $name:ident = $tag:ident: sized $raw:ty => $out:ty; $($rest:tt)*) => {
        $crate::multi::multi_key_full! {
            $(#[$m])* $name in $D = $($T)::+::$tag;
            raw $raw = $crate::ffi::sized!($raw);
            out<'a> $out;
            absent [];
            convert |raw, _present| $crate::multi::try_convert(raw)
        }
        $crate::multi::multi_keys!(domain = $D, tags = $($T)::+; $($rest)*);
    };
}

/// 以完整规格声明一个 key 标记类型并实现 [`Key`]。
macro_rules! multi_key_full {
    (
        $(#[$m:meta])* $name:ident in $D:ty = $tag:expr;
        raw $raw_ty:ty = $init:expr;
        out<$a:lifetime> $out:ty;
        absent [$($code:expr),*];
        convert |$raw:ident, $present:ident| $conv:expr
    ) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $name;

        impl $crate::multi::sealed::Sealed<$D> for $name {}

        impl $crate::multi::Key<$D> for $name {
            type Raw = $raw_ty;
            type Output<$a> = $out;
            const TAG: ::std::os::raw::c_int = $tag;

            fn raw() -> Self::Raw {
                $init
            }

            fn absent(code: $crate::ffi::Result::Type) -> bool {
                // 没有缺失码的 key 不看 `code`；这样写避免未使用变量的告警。
                let _ = code;
                false $(|| code == $code)*
            }

            unsafe fn convert<$a>(
                $raw: Self::Raw,
                $present: bool,
            ) -> $crate::error::Result<Self::Output<$a>> {
                // 只有可缺失的 key 才看 `present`。
                let _ = $present;
                $conv
            }
        }
    };
}

pub(crate) use {multi_key_full, multi_keys};

#[cfg(test)]
mod tests {
    use crate::{
        Terminal,
        screen::{cell_query, row_query},
        search::{self, Search},
        selection::gesture::{self, Gesture, PressEvent},
        snapshot::{self, Decoder},
        style::RgbColor,
        terminal::{Point, PointCoordinate, query},
    };

    #[test]
    fn terminal_get_multi_matches_getters() {
        let mut terminal = Terminal::new(20, 4).unwrap();
        terminal.vt_write(b"\x1b]2;title\x07\x1b]7;file:///tmp\x07\x1b[1mhi");
        terminal.set_default_fg_color(Some(RgbColor { r: 1, g: 2, b: 3 })).unwrap();

        let (cols, rows, x, y, title, pwd, fg, bg, style, screen, palette, flags) = terminal
            .get_multi((
                query::Cols,
                query::Rows,
                query::CursorX,
                query::CursorY,
                query::Title,
                query::Pwd,
                query::FgColor,
                // 没有设置背景色：记为 None，后面的 key 照常读出。
                query::BgColor,
                query::CursorStyle,
                query::ActiveScreen,
                query::ColorPalette,
                query::KittyKeyboardFlags,
            ))
            .unwrap();
        assert_eq!((cols, rows), (20, 4));
        assert_eq!((x, y), (2, 0));
        assert_eq!((title, pwd), ("title", "file:///tmp"));
        assert_eq!(fg, terminal.fg_color().unwrap());
        assert_eq!(bg, None);
        assert_eq!(style, terminal.cursor_style().unwrap());
        assert!(style.bold);
        assert_eq!(screen, terminal.active_screen().unwrap());
        assert_eq!(palette.0, terminal.color_palette().unwrap().0);
        assert_eq!(flags, terminal.kitty_keyboard_flags().unwrap());

        let (memory, scrollbar, ground, at_prompt, max_bytes, selection, shape) = terminal
            .get_multi((
                query::MemoryUsage,
                query::Scrollbar,
                query::VtGround,
                query::CursorAtPrompt,
                query::ScrollbackMaxBytes,
                query::Selection,
                query::MouseShape,
            ))
            .unwrap();
        assert_eq!(memory, terminal.memory_usage().unwrap());
        let expected = terminal.scrollbar().unwrap();
        assert_eq!((scrollbar.total, scrollbar.offset, scrollbar.len), (expected.total, expected.offset, expected.len));
        assert!(ground);
        assert!(!at_prompt);
        assert_eq!(max_bytes, terminal.scrollback_max_bytes().unwrap());
        assert!(selection.is_none());
        assert_eq!(shape, terminal.mouse_shape().unwrap());
    }

    #[test]
    fn screen_row_and_cell_get_multi_match_getters() {
        let mut terminal = Terminal::new(6, 3).unwrap();
        terminal.vt_write("\x1b[41m中\x1b[0mabcdef".as_bytes());
        for x in 0..6 {
            let grid_ref = terminal.grid_ref(Point::Active(PointCoordinate { x, y: 0 })).unwrap();
            let cell = grid_ref.cell().unwrap();
            let (codepoint, wide, tag, text, styled, style_id, semantic) = cell
                .get_multi((
                    cell_query::Codepoint,
                    cell_query::Wide,
                    cell_query::ContentTag,
                    cell_query::HasText,
                    cell_query::HasStyling,
                    cell_query::StyleId,
                    cell_query::SemanticContent,
                ))
                .unwrap();
            assert_eq!(codepoint, cell.codepoint().unwrap());
            assert_eq!(wide, cell.wide().unwrap());
            assert_eq!(tag, cell.content_tag().unwrap());
            assert_eq!(text, cell.has_text().unwrap());
            assert_eq!(styled, cell.has_styling().unwrap());
            assert_eq!(style_id, cell.style_id().unwrap());
            assert_eq!(semantic, cell.semantic_content().unwrap());

            let row = grid_ref.row().unwrap();
            let (wrap, cont, styled_row, dirty, prompt) = row
                .get_multi((
                    row_query::Wrap,
                    row_query::WrapContinuation,
                    row_query::Styled,
                    row_query::Dirty,
                    row_query::SemanticPrompt,
                ))
                .unwrap();
            assert_eq!(wrap, row.is_wrapped().unwrap());
            assert_eq!(cont, row.is_wrap_continuation().unwrap());
            assert_eq!(styled_row, row.is_styled().unwrap());
            assert_eq!(dirty, row.is_dirty().unwrap());
            assert_eq!(prompt, row.semantic_prompt().unwrap());
        }
        // 第一行写满后软换行到第二行。
        let row = terminal.grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 })).unwrap().row().unwrap();
        assert_eq!(row.get_multi((row_query::Wrap,)).unwrap(), (true,));
    }

    #[test]
    fn search_get_multi_matches_getters() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"find me, find me");
        let mut search = Search::new(&mut terminal).unwrap();
        let (needle, index, status) =
            search.get_multi((search::query::Needle, search::query::SelectedIndex, search::query::Status)).unwrap();
        assert_eq!((needle, index, status), (None, None, search::Status::Complete));

        search.set_needle(&mut terminal, "find").unwrap();
        search.select_next(&mut terminal).unwrap();
        let (needle, total, index, scroll) = search
            .get_multi((
                search::query::Needle,
                search::query::TotalMatches,
                search::query::SelectedIndex,
                search::query::SelectScroll,
            ))
            .unwrap();
        assert_eq!(needle, Some("find"));
        assert_eq!(total, 2);
        assert_eq!(index, Some(0));
        assert_eq!(scroll, search::Scroll::IfNeeded);
    }

    #[test]
    fn gesture_get_multi_matches_getters() {
        let mut terminal = Terminal::new(20, 3).unwrap();
        terminal.vt_write(b"hello world");
        let mut gesture = Gesture::new().unwrap();
        let (count, anchor) =
            gesture.get_multi(&terminal, (gesture::query::ClickCount, gesture::query::Anchor)).unwrap();
        assert_eq!(count, 0);
        assert!(anchor.is_none());

        let mut press = PressEvent::new().unwrap();
        let grid_ref = terminal.grid_ref(Point::Active(PointCoordinate { x: 1, y: 0 })).unwrap();
        press.apply(&mut gesture, &terminal, grid_ref).unwrap();
        let (count, dragged, autoscroll, behavior, anchor) = gesture
            .get_multi(
                &terminal,
                (
                    gesture::query::ClickCount,
                    gesture::query::Dragged,
                    gesture::query::Autoscroll,
                    gesture::query::Behavior,
                    gesture::query::Anchor,
                ),
            )
            .unwrap();
        assert_eq!(count, gesture.click_count(&terminal).unwrap());
        assert_eq!(count, 1);
        assert_eq!(dragged, gesture.dragged(&terminal).unwrap());
        assert_eq!(autoscroll, gesture.autoscroll(&terminal).unwrap());
        assert_eq!(behavior, gesture.behavior(&terminal).unwrap());
        let anchor = anchor.unwrap();
        assert_eq!(
            terminal.point_from_grid_ref(&anchor, crate::terminal::PointSpace::Active).unwrap(),
            Some(PointCoordinate { x: 1, y: 0 })
        );
    }

    #[test]
    fn decoder_get_multi_matches_getters() {
        // 快照把视口附近的内容放进 READY 之前的前缀，更早的回滚区才作为历史
        // page 增量解码，所以要写足够多的行。
        let mut terminal = Terminal::new(20, 3).unwrap();
        let lines: Vec<u8> = (0..3000).flat_map(|i| format!("line {i}\r\n").into_bytes()).collect();
        terminal.vt_write(&lines);
        let bytes = terminal.encode_snapshot_alloc(None).unwrap().unwrap();
        let decoder = Decoder::new_buf(&bytes).unwrap();
        let (max, offset) = decoder
            .get_multi((snapshot::decoder_query::MaxContinuationBytes, snapshot::decoder_query::SourceOffset))
            .unwrap();
        assert_eq!(max, decoder.max_continuation_bytes().unwrap());
        assert_eq!(offset, decoder.source_offset().unwrap());

        let mut incremental = decoder.ready().unwrap();
        let progress = incremental.next().unwrap().unwrap();
        let (screen, rows, remaining, primary, alternate) = progress
            .as_decoder()
            .get_multi((
                snapshot::decoder_query::ProgressScreen,
                snapshot::decoder_query::ProgressRows,
                snapshot::decoder_query::ProgressRemaining,
                snapshot::decoder_query::HistoryRowsPrimary,
                // 快照没有声明备用屏：记为 None。
                snapshot::decoder_query::HistoryRowsAlternate,
            ))
            .unwrap();
        assert_eq!(screen, progress.screen().unwrap());
        assert_eq!(rows, progress.rows().unwrap());
        assert_eq!(remaining, progress.remaining().unwrap());
        assert_eq!(primary, progress.as_decoder().history_rows_primary().unwrap());
        assert_eq!(alternate, progress.as_decoder().history_rows_alternate().unwrap());
    }

    #[cfg(feature = "kitty-graphics")]
    #[test]
    fn kitty_get_multi_matches_getters() {
        use crate::kitty::graphics::{PlacementIterator, image_query, placement_query};

        let mut terminal = Terminal::new(20, 5).unwrap();
        // 图片放置的网格大小按单元格像素换算，所以要给出单元格尺寸。
        terminal.resize(20, 5, 10, 20).unwrap();
        // 传输并显示一张 2x1 的 RGB 图片（6 个零字节）。
        terminal.vt_write(b"\x1b_Ga=T,f=24,s=2,v=1,i=7,c=2,r=1;AAAAAAAA\x1b\\");
        let graphics = terminal.kitty_graphics().unwrap();
        let image = graphics.image(7).unwrap();
        let (id, width, height, format, len, generation) = image
            .get_multi((
                image_query::Id,
                image_query::Width,
                image_query::Height,
                image_query::Format,
                image_query::DataLen,
                image_query::Generation,
            ))
            .unwrap();
        assert_eq!((id, width, height), (7, 2, 1));
        assert_eq!(format, image.format().unwrap());
        assert_eq!(len, image.data().unwrap().unwrap().len());
        assert_eq!(generation, image.generation().unwrap());

        let mut placements = PlacementIterator::new().unwrap();
        let mut iteration = placements.update(&graphics).unwrap();
        let placement = iteration.next().unwrap();
        let (image_id, columns, rows, z, is_virtual) = placement
            .get_multi((
                placement_query::ImageId,
                placement_query::Columns,
                placement_query::Rows,
                placement_query::Z,
                placement_query::IsVirtual,
            ))
            .unwrap();
        assert_eq!(image_id, 7);
        assert_eq!((columns, rows), (2, 1));
        assert_eq!(z, placement.z().unwrap());
        assert_eq!(is_virtual, placement.is_virtual().unwrap());
    }
}
