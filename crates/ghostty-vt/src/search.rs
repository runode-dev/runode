//! 在终端内容（包括回滚区）中搜索字符串。
//!
//! [`Search`] 在创建它的终端里搜索用 [`Search::set_needle`] 设置的字符串，
//! 覆盖主屏和备用屏的活动区与回滚区。终端搜索的难点由 libghostty 内部处理：
//! 结果与实时屏幕保持同步，跨主屏/备用屏切换保留（进出 vim 这类全屏程序不会
//! 让回滚区搜索重来），并能从 resize、重排、重置和回滚区裁剪中恢复。
//!
//! 搜索开始时是空闲的。设置 needle 启动搜索，换 needle 从头重来，清空 needle
//! 回到空闲。匹配按字节精确比较，只有 ASCII 字母不区分大小写。
//!
//! # 驱动搜索
//!
//! 搜索大量回滚区需要时间，所以工作被拆成由调用方驱动的小步：
//!
//! - [`Search::tick`] 在搜索已复制的数据上推进有限的一步，不读终端。
//! - [`Search::feed`] 读取终端，复制更多数据并获知终端的变化。feed 是搜索
//!   得知终端变化的唯一途径，所以使用期间要持续定期 feed，即使已经报告
//!   完成。
//! - [`Search::run`] 是阻塞的便捷方法，反复 feed 和 tick 直到追上终端。
//!
//! [`Status::Complete`] 表示搜索已追上**最近一次 feed 时**的终端，并不意味
//! 永远完成：之后的终端写入需要再次 feed 才能看到。
//!
//! # 匹配就是选区
//!
//! 每个匹配都以非矩形的 [`Selection`] 快照返回，所以现有的选区 API 都能直接
//! 用于匹配：[`Terminal::format_selection_alloc`] 复制匹配文本，
//! [`Terminal::point_from_grid_ref`] 配合 [`PointSpace::Viewport`] 定位高亮矩形，
//! [`Selection::contains`] 做命中测试，[`Terminal::set_selection`] 把匹配设为
//! 终端选区。
//!
//! 返回的匹配遵循通常的快照生命周期规则：只在下一次修改终端的操作之前有效。
//! 这里用借用表达这条规则：匹配借用终端，借用期间无法调用
//! [`Terminal::vt_write`] 等需要 `&mut Terminal` 的方法。请在 feed 之后读取
//! 匹配、在终端再次变化前用完，不要缓存。选中的匹配在内部会随终端变化保持
//! 准确，所以跟踪一个匹配的正确方式是每次 feed 后重新读取
//! [`Search::selected_match`]。
//!
//! # 与终端的生命周期
//!
//! 搜索借用创建它的终端，但不持有 Rust 借用：[`Search`] 可以与终端一起存放，
//! 终端照常写入。需要读写终端的方法都显式接收终端参数，并检查它就是创建
//! 搜索的那个终端，否则返回 [`Error::InvalidValue`]。搜索与终端可以按任意
//! 顺序释放；终端先释放时，所有需要终端的方法以及 [`Search::tick`] 都返回
//! [`Error::InvalidValue`]。一个搜索不能换绑到别的终端。
//!
//! # 示例
//!
//! ```rust
//! use libghostty_vt::{Terminal, search::Search};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut terminal = Terminal::new(80, 24)?;
//! terminal.vt_write(b"$ make test\r\nerror: missing semicolon\r\n$ grep ERROR build.log\r\n");
//!
//! // 用户打开查找栏并输入了查询。
//! let mut search = Search::new(&mut terminal)?;
//! search.set_needle(&mut terminal, "error")?;
//! // 一次性搜索直接跑完；交互式的宿主应在事件循环里交替调用 tick 和 feed。
//! search.run(&mut terminal)?;
//! assert_eq!(search.total_matches()?, 2);
//!
//! // 用户按下回车：选中下一个匹配（从最新的开始，向更旧的内容移动）。
//! assert!(search.select_next(&mut terminal)?);
//! assert_eq!(search.selected_index()?, Some(0));
//!
//! // 每帧 feed 一次以追上终端变化，再读视口内的匹配来画高亮。
//! search.feed(&mut terminal)?;
//! for m in search.viewport_matches(&terminal)? {
//!     let _start = m.start();
//! }
//! # Ok(())
//! # }
//! ```

use std::{marker::PhantomData, ptr::NonNull, rc::Weak};

use crate::{
    alloc::{Allocator, Object},
    error::{Error, Result, from_optional_result, from_result},
    ffi,
    selection::Selection,
    terminal::Terminal,
};

#[cfg(doc)]
use crate::terminal::PointSpace;

/// 绑定到一个终端的搜索。见[模块文档](self)。
///
/// 释放搜索会解除它在终端里的登记，C 侧要求这一步与终端的其他访问串行。
/// 不要在终端的回调（如 [`Terminal::on_bell`]）里 drop 搜索：回调运行在
/// `vt_write` 期间，此时终端正被修改。
#[derive(Debug)]
pub struct Search<'alloc> {
    inner: Object<'alloc, ffi::SearchImpl>,
    // 创建搜索的终端。只用于核对调用方传入的终端，从不经由它访问终端。
    terminal: NonNull<ffi::TerminalImpl>,
    // 终端的存活令牌。指针相等且令牌仍然存活，才能说明传入的就是创建搜索
    // 的那个终端：终端释放后地址可能被新终端复用。
    alive: Weak<std::cell::Cell<u64>>,
    // 最近一次 feed/run 时终端的变更计数。不跟踪的匹配只在终端此后没有变过
    // 时有效。
    fed_generation: u64,
    // C 侧的搜索不能并发调用，与 crate 内其他句柄一样保持 !Send/!Sync。
    _phan: PhantomData<*mut ()>,
}

impl<'alloc> Search<'alloc> {
    /// 创建一个绑定到 `terminal` 的搜索。
    ///
    /// 搜索开始时空闲、没有 needle：报告 [`Status::Complete`]，找不到任何
    /// 东西。用 [`Search::set_needle`] 开始搜索。
    ///
    /// 创建很廉价，不读取终端内容，但会把搜索登记到终端上，以便两者可以按
    /// 任意顺序释放，所以需要独占终端。
    pub fn new(terminal: &mut Terminal<'_, '_>) -> Result<Self> {
        // SAFETY: NULL 分配器总是有效的。
        unsafe { Self::new_inner(std::ptr::null(), terminal) }
    }

    /// 用自定义分配器创建一个绑定到 `terminal` 的搜索。
    ///
    /// 关于自定义内存管理和生命周期，见
    /// [crate 级文档](crate#memory-management-and-lifetimes)。
    pub fn new_with_alloc<'ctx: 'alloc>(
        alloc: &'alloc Allocator<'ctx>,
        terminal: &mut Terminal<'_, '_>,
    ) -> Result<Self> {
        // SAFETY: 借用检查保证分配器有效。
        unsafe { Self::new_inner(alloc.to_raw(), terminal) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator, terminal: &mut Terminal<'_, '_>) -> Result<Self> {
        // 只有拥有句柄的终端才有存活令牌；回调里的借用视图只能拿到
        // `&Terminal`，到不了这里，所以这里总有令牌。
        let alive = terminal.alive.as_ref().map(std::rc::Rc::downgrade).ok_or(Error::InvalidValue)?;
        let mut raw: ffi::Search = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_search_new(alloc, &raw mut raw, terminal.inner.as_raw()) };
        from_result(result)?;
        Ok(Self {
            inner: Object::new(raw)?,
            terminal: terminal.inner.ptr,
            // 新建的搜索没有任何匹配，对当前终端来说是最新的。
            fed_generation: terminal.generation().unwrap_or_default(),
            alive,
            _phan: PhantomData,
        })
    }

    /// 确认 `terminal` 就是创建这个搜索、且仍然存活的那个终端。
    fn check_terminal(&self, terminal: &Terminal<'_, '_>) -> Result<()> {
        if self.terminal == terminal.inner.ptr && self.alive.strong_count() > 0 {
            Ok(())
        } else {
            Err(Error::InvalidValue)
        }
    }

    /// 设置要搜索的字符串。
    ///
    /// 字符串会被复制。匹配按字节精确比较，只有 ASCII 字母不区分大小写。
    /// 换一个 needle 会丢弃全部结果从头搜索；设置与当前相同的 needle（按
    /// 匹配的规则比较）会保留现有结果，所以查找栏可以随意重复提交。空字符串
    /// 清空 needle，搜索回到空闲。
    ///
    /// 替换或清空 needle 会释放搜索在终端里持有的跟踪状态，所以需要独占
    /// 终端。
    pub fn set_needle(&mut self, terminal: &mut Terminal<'_, '_>, needle: &str) -> Result<&mut Self> {
        self.check_terminal(terminal)?;
        let raw = ffi::String::from(needle);
        let result =
            unsafe { ffi::ghostty_search_set(self.inner.as_raw(), ffi::SearchOption::NEEDLE, (&raw const raw).cast()) };
        from_result(result)?;
        Ok(self)
    }

    /// 清空 needle，搜索回到空闲。等同于设置空字符串。
    pub fn clear_needle(&mut self, terminal: &mut Terminal<'_, '_>) -> Result<&mut Self> {
        self.check_terminal(terminal)?;
        let result =
            unsafe { ffi::ghostty_search_set(self.inner.as_raw(), ffi::SearchOption::NEEDLE, std::ptr::null()) };
        from_result(result)?;
        Ok(self)
    }

    /// 当前的 needle；没有设置时返回 `None`。
    pub fn needle(&self) -> Result<Option<&str>> {
        let raw = self.get_optional::<ffi::String>(ffi::SearchData::NEEDLE)?;
        // SAFETY: 字节借用自搜索，在 needle 改变或搜索释放前有效；两者都需要
        // `&mut self`，被这里的共享借用排除了。needle 只能经 `set_needle` 以
        // `&str` 设置，但仍按不可信输入校验 UTF-8。
        raw.map(|raw| std::str::from_utf8(unsafe { raw.to_bytes() }).map_err(|_| Error::InvalidValue)).transpose()
    }

    /// 在搜索已复制的数据上推进有限的一步，返回推进后的状态。
    ///
    /// 不读取终端。状态为 [`Status::Running`] 时循环调用；变成
    /// [`Status::FeedRequired`] 时调用 [`Search::feed`] 解除阻塞。终端已释放
    /// 时返回 [`Error::InvalidValue`]。
    pub fn tick(&mut self) -> Result<Status> {
        let mut status = ffi::SearchStatus::COMPLETE;
        let result = unsafe { ffi::ghostty_search_tick(self.inner.as_raw(), &raw mut status) };
        from_result(result)?;
        status.try_into().map_err(|_| Error::InvalidValue)
    }

    /// 读取终端以更新搜索。
    ///
    /// 每次 feed 让搜索追上终端：核对跟踪的屏幕、重新扫描活动区、刷新视口
    /// 内的匹配列表、给回滚区搜索提供下一块数据，并剔除被回滚区淘汰而失效
    /// 的结果。feed 是搜索得知终端变化的唯一途径，所以使用期间要持续定期
    /// feed，即使已经报告完成。每次调用的工作量有界。
    pub fn feed(&mut self, terminal: &mut Terminal<'_, '_>) -> Result<()> {
        self.check_terminal(terminal)?;
        from_result(unsafe { ffi::ghostty_search_feed(self.inner.as_raw()) })?;
        self.fed_generation = terminal.generation().unwrap_or_default();
        Ok(())
    }

    /// 反复 feed 和 tick，直到搜索追上终端。
    ///
    /// 这是给一次性、单线程宿主用的阻塞便捷方法。它至少 feed 一次，所以也
    /// 会拾取上次 feed 以来的终端变化，然后循环到 [`Status::Complete`]。
    /// 搜索很大的回滚区可能耗时较长，交互式宿主应自己驱动
    /// [`Search::tick`] 和 [`Search::feed`]。
    pub fn run(&mut self, terminal: &mut Terminal<'_, '_>) -> Result<()> {
        self.check_terminal(terminal)?;
        from_result(unsafe { ffi::ghostty_search_run(self.inner.as_raw()) })?;
        self.fed_generation = terminal.generation().unwrap_or_default();
        Ok(())
    }

    fn select(&mut self, terminal: &mut Terminal<'_, '_>, option: ffi::SearchOption::Type) -> Result<bool> {
        self.check_terminal(terminal)?;
        // 选择选项的值保留给将来使用，必须是 NULL。
        let result = unsafe { ffi::ghostty_search_set(self.inner.as_raw(), option, std::ptr::null()) };
        Ok(from_optional_result(result, ())?.is_some())
    }

    /// 选中下一个匹配，向更旧的内容移动：从屏幕底部向上进入历史，这是从
    /// 提示符开始搜索时通常想要的方向。越过最旧的匹配后回绕。
    ///
    /// 会先追上终端，所以与 feed 的先后无关；并按
    /// [`Search::set_select_scroll`] 的策略滚动视口到新选中的匹配。没有任何
    /// 匹配时返回 `Ok(false)`。
    pub fn select_next(&mut self, terminal: &mut Terminal<'_, '_>) -> Result<bool> {
        self.select(terminal, ffi::SearchOption::SELECT_NEXT)
    }

    /// 选中上一个匹配，向更新的内容移动，越过最新的匹配后回绕。其余同
    /// [`Search::select_next`]。
    pub fn select_prev(&mut self, terminal: &mut Terminal<'_, '_>) -> Result<bool> {
        self.select(terminal, ffi::SearchOption::SELECT_PREV)
    }

    /// 设置选中匹配时的视口滚动策略，一直生效到再次修改。`None` 恢复默认的
    /// [`Scroll::IfNeeded`]。只修改搜索自己的状态，不读终端。
    pub fn set_select_scroll(&mut self, scroll: Option<Scroll>) -> Result<&mut Self> {
        let raw = scroll.map(ffi::SearchScroll::Type::from);
        let ptr = raw.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
        let result =
            unsafe { ffi::ghostty_search_set(self.inner.as_raw(), ffi::SearchOption::SELECT_SCROLL, ptr.cast()) };
        from_result(result)?;
        Ok(self)
    }

    /// 当前的视口滚动策略。
    pub fn select_scroll(&self) -> Result<Scroll> {
        self.get::<ffi::SearchScroll::Type>(ffi::SearchData::SELECT_SCROLL)?.try_into().map_err(|_| Error::InvalidValue)
    }

    /// 当前的搜索状态。
    ///
    /// 所有读取都反映最近一次 feed 时终端的活动屏。程序切到备用屏后，下一次
    /// feed 会把计数、匹配和选中项切换为备用屏的结果；主屏的结果（包括已
    /// 完成的回滚区搜索）会被保留，切回时恢复。
    pub fn status(&self) -> Result<Status> {
        self.get::<ffi::SearchStatus::Type>(ffi::SearchData::STATUS)?.try_into().map_err(|_| Error::InvalidValue)
    }

    /// 活动屏上到目前为止找到的匹配总数。第一次 feed 之前为零。
    pub fn total_matches(&self) -> Result<usize> {
        self.get(ffi::SearchData::TOTAL_MATCHES)
    }

    /// 选中匹配的序号；没有选中时返回 `None`。
    ///
    /// 序号按 [`Search::matches`] 的从新到旧顺序，0 是最新的匹配，所以
    /// “第 k 个，共 n 个”的查找栏显示 `index + 1` / [`Search::total_matches`]。
    pub fn selected_index(&self) -> Result<Option<usize>> {
        self.get_optional(ffi::SearchData::SELECTED_INDEX)
    }

    /// 选中的匹配；没有选中时返回 `None`。
    ///
    /// 返回的是不跟踪的快照，借用终端，只在终端下一次变化前有效。
    ///
    /// 终端在上一次 [`Search::feed`] / [`Search::run`] 之后变过时返回
    /// [`Error::OutOfDate`]：先 feed 再读。
    pub fn selected_match<'t>(&self, terminal: &'t Terminal<'_, '_>) -> Result<Option<Selection<'t>>> {
        self.check_terminal(terminal)?;
        let raw = self.get_optional(ffi::SearchData::SELECTED_MATCH)?;
        // SAFETY: 匹配来自 `terminal`（上面已核对），返回值借用它，终端变化前
        // 无法再修改终端。
        Ok(raw.map(|raw| unsafe { Selection::from_raw(raw) }))
    }

    /// 活动屏上的全部匹配，从新到旧：从活动区底部向上直到回滚区。
    ///
    /// 返回的是不跟踪的快照，借用终端，只在终端下一次变化前有效。
    ///
    /// 终端在上一次 [`Search::feed`] / [`Search::run`] 之后变过时返回
    /// [`Error::OutOfDate`]：先 feed 再读。
    pub fn matches<'t>(&self, terminal: &'t Terminal<'_, '_>) -> Result<Vec<Selection<'t>>> {
        let mut out = Vec::new();
        self.matches_into(terminal, ffi::SearchData::MATCHES, &mut out)?;
        Ok(out)
    }

    /// 覆盖视口的那些 page 上的匹配，用于绘制高亮矩形。
    ///
    /// 列表在 feed 时计算并缓存，反映最近一次 feed 时的视口。匹配按 page
    /// 查找，所以与视口同在一个 page 上的匹配可能略微超出可见区域（Ghostty
    /// 自己的渲染器也是如此）。用 [`Terminal::point_from_grid_ref`] 把端点转换
    /// 到视口坐标就能自然裁掉它们：跳过转换失败或行号超出可见行数的匹配。
    ///
    /// 返回的是不跟踪的快照，借用终端，只在终端下一次变化前有效。
    ///
    /// 终端在上一次 [`Search::feed`] / [`Search::run`] 之后变过时返回
    /// [`Error::OutOfDate`]：先 feed 再读。
    pub fn viewport_matches<'t>(&self, terminal: &'t Terminal<'_, '_>) -> Result<Vec<Selection<'t>>> {
        let mut out = Vec::new();
        self.viewport_matches_into(terminal, &mut out)?;
        Ok(out)
    }

    /// 同 [`Search::viewport_matches`]，但把结果写入 `out`（先清空），以便
    /// 每帧复用同一块分配。
    pub fn viewport_matches_into<'t>(
        &self,
        terminal: &'t Terminal<'_, '_>,
        out: &mut Vec<Selection<'t>>,
    ) -> Result<()> {
        self.matches_into(terminal, ffi::SearchData::VIEWPORT_MATCHES, out)
    }

    fn matches_into<'t>(
        &self,
        terminal: &'t Terminal<'_, '_>,
        tag: ffi::SearchData::Type,
        out: &mut Vec<Selection<'t>>,
    ) -> Result<()> {
        self.check_terminal(terminal)?;
        out.clear();
        // 历史区的匹配是不跟踪的 page 指针，要到下一次 feed 才会剔除失效项。
        // 终端变过而没有重新 feed 时，这些指针可能指向已释放或被复用的 page。
        if terminal.generation() != Some(self.fed_generation) {
            return Err(Error::OutOfDate);
        }
        let mut raw: Vec<ffi::Selection> = Vec::new();
        loop {
            let mut buf = ffi::SelectionBuffer {
                // 容量为零时传 NULL：C 侧把它当作查询所需容量。
                ptr: if raw.capacity() == 0 { std::ptr::null_mut() } else { raw.as_mut_ptr() },
                cap: raw.capacity(),
                len: 0,
            };
            let result = unsafe { ffi::ghostty_search_get(self.inner.as_raw(), tag, (&raw mut buf).cast()) };
            match result {
                ffi::Result::SUCCESS => {
                    // SAFETY: 成功时 C 侧写入了 `buf.len <= cap` 个完整的条目。
                    unsafe { raw.set_len(buf.len) };
                    break;
                }
                // `buf.len` 是所需容量。视口匹配会在两次调用之间重新收集，
                // 但不读终端，结果不会变，所以一般重试一次即可；仍按循环处理。
                ffi::Result::OUT_OF_SPACE => raw.reserve_exact(buf.len),
                code => return from_result(code),
            }
        }
        // SAFETY: 匹配来自 `terminal`（上面已核对），返回值借用它。
        out.extend(raw.into_iter().map(|raw| unsafe { Selection::from_raw(raw) }));
        Ok(())
    }

    /// 一次 FFI 调用读取多个搜索字段，key 见 [`query`]。
    ///
    /// 只读搜索自己的内存，不读终端。结果与逐个调用对应的 getter 一致，详见
    /// [`crate::multi`]。需要终端借用和缓冲区的选中匹配与匹配列表不在批量
    /// 读取之列。
    pub fn get_multi<K: crate::multi::Keys<crate::multi::domain::Search>>(&self, keys: K) -> Result<K::Output<'_>> {
        let search = self.inner.as_raw();
        // SAFETY: 参数原样交给 `ghostty_search_get_multi`；借用型结果（needle）
        // 借用 `self`。
        unsafe {
            keys.read(&mut |tags, values, written| {
                ffi::ghostty_search_get_multi(search, tags.len(), tags.as_ptr(), values.as_mut_ptr(), written)
            })
        }
    }

    fn get<T>(&self, tag: ffi::SearchData::Type) -> Result<T> {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_search_get(self.inner.as_raw(), tag, value.as_mut_ptr().cast()) };
        from_result(result)?;
        // SAFETY: 成功时值已被写入。
        Ok(unsafe { value.assume_init() })
    }

    fn get_optional<T>(&self, tag: ffi::SearchData::Type) -> Result<Option<T>> {
        let mut value = std::mem::MaybeUninit::<T>::zeroed();
        let result = unsafe { ffi::ghostty_search_get(self.inner.as_raw(), tag, value.as_mut_ptr().cast()) };
        crate::error::from_optional_result_uninit(result, value)
    }
}

impl Drop for Search<'_> {
    fn drop(&mut self) {
        // 终端还活着时，这会释放搜索在终端里持有的跟踪状态；终端已释放时
        // C 侧已经解绑，只释放搜索自己的内存。
        unsafe { ffi::ghostty_search_free(self.inner.as_raw()) }
    }
}

/// [`Search::get_multi`] 的 key，每个 key 的输出与同名 getter 相同。
pub mod query {
    use crate::{
        ffi,
        multi::{domain::Search as D, multi_key_full, multi_keys},
    };

    multi_keys! {
        domain = D, tags = ffi::SearchData;
        /// [`Search::status`](super::Search::status)
        Status = STATUS: try ffi::SearchStatus::Type => super::Status;
        /// [`Search::total_matches`](super::Search::total_matches)
        TotalMatches = TOTAL_MATCHES: copy usize;
        /// [`Search::selected_index`](super::Search::selected_index)
        SelectedIndex = SELECTED_INDEX: opt[NO_VALUE] usize => usize;
        /// [`Search::select_scroll`](super::Search::select_scroll)
        SelectScroll = SELECT_SCROLL: try ffi::SearchScroll::Type => super::Scroll;
    }

    multi_key_full! {
        /// [`Search::needle`](super::Search::needle)，借用搜索。
        Needle in D = ffi::SearchData::NEEDLE;
        raw ffi::String = ffi::String { ptr: std::ptr::null(), len: 0 };
        out<'a> Option<&'a str>;
        absent [ffi::Result::NO_VALUE];
        // SAFETY: needle 借用搜索，`'a` 由 `Search::get_multi` 绑定到搜索借用上。
        convert |raw, present| if present {
            std::str::from_utf8(unsafe { raw.to_bytes() })
                .map(Some)
                .map_err(|_| crate::Error::InvalidValue)
        } else {
            Ok(None)
        }
    }
}

/// 搜索的进度状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum Status {
    /// [`Search::tick`] 无需访问终端就能继续推进。
    Running = ffi::SearchStatus::RUNNING,
    /// 在 [`Search::feed`] 之前无法推进。刚设置 needle 后也是这个状态，
    /// 因为搜索还没看过终端。
    FeedRequired = ffi::SearchStatus::FEED_REQUIRED,
    /// 已追上最近一次 feed 时的终端。这不代表永远完成，之后的终端写入需要
    /// 再次 feed 才能看到。没有 needle 的搜索也报告完成。
    Complete = ffi::SearchStatus::COMPLETE,
}

/// 用 [`Search::select_next`]/[`Search::select_prev`] 选中匹配时的视口滚动
/// 策略。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, int_enum::IntEnum)]
#[repr(i32)]
#[non_exhaustive]
pub enum Scroll {
    /// 匹配不可见时才滚动视口使其可见。这是默认值。
    #[default]
    IfNeeded = ffi::SearchScroll::IF_NEEDED,
    /// 从不滚动视口。
    None = ffi::SearchScroll::NONE,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        selection::FormatOptions,
        terminal::{PointSpace, ScrollViewport},
    };

    /// 匹配所覆盖的文本。软换行处会拼接起来，所以跨行的匹配也是完整的。
    fn text_of(terminal: &Terminal<'_, '_>, selection: &Selection<'_>) -> String {
        let bytes = terminal
            .format_selection_alloc(None, FormatOptions::new().with_selection(selection).with_unwrap(true))
            .unwrap()
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn build_log() -> Terminal<'static, 'static> {
        let mut terminal = Terminal::new(60, 6).unwrap();
        for line in [
            "$ make test\r\n",
            "compiling module A... ok\r\n",
            "compiling module B... error: missing semicolon\r\n",
            "linking... error: undefined symbol\r\n",
            "$ grep -n ERROR build.log\r\n",
        ] {
            terminal.vt_write(line.as_bytes());
        }
        terminal
    }

    #[test]
    fn run_finds_case_insensitive_matches_newest_first() {
        let mut terminal = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        // 空闲的搜索报告完成，什么也找不到。
        assert_eq!(search.status().unwrap(), Status::Complete);
        assert_eq!(search.needle().unwrap(), None);
        assert_eq!(search.total_matches().unwrap(), 0);

        search.set_needle(&mut terminal, "error").unwrap();
        assert_eq!(search.needle().unwrap(), Some("error"));
        // 设置 needle 后还没看过终端，需要 feed。
        assert_eq!(search.status().unwrap(), Status::FeedRequired);
        search.run(&mut terminal).unwrap();
        assert_eq!(search.status().unwrap(), Status::Complete);
        assert_eq!(search.total_matches().unwrap(), 3);

        let matches = search.matches(&terminal).unwrap();
        let texts: Vec<_> = matches.iter().map(|m| text_of(&terminal, m)).collect();
        assert_eq!(texts, ["ERROR", "error", "error"]);
        // 从新到旧：第一个匹配在最下面一行。
        let rows: Vec<_> = matches
            .iter()
            .map(|m| terminal.point_from_grid_ref(&m.start(), PointSpace::Active).unwrap().unwrap().y)
            .collect();
        assert_eq!(rows, [4, 3, 2]);
        assert!(matches.iter().all(|m| !m.is_rectangle()));
    }

    #[test]
    fn tick_and_feed_drive_the_search() {
        let mut terminal = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "error").unwrap();
        // 没 feed 之前 tick 无法推进。
        assert_eq!(search.tick().unwrap(), Status::FeedRequired);
        let mut feeds = 0;
        loop {
            match search.tick().unwrap() {
                Status::Running => {}
                Status::FeedRequired => {
                    search.feed(&mut terminal).unwrap();
                    feeds += 1;
                    assert!(feeds < 100, "search never completed");
                }
                Status::Complete => break,
            }
        }
        assert_eq!(search.total_matches().unwrap(), 3);

        // 之后的写入要再 feed 才能看到。
        terminal.vt_write(b"another error\r\n");
        assert_eq!(search.total_matches().unwrap(), 3);
        search.feed(&mut terminal).unwrap();
        assert_eq!(search.total_matches().unwrap(), 4);
    }

    #[test]
    fn needle_changes_restart_and_clearing_returns_to_idle() {
        let mut terminal = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "error").unwrap();
        search.run(&mut terminal).unwrap();
        // 按匹配规则相同的 needle 保留结果。
        search.set_needle(&mut terminal, "ERROR").unwrap();
        assert_eq!(search.status().unwrap(), Status::Complete);
        assert_eq!(search.total_matches().unwrap(), 3);
        // 换 needle 从头开始。
        search.set_needle(&mut terminal, "module").unwrap();
        assert_eq!(search.status().unwrap(), Status::FeedRequired);
        search.run(&mut terminal).unwrap();
        assert_eq!(search.total_matches().unwrap(), 2);
        // 清空回到空闲；空字符串也一样。
        search.clear_needle(&mut terminal).unwrap();
        assert_eq!(search.needle().unwrap(), None);
        assert_eq!(search.total_matches().unwrap(), 0);
        search.set_needle(&mut terminal, "module").unwrap();
        search.set_needle(&mut terminal, "").unwrap();
        assert_eq!(search.needle().unwrap(), None);
        assert!(!search.select_next(&mut terminal).unwrap());
    }

    #[test]
    fn select_next_and_prev_wrap_around() {
        let mut terminal = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "error").unwrap();
        assert_eq!(search.selected_index().unwrap(), None);
        assert!(search.selected_match(&terminal).unwrap().is_none());

        // select_next 自己会先追上终端，不需要先 feed。
        let mut order = Vec::new();
        for _ in 0..4 {
            assert!(search.select_next(&mut terminal).unwrap());
            order.push(search.selected_index().unwrap().unwrap());
        }
        assert_eq!(order, [0, 1, 2, 0]);
        assert!(search.select_prev(&mut terminal).unwrap());
        assert_eq!(search.selected_index().unwrap(), Some(2));

        let selected = search.selected_match(&terminal).unwrap().unwrap();
        let all = search.matches(&terminal).unwrap();
        assert!(selected.equals(&terminal, &all[2]).unwrap());
    }

    #[test]
    fn selecting_scrolls_the_viewport_unless_disabled() {
        let mut terminal = Terminal::new(20, 4).unwrap();
        terminal.vt_write(b"needle here\r\n");
        for i in 0..50 {
            terminal.vt_write(format!("filler {i}\r\n").as_bytes());
        }
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "needle").unwrap();

        search.set_select_scroll(Some(Scroll::None)).unwrap();
        assert_eq!(search.select_scroll().unwrap(), Scroll::None);
        assert!(search.select_next(&mut terminal).unwrap());
        assert!(terminal.viewport_active().unwrap(), "Scroll::None never scrolls");

        // 默认策略在匹配不可见时滚动视口。
        search.set_select_scroll(None).unwrap();
        assert_eq!(search.select_scroll().unwrap(), Scroll::IfNeeded);
        assert!(search.select_next(&mut terminal).unwrap());
        assert!(!terminal.viewport_active().unwrap());

        // 滚动后，视口内的匹配就是那一个。
        search.feed(&mut terminal).unwrap();
        let mut viewport = Vec::new();
        search.viewport_matches_into(&terminal, &mut viewport).unwrap();
        assert_eq!(viewport.len(), 1);
        assert_eq!(text_of(&terminal, &viewport[0]), "needle");
        assert!(terminal.point_from_grid_ref(&viewport[0].start(), PointSpace::Viewport).unwrap().is_some());
        drop(viewport);

        // 回到底部后匹配不在视口里了。列表按 page 收集，可能仍包含同一 page
        // 上视口外的匹配，转换到视口坐标即可把它们裁掉。
        terminal.scroll_viewport(ScrollViewport::Bottom);
        search.feed(&mut terminal).unwrap();
        for m in search.viewport_matches(&terminal).unwrap() {
            assert!(terminal.point_from_grid_ref(&m.start(), PointSpace::Viewport).unwrap().is_none());
        }
    }

    #[test]
    fn other_terminals_are_rejected() {
        let mut terminal = build_log();
        let mut other = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        assert!(matches!(search.set_needle(&mut other, "error"), Err(Error::InvalidValue)));
        search.set_needle(&mut terminal, "error").unwrap();
        assert!(matches!(search.run(&mut other), Err(Error::InvalidValue)));
        search.run(&mut terminal).unwrap();
        assert!(matches!(search.matches(&other), Err(Error::InvalidValue)));
        assert!(matches!(search.selected_match(&other), Err(Error::InvalidValue)));
    }

    /// 终端先释放：需要终端的操作和 tick 都失败，但搜索仍可安全释放；之后
    /// 新建的终端（即使恰好复用了同一地址）也不会被当成原来那个。
    #[test]
    fn terminal_dropped_before_search() {
        let mut terminal = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "error").unwrap();
        search.run(&mut terminal).unwrap();
        drop(terminal);

        assert!(matches!(search.tick(), Err(Error::InvalidValue)));
        // 只读搜索自己内存的数据仍然可读。
        assert_eq!(search.total_matches().unwrap(), 3);
        let mut replacement = build_log();
        assert!(matches!(search.feed(&mut replacement), Err(Error::InvalidValue)));
        assert!(matches!(search.matches(&replacement), Err(Error::InvalidValue)));
        drop(search);
    }

    #[test]
    fn search_dropped_before_terminal() {
        let mut terminal = build_log();
        {
            let mut search = Search::new(&mut terminal).unwrap();
            search.set_needle(&mut terminal, "error").unwrap();
            search.run(&mut terminal).unwrap();
        }
        // 搜索释放了它在终端里的跟踪状态，终端照常工作。
        terminal.vt_write(b"more error output\r\n");
        terminal.resize(30, 4, 8, 16).unwrap();
    }

    /// 多个搜索可以共享一个终端，并能跟随 resize 后的重排。
    #[test]
    fn searches_share_a_terminal_and_survive_resize() {
        let mut terminal = build_log();
        let mut a = Search::new(&mut terminal).unwrap();
        let mut b = Search::new(&mut terminal).unwrap();
        a.set_needle(&mut terminal, "error").unwrap();
        b.set_needle(&mut terminal, "compiling").unwrap();
        a.run(&mut terminal).unwrap();
        b.run(&mut terminal).unwrap();
        assert_eq!((a.total_matches().unwrap(), b.total_matches().unwrap()), (3, 2));

        terminal.resize(12, 6, 8, 16).unwrap();
        a.run(&mut terminal).unwrap();
        assert_eq!(a.total_matches().unwrap(), 3);
        for m in a.matches(&terminal).unwrap() {
            assert_eq!(text_of(&terminal, &m).to_ascii_lowercase(), "error");
        }
    }

    /// 终端变过而没有重新 feed 时，不跟踪的匹配可能指向已释放的 page，
    /// 必须拒绝读取；重新 feed 后恢复正常。
    #[test]
    fn matches_are_rejected_after_the_terminal_changes_until_fed() {
        let mut terminal = Terminal::new(40, 5).unwrap();
        terminal.set_scrollback_max_lines(Some(1000)).unwrap();
        for i in 0..200 {
            terminal.vt_write(format!("needle {i}\r\n").as_bytes());
        }
        let mut search = Search::new(&mut terminal).unwrap();
        search.set_needle(&mut terminal, "needle").unwrap();
        search.run(&mut terminal).unwrap();
        assert_eq!(search.matches(&terminal).unwrap().len(), 200);

        // 清掉回滚区（CSI 3 J），历史区的匹配随之失效。
        terminal.vt_write(b"\x1b[3J");
        assert!(matches!(search.matches(&terminal).unwrap_err(), Error::OutOfDate));
        assert!(matches!(search.viewport_matches(&terminal).unwrap_err(), Error::OutOfDate));
        // 重设相同的 needle 会保留旧结果，所以不能算作刷新。
        search.set_needle(&mut terminal, "needle").unwrap();
        assert!(matches!(search.matches(&terminal).unwrap_err(), Error::OutOfDate));

        search.feed(&mut terminal).unwrap();
        let matches = search.matches(&terminal).unwrap();
        assert!(matches.len() < 200, "cleared scrollback must drop matches");
        for m in &matches {
            assert_eq!(text_of(&terminal, m), "needle");
        }
    }

    /// 只改配置的 setter 也算变更：宁可多判过期，也不漏判。
    #[test]
    fn any_mutation_marks_matches_out_of_date() {
        let mut terminal = build_log();
        let mut search = Search::new(&mut terminal).unwrap();
        // 新建的搜索没有匹配，可以直接读。
        assert!(search.matches(&terminal).unwrap().is_empty());
        search.set_needle(&mut terminal, "error").unwrap();
        search.run(&mut terminal).unwrap();
        terminal.set_title(Some("x")).unwrap();
        assert!(matches!(search.matches(&terminal).unwrap_err(), Error::OutOfDate));
        search.run(&mut terminal).unwrap();
        assert_eq!(search.matches(&terminal).unwrap().len(), 3);
    }
}
