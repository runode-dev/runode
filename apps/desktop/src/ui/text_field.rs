//! 单行文字输入框：终端的搜索栏、选 agent 和分支的弹窗、文件树和 workspace 就地改名都用它。
//!
//! 输入框管文字编辑（光标、选区、鼠标点选拖选、输入法组字、撤销重做），把文字的变化和回车、
//! Shift+回车、Esc 作为 `TextFieldEvent` 交给调用方；输入框外面的东西（搜索的匹配数、上下切换
//! 按钮、候选列表）由调用方画。回车、Shift+回车、Esc 绑定的是 `SearchNext`、`SearchPrevious`、
//! `EndSearch`，终端没开搜索栏时这几个动作也由终端自己响应。
//!
//! 多行输入框 `TextArea` 和它共用的编辑内核也在这里：撤销记录 `History`、按字符和按词移动、
//! 字节偏移和输入法用的 UTF-16 偏移之间的换算。

use std::ops::Range;

use gpui::{
    AccessibleAction, App, Bounds, ClipboardItem, Context, DispatchPhase, Element, ElementId, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, Role, ShapedLine, SharedString,
    Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px,
    relative, size,
};

use gpui::accesskit::ActionData;

use crate::ui::actions::{Copy, Cut, Paste, Redo, SelectAll, Undo};

actions!(runode, [SearchNext, SearchPrevious, EndSearch]);

const CARET_WIDTH: Pixels = px(1.5);
const CARET_HEIGHT: Pixels = px(14.);
/// 撤销最多能退回的步数。
const UNDO_LIMIT: usize = 100;

/// 输入框交给调用方的事件：文字变了（`Changed`，不含正在组的字）、回车（`Next`）、
/// Shift+回车（`Previous`）和 Esc（`Dismiss`）。
pub enum TextFieldEvent {
    Changed(String),
    Next,
    Previous,
    Dismiss,
}

pub struct TextField {
    focus_handle: FocusHandle,
    /// 输入框里的全部文字；输入法组字时也包括正在组的字。
    text: String,
    /// 选区，按字节偏移；为空时就是光标位置。
    selected: Range<usize>,
    /// 光标在选区开头，即选区是从右往左选出来的。
    reversed: bool,
    /// 正在组的字在 `text` 里的范围，确认后才算进 `query`。
    marked: Option<Range<usize>>,
    /// 最近一次经 `TextFieldEvent::Changed` 交出去的文字，不含组字。
    query: String,
    /// 按住鼠标拖选中；双击后拖动按词扩展。
    drag: Option<Drag>,
    history: History,
    /// 空着时画的提示，默认是「搜索」；拿来就地改名时不画。
    placeholder: Option<SharedString>,
    /// 报给辅助工具的名字，比如设置项的标题；界面上不画。
    label: Option<SharedString>,
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

/// 编辑的类型，连续同类的编辑合成一步撤销。
#[derive(Clone, Copy, PartialEq)]
pub(super) enum EditKind {
    Insert,
    Delete,
}

/// 撤销、重做时恢复的状态。
pub(super) struct Snapshot {
    pub(super) text: String,
    pub(super) selected: Range<usize>,
    pub(super) reversed: bool,
}

/// 撤销、重做用的编辑前状态。
#[derive(Default)]
pub(super) struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// 上一次编辑的类型，连续打字或连续删除合成一步撤销；挪光标后清空。
    last_edit: Option<EditKind>,
}

impl History {
    /// 编辑前记下当前状态 `now` 供撤销；`kind` 和上一次相同时并进上一步（不调 `now`），`None`
    /// 总是单独一步。
    pub(super) fn record(&mut self, kind: Option<EditKind>, now: impl FnOnce() -> Snapshot) {
        if kind.is_none() || kind != self.last_edit {
            self.undo.push(now());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.last_edit = kind;
        self.redo.clear();
    }

    /// 挪过光标或选区：下一次编辑不再并进上一步。
    pub(super) fn break_merge(&mut self) {
        self.last_edit = None;
    }

    /// 退回一步：`now` 进重做栈，返回要恢复的状态；没有可退的时什么都不做。
    pub(super) fn undo(&mut self, now: Snapshot) -> Option<Snapshot> {
        let snapshot = self.undo.pop()?;
        self.redo.push(now);
        self.last_edit = None;
        Some(snapshot)
    }

    /// 重做一步：`now` 进撤销栈，返回要恢复的状态；没有可重做的时什么都不做。
    pub(super) fn redo(&mut self, now: Snapshot) -> Option<Snapshot> {
        let snapshot = self.redo.pop()?;
        self.undo.push(now);
        self.last_edit = None;
        Some(snapshot)
    }
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl TextField {
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
            history: History::default(),
            placeholder: Some(rust_i18n::t!("search.placeholder").into_owned().into()),
            label: None,
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

    /// 报给辅助工具的名字换成 `label`。
    pub fn with_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// 辅助工具直接写入的文字：换掉全部（可撤销），光标放到末尾，和打字一样发 `Changed`。换行这些控制
    /// 字符和打字时一样滤掉，不然写进配置会多出一行。
    fn set_value(&mut self, value: String, cx: &mut Context<Self>) {
        let value = without_controls(&value);
        self.record(None);
        self.selected = value.len()..value.len();
        self.reversed = false;
        self.marked = None;
        self.text = value;
        self.sync_query(cx);
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
        self.history.break_merge();
        cx.notify();
    }

    /// 选区的另一端不动，把光标这一端挪到 `offset`。
    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let anchor = if self.reversed { self.selected.end } else { self.selected.start };
        self.reversed = offset < anchor;
        self.selected = offset.min(anchor)..offset.max(anchor);
        self.history.break_merge();
        cx.notify();
    }

    /// 拖选到鼠标所在的字符；双击后的拖选按整词扩展。
    fn drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let index = self.index_for_x(x);
        match &self.drag {
            Some(Drag::Char) => self.select_to(index, cx),
            Some(Drag::Word(anchor)) => {
                let word = word_range_at(&self.text, index);
                self.reversed = word.start < anchor.start;
                self.selected = word.start.min(anchor.start)..word.end.max(anchor.end);
                cx.notify();
            }
            None => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), selected: self.selected.clone(), reversed: self.reversed }
    }

    /// 编辑前记下当前状态供撤销，见 `History::record`。
    fn record(&mut self, kind: Option<EditKind>) {
        let now = || Snapshot { text: self.text.clone(), selected: self.selected.clone(), reversed: self.reversed };
        self.history.record(kind, now);
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.text = snapshot.text;
        self.selected = snapshot.selected;
        self.reversed = snapshot.reversed;
        self.marked = None;
        self.sync_query(cx);
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        // 组字时撤销会和输入法手里的状态对不上。
        if self.marked.is_some() {
            return;
        }
        if let Some(snapshot) = self.history.undo(self.snapshot()) {
            self.restore(snapshot, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() {
            return;
        }
        if let Some(snapshot) = self.history.redo(self.snapshot()) {
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

    /// 去掉组字后的文字有变化时发 `TextFieldEvent::Changed`。
    fn sync_query(&mut self, cx: &mut Context<Self>) {
        let committed = match &self.marked {
            Some(marked) => [&self.text[..marked.start], &self.text[marked.end..]].concat(),
            None => self.text.clone(),
        };
        if committed != self.query {
            self.query = committed.clone();
            cx.emit(TextFieldEvent::Changed(committed));
        }
        cx.notify();
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
            "left" if mods.alt => previous_word(&self.text, cursor),
            "left" if !mods.shift && !self.selected.is_empty() => self.selected.start,
            "left" => previous_char(&self.text, cursor),
            "right" if mods.platform => self.text.len(),
            "right" if mods.alt => next_word(&self.text, cursor),
            "right" if !mods.shift && !self.selected.is_empty() => self.selected.end,
            "right" => next_char(&self.text, cursor),
            "up" | "home" => 0,
            "down" | "end" => self.text.len(),
            "backspace" | "delete" => {
                let forward = event.keystroke.key == "delete";
                let range = if !self.selected.is_empty() {
                    self.selected.clone()
                } else if forward {
                    cursor..if mods.alt { next_word(&self.text, cursor) } else { next_char(&self.text, cursor) }
                } else if mods.platform {
                    0..cursor
                } else if mods.alt {
                    previous_word(&self.text, cursor)..cursor
                } else {
                    previous_char(&self.text, cursor)..cursor
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
                let word = word_range_at(&self.text, index);
                self.selected = word.clone();
                self.reversed = false;
                self.history.break_merge();
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
        // 输入框只有一行，多行内容只取第一行。
        let line = text.lines().next().unwrap_or_default();
        self.record(None);
        self.replace(self.selected.clone(), line, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.text.len();
        self.reversed = false;
        self.history.break_merge();
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        copy_selection(&self.text, &self.selected, cx);
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() || self.marked.is_some() {
            return;
        }
        self.copy(&Copy, window, cx);
        self.record(None);
        self.replace(self.selected.clone(), "", cx);
    }
}

pub(super) fn previous_char(text: &str, offset: usize) -> usize {
    text[..offset].char_indices().next_back().map_or(0, |(i, _)| i)
}

pub(super) fn next_char(text: &str, offset: usize) -> usize {
    text[offset..].chars().next().map_or(text.len(), |c| offset + c.len_utf8())
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 往左到上一个词的开头：先跳过非词字符（包括换行），再跳过词字符。
pub(super) fn previous_word(text: &str, offset: usize) -> usize {
    let mut chars = text[..offset].char_indices().rev().peekable();
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

/// 往右到下一个词的末尾：先跳过非词字符（包括换行），再跳过词字符。
pub(super) fn next_word(text: &str, offset: usize) -> usize {
    let rest = &text[offset..];
    let mut chars = rest.char_indices().peekable();
    while chars.next_if(|(_, c)| !is_word_char(*c)).is_some() {}
    while chars.next_if(|(_, c)| is_word_char(*c)).is_some() {}
    offset + chars.peek().map_or(rest.len(), |(i, _)| *i)
}

/// 双击选中的范围：点在词上选整个词，否则选连续的同类字符。
pub(crate) fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    let Some(c) = text[offset..].chars().next().or_else(|| text[..offset].chars().next_back()) else {
        return offset..offset;
    };
    let same = |x: char| is_word_char(x) == is_word_char(c) && (is_word_char(c) || x == c);
    let start = text[..offset].char_indices().rev().take_while(|(_, x)| same(*x)).last().map_or(offset, |(i, _)| i);
    let end = text[offset..].char_indices().find(|(_, x)| !same(*x)).map_or(text.len(), |(i, _)| offset + i);
    start..end
}

/// 选中了文字时把它复制到剪贴板。
pub(super) fn copy_selection(text: &str, selected: &Range<usize>, cx: &mut App) {
    if !selected.is_empty() {
        cx.write_to_clipboard(ClipboardItem::new_string(text[selected.clone()].to_owned()));
    }
}

/// 输入法按 UTF-16 计的偏移换成 `text` 里的字节偏移；超出末尾的算末尾。
pub(super) fn offset_from_utf16(text: &str, utf16: usize) -> usize {
    let mut count = 0;
    for (i, c) in text.char_indices() {
        if count >= utf16 {
            return i;
        }
        count += c.len_utf16();
    }
    text.len()
}

pub(super) fn offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..offset].encode_utf16().count()
}

pub(super) fn range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    offset_from_utf16(text, range.start)..offset_from_utf16(text, range.end)
}

pub(super) fn range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    offset_to_utf16(text, range.start)..offset_to_utf16(text, range.end)
}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let field = cx.entity().downgrade();
        div()
            .id("text-field")
            .role(Role::TextInput)
            .when_some(self.label.clone(), |el, label| el.aria_label(label))
            .when_some(self.placeholder.clone(), |el, placeholder| el.aria_placeholder(placeholder))
            .aria_value(SharedString::from(self.query.clone()))
            .on_a11y_action(AccessibleAction::SetValue, move |data, _, cx| {
                if let Some(ActionData::Value(value)) = data {
                    let value = value.to_string();
                    field.update(cx, |field, cx| field.set_value(value, cx)).ok();
                }
            })
            .key_context("TextField")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|_, _: &SearchNext, _, cx| cx.emit(TextFieldEvent::Next)))
            .on_action(cx.listener(|_, _: &SearchPrevious, _, cx| cx.emit(TextFieldEvent::Previous)))
            .on_action(cx.listener(|_, _: &EndSearch, _, cx| cx.emit(TextFieldEvent::Dismiss)))
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .child(TextFieldText { field: cx.entity() })
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.text, &range);
        actual_range.replace(range_to_utf16(&self.text, &range));
        Some(self.text[range].to_owned())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: range_to_utf16(&self.text, &self.selected), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| range_to_utf16(&self.text, range))
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
            .map(|range| range_from_utf16(&self.text, &range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        // 回车、制表符等由按键绑定处理，不进文字。
        let text = without_controls(text);
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
            .map(|range| range_from_utf16(&self.text, &range))
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
                let selected = range_from_utf16(text, &selected);
                range.start + selected.start..range.start + selected.end
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
        let range = range_from_utf16(&self.text, &range);
        let x = |index| bounds.left() + layout.x_for_index(index) - self.scroll_x;
        Some(Bounds::from_corners(point(x(range.start), bounds.top()), point(x(range.end), bounds.bottom())))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.bounds?;
        let index = self.layout.as_ref()?.index_for_x(point.x - bounds.left() + self.scroll_x)?;
        Some(offset_to_utf16(&self.text, index))
    }
}

/// 输入框里的文字、选区和光标，自己排版绘制以便按位置换算字符。
struct TextFieldText {
    field: Entity<TextField>,
}

struct TextFieldTextLayout {
    line: ShapedLine,
    /// 占位文字；有内容时为 `None`。
    placeholder: Option<ShapedLine>,
    scroll_x: Pixels,
    selection: Option<PaintQuad>,
    /// 有选区时不画光标。
    caret: Option<PaintQuad>,
}

impl IntoElement for TextFieldText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextFieldText {
    type RequestLayoutState = ();
    type PrepaintState = TextFieldTextLayout;

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
    ) -> TextFieldTextLayout {
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
                    underline: Some(UnderlineStyle { color: Some(style.color), thickness: px(1.), wavy: false }),
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
        TextFieldTextLayout { line, placeholder, scroll_x, selection, caret }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut TextFieldTextLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let field = self.field.read(cx);
        let focus_handle = field.focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        let dragging = field.drag.is_some();
        window.handle_input(&focus_handle, crate::ui::input_handler::ElementInput::new(bounds, self.field.clone()), cx);

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

/// 去掉回车、换行、制表符这些控制字符：输入框只有一行，写进去的文字不带它们。
fn without_controls(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_text_drops_control_characters() {
        assert_eq!(without_controls("a\nb\r\tc\u{7f}中"), "abc中");
    }

    fn at(text: &str) -> impl FnOnce() -> Snapshot {
        let text = text.to_owned();
        move || Snapshot { selected: text.len()..text.len(), text, reversed: false }
    }

    #[test]
    fn same_kind_edits_merge_until_the_caret_moves() {
        let mut history = History::default();
        history.record(Some(EditKind::Insert), at(""));
        history.record(Some(EditKind::Insert), at("a"));
        history.record(Some(EditKind::Delete), at("ab"));
        history.break_merge();
        history.record(Some(EditKind::Delete), at("a"));
        history.record(None, at(""));
        history.record(None, at("x"));
        let undone: Vec<_> = std::iter::from_fn(|| history.undo(at("now")()).map(|s| s.text)).collect();
        assert_eq!(undone, ["x", "", "a", "ab", ""]);
        assert_eq!(history.redo(at("")()).map(|s| s.text).as_deref(), Some("now"));
        // 撤销过后再打字从新的一步开始，重做的记录清掉。
        history.record(Some(EditKind::Insert), at("q"));
        assert!(history.redo(at("")()).is_none());
    }
}
