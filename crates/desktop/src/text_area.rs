//! 多行文字输入框：Git 面板写提交说明用。
//!
//! 写法照搜索栏的 `SearchField`：自己管文字编辑（光标、选区、鼠标点选拖选、输入法组字、
//! 撤销重做），自己排版绘制以便按位置换算字符。多出来的是按宽度软换行、换行符硬换行、上下按
//! 视觉行移动，以及高度随行数在上下限之间伸缩、超过上限时竖着滚动。字号、颜色、字体继承父元素
//! 的文本样式；边框、内边距和底色由调用方包在外面画，这里只画文字、选区、光标和提示文字。

use std::ops::Range;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, DispatchPhase, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    Render, ScrollWheelEvent, SharedString, Style, TextAlign, TextRun, TextStyle, UTF16Selection, UnderlineStyle,
    Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};

use crate::{
    search_bar::{Cut, Redo, Undo},
    terminal_view::{Copy, Paste, SelectAll},
};

actions!(
    runode,
    [
        /// 在多行输入框里按 cmd-enter：提交。
        SubmitText
    ]
);

const CARET_WIDTH: Pixels = px(1.5);
/// 撤销最多能退回的步数。
const UNDO_LIMIT: usize = 100;
/// 选区跨过换行符时行尾多涂这么宽，看得出换行也选上了。
const NEWLINE_WIDTH: Pixels = px(5.);
/// 提示文字往右让开光标的距离。
const PLACEHOLDER_INSET: Pixels = px(2.);

pub enum TextAreaEvent {
    /// 已经确定的文字变了，不含组字的变化。
    Changed,
    /// 按了 cmd-enter。
    Submit,
}

pub struct TextArea {
    focus_handle: FocusHandle,
    /// 全部文字；输入法组字时也包括正在组的字。
    content: String,
    /// 选区，按字节偏移；为空时就是光标位置。
    selected: Range<usize>,
    /// 光标在选区开头，即选区是从右往左选出来的。
    reversed: bool,
    /// 光标正好停在软换行处时画在上一行末尾而不是下一行开头：按 End、点在行尾后面时这样。
    upstream: bool,
    /// 连续上下移动时要保持的横坐标；别的移动和编辑后清空。
    goal_x: Option<Pixels>,
    /// 正在组的字在 `content` 里的范围。
    marked: Option<Range<usize>>,
    /// 去掉组字后的文字，即 `text()` 交出去的。
    committed: String,
    /// 空着时画的提示文字。
    placeholder: SharedString,
    /// 高度在这么多行之间伸缩。
    min_lines: usize,
    max_lines: usize,
    /// 按住鼠标拖选中；双击、三击后拖动按词、按行扩展。
    drag: Option<Drag>,
    /// 撤销、重做用的编辑前状态。
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// 上一次编辑的类型，连续打字或连续删除合成一步撤销；挪光标后清空。
    last_edit: Option<EditKind>,
    /// 文字每改一次加一，用来判断 `rows` 是不是按当前文字排的。
    version: u64,
    /// 排好的视觉行和排它时的 `version`。按键、鼠标、输入法换算位置都靠它；两帧之间文字改过
    /// 时按 `metrics` 重排。
    rows: Vec<Row>,
    rows_version: Option<u64>,
    /// 上一帧的文本样式、折行宽度和输入框位置。
    metrics: Option<Metrics>,
    /// 竖向滚动量。
    scroll_y: Pixels,
    /// 光标动过或文字改过，下一帧滚到让光标露出来；滚轮滚动时不设，免得又被拉回去。
    autoscroll: bool,
}

struct Metrics {
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
    wrap_width: Pixels,
    bounds: Bounds<Pixels>,
}

enum Drag {
    Char,
    /// 双击选中的词、三击选中的行，拖动时它始终在选区里。
    Word(Range<usize>),
    Line(Range<usize>),
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

impl EventEmitter<TextAreaEvent> for TextArea {}

impl TextArea {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            selected: 0..0,
            reversed: false,
            upstream: false,
            goal_x: None,
            marked: None,
            committed: String::new(),
            placeholder: SharedString::default(),
            min_lines: 1,
            max_lines: 10,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            version: 0,
            rows: Vec::new(),
            rows_version: None,
            metrics: None,
            scroll_y: px(0.),
            autoscroll: false,
        }
    }

    /// 已经确定的文字，不含正在组的字。
    pub fn text(&self) -> &str {
        &self.committed
    }

    /// 换掉全部文字（可撤销），光标放到末尾，发 `Changed`。
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        let text = normalize_newlines(&text);
        self.record(None);
        self.selected = text.len()..text.len();
        self.reversed = false;
        self.upstream = false;
        self.goal_x = None;
        self.marked = None;
        self.content = text;
        // 文字没变时 `sync` 不发，这里照样发一次，调用方不必区分。
        if !self.sync(cx) {
            cx.emit(TextAreaEvent::Changed);
        }
    }

    /// 空着时画的提示文字，颜色调淡。
    pub fn set_placeholder(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.placeholder = text;
        cx.notify();
    }

    /// 高度随内容在 `min..=max` 行之间变，超过 `max` 行时竖着滚动，始终让光标露出来。默认 1..=10。
    pub fn set_line_limits(&mut self, min: usize, max: usize, cx: &mut Context<Self>) {
        self.min_lines = min.max(1);
        self.max_lines = max.max(self.min_lines);
        self.autoscroll = true;
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    fn move_to(&mut self, offset: usize, upstream: bool, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        self.upstream = upstream;
        self.moved(cx);
    }

    /// 选区的另一端不动，把光标这一端挪到 `offset`。
    fn select_to(&mut self, offset: usize, upstream: bool, cx: &mut Context<Self>) {
        let anchor = if self.reversed { self.selected.end } else { self.selected.start };
        self.reversed = offset < anchor;
        self.selected = offset.min(anchor)..offset.max(anchor);
        self.upstream = upstream;
        self.moved(cx);
    }

    /// 选中 `range`，光标在末尾。
    fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selected = range;
        self.reversed = false;
        self.upstream = false;
        self.moved(cx);
    }

    /// 光标或选区挪过之后：断开撤销的合并，忘掉上下移动的横坐标，下一帧让光标露出来。
    fn moved(&mut self, cx: &mut Context<Self>) {
        self.last_edit = None;
        self.goal_x = None;
        self.autoscroll = true;
        cx.notify();
    }

    /// 拖选到鼠标所在的字符；双击、三击后的拖选按整词、整行扩展。
    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let (index, upstream) = self.index_for_point(position, window);
        let (anchor, unit) = match &self.drag {
            Some(Drag::Char) => return self.select_to(index, upstream, cx),
            Some(Drag::Word(anchor)) => (anchor.clone(), word_range_at(&self.content, index)),
            Some(Drag::Line(anchor)) => (anchor.clone(), line_range_at(&self.content, index)),
            None => return,
        };
        self.reversed = unit.start < anchor.start;
        self.selected = unit.start.min(anchor.start)..unit.end.max(anchor.end);
        self.upstream = false;
        self.moved(cx);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.content.clone(),
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
        self.content = snapshot.text;
        self.selected = snapshot.selected;
        self.reversed = snapshot.reversed;
        self.upstream = false;
        self.goal_x = None;
        self.marked = None;
        self.last_edit = None;
        self.sync(cx);
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
        self.content.replace_range(range.clone(), new_text);
        let end = range.start + new_text.len();
        self.selected = end..end;
        self.reversed = false;
        self.upstream = false;
        self.goal_x = None;
        self.marked = None;
        self.sync(cx);
    }

    /// 文字改过之后：作废排好的视觉行，去掉组字后的文字有变化时发 `Changed`，返回发没发。
    fn sync(&mut self, cx: &mut Context<Self>) -> bool {
        self.version += 1;
        self.autoscroll = true;
        let committed = match &self.marked {
            Some(marked) => [&self.content[..marked.start], &self.content[marked.end..]].concat(),
            None => self.content.clone(),
        };
        let changed = committed != self.committed;
        if changed {
            self.committed = committed;
            cx.emit(TextAreaEvent::Changed);
        }
        cx.notify();
        changed
    }

    /// 文字改过而还没重画时按上一帧的样式和宽度重排视觉行；一次都没画过时每段硬换行算一行。
    fn ensure_rows(&mut self, window: &mut Window) {
        if self.rows_version == Some(self.version) {
            return;
        }
        let rows = match &self.metrics {
            Some(metrics) => window
                .text_system()
                .shape_text(
                    self.content.clone().into(),
                    metrics.font_size,
                    &self.runs(&metrics.style),
                    Some(metrics.wrap_width),
                    None,
                )
                .map_or_else(|_| plain_rows(&self.content, metrics.font_size), |lines| rows_from_lines(&lines)),
            None => plain_rows(&self.content, px(1.)),
        };
        self.rows = rows;
        self.rows_version = Some(self.version);
    }

    /// 窗口里的一点对应的光标位置。
    fn index_for_point(&mut self, position: Point<Pixels>, window: &mut Window) -> (usize, bool) {
        self.ensure_rows(window);
        let Some(metrics) = &self.metrics else {
            return (self.content.len(), false);
        };
        let at = point(position.x - metrics.bounds.left(), position.y - metrics.bounds.top() + self.scroll_y);
        index_at_point(&self.rows, at, metrics.line_height)
    }

    /// 排版用的样式段：正在组的字加下划线。
    fn runs(&self, style: &TextStyle) -> Vec<TextRun> {
        let run = |len: usize| TextRun {
            len,
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        match &self.marked {
            Some(marked) => [
                run(marked.start),
                TextRun {
                    underline: Some(UnderlineStyle {
                        color: Some(style.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run(marked.len())
                },
                run(self.content.len() - marked.end),
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run(self.content.len())],
        }
    }

    /// 内容比框高出多少，即最多能往下滚多少。
    fn max_scroll(&self) -> Pixels {
        let Some(metrics) = &self.metrics else {
            return px(0.);
        };
        (metrics.line_height * self.rows.len() - metrics.bounds.size.height).max(px(0.))
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // 组字时这些键归输入法；ctrl 组合不在这里处理。
        let mods = &event.keystroke.modifiers;
        if self.marked.is_some() || mods.control {
            return;
        }
        self.ensure_rows(window);
        let cursor = self.cursor();
        let key = event.keystroke.key.as_str();
        // 上下移动后要接着保持的横坐标。
        let mut goal_x = None;
        let (target, upstream) = match key {
            // cmd-enter 由按键绑定发 `SubmitText`，不会走到这里。
            "enter" if !mods.platform => {
                self.record(None);
                self.replace(self.selected.clone(), "\n", cx);
                cx.stop_propagation();
                return;
            }
            "left" if mods.platform => (row_start(&self.rows, cursor, self.upstream), false),
            "left" if mods.alt => (previous_word(&self.content, cursor), false),
            "left" if !mods.shift && !self.selected.is_empty() => (self.selected.start, false),
            "left" => (previous_char(&self.content, cursor), false),
            "right" if mods.platform => row_end(&self.rows, cursor, self.upstream),
            "right" if mods.alt => (next_word(&self.content, cursor), false),
            "right" if !mods.shift && !self.selected.is_empty() => (self.selected.end, false),
            "right" => (next_char(&self.content, cursor), false),
            "up" if mods.platform => (0, false),
            "down" if mods.platform => (self.content.len(), false),
            "up" | "down" => {
                let up = key == "up";
                // 有选区时不按 shift：从选区靠移动方向的那一头出发。
                let (from, upstream) = match (mods.shift || self.selected.is_empty(), up) {
                    (true, _) => (cursor, self.upstream),
                    (false, true) => (self.selected.start, false),
                    (false, false) => (self.selected.end, false),
                };
                let goal = self.goal_x.unwrap_or_else(|| caret_x(&self.rows, from, upstream));
                goal_x = Some(goal);
                vertical(&self.rows, from, upstream, goal, up)
            }
            "home" => (row_start(&self.rows, cursor, self.upstream), false),
            "end" => row_end(&self.rows, cursor, self.upstream),
            "backspace" | "delete" => {
                let forward = key == "delete";
                let range = if !self.selected.is_empty() {
                    self.selected.clone()
                } else if forward {
                    let end = if mods.alt {
                        next_word(&self.content, cursor)
                    } else if mods.platform {
                        row_end(&self.rows, cursor, self.upstream).0
                    } else {
                        cursor
                    };
                    // 已经在行尾时 cmd-delete 删掉后面一个字符，和 cmd-backspace 对称。
                    cursor..if end > cursor { end } else { next_char(&self.content, cursor) }
                } else {
                    let start = if mods.alt {
                        previous_word(&self.content, cursor)
                    } else if mods.platform {
                        row_start(&self.rows, cursor, self.upstream)
                    } else {
                        cursor
                    };
                    // 已经在视觉行首时 cmd-backspace 退一个字符：在硬换行处就是删掉换行符，和上一行并起来。
                    (if start < cursor { start } else { previous_char(&self.content, cursor) })..cursor
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
            self.select_to(target, upstream, cx);
        } else {
            self.move_to(target, upstream, cx);
        }
        self.goal_x = goal_x;
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        // 组字时点击不改选区，免得把正在组的字拆开。
        if self.marked.is_some() {
            return;
        }
        let (index, upstream) = self.index_for_point(event.position, window);
        self.drag = match event.click_count {
            2 => {
                let word = word_range_at(&self.content, index);
                self.select_range(word.clone(), cx);
                Some(Drag::Word(word))
            }
            n if n >= 3 => {
                let line = line_range_at(&self.content, index);
                self.select_range(line.clone(), cx);
                Some(Drag::Line(line))
            }
            _ => {
                if event.modifiers.shift {
                    self.select_to(index, upstream, cx);
                } else {
                    self.move_to(index, upstream, cx);
                }
                Some(Drag::Char)
            }
        };
    }

    /// 内容超出框高时滚轮竖着滚动；没超出时交给外层。
    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let max = self.max_scroll();
        let Some(metrics) = &self.metrics else {
            return;
        };
        if max <= px(0.) {
            return;
        }
        let delta = event.delta.pixel_delta(metrics.line_height).y;
        self.scroll_y = (self.scroll_y - delta).clamp(px(0.), max);
        self.autoscroll = false;
        cx.stop_propagation();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = normalize_newlines(&text);
        if text.is_empty() && self.selected.is_empty() {
            return;
        }
        self.record(None);
        self.replace(self.selected.clone(), &text, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_range(0..self.content.len(), cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected.clone()].to_owned()));
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
        for (i, c) in self.content.char_indices() {
            if count >= utf16 {
                return i;
            }
            count += c.len_utf16();
        }
        self.content.len()
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].encode_utf16().count()
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
}

impl Focusable for TextArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("TextArea")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|_, _: &SubmitText, _, cx| cx.emit(TextAreaEvent::Submit)))
            .w_full()
            .cursor_text()
            .child(TextAreaText { area: cx.entity() })
    }
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
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
        self.sync(cx);
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
        // 回车由 `key_down` 插入换行，制表符不插入；输入法上屏的文字里真有换行时照样保留。
        let typed = text;
        let text: String = normalize_newlines(typed).chars().filter(|&c| c == '\n' || !c.is_control()).collect();
        // 只有控制字符（比如按 tab 送来的 `\t`）时什么都不做，免得把选中的文字删掉。
        if text.is_empty() && self.marked.is_none() && (range.is_empty() || !typed.is_empty()) {
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
        self.content.replace_range(range.clone(), text);
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
        self.upstream = false;
        self.goal_x = None;
        self.sync(cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.ensure_rows(window);
        let line_height = self.metrics.as_ref()?.line_height;
        let range = self.range_from_utf16(&range);
        let ix = row_at(&self.rows, range.start, false);
        let row = self.rows.get(ix)?;
        // 跨行的范围只框住开头那一行，够输入法摆候选窗了。
        let top = bounds.top() + line_height * ix - self.scroll_y;
        let left = bounds.left() + x_in_row(row, range.start);
        let right = bounds.left() + x_in_row(row, range.end.min(row.range.end));
        Some(Bounds::from_corners(point(left, top), point(right, top + line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.metrics.as_ref()?;
        let (index, _) = self.index_for_point(point, window);
        Some(self.offset_to_utf16(index))
    }
}

/// 一个视觉行：一段硬换行按宽度折出来的一行。
#[derive(Clone, Debug, PartialEq)]
struct Row {
    /// 在全文里的字节范围，不含换行符。
    range: Range<usize>,
    /// 行尾是软换行，`range.end` 同时是下一行的开头。
    soft: bool,
    /// 行里能停光标的位置和它相对行首的横坐标，按偏移升序；第一个是行首，最后一个是行尾，空行只有一个。
    stops: Vec<(usize, Pixels)>,
}

/// 把一段硬换行（在全文里从 `line_start` 起、长 `len` 字节）在折行处 `breaks`（行内偏移）
/// 切成视觉行。`glyphs` 是各字形的行内偏移和不折行时的横坐标，按偏移升序；`width` 是整段
/// 不折行时的宽度。
fn split_line(line_start: usize, len: usize, glyphs: &[(usize, Pixels)], breaks: &[usize], width: Pixels) -> Vec<Row> {
    // 和 `LineLayout::x_for_index` 一样：第一个不早于 `index` 的字形的位置，都没有时是行宽。
    let x_at = |index: usize| {
        let ix = glyphs.partition_point(|(i, _)| *i < index);
        glyphs.get(ix).map_or(width, |(_, x)| *x)
    };
    let mut rows = Vec::with_capacity(breaks.len() + 1);
    let mut start = 0;
    for (n, end) in breaks.iter().copied().chain([len]).enumerate() {
        let origin = x_at(start);
        let mut stops = vec![(line_start + start, px(0.))];
        let first = glyphs.partition_point(|(i, _)| *i <= start);
        for &(i, x) in glyphs[first..].iter().take_while(|(i, _)| *i < end) {
            // 几个字形算在同一个字符上时只留第一个。
            if stops.last().is_some_and(|(last, _)| *last < line_start + i) {
                stops.push((line_start + i, x - origin));
            }
        }
        // 空行的行首就是行尾，只留一个。
        if end > start {
            stops.push((line_start + end, x_at(end) - origin));
        }
        rows.push(Row {
            range: line_start + start..line_start + end,
            soft: n < breaks.len(),
            stops,
        });
        start = end;
    }
    rows
}

/// `shape_text` 排出来的各段硬换行切成视觉行。
fn rows_from_lines(lines: &[WrappedLine]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in lines {
        let layout = &line.unwrapped_layout;
        let glyphs: Vec<(usize, Pixels)> = layout
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| (glyph.index, glyph.position.x)))
            .collect();
        let breaks: Vec<usize> = line
            .wrap_boundaries()
            .iter()
            .map(|boundary| layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index)
            .collect();
        rows.extend(split_line(line_start, line.len(), &glyphs, &breaks, layout.width));
        line_start += line.len() + 1;
    }
    rows
}

/// 还没排过版时的退路，也给测试用：每段硬换行一行，每个字符宽 `char_width`。
fn plain_rows(text: &str, char_width: Pixels) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let glyphs: Vec<(usize, Pixels)> =
            line.char_indices().enumerate().map(|(n, (i, _))| (i, char_width * n as f32)).collect();
        let width = char_width * line.chars().count() as f32;
        rows.extend(split_line(line_start, line.len(), &glyphs, &[], width));
        line_start += line.len() + 1;
    }
    rows
}

/// `offset` 所在的视觉行。软换行处的偏移既是上一行的行尾也是下一行的行首，`upstream` 时算
/// 上一行。
fn row_at(rows: &[Row], offset: usize, upstream: bool) -> usize {
    let ix = rows.partition_point(|row| row.range.start <= offset).saturating_sub(1);
    if upstream && ix > 0 && rows[ix].range.start == offset && rows[ix - 1].soft { ix - 1 } else { ix }
}

/// 行里 `offset` 处光标的横坐标，相对行首。
fn x_in_row(row: &Row, offset: usize) -> Pixels {
    row.stops.iter().find(|(i, _)| *i >= offset).or(row.stops.last()).map_or(px(0.), |(_, x)| *x)
}

fn caret_x(rows: &[Row], offset: usize, upstream: bool) -> Pixels {
    rows.get(row_at(rows, offset, upstream)).map_or(px(0.), |row| x_in_row(row, offset))
}

/// 行里离横坐标 `x` 最近的光标位置；落在软换行的行尾时一并返回 `upstream`。
fn index_in_row(row: &Row, x: Pixels) -> (usize, bool) {
    let mut best = row.stops[0];
    for &stop in &row.stops[1..] {
        if (stop.1 - x).abs() < (best.1 - x).abs() {
            best = stop;
        }
    }
    (best.0, row.soft && best.0 == row.range.end)
}

/// 相对内容左上角（已算上滚动）的一点对应的光标位置：在第一行上面算全文开头，在最后一行
/// 下面算全文末尾。
fn index_at_point(rows: &[Row], at: Point<Pixels>, line_height: Pixels) -> (usize, bool) {
    if at.y < px(0.) {
        return (0, false);
    }
    match rows.get((at.y / line_height) as usize) {
        Some(row) => index_in_row(row, at.x),
        None => (rows.last().map_or(0, |row| row.range.end), false),
    }
}

/// 从 `offset` 往上或往下挪一个视觉行，停在横坐标最接近 `goal_x` 的位置；第一行再往上到全文
/// 开头，最后一行再往下到全文末尾。
fn vertical(rows: &[Row], offset: usize, upstream: bool, goal_x: Pixels, up: bool) -> (usize, bool) {
    let row = row_at(rows, offset, upstream);
    let target = if up { row.checked_sub(1) } else { Some(row + 1).filter(|&next| next < rows.len()) };
    match target {
        Some(target) => index_in_row(&rows[target], goal_x),
        None if up => (0, false),
        None => (rows.last().map_or(0, |row| row.range.end), false),
    }
}

/// 光标所在视觉行的行首。
fn row_start(rows: &[Row], offset: usize, upstream: bool) -> usize {
    rows.get(row_at(rows, offset, upstream)).map_or(0, |row| row.range.start)
}

/// 光标所在视觉行的行尾；行尾是软换行时光标画在这一行末尾。
fn row_end(rows: &[Row], offset: usize, upstream: bool) -> (usize, bool) {
    rows.get(row_at(rows, offset, upstream)).map_or((offset, false), |row| (row.range.end, row.soft))
}

/// 选区在各视觉行上要涂的横向范围：（行号，起点，终点），相对行首。选区跨过换行符的行在行尾
/// 多涂 `NEWLINE_WIDTH`，整行都选中的空行也看得出来。
fn selection_spans(rows: &[Row], selected: &Range<usize>) -> Vec<(usize, Pixels, Pixels)> {
    rows.iter()
        .enumerate()
        .filter_map(|(ix, row)| {
            let start = selected.start.max(row.range.start);
            let end = selected.end.min(row.range.end);
            let newline = !row.soft && selected.start <= row.range.end && selected.end > row.range.end;
            if start > end || (start == end && !newline) {
                return None;
            }
            let right = x_in_row(row, end) + if newline { NEWLINE_WIDTH } else { px(0.) };
            Some((ix, x_in_row(row, start), right))
        })
        .collect()
}

fn previous_char(text: &str, offset: usize) -> usize {
    text[..offset].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_char(text: &str, offset: usize) -> usize {
    text[offset..].chars().next().map_or(text.len(), |c| offset + c.len_utf8())
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 往左到上一个词的开头：先跳过非词字符（包括换行），再跳过词字符。
fn previous_word(text: &str, offset: usize) -> usize {
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
fn next_word(text: &str, offset: usize) -> usize {
    let rest = &text[offset..];
    let mut chars = rest.char_indices().peekable();
    while chars.next_if(|(_, c)| !is_word_char(*c)).is_some() {}
    while chars.next_if(|(_, c)| is_word_char(*c)).is_some() {}
    offset + chars.peek().map_or(rest.len(), |(i, _)| *i)
}

/// `offset` 所在那段硬换行的范围，不含换行符。
fn hard_line_at(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    start..end
}

/// 三击选中的范围：所在那段硬换行连同行尾的换行符。
fn line_range_at(text: &str, offset: usize) -> Range<usize> {
    let line = hard_line_at(text, offset);
    line.start..if line.end < text.len() { line.end + 1 } else { line.end }
}

/// 双击选中的范围：点在词上选整个词，否则选连续的同类字符；不越过换行。
fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    let line = hard_line_at(text, offset);
    let (text, offset) = (&text[line.clone()], offset - line.start);
    let Some(c) = text[offset..].chars().next().or_else(|| text[..offset].chars().next_back()) else {
        return line.start + offset..line.start + offset;
    };
    let same = |x: char| is_word_char(x) == is_word_char(c) && (is_word_char(c) || x == c);
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, x)| same(*x))
        .last()
        .map_or(offset, |(i, _)| i);
    let end = text[offset..]
        .char_indices()
        .find(|(_, x)| !same(*x))
        .map_or(text.len(), |(i, _)| offset + i);
    line.start + start..line.start + end
}

/// 换行统一成 `\n`：`\r\n` 和单独的 `\r` 都换掉。
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 留给光标的宽度之外才是折行宽度，免得行尾的光标被裁掉。
fn wrap_width(width: Pixels) -> Pixels {
    (width - CARET_WIDTH).max(px(1.))
}

/// 提示文字往右让开了光标，折行宽度跟着减。
fn placeholder_wrap_width(width: Pixels) -> Pixels {
    (wrap_width(width) - CARET_WIDTH - PLACEHOLDER_INSET).max(px(1.))
}

/// 提示文字的样式段：颜色调淡。
fn placeholder_run(style: &TextStyle, len: usize) -> TextRun {
    TextRun {
        len,
        font: style.font(),
        color: style.color.opacity(0.4),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// 输入框里的文字、选区和光标，自己排版绘制以便按位置换算字符。
struct TextAreaText {
    area: Entity<TextArea>,
}

struct TextAreaTextLayout {
    /// 每段硬换行排好的文字。
    lines: Vec<WrappedLine>,
    /// 提示文字；有内容时为 `None`。
    placeholder: Option<Vec<WrappedLine>>,
    rows: Vec<Row>,
    /// 排这一帧时文字的 `version`。
    version: u64,
    style: TextStyle,
    font_size: Pixels,
    line_height: Pixels,
    wrap_width: Pixels,
    scroll_y: Pixels,
    selections: Vec<PaintQuad>,
    /// 有选区时不画光标。
    caret: Option<PaintQuad>,
}

impl IntoElement for TextAreaText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// 一组排好的硬换行一共占几个视觉行。
fn row_count(lines: &[WrappedLine]) -> usize {
    lines.iter().map(|line| line.wrap_boundaries().len() + 1).sum()
}

/// 画一组排好的硬换行，左上角在 `origin`；整段落在 `clip` 上下之外的跳过。
fn paint_lines(
    lines: &[WrappedLine],
    origin: Point<Pixels>,
    line_height: Pixels,
    clip: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut top = origin.y;
    for line in lines {
        let bottom = top + line_height * (line.wrap_boundaries().len() + 1);
        if bottom > clip.top() && top < clip.bottom() {
            line.paint(point(origin.x, top), line_height, TextAlign::Left, None, window, cx).ok();
        }
        top = bottom;
    }
}

impl Element for TextAreaText {
    type RequestLayoutState = ();
    type PrepaintState = TextAreaTextLayout;

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
        _: &mut App,
    ) -> (LayoutId, ()) {
        // 高度要等知道宽度、折好行才定，所以量尺寸放到排版时做；那时文本样式已经出栈，先取好。
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let area = self.area.clone();
        let mut layout = Style::default();
        layout.size.width = relative(1.).into();
        let id = window.request_measured_layout(layout, move |known, available, window, cx| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let area = area.read(cx);
            // 和 `prepaint` 用一样的折行宽度和样式段，排好的结果在同一帧里能复用。
            let count = |text: SharedString, runs: &[TextRun], wrap: Option<Pixels>, window: &mut Window| match wrap {
                Some(wrap) => window
                    .text_system()
                    .shape_text(text.clone(), font_size, runs, Some(wrap), None)
                    .map_or_else(|_| text.split('\n').count(), |lines| row_count(&lines)),
                None => text.split('\n').count(),
            };
            let mut rows = count(area.content.clone().into(), &area.runs(&style), width.map(wrap_width), window);
            // 空着时提示文字折成几行就撑几行，免得被裁掉。
            if area.content.is_empty() && !area.placeholder.is_empty() {
                let run = placeholder_run(&style, area.placeholder.len());
                let wrap = width.map(placeholder_wrap_width);
                rows = rows.max(count(area.placeholder.clone(), &[run], wrap, window));
            }
            size(width.unwrap_or_default(), line_height * rows.clamp(area.min_lines, area.max_lines))
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> TextAreaTextLayout {
        let area = self.area.read(cx);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let wrap_width = wrap_width(bounds.size.width);
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(area.content.clone().into(), font_size, &area.runs(&style), Some(wrap_width), None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();
        let rows = rows_from_lines(&lines);
        let placeholder = (area.content.is_empty() && !area.placeholder.is_empty()).then(|| {
            let run = placeholder_run(&style, area.placeholder.len());
            let wrap = placeholder_wrap_width(bounds.size.width);
            window
                .text_system()
                .shape_text(area.placeholder.clone(), font_size, &[run], Some(wrap), None)
                .map(|lines| lines.into_iter().collect())
                .unwrap_or_default()
        });

        // 竖向滚动：光标动过时滚到它露出来，内容变短时也别在下面留空。
        let cursor = area.cursor();
        let caret_row = row_at(&rows, cursor, area.upstream);
        let max_scroll = (line_height * rows.len() - bounds.size.height).max(px(0.));
        let mut scroll_y = area.scroll_y;
        if area.autoscroll {
            let top = line_height * caret_row;
            if top < scroll_y {
                scroll_y = top;
            }
            if top + line_height > scroll_y + bounds.size.height {
                scroll_y = top + line_height - bounds.size.height;
            }
        }
        let scroll_y = scroll_y.clamp(px(0.), max_scroll);

        let row_top = |row: usize| bounds.top() - scroll_y + line_height * row;
        let selections = selection_spans(&rows, &area.selected)
            .into_iter()
            .map(|(row, left, right)| {
                fill(
                    Bounds::from_corners(
                        point(bounds.left() + left, row_top(row)),
                        point(bounds.left() + right, row_top(row) + line_height),
                    ),
                    style.color.opacity(0.3),
                )
            })
            .collect();
        let caret = (area.selected.is_empty() && !rows.is_empty()).then(|| {
            let height = (font_size * 1.25).min(line_height);
            fill(
                Bounds::new(
                    point(
                        bounds.left() + x_in_row(&rows[caret_row], cursor),
                        row_top(caret_row) + (line_height - height) / 2.,
                    ),
                    size(CARET_WIDTH, height),
                ),
                style.color,
            )
        });
        TextAreaTextLayout {
            lines,
            placeholder,
            rows,
            version: area.version,
            style,
            font_size,
            line_height,
            wrap_width,
            scroll_y,
            selections,
            caret,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut TextAreaTextLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        let area = self.area.read(cx);
        let focus_handle = area.focus_handle.clone();
        let focused = focus_handle.is_focused(window);
        let dragging = area.drag.is_some();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.area.clone()), cx);

        let line_height = layout.line_height;
        let origin = point(bounds.left(), bounds.top() - layout.scroll_y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for selection in layout.selections.drain(..) {
                window.paint_quad(selection);
            }
            paint_lines(&layout.lines, origin, line_height, bounds, window, cx);
            // 没有内容时光标停在提示文字前面。
            if let Some(placeholder) = &layout.placeholder {
                let origin = point(origin.x + CARET_WIDTH + PLACEHOLDER_INSET, origin.y);
                paint_lines(placeholder, origin, line_height, bounds, window, cx);
            }
            if focused && let Some(caret) = layout.caret.take() {
                window.paint_quad(caret);
            }
        });

        // 拖选时鼠标出了输入框也要跟着选，所以监听整个窗口的移动和松开。
        if dragging {
            let area = self.area.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left) {
                    area.update(cx, |area, cx| area.drag_to(event.position, window, cx));
                }
            });
            let area = self.area.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    area.update(cx, |area, _| area.drag = None);
                }
            });
        }

        let metrics = Metrics {
            style: layout.style.clone(),
            font_size: layout.font_size,
            line_height,
            wrap_width: layout.wrap_width,
            bounds,
        };
        let rows = std::mem::take(&mut layout.rows);
        let (version, scroll_y) = (layout.version, layout.scroll_y);
        self.area.update(cx, |area, _| {
            area.rows = rows;
            area.rows_version = Some(version);
            area.metrics = Some(metrics);
            area.scroll_y = scroll_y;
            area.autoscroll = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: Pixels = px(10.);

    /// 每个字符宽 10，每段硬换行按 `columns` 个字符硬折：测试用的等宽排版。
    fn wrapped(text: &str, columns: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in text.split('\n') {
            let indices: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
            let glyphs: Vec<(usize, Pixels)> = indices.iter().enumerate().map(|(n, &i)| (i, W * n as f32)).collect();
            let breaks: Vec<usize> = indices.iter().copied().skip(columns).step_by(columns).collect();
            rows.extend(split_line(line_start, line.len(), &glyphs, &breaks, W * indices.len() as f32));
            line_start += line.len() + 1;
        }
        rows
    }

    #[test]
    fn split_line_makes_rows_with_stops_relative_to_row_start() {
        let rows = wrapped("abcdefg\nhi", 3);
        let ranges: Vec<_> = rows.iter().map(|row| (row.range.clone(), row.soft)).collect();
        assert_eq!(ranges, [(0..3, true), (3..6, true), (6..7, false), (8..10, false)]);
        assert_eq!(rows[1].stops, [(3, px(0.)), (4, W), (5, W * 2.), (6, W * 3.)]);
        assert_eq!(rows[2].stops, [(6, px(0.)), (7, W)]);
    }

    #[test]
    fn empty_text_and_trailing_newline_still_have_rows() {
        assert_eq!(plain_rows("", W), [Row { range: 0..0, soft: false, stops: vec![(0, px(0.))] }]);
        let rows = plain_rows("ab\n", W);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].range, 3..3);
    }

    #[test]
    fn soft_wrap_offset_belongs_to_next_row_unless_upstream() {
        let rows = wrapped("abcdef", 3);
        assert_eq!(row_at(&rows, 3, false), 1);
        assert_eq!(row_at(&rows, 3, true), 0);
        assert_eq!(caret_x(&rows, 3, false), px(0.));
        assert_eq!(caret_x(&rows, 3, true), W * 3.);
        // 硬换行处没有歧义：换行符前面是上一行的行尾，后面是下一行的行首。
        let rows = wrapped("abc\ndef", 3);
        assert_eq!(row_at(&rows, 3, false), 0);
        assert_eq!(row_at(&rows, 4, true), 1);
    }

    #[test]
    fn vertical_moves_keep_goal_x_and_land_on_row_ends() {
        // 视觉行：abcd | efgh | ij（硬换行）| klmnop 折成 klmn | op
        let text = "abcdefghij\nklmnop";
        let rows = wrapped(text, 4);
        assert_eq!(rows.len(), 5);
        // 从 c（x = 20）往下到 g，再往下到第三行只有 ij，停在行尾。
        assert_eq!(vertical(&rows, 2, false, W * 2., false), (6, false));
        assert_eq!(vertical(&rows, 6, false, W * 3., false), (10, false));
        // 保持原来的横坐标 30：从 ij 行尾继续往下回到 n。
        assert_eq!(vertical(&rows, 10, false, W * 3., false), (14, false));
        // 横坐标超过软换行的行尾：停在行尾，光标画在这一行。
        assert_eq!(vertical(&rows, 9, false, W * 9., true), (8, true));
        // 第一行再往上到全文开头，最后一行再往下到全文末尾。
        assert_eq!(vertical(&rows, 2, false, W * 2., true), (0, false));
        assert_eq!(vertical(&rows, 15, false, W, false), (text.len(), false));
        // 光标画在上一行末尾时，往下是从上一行出发。
        assert_eq!(vertical(&rows, 4, true, W * 4., false), (8, true));
        assert_eq!(vertical(&rows, 4, false, W * 0., false), (8, false));
    }

    #[test]
    fn row_start_and_end_follow_visual_rows() {
        let rows = wrapped("abcdefg", 3);
        assert_eq!(row_start(&rows, 4, false), 3);
        assert_eq!(row_end(&rows, 4, false), (6, true));
        assert_eq!(row_start(&rows, 6, true), 3);
        assert_eq!(row_end(&rows, 6, false), (7, false));
    }

    #[test]
    fn point_maps_to_row_and_clamps_outside_content() {
        let rows = wrapped("abcdef\nxy", 3);
        let lh = px(20.);
        assert_eq!(index_at_point(&rows, point(W * 1.4, px(25.)), lh), (4, false));
        assert_eq!(index_at_point(&rows, point(W * 9., px(5.)), lh), (3, true));
        assert_eq!(index_at_point(&rows, point(W * 9., px(25.)), lh), (6, false));
        assert_eq!(index_at_point(&rows, point(px(0.), px(-1.)), lh), (0, false));
        assert_eq!(index_at_point(&rows, point(px(0.), px(500.)), lh), (9, false));
    }

    #[test]
    fn selection_spans_cover_rows_and_selected_newlines() {
        let rows = wrapped("abcdef\n\nxy", 3);
        // 从 b 选到 x 后面：第一行 b..行尾，第二行整行（软换行，不多涂），第三行行尾换行，空行，x。
        let spans = selection_spans(&rows, &(1..9));
        assert_eq!(
            spans,
            [
                (0, W, W * 3.),
                (1, px(0.), W * 3. + NEWLINE_WIDTH),
                (2, px(0.), NEWLINE_WIDTH),
                (3, px(0.), W),
            ]
        );
        assert!(selection_spans(&rows, &(4..4)).is_empty());
        // 从软换行处开始的选区不在上一行画。
        assert_eq!(selection_spans(&rows, &(3..4)), [(1, px(0.), W)]);
    }

    #[test]
    fn word_moves_cross_newlines_and_double_click_stays_in_line() {
        let text = "fix: 修复\n  bug_1 now";
        assert_eq!(next_word(text, 0), 3);
        assert_eq!(next_word(text, 3), "fix: 修复".len());
        assert_eq!(next_word(text, "fix: 修复".len()), text.find(" now").unwrap());
        assert_eq!(previous_word(text, text.find("bug").unwrap()), "fix: ".len());
        assert_eq!(previous_word(text, text.len()), text.len() - 3);
        let bug = text.find("bug").unwrap();
        assert_eq!(word_range_at(text, bug + 2), bug..bug + 5);
        // 点在行尾（换行符前）选的是前面的词，不把换行选进去。
        let end = "fix: 修复".len();
        assert_eq!(word_range_at(text, end), "fix: ".len()..end);
        // 连续的空格算一段。
        assert_eq!(word_range_at(text, end + 1), end + 1..end + 3);
        assert_eq!(word_range_at("", 0), 0..0);
    }

    #[test]
    fn triple_click_selects_hard_line_with_its_newline() {
        let text = "one\ntwo\nthree";
        assert_eq!(line_range_at(text, 5), 4..8);
        assert_eq!(line_range_at(text, 10), 8..13);
        assert_eq!(line_range_at(text, 3), 0..4);
    }

    #[test]
    fn chars_step_over_multibyte_and_newlines() {
        let text = "a😀\n中";
        assert_eq!(next_char(text, 1), 5);
        assert_eq!(previous_char(text, 5), 1);
        assert_eq!(next_char(text, 5), 6);
        assert_eq!(next_char(text, text.len()), text.len());
        assert_eq!(previous_char(text, 0), 0);
    }

    #[test]
    fn newlines_are_normalized() {
        assert_eq!(normalize_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }
}
