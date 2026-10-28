//! 多行文字输入框。
//!
//! 写法照单行输入框 `InputState`：自己管文字编辑（光标、选区、鼠标点选拖选、输入法组字、
//! 撤销重做），自己排版绘制以便按位置换算字符；撤销记录 `History`、按字符和按词移动、UTF-16
//! 换算和它共用。多出来的是按宽度软换行、换行符硬换行、上下按视觉行移动，以及高度随行数在上下限
//! 之间伸缩、超过上限时竖着滚动。字号、颜色、字体继承父元素的文本样式；边框、内边距和底色由外框组件
//! `Textarea` 画，`TextareaState` 只画文字、选区、光标和提示文字。
//!
//! 按视觉行换算光标位置和选区的纯函数在 `rows`，排版绘制的元素在 `element`。

mod element;
mod rows;

use std::ops::Range;

use gpui::{
    AccessibleAction, App, Bounds, Context, ElementId, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, HighlightStyle, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, Render, Role, ScrollEvent,
    SharedString, TextRun, TextStyle, UTF16Selection, UnderlineStyle, Window, actions, combine_highlights, div, point,
    prelude::*, px,
};

use gpui::accesskit::ActionData;

use super::input::{
    EditKind, Frame, History, Snapshot, copy_selection, forward_frame, ignores_input, next_char, next_word,
    offset_to_utf16, previous_char, previous_word, range_from_utf16, range_to_utf16,
};
use crate::actions::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use element::TextareaText;
use rows::{Row, caret_x, index_at_point, plain_rows, row_at, row_end, row_start, rows_from_lines, vertical, x_in_row};

actions!(
    runode,
    [
        /// 在多行输入框里按 cmd-enter：提交。
        SubmitText
    ]
);

const CARET_WIDTH: Pixels = px(1.5);
/// 选区跨过换行符时行尾多涂这么宽，看得出换行也选上了。
const NEWLINE_WIDTH: Pixels = px(5.);
/// 提示文字往右让开光标的距离。
const PLACEHOLDER_INSET: Pixels = px(2.);

pub enum TextareaEvent {
    /// 已经确定的文字变了，不含组字的变化。
    Changed,
    /// 按了 cmd-enter。
    Submit,
}

pub struct TextareaState {
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
    /// 文字的样式段（语法高亮），按 `content` 里的字节范围从前到后排、互不重叠。改文字时跟着挪，
    /// 被改到的那部分去掉，等调用方按新文字重算后经 `set_highlights` 换上。
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// 去掉组字后的文字，即 `text()` 交出去的。
    committed: String,
    /// 空着时画的提示文字。
    placeholder: SharedString,
    /// 报给辅助工具的名字；界面上不画。
    label: Option<SharedString>,
    /// 高度在这么多行之间伸缩。
    min_lines: usize,
    max_lines: usize,
    /// 高度撑满父元素，不随行数伸缩，见 `fill_height`。
    fill: bool,
    /// 按住鼠标拖选中；双击、三击后拖动按词、按行扩展。
    drag: Option<Drag>,
    history: History,
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

impl EventEmitter<TextareaEvent> for TextareaState {}

impl TextareaState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: String::new(),
            selected: 0..0,
            reversed: false,
            upstream: false,
            goal_x: None,
            marked: None,
            highlights: Vec::new(),
            committed: String::new(),
            placeholder: SharedString::default(),
            label: None,
            min_lines: 1,
            max_lines: 10,
            fill: false,
            drag: None,
            history: History::default(),
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
        self.replace_all(text);
        // 文字没变时 `sync` 不发，这里照样发一次，调用方不必区分。
        if !self.sync(cx) {
            cx.emit(TextareaEvent::Changed);
        }
    }

    /// 换上全部文字，清空撤销记录，光标放到开头，发 `Changed`：拿来编辑一份读进来的文件时用。
    pub fn load_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.set_text(text, cx);
        self.history = History::default();
        self.move_to(0, false, cx);
    }

    /// 文字的版本，每改一次（含输入法组字）变一次；交给 `set_highlights` 认出算样式段时的文字。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 选区跨过的硬换行行号（从 0 数，不含 `end`），没选东西时是 `None`。
    pub fn selected_lines(&self) -> Option<Range<usize>> {
        lines_spanned(&self.content, self.selected.clone())
    }

    /// 换上按版本 `version` 的文字算的样式段（字节范围，从前到后不重叠）。文字已经又改过、或者正在组字时
    /// 对不上，丢掉。
    pub fn set_highlights(
        &mut self,
        version: u64,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        cx: &mut Context<Self>,
    ) {
        if version != self.version || self.marked.is_some() {
            return;
        }
        debug_assert!(highlights.iter().all(|(range, _)| range.end <= self.content.len()));
        self.highlights = highlights;
        cx.notify();
    }

    fn shift_highlights(&mut self, range: &Range<usize>, len: usize) {
        shift_spans(&mut self.highlights, range, len);
    }

    /// 整个换掉文字（撤销、`set_text`）：只当中间不同的那段改了，样式段照常跟着挪，不整篇褪色。
    fn replace_all(&mut self, text: String) {
        let (range, len) = changed_middle(&self.content, &text);
        self.shift_highlights(&range, len);
        self.content = text;
    }

    /// 在光标处插入 `text`（有选区时换掉选区，可撤销），光标停在它后面。
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.record(None);
        self.replace(self.selected.clone(), &normalize_newlines(text), cx);
    }

    /// 空着时画的提示文字，颜色调淡。
    pub fn set_placeholder(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.placeholder = text;
        cx.notify();
    }

    /// 报给辅助工具的名字换成 `label`。
    pub fn set_label(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.label = Some(label.into());
        cx.notify();
    }

    /// 高度随内容在 `min..=max` 行之间变，超过 `max` 行时竖着滚动，始终让光标露出来。
    pub fn set_line_limits(&mut self, min: usize, max: usize, cx: &mut Context<Self>) {
        self.min_lines = min.max(1);
        self.max_lines = max.max(self.min_lines);
        self.autoscroll = true;
        cx.notify();
    }

    /// 高度撑满父元素，不再按 `set_line_limits` 的行数伸缩，内容超出时竖着滚动；外框也要撑满。
    pub fn fill_height(&mut self, cx: &mut Context<Self>) {
        self.fill = true;
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
        self.history.break_merge();
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
        Snapshot { text: self.content.clone(), selected: self.selected.clone(), reversed: self.reversed }
    }

    /// 编辑前记下当前状态供撤销，见 `History::record`。
    fn record(&mut self, kind: Option<EditKind>) {
        let now = || Snapshot { text: self.content.clone(), selected: self.selected.clone(), reversed: self.reversed };
        self.history.record(kind, now);
    }

    fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.replace_all(snapshot.text);
        self.selected = snapshot.selected;
        self.reversed = snapshot.reversed;
        self.upstream = false;
        self.goal_x = None;
        self.marked = None;
        self.sync(cx);
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
        self.shift_highlights(&range, new_text.len());
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
            cx.emit(TextareaEvent::Changed);
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

    /// 排版用的样式段：叠上 `highlights`，正在组的字加下划线。
    fn runs(&self, style: &TextStyle) -> Vec<TextRun> {
        let run = |style: &TextStyle, len: usize| TextRun {
            len,
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: style.underline,
            strikethrough: None,
        };
        let plain = TextStyle { underline: None, ..style.clone() };
        let marked = self.marked.iter().map(|marked| {
            let underline = UnderlineStyle { color: Some(style.color), thickness: px(1.), wavy: false };
            (marked.clone(), HighlightStyle { underline: Some(underline), ..HighlightStyle::default() })
        });
        let mut runs = Vec::new();
        let mut at = 0;
        for (range, highlight) in combine_highlights(self.highlights.iter().cloned(), marked) {
            if range.start > at {
                runs.push(run(&plain, range.start - at));
            }
            runs.push(run(&plain.clone().highlight(highlight), range.len()));
            at = range.end;
        }
        if at < self.content.len() || runs.is_empty() {
            runs.push(run(&plain, self.content.len() - at));
        }
        runs.retain(|run| run.len > 0);
        runs
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
    fn scroll_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
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
        // 组字时输入法把 Cmd+V 交回来（`doCommandBySelector:`）也会派发到这里：换掉选区会把还没确定的字
        // 当成已经打上的，之后输入法再改它就对不上了，同 `cut`、`undo`。
        if self.marked.is_some() {
            return;
        }
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
        copy_selection(&self.content, &self.selected, cx);
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

/// 多行输入框的外框（照 shadcn 的 `Textarea`），里面是 `TextareaState`：和单行的 `Input` 一样的边框、底色、
/// 圆角和左右留白，另有上下留白，高度跟着行数。子元素画在文字后面。调用方接着写的样式盖在这些上面，见
/// `Input`。
#[derive(IntoElement)]
pub struct Textarea {
    frame: Frame<TextareaState>,
}

impl Textarea {
    pub fn new(state: &Entity<TextareaState>) -> Self {
        Self { frame: Frame::new(state) }
    }

    /// 外框的元素 id。外框仍是个 `Div`，只多了 id，用不上 `on_click` 这些要 id 的写法。
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.frame.div.interactivity().element_id = Some(id.into());
        self
    }
}

impl RenderOnce for Textarea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (div, state, children) = self.frame.finish(window, cx, |div| div.py(px(6.)));
        div.child(state).children(children)
    }
}

forward_frame!(Textarea);

impl Focusable for TextareaState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextareaState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let area = cx.entity().downgrade();
        div()
            .id("text-area")
            .role(Role::MultilineTextInput)
            .when_some(self.label.clone(), |el, label| el.aria_label(label))
            .when(!self.placeholder.is_empty(), |el| el.aria_placeholder(self.placeholder.clone()))
            .aria_value(SharedString::from(self.committed.clone()))
            // 辅助工具直接写入的文字和 `set_text` 一样：可撤销，发 `Changed`。
            .on_a11y_action(AccessibleAction::SetValue, move |data, _, cx| {
                if let Some(ActionData::Value(value)) = data {
                    let value = value.to_string();
                    area.update(cx, |area, cx| area.set_text(value, cx)).ok();
                }
            })
            .key_context("TextArea")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|_, _: &SubmitText, _, cx| cx.emit(TextareaEvent::Submit)))
            .w_full()
            .when(self.fill, |el| el.h_full())
            .cursor_text()
            .child(TextareaText { area: cx.entity() })
    }
}

impl EntityInputHandler for TextareaState {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.content, &range);
        actual_range.replace(range_to_utf16(&self.content, &range));
        Some(self.content[range].to_owned())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: range_to_utf16(&self.content, &self.selected), reversed: self.reversed })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| range_to_utf16(&self.content, range))
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
            .map(|range| range_from_utf16(&self.content, &range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        // 回车由 `key_down` 插入换行，制表符不插入；输入法上屏的文字里真有换行时照样保留。
        let typed = text;
        let text: String = normalize_newlines(typed).chars().filter(|&c| c == '\n' || !c.is_control()).collect();
        if ignores_input(typed, &text, &range, self.marked.is_some()) {
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
            .map(|range| range_from_utf16(&self.content, &range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        if self.marked.is_none() {
            self.record(Some(EditKind::Insert));
        }
        self.shift_highlights(&range, text.len());
        self.content.replace_range(range.clone(), text);
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
        let range = range_from_utf16(&self.content, &range);
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
        Some(offset_to_utf16(&self.content, index))
    }
}

/// `offset` 所在那段硬换行的范围，不含换行符。
fn hard_line_at(text: &str, offset: usize) -> Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    start..end
}

/// `range` 跨过的硬换行行号；结束在行首时不算那一行，三击选中的一行连同换行符仍是一行。
fn lines_spanned(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    if range.is_empty() {
        return None;
    }
    let line = |offset: usize| text[..offset].matches('\n').count();
    let last = if text[..range.end].ends_with('\n') { range.end - 1 } else { range.end };
    Some(line(range.start)..line(last) + 1)
}

/// 三击选中的范围：所在那段硬换行连同行尾的换行符。
fn line_range_at(text: &str, offset: usize) -> Range<usize> {
    let line = hard_line_at(text, offset);
    line.start..if line.end < text.len() { line.end + 1 } else { line.end }
}

/// 双击选中的范围：和单行输入框一样，只是不越过换行。
fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    let line = hard_line_at(text, offset);
    let word = super::input::word_range_at(&text[line.clone()], offset - line.start);
    line.start + word.start..line.start + word.end
}

/// `range` 换成了 `len` 字节的新文字：后面的样式段跟着挪，和 `range` 重叠的部分去掉。
fn shift_spans(spans: &mut Vec<(Range<usize>, HighlightStyle)>, range: &Range<usize>, len: usize) {
    let shift = |at: usize| at - range.end + range.start + len;
    let mut shifted = Vec::with_capacity(spans.len());
    for (span, style) in spans.drain(..) {
        if span.start < range.start {
            shifted.push((span.start..span.end.min(range.start), style));
        }
        if span.end > range.end {
            shifted.push((shift(span.start.max(range.end))..shift(span.end), style));
        }
    }
    *spans = shifted;
}

/// `old` 换成 `new` 时中间不同的那段：返回它在 `old` 里的范围和在 `new` 里的长度，两头落在字的边界上。
fn changed_middle(old: &str, new: &str) -> (Range<usize>, usize) {
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let room = a.len().min(b.len()) - prefix;
    let mut suffix = a.iter().rev().zip(b.iter().rev()).take(room).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(a.len() - suffix) {
        suffix -= 1;
    }
    (prefix..a.len() - suffix, b.len() - prefix - suffix)
}

/// 换行统一成 `\n`：`\r\n` 和单独的 `\r` 都换掉。
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_lines_skip_the_line_after_a_trailing_newline() {
        let text = "a\nbb\nc";
        assert_eq!(lines_spanned(text, 2..2), None);
        assert_eq!(lines_spanned(text, 2..3), Some(1..2));
        assert_eq!(lines_spanned(text, line_range_at(text, 2)), Some(1..2));
        assert_eq!(lines_spanned(text, 0..text.len()), Some(0..3));
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

    /// 改文字时样式段跟着挪：前面的不动，后面的平移，被改到的那部分去掉，范围始终落在字的边界上。
    #[test]
    fn highlights_follow_edits() {
        let style = HighlightStyle::default();
        let mut highlights = vec![(0..3, style), (4..10, style), (12..14, style)];
        shift_spans(&mut highlights, &(6..8), 1);
        let ranges: Vec<_> = highlights.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..3, 4..6, 7..9, 11..13]);
    }

    /// 撤销这类整篇换掉的改动只算中间不同的那段，样式段不整篇丢掉；切开多字节字时退到字的边界。
    #[test]
    fn whole_text_changes_touch_only_the_middle() {
        assert_eq!(changed_middle("let a = 1;", "let ab = 1;"), (5..5, 1));
        assert_eq!(changed_middle("abc", "abc"), (3..3, 0));
        assert_eq!(changed_middle("aa", "aaa"), (2..2, 1));
        assert_eq!(changed_middle("中文", "中字"), (3..6, 3));
        assert_eq!(changed_middle("x", ""), (0..1, 0));
    }

    #[test]
    fn newlines_are_normalized() {
        assert_eq!(normalize_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }
}
