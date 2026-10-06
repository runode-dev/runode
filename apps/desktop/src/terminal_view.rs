//! 单个终端会话的 GPUI 视图：输入分发和单元格绘制。
//!
//! 这里是 `TerminalView` 本身、它的动作和事件，以及渲染出的元素树。其余按职责分在子模块里：
//! 建视图、启动 shell 和读输出（`lifecycle`）、界面这份 VT 的状态机（`screen`）、按键和鼠标
//! （`input`，按键翻译成终端的按键事件在 `keys`）、绑定的动作（`actions`）、
//! 搜索栏（`search`）、灰字建议（`suggestion`）、输入的语法高亮（`prompt_highlight`）、
//! 命令补全菜单（`completion_menu`）、输入法（`ime`）、终端网格元素（`element`）、画一帧（`paint`，
//! 方框线、块元素这些自绘字符在 `sprites`），
//! 以及尺寸归别的前端管时 VT 的哪一块画进视图（`crop`）。

mod actions;
mod completion_menu;
mod crop;
mod cursor_blink;
mod element;
mod ime;
mod input;
mod keys;
mod lifecycle;
mod paint;
mod prompt_highlight;
mod screen;
mod search;
mod sprites;
mod suggestion;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Action, App, Bounds, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable, Font, Pixels, Point,
    Render, ShapedLine, Subscription, Task, Window, actions, div, prelude::*, px, rgb,
};
use runode_config::Config;
use runode_protocol::SessionId;
use runode_shared_types::{color::Rgb, frame::Frame, grid::GridSize};
use runode_terminal::{history, session::Session};

use crate::{
    session_host::LinkEvent,
    ui::{hsla, text_field::TextField},
};
use completion_menu::{CompletionMenu, PendingKey};
use crop::Crop;
use element::TerminalElement;
use screen::ScreenState;

actions!(
    runode,
    [
        PasteSelection,
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

/// 写好的文件怎么处理：复制路径（`CopyPath`）、把路径粘贴进终端（`PastePath`）或者用系统默认
/// 程序打开（`Open`）。和快捷键配置里解析出来的是同一个类型，不用再转换。
pub use runode_config::keybind::ScreenFile;
/// 回到显示时主线程最多等这么久拿到宿主给的屏幕，见 `TerminalView::wait_for_screen`。
pub(crate) use screen::SHOW_WAIT;
pub use search::{SearchSelection, StartSearch};

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
    /// 前台 agent 从工作中停了下来（干完了、等着用户回答或者退出了）。
    AgentFinished,
    /// 前台 agent 停下来等用户回答（要不要执行命令、选哪一项等）。从工作中直接变成这样时
    /// 紧跟在 `AgentFinished` 之后发。
    AgentBlocked,
    /// shell 已经退出，这个终端该关掉了。
    Exited,
}

/// 收宿主发来的事件的一端。读事件的任务和回到显示时主线程上的等待（见 `set_visible`）轮流用它。
type Events = Rc<RefCell<UnboundedReceiver<LinkEvent>>>;

pub struct TerminalView {
    /// 界面这份 VT 现在的样子，以及宿主公布的会话状态（标题、agent、目录）、视图的尺寸、shell
    /// 退出了没有，见 `ScreenState`。
    screen: ScreenState<Session>,
    /// 收这个会话的事件的一端；重新登记（断开后重连、换会话）时换掉。
    events: Events,
    /// 离开显示后等 `HIDE_GRACE` 再丢掉界面这份 VT 的计时器。
    _hide_timer: Option<Task<()>>,
    /// 建视图时就已经到了的那件「之后不再有事件」的事（退出、断开）：要等外层订阅好这个视图的事件
    /// 再处理，读事件的任务和回到显示时等屏幕的地方先处理它。
    pending_end: Option<LinkEvent>,
    /// 最近画出的默认前景色和背景色；没有界面这份 VT（只看状态、断开）时标签栏和背景用它。
    colors: (Rgb, Rgb),
    /// 调用方记着的这个终端的目录，宿主还没报告目录时用，见 `reattach`。
    start_dir: Option<PathBuf>,
    /// 宿主里的会话。用 `end` 结束它；视图没了只是不再看它，见 `Drop`。`deferred` 建的视图开会话
    /// 之前为 `None`。
    id: Option<SessionId>,
    /// 已经用 `end` 结束了会话，丢掉视图时不再发。
    ended: bool,
    /// 宿主发来了 `HostMsg::Bell`，还没通知外层，见 `ring_bell`。
    bell_pending: bool,
    /// 已经请宿主启动了 shell。
    started: bool,
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
    /// 提示符上输入的语法高亮，绘制时盖在单元格上。
    highlight: Rc<prompt_highlight::Highlight>,
    /// 有了新输出或配置变了，屏幕上的输入可能变了，下次绘制前重读，见 `refresh_input`。
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
    /// 单元格网格的原点，用于把指针位置换算成单元格；平滑滚动错开时是错开后的位置，尺寸归别的前端管、
    /// 只画了 VT 的一块时是裁掉的那几列几行挪出视图后 VT 第 0 列第 0 行的位置。
    grid_origin: Point<Pixels>,
    /// 尺寸归别的前端管、VT 和视图对不上时上一帧画了 VT 的哪一块，下一帧光标没出视图就接着画这一块；
    /// 对得上时为空。
    crop: Option<Crop>,
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
    /// 最近一次键盘输入、终端输出或重新获得焦点的时刻，闪烁的空闲期限从这里算。
    cursor_blink_active_at: Instant,
    /// 闪烁因空闲到期停了，光标常亮；有了活动（`reset_cursor_blink`）才恢复。
    cursor_blink_stopped: bool,
    /// 接上的是提前启动的 shell 时它启动用的尺寸，第一次布局时比对过就清掉。
    adopted_size: Option<GridSize>,
    /// 要启动 shell，等下一次布局量出实际尺寸再启动。shell 读启动配置期间才收到尺寸变化时，
    /// 第一个提示符仍按旧尺寸画：zsh 的 PROMPT_SP 按旧列数补空格，终端比那窄时折行，
    /// 反白的 `%` 就留在了提示符上面一行。
    start_pending: bool,
    /// 打开着的搜索栏输入框，以及对它事件的订阅。
    search_field: Option<(Entity<TextField>, Subscription)>,
    /// 收宿主发来的输出和状态的任务，见 `read_events`。
    _reader: Task<()>,
    /// 前台 agent 上次换了种类或状态的时刻，agent 列表按它排同一状态里的先后。
    agent_changed_at: Instant,
    _hold_timeout: Option<Task<()>>,
    /// 有焦点、光标在闪时才运行的闪烁计时器，见 `sync_cursor_blink`。
    _cursor_blink: Option<Task<()>>,
    /// 拖选到网格外时运行的自动滚动计时器。
    _autoscroll: Option<Task<()>>,
    _config_watch: Subscription,
    /// 窗口重新激活时恢复空闲停下的光标闪烁，见 `reset_cursor_blink`。
    _activation_watch: Subscription,
    _appearance_watch: Subscription,
    /// 别的终端在和宿主断开后点了「在原目录重开」、重新连上了宿主，见 `host_reconnected`。
    _reconnect_watch: Subscription,
    /// 包住终端和搜索栏的外层：焦点进到其中任何一处都算这个终端获得了焦点。
    pane_focus: FocusHandle,
    _focus_watch: [Subscription; 4],
}

impl EventEmitter<TerminalEvent> for TerminalView {}

/// 丢掉视图只算不再看这个会话，会话在宿主里照旧跑着（宿主在 app 里时随 app 退出结束）。
/// 关标签、关分屏这类用户明确不要这个终端的路径先调 `end` 结束它，之后丢掉时不再发什么。
impl Drop for TerminalView {
    fn drop(&mut self) {
        if !self.ended
            && let Some(id) = self.id
        {
            crate::session_host::link().detach(id);
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (foreground, background) = self.colors();
        self.check_completion();
        // 搜索栏和终端是兄弟节点，不在 `Terminal` 按键上下文里：在搜索栏里打字时，
        // ⌘← 之类映射给程序的快捷键不能生效。
        let search_bar = self.search_field.as_ref().map(|(field, _)| self.render_search_bar(field, cx));
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
                    .cursor(if self.screen.live().is_some_and(Session::mouse_tracking) {
                        CursorStyle::Arrow
                    } else {
                        CursorStyle::IBeam
                    })
                    .child(TerminalElement { view: cx.entity() }),
            );
        let lost = self.screen.is_lost().then(|| self.render_lost_bar(foreground, background, cx));
        let size_owner = self.render_size_owner(foreground, background, cx);
        div()
            .track_focus(&self.pane_focus)
            .relative()
            .size_full()
            .child(terminal)
            .children(search_bar)
            .children(size_owner)
            .children(lost)
    }
}

impl TerminalView {
    /// 尺寸归别的前端管、VT 和视图的行列对不上时这一帧画 VT 的哪一块（见 `Crop::follow`），记下来给
    /// 下一帧接着用；对得上或者尺寸归这边管时为空，照常从视图左上角画。
    fn crop_for(&mut self, frame: &Frame) -> Option<Crop> {
        let view = self.screen.last_size();
        if self.screen.size_owner().is_none() || (frame.cols, frame.rows) == (view.cols, view.rows) {
            self.crop = None;
            return None;
        }
        let vt = GridSize { cols: frame.cols, rows: frame.rows, ..view };
        let crop = Crop::follow(vt, view, frame.cursor.map(|cursor| (cursor.x, cursor.y)), self.crop);
        self.crop = Some(crop);
        Some(crop)
    }

    /// 尺寸归别的前端管、VT 和视图的行列对不上时右下角的提示「尺寸由 ‹设备› 控制」（右上角让给搜索栏）；
    /// 点它（或者点终端、在这里打字）就接管。
    fn render_size_owner(
        &self,
        foreground: Rgb,
        background: Rgb,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let owner = self.screen.size_owner()?;
        let vt = self.screen.live()?.size();
        let view = self.screen.last_size();
        if (vt.cols, vt.rows) == (view.cols, view.rows) {
            return None;
        }
        let message = match &owner.device {
            Some(device) => rust_i18n::t!("size_owner.controlled_by", device = device),
            None => rust_i18n::t!("size_owner.controlled_elsewhere"),
        };
        let fg = hsla(foreground);
        Some(
            div()
                .id("size-owner")
                .absolute()
                .bottom(px(6.))
                .right(px(8.))
                .px(px(8.))
                .py(px(2.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(4.))
                .bg(hsla(background.mix(foreground, 0.1)))
                .border_1()
                .border_color(fg.opacity(0.15))
                .occlude()
                .text_size(px(11.))
                .text_color(fg.opacity(0.8))
                .cursor(CursorStyle::PointingHand)
                .hover(|label| label.text_color(fg))
                .child(message.into_owned())
                .child(div().text_color(fg.opacity(0.5)).child(rust_i18n::t!("size_owner.take_over").into_owned()))
                .on_click(cx.listener(|view, _, window, cx| {
                    window.focus(&view.focus_handle, cx);
                    view.report_focus(true);
                })),
        )
    }

    /// 和宿主断开后盖在底部的提示：画面停在最后一屏，以及「在原目录重开」。
    fn render_lost_bar(&self, foreground: Rgb, background: Rgb, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let fg = hsla(foreground);
        div()
            .id("host-lost")
            .absolute()
            .bottom(px(8.))
            .left(px(8.))
            .right(px(8.))
            .px(px(10.))
            .py(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(6.))
            .bg(hsla(background.mix(foreground, 0.1)))
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .occlude()
            .text_size(px(12.))
            .text_color(fg)
            .cursor(CursorStyle::Arrow)
            .child(div().flex_1().min_w_0().child(rust_i18n::t!("host_lost.message").into_owned()))
            .child(
                div()
                    .id("host-lost-reopen")
                    .flex_none()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(fg.opacity(0.1))
                    .hover(|button| button.bg(fg.opacity(0.2)))
                    .child(rust_i18n::t!("host_lost.reopen").into_owned())
                    .on_click(cx.listener(|view, _, window, cx| view.reopen(window, cx))),
            )
    }
}
