//! 单个终端会话的 GPUI 视图：输入分发和单元格绘制。
//!
//! 这里是 `TerminalView` 本身、它的动作和事件，以及渲染出的元素树。其余按职责分在子模块里：
//! 建视图、启动 shell 和读输出（`lifecycle`）、按键和鼠标（`input`）、绑定的动作（`actions`）、
//! 搜索栏（`search`）、灰字建议（`suggestion`）、命令补全菜单（`completion_menu`）、
//! 输入法（`ime`）、终端网格元素（`element`），以及画一帧（`paint`）。

mod actions;
mod completion_menu;
mod element;
mod ime;
mod input;
mod lifecycle;
mod paint;
mod search;
mod suggestion;

use std::{
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Action, App, Bounds, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable, Font, Hsla,
    Pixels, Point, Render, ShapedLine, Subscription, Task, Window, actions, div, prelude::*, px, rgb,
};
use runode_config::Config;
use runode_shared_types::{color::Rgb, grid::GridSize};
use runode_terminal::{history, session::Session};

use crate::search_bar::SearchField;
use completion_menu::{CompletionMenu, PendingKey};
use element::TerminalElement;

actions!(
    runode,
    [
        Copy,
        Paste,
        PasteSelection,
        SelectAll,
        ClearScreen,
        ScrollToTop,
        ScrollToBottom,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToSelection,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize
    ]
);

/// 把这段文本原样发给程序；用来把 ⌘← 之类的快捷键映射成 shell 认识的控制字符。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct SendText(pub String);

/// 视口跳到上一个（负数）或下一个提示符。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct JumpToPrompt(pub isize);

/// 把屏幕连同回滚历史写进临时文件，再按 `ScreenFile` 处理这个文件。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct WriteScreenFile(pub ScreenFile);

#[derive(Clone, Copy, PartialEq)]
pub enum ScreenFile {
    /// 把文件路径复制到剪贴板。
    CopyPath,
    /// 把文件路径粘贴进终端。
    PastePath,
    /// 用系统默认程序打开文件。
    Open,
}

const MIN_FONT_SIZE: f32 = 6.;
const MAX_FONT_SIZE: f32 = 72.;
/// 程序没设置标题、也读不到前台进程时用的标题。
pub const DEFAULT_TITLE: &str = "Runode";
/// 发出输入后最多等这么久的回显；在那之前屏幕上的输入可能还没更新，不接受按旧屏幕算出的建议。
const ECHO_WAIT: Duration = Duration::from_millis(200);

/// 显示着的灰字建议。
struct Suggestion {
    /// 接在光标后面、还没输入的那部分。
    rest: String,
    /// 算它时读屏幕的时刻。
    read_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    cell: gpui::Size<Pixels>,
    ascent: Pixels,
    descent: Pixels,
}

/// 字形缓存按（粗体、斜体）分成四张表，查找时直接用 `&str`，命中时不必为键分配 `String`。
/// 值放在 `Rc` 里：`ShapedLine` 内联着一整块装饰数组，每格每帧复制一份会占去绘制的大头。
type GlyphCache = [HashMap<String, Rc<ShapedLine>>; 4];

/// 终端视图通知外层（标签栏）的事件。
pub enum TerminalEvent {
    /// 运行中的程序改了标题（OSC 0/2）。
    TitleChanged,
    /// 焦点进入了这个终端（包括它的搜索栏）。
    Focused,
    /// 程序响铃（BEL）。
    Bell,
    /// 前台 agent 从工作中停了下来（干完了、等着输入或者退出了）。
    AgentFinished,
    /// shell 已经退出，这个终端该关掉了。
    Exited,
}

pub struct TerminalView {
    session: Session,
    config: Arc<Config>,
    focus_handle: FocusHandle,
    font: Font,
    font_size: Pixels,
    metrics: Option<Metrics>,
    /// 按文本和样式缓存的字形排版结果；颜色在绘制时再上。
    glyphs: GlyphCache,
    /// 输入法尚未上屏的预编辑文本，画在光标处。
    marked_text: Option<String>,
    /// 程序开着鼠标上报时，精确滚动（触控板）不足一行的余量；滚回滚历史时余量由
    /// `Session::scroll_smoothly` 记着，画成平滑滚动。
    scroll_remainder: f32,
    /// 按命令历史给出的灰字建议，画在光标后面。
    suggestion: Option<Suggestion>,
    suggester: history::Suggester,
    /// 有了新输出或配置变了，屏幕上的输入可能变了，下次绘制前重读，见 `refresh_suggestion`。
    input_changed: bool,
    /// 上次查建议时命令历史的版本，历史变了也要重查。
    history_generation: u64,
    /// 按 Tab 弹出的命令补全菜单。
    completion: Option<CompletionMenu>,
    /// 回显之前按下、等着处理的补全键，以及等回显的计时器。
    completion_pending: Option<PendingKey>,
    _completion_wait: Option<Task<()>>,
    /// 最近一次收到输出的时刻，用来判断刚发出的输入回显了没有。
    output_at: Option<Instant>,
    /// 光标单元格上次绘制的位置，供输入法候选窗定位。
    cursor_bounds: Option<Bounds<Pixels>>,
    /// 单元格网格的原点，用于把指针位置换算成单元格；平滑滚动错开时是错开后的位置。
    grid_origin: Point<Pixels>,
    /// 左键按下后正在拖动选择，这次的移动和松开都归选区，不上报给程序。
    selecting: bool,
    /// 按下的那一下已经上报给了程序。分屏时每个终端都在窗口上监听移动和松开，
    /// 只有按下发生在自己这里的终端才上报对应的拖动和松开。
    reporting_press: bool,
    /// 这次左键按下是一次不带修饰键的单击，按在哪一格；松开时还在这一格就把光标挪过去。
    click_cell: Option<(i32, i32)>,
    /// 闪烁光标当前处于亮的一半周期。
    cursor_blink_visible: bool,
    /// 当前这半个闪烁周期从何时开始。
    cursor_blink_since: Instant,
    /// 接上的是提前启动的 shell 时它启动用的尺寸，第一次布局时比对过就清掉。
    adopted_size: Option<GridSize>,
    /// 要启动 shell，等下一次布局量出实际尺寸再启动。shell 读启动配置期间才收到尺寸变化时，
    /// 第一个提示符仍按旧尺寸画：zsh 的 PROMPT_SP 按旧列数补空格，终端比那窄时折行，
    /// 反白的 `%` 就留在了提示符上面一行。
    start_pending: bool,
    /// 打开着的搜索栏输入框，以及对它事件的订阅。
    search_field: Option<(Entity<SearchField>, Subscription)>,
    _reader: Task<()>,
    /// shell 启动后才有。
    _foreground_poll: Option<Task<()>>,
    /// 上次因为有输出而重读前台进程的时刻。
    foreground_read_at: Instant,
    /// 输出太密时推迟的那次重读。
    _foreground_refresh: Option<Task<()>>,
    _hold_timeout: Option<Task<()>>,
    /// 有焦点时才运行的闪烁计时器。
    _cursor_blink: Option<Task<()>>,
    /// 拖选到网格外时运行的自动滚动计时器。
    _autoscroll: Option<Task<()>>,
    _config_watch: Subscription,
    _appearance_watch: Subscription,
    /// 包住终端和搜索栏的外层：焦点进到其中任何一处都算这个终端获得了焦点。
    pane_focus: FocusHandle,
    _focus_watch: [Subscription; 3],
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = self.session.frame().background;
        self.check_completion();
        // 搜索栏和终端是兄弟节点，不在 `Terminal` 按键上下文里：在搜索栏里打字时，
        // ⌘← 之类映射给程序的快捷键不能生效。
        let search_bar = self
            .search_field
            .as_ref()
            .map(|(field, _)| self.render_search_bar(field, cx));
        let terminal = div()
            .id("terminal")
            // 搜索栏开着时多一个 `searching` 标记，只在这时才让 Esc 关搜索而不发给程序。
            .key_context(if self.search_field.is_some() { "Terminal searching" } else { "Terminal" })
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_selection))
            .on_action(cx.listener(Self::clear_screen))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::scroll_to_top))
            .on_action(cx.listener(Self::scroll_to_bottom))
            .on_action(cx.listener(Self::scroll_page_up))
            .on_action(cx.listener(Self::scroll_page_down))
            .on_action(cx.listener(Self::scroll_to_selection))
            .on_action(cx.listener(Self::send_text))
            .on_action(cx.listener(Self::jump_to_prompt))
            .on_action(cx.listener(Self::write_screen_file))
            .on_action(cx.listener(Self::start_search))
            .on_action(cx.listener(Self::search_selection))
            .on_action(cx.listener(Self::search_next))
            .on_action(cx.listener(Self::search_previous))
            .on_action(cx.listener(Self::end_search))
            .on_action(cx.listener(Self::increase_font_size))
            .on_action(cx.listener(Self::decrease_font_size))
            .on_action(cx.listener(Self::reset_font_size))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(background.to_u32()))
            .child(
                div()
                    .flex_1()
                    .pl(px(self.config.window_padding_x.0))
                    .pr(px(self.config.window_padding_x.1))
                    .pt(px(self.config.window_padding_y.0))
                    .pb(px(self.config.window_padding_y.1))
                    .on_any_mouse_down(cx.listener(Self::mouse_down))
                    // 程序开了鼠标上报时点击归程序，指针不显示成文本选择的样子。
                    .cursor(if self.session.mouse_tracking() {
                        CursorStyle::Arrow
                    } else {
                        CursorStyle::IBeam
                    })
                    .child(TerminalElement { view: cx.entity() }),
            );
        div()
            .track_focus(&self.pane_focus)
            .relative()
            .size_full()
            .child(terminal)
            .children(search_bar)
    }
}

pub fn hsla(color: Rgb) -> Hsla {
    rgb(color.to_u32()).into()
}

