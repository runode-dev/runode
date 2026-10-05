//! 终端里的搜索：搜索栏的输入框，以及打开、切换、关闭搜索的动作。
//!
//! 输入框管文字编辑（光标、选区、鼠标点选拖选、输入法组字），把搜索词的变化作为事件
//! 交给终端视图；匹配数、上下切换按钮由终端视图画在输入框旁边。

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyDownEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Render, ShapedLine, SharedString, Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, actions,
    div, fill, point, prelude::*, px, relative, size,
};

use crate::terminal_view::{Copy, Paste, SelectAll};

actions!(
    runode,
    [
        /// 打开搜索栏；已经打开时把焦点移过去。
        StartSearch,
        /// 用当前选区的文字搜索。
        SearchSelection,
        SearchNext,
        SearchPrevious,
        EndSearch,
        /// 以下几个只在搜索框里用：终端里没有可编辑的文字。
        Cut,
        Undo,
        Redo
    ]
);

const CARET_WIDTH: Pixels = px(1.5);
const CARET_HEIGHT: Pixels = px(14.);
/// 撤销最多能退回的步数。
const UNDO_LIMIT: usize = 100;

pub enum SearchFieldEvent {
    Changed(String),
    Next,
    Previous,
    Dismiss,
}

pub struct SearchField {
    focus_handle: FocusHandle,
    /// 输入框里的全部文字；输入法组字时也包括正在组的字。
    text: String,
    /// 选区，按字节偏移；为空时就是光标位置。
    selected: Range<usize>,
    /// 光标在选区开头，即选区是从右往左选出来的。
    reversed: bool,
    /// 正在组的字在 `text` 里的范围，确认后才算进搜索词。
    marked: Option<Range<usize>>,
    /// 最近一次交给终端视图的搜索词，不含组字。
    query: String,
    /// 按住鼠标拖选中；双击后拖动按词扩展。
    drag: Option<Drag>,
    /// 撤销、重做用的编辑前状态。
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// 上一次编辑的类型，连续打字或连续删除合成一步撤销；挪光标后清空。
    last_edit: Option<EditKind>,
    /// 空着时画的提示，默认是「搜索」；拿来就地改名时不画。
    placeholder: Option<SharedString>,
    /// 上一帧排好的文字、输入框位置和横向滚动量，鼠标点选、输入法摆候选窗都靠它们换算。
    layout: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
    scroll_x: Pixels,
}

enum Drag {
    Char,
    /// 双击选中的词，拖动时它始终在选区里。
    Word(Range<usize>),
}

#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    Insert,
    Delete,
}

struct Snapshot {
    text: String,
    selected: Range<usize>,
    reversed: bool,
}

impl EventEmitter<SearchFieldEvent> for SearchField {}

impl SearchField {
    pub fn new(query: String, cx: &mut Context<Self>) -> Self {
        let end = query.len();
        Self {
            focus_handle: cx.focus_handle(),
            text: query.clone(),
            selected: end..end,
            reversed: false,
            marked: None,
            query,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            placeholder: Some(rust_i18n::t!("search.placeholder").into_owned().into()),
            layout: None,
            bounds: None,
            scroll_x: px(0.),
        }
    }

    /// 就地编辑一段已有文字用的输入框：不画「搜索」提示，前 `select` 个字节选中，直接打字就
    /// 替换掉它们。
    pub fn editing(text: String, select: usize, cx: &mut Context<Self>) -> Self {
        let select = (0..=select.min(text.len())).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
        let mut field = Self::new(text, cx);
        field.placeholder = None;
        field.selected = 0..select;
        field
    }

    /// 空着时画的提示换成 `text`。
    pub fn with_placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = Some(text.into());
        self
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// 换掉整个搜索词，光标放到末尾；不发 `Changed`，调用方自己去搜。
    pub fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.record(None);
        self.selected = query.len()..query.len();
        self.reversed = false;
        self.marked = None;
        self.text = query.clone();
        self.query = query;
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        self.last_edit = None;
        cx.notify();
    }

    /// 选区的另一端不动，把光标这一端挪到 `offset`。
    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let anchor = if self.reversed { self.selected.end } else { self.selected.start };
        self.reversed = offset < anchor;
        self.selected = offset.min(anchor)..offset.max(anchor);
        self.last_edit = None;
        cx.notify();
    }

    /// 拖选到鼠标所在的字符；双击后的拖选按整词扩展。
    fn drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let index = self.index_for_x(x);
        match &self.drag {
            Some(Drag::Char) => self.select_to(index, cx),
            Some(Drag::Word(anchor)) => {
                let word = self.word_range_at(index);
                self.reversed = word.start < anchor.start;
                self.selected = word.start.min(anchor.start)..word.end.max(anchor.end);
                cx.notify();
            }
            None => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selected: self.selected.clone(),
            reversed: self.reversed,
        }
    }

    /// 编辑前记下当前状态供撤销；`kind` 和上一次相同时并进上一步，`None` 总是单独一步。
    fn record(&mut self, kind: Option<EditKind>) {
        if kind.is_none() || kind != self.last_edit {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.last_edit = kind;
        self.redo.clear();
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.text = snapshot.text;
        self.selected = snapshot.selected;
        self.reversed = snapshot.reversed;
        self.marked = None;
        self.last_edit = None;
        self.sync_query(cx);
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        // 组字时撤销会和输入法手里的状态对不上。
        if self.marked.is_some() {
            return;
        }
        if let Some(snapshot) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() {
            return;
        }
        if let Some(snapshot) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(snapshot, cx);
        }
    }

    /// 把 `range` 换成 `new_text`，光标停在新文字后面。
    fn replace(&mut self, range: Range<usize>, new_text: &str, cx: &mut Context<Self>) {
        self.text.replace_range(range.clone(), new_text);
        let end = range.start + new_text.len();
        self.selected = end..end;
        self.reversed = false;
        self.marked = None;
        self.sync_query(cx);
    }

    /// 去掉组字后的文字有变化时通知终端视图重新搜索。
    fn sync_query(&mut self, cx: &mut Context<Self>) {
        let committed = match &self.marked {
            Some(marked) => [&self.text[..marked.start], &self.text[marked.end..]].concat(),
            None => self.text.clone(),
        };
        if committed != self.query {
            self.query = committed.clone();
            cx.emit(SearchFieldEvent::Changed(committed));
        }
        cx.notify();
    }

    fn previous_char(&self, offset: usize) -> usize {
        self.text[..offset].char_indices().next_back().map_or(0, |(i, _)| i)
    }

    fn next_char(&self, offset: usize) -> usize {
        self.text[offset..].chars().next().map_or(self.text.len(), |c| offset + c.len_utf8())
    }

    /// 往左到上一个词的开头：先跳过非词字符，再跳过词字符。
    fn previous_word(&self, offset: usize) -> usize {
        let mut chars = self.text[..offset].char_indices().rev().peekable();
        while chars.next_if(|(_, c)| !is_word_char(*c)).is_some() {}
        let mut start = chars.peek().map_or(0, |(i, _)| *i);
        for (i, c) in chars {
            if !is_word_char(c) {
                break;
            }
            start = i;
        }
        start
    }

    /// 往右到下一个词的末尾：先跳过非词字符，再跳过词字符。
    fn next_word(&self, offset: usize) -> usize {
        let rest = &self.text[offset..];
        let mut chars = rest.char_indices().peekable();
        while chars.next_if(|(_, c)| !is_word_char(*c)).is_some() {}
        while chars.next_if(|(_, c)| is_word_char(*c)).is_some() {}
        offset + chars.peek().map_or(rest.len(), |(i, _)| *i)
    }

    /// 双击选中的范围：点在词上选整个词，否则选连续的同类字符。
    fn word_range_at(&self, offset: usize) -> Range<usize> {
        let Some(c) = self.text[offset..].chars().next().or_else(|| self.text[..offset].chars().next_back()) else {
            return offset..offset;
        };
        let same = |x: char| is_word_char(x) == is_word_char(c) && (is_word_char(c) || x == c);
        let start = self.text[..offset]
            .char_indices()
            .rev()
            .take_while(|(_, x)| same(*x))
            .last()
            .map_or(offset, |(i, _)| i);
        let end = self.text[offset..]
            .char_indices()
            .find(|(_, x)| !same(*x))
            .map_or(self.text.len(), |(i, _)| offset + i);
        start..end
    }

    fn index_for_x(&self, x: Pixels) -> usize {
        let (Some(layout), Some(bounds)) = (&self.layout, self.bounds) else {
            return self.text.len();
        };
        layout.closest_index_for_x(x - bounds.left() + self.scroll_x)
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 组字时这些键归输入法。
        if self.marked.is_some() {
            return;
        }
        let mods = &event.keystroke.modifiers;
        let cursor = self.cursor();
        let target = match event.keystroke.key.as_str() {
            "left" if mods.platform => 0,
            "left" if mods.alt => self.previous_word(cursor),
            "left" if !mods.shift && !self.selected.is_empty() => self.selected.start,
            "left" => self.previous_char(cursor),
            "right" if mods.platform => self.text.len(),
            "right" if mods.alt => self.next_word(cursor),
            "right" if !mods.shift && !self.selected.is_empty() => self.selected.end,
            "right" => self.next_char(cursor),
            "up" | "home" => 0,
            "down" | "end" => self.text.len(),
            "backspace" | "delete" => {
                let forward = event.keystroke.key == "delete";
                let range = if !self.selected.is_empty() {
                    self.selected.clone()
                } else if forward {
                    cursor..if mods.alt { self.next_word(cursor) } else { self.next_char(cursor) }
                } else if mods.platform {
                    0..cursor
                } else if mods.alt {
                    self.previous_word(cursor)..cursor
                } else {
                    self.previous_char(cursor)..cursor
                };
                if !range.is_empty() {
                    self.record(Some(EditKind::Delete));
                    self.replace(range, "", cx);
                }
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        if mods.shift {
            self.select_to(target, cx);
        } else {
            self.move_to(target, cx);
        }
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        // 组字时点击不改选区，免得把正在组的字拆开。
        if self.marked.is_some() {
            return;
        }
        let index = self.index_for_x(event.position.x);
        self.drag = match event.click_count {
            2 => {
                let word = self.word_range_at(index);
                self.selected = word.clone();
                self.reversed = false;
                self.last_edit = None;
                cx.notify();
                Some(Drag::Word(word))
            }
            n if n >= 3 => {
                self.select_all(&SelectAll, window, cx);
                None
            }
            _ => {
                if event.modifiers.shift {
                    self.select_to(index, cx);
                } else {
                    self.move_to(index, cx);
                }
                Some(Drag::Char)
            }
        };
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        // 搜索只在一行里找，多行内容只取第一行。
        let line = text.lines().next().unwrap_or_default();
        self.record(None);
        self.replace(self.selected.clone(), line, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.text.len();
        self.reversed = false;
        self.last_edit = None;
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.text[self.selected.clone()].to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() || self.marked.is_some() {
            return;
        }
        self.copy(&Copy, window, cx);
        self.record(None);
        self.replace(self.selected.clone(), "", cx);
    }

    fn offset_from_utf16(&self, utf16: usize) -> usize {
        let mut count = 0;
        for (i, c) in self.text.char_indices() {
            if count >= utf16 {
                return i;
            }
            count += c.len_utf16();
        }
        self.text.len()
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.text[..offset].encode_utf16().count()
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Focusable for SearchField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("SearchBar")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|_, _: &SearchNext, _, cx| cx.emit(SearchFieldEvent::Next)))
            .on_action(cx.listener(|_, _: &SearchPrevious, _, cx| cx.emit(SearchFieldEvent::Previous)))
            .on_action(cx.listener(|_, _: &EndSearch, _, cx| cx.emit(SearchFieldEvent::Dismiss)))
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .child(SearchText { field: cx.entity() })
    }
}

impl EntityInputHandler for SearchField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.text[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        self.sync_query(cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        // 回车、制表符等由按键绑定处理，不进搜索词。
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() && range.is_empty() && self.marked.is_none() {
            return;
        }
        // 组字确认上屏时，开始组字那一刻已经记过了。
        if self.marked.is_none() {
            self.record(Some(EditKind::Insert));
        }
        self.replace(range, &text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        if self.marked.is_none() {
            self.record(Some(EditKind::Insert));
        }
        self.text.replace_range(range.clone(), text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        // 输入法给的选区相对于组字开头，按 UTF-16 计。
        self.selected = match selected {
            Some(selected) => {
                let offset = |utf16: usize| {
                    let mut count = 0;
                    text.char_indices()
                        .find(|(_, c)| {
                            let hit = count >= utf16;
                            count += c.len_utf16();
                            hit
                        })
                        .map_or(text.len(), |(i, _)| i)
                };
                range.start + offset(selected.start)..range.start + offset(selected.end)
            }
            None => range.start + text.len()..range.start + text.len(),
        };
        self.reversed = false;
        self.sync_query(cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_from_utf16(&range);
        let x = |index| bounds.left() + layout.x_for_index(index) - self.scroll_x;
        Some(Bounds::from_corners(
            point(x(range.start), bounds.top()),
            point(x(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.bounds?;
        let index = self.layout.as_ref()?.index_for_x(point.x - bounds.left() + self.scroll_x)?;
        Some(self.offset_to_utf16(index))
    }
}

/// 输入框里的文字、选区和光标，自己排版绘制以便按位置换算字符。
struct SearchText {
    field: Entity<SearchField>,
}

struct SearchTextLayout {
    line: ShapedLine,
    /// 占位文字；有内容时为 `None`。
    placeholder: Option<ShapedLine>,
    scroll_x: Pixels,
    selection: Option<PaintQuad>,
    /// 有选区时不画光标。
    caret: Option<PaintQuad>,
}

impl IntoElement for SearchText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SearchText {
    type RequestLayoutState = ();
    type PrepaintState = SearchTextLayout;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> SearchTextLayout {
        let field = self.field.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let run = |len: usize, color| TextRun {
            len,
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match &field.marked {
            Some(marked) => [
                run(marked.start, style.color),
                TextRun {
                    underline: Some(UnderlineStyle {
                        color: Some(style.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run(marked.len(), style.color)
                },
                run(field.text.len() - marked.end, style.color),
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run(field.text.len(), style.color)],
        };
        let line = window.text_system().shape_line(field.text.clone().into(), font_size, &runs, None);
        let placeholder = field.placeholder.clone().filter(|_| field.text.is_empty()).map(|text| {
            let len = text.len();
            window.text_system().shape_line(text, font_size, &[run(len, style.color.opacity(0.4))], None)
        });

        // 横向滚动到光标露出来为止，文字缩短时也别在右边留空。
        let width = bounds.size.width - CARET_WIDTH;
        let caret_x = line.x_for_index(field.cursor());
        let mut scroll_x = field.scroll_x;
        if caret_x - scroll_x > width {
            scroll_x = caret_x - width;
        }
        if caret_x < scroll_x {
            scroll_x = caret_x;
        }
        let max_scroll = line.width() - width;
        if scroll_x > max_scroll {
            scroll_x = max_scroll;
        }
        if scroll_x < px(0.) {
            scroll_x = px(0.);
        }

        let x = |index| bounds.left() + line.x_for_index(index) - scroll_x;
        let selection = (!field.selected.is_empty()).then(|| {
            fill(
                Bounds::from_corners(
                    point(x(field.selected.start), bounds.top()),
                    point(x(field.selected.end), bounds.bottom()),
                ),
                style.color.opacity(0.3),
            )
        });
        let caret = field.selected.is_empty().then(|| {
            fill(
                Bounds::new(
                    point(x(field.cursor()), bounds.center().y - CARET_HEIGHT / 2.),
                    size(CARET_WIDTH, CARET_HEIGHT),
                ),
                style.color,
            )
        });
        SearchTextLayout {
            line,
            placeholder,
            scroll_x,
            selection,
            caret,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut SearchTextLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let field = self.field.read(cx);
        let focus_handle = field.focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        let dragging = field.drag.is_some();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.field.clone()), cx);

        let line_height = window.line_height();
        if focused && let Some(selection) = layout.selection.take() {
            window.paint_quad(selection);
        }
        let origin = point(bounds.left() - layout.scroll_x, bounds.top());
        layout.line.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
        // 没有内容时光标停在占位文字前面。
        if let Some(placeholder) = &layout.placeholder {
            let origin = point(bounds.left() + CARET_WIDTH + px(2.), bounds.top());
            placeholder.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
        }
        if focused && let Some(caret) = layout.caret.take() {
            window.paint_quad(caret);
        }

        // 拖选时鼠标出了输入框也要跟着选，所以监听整个窗口的移动和松开。
        if dragging {
            let field = self.field.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left) {
                    field.update(cx, |field, cx| field.drag_to(event.position.x, cx));
                }
            });
            let field = self.field.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    field.update(cx, |field, _| field.drag = None);
                }
            });
        }

        let line = layout.line.clone();
        let scroll_x = layout.scroll_x;
        self.field.update(cx, |field, _| {
            field.layout = Some(line);
            field.bounds = Some(bounds);
            field.scroll_x = scroll_x;
        });
    }
}
