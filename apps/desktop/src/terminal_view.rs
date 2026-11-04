//! 单个终端会话的 GPUI 视图：输入分发和单元格绘制。
//!
//! 这里是 `TerminalView` 本身、它的动作和事件，以及渲染出的元素树。其余按职责分在子模块里：
//! 建视图、启动 shell 和读输出（`lifecycle`）、界面这份 VT 的状态机（`screen`）、按键和鼠标
//! （`input`，按键翻译成终端的按键事件在 `keys`）、绑定的动作（`actions`）、
//! 搜索栏（`search`）、输入法（`ime`）、终端网格元素（`element`）、画一帧（`paint`，
//! 方框线、块元素这些自绘字符在 `sprites`），
//! 以及尺寸归别的前端管时 VT 的哪一块画进视图（`crop`）。

mod actions;
mod context_menu;
mod crop;
mod cursor_blink;
mod dictation;
mod element;
mod ime;
mod input;
pub(crate) mod keys;
mod lifecycle;
mod paint;
mod screen;
mod search;
mod sprites;

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, sync::Arc, time::Instant};

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    Action, App, Bounds, Context, CursorStyle, Entity, EventEmitter, FocusHandle, Focusable, Font, Pixels, Point,
    Render, RenderImage, Role, ShapedLine, SharedString, Subscription, Task, Window, actions, div, prelude::*, px,
    relative, rgb, svg,
};
use runode_config::Config;
use runode_protocol::{Conversation, SessionId};
use runode_terminal::session::{Link, LinkTarget, Session};
use runode_types::{
    color::Rgb,
    frame::{Frame, FrameImage},
    grid::GridSize,
};

use crate::ui::a11y::StaticText;
use crate::{
    host_client::{self, LinkEvent, hosts::HostId},
    ui::{
        a11y::Press,
        font_size::FontSize,
        hsla,
        icons::ARROW_DOWN_ICON,
        input::InputState,
        menu::{Menu, MenuHost as _},
    },
};
use crop::Crop;
use element::TerminalElement;
use screen::ScreenState;

actions!(
    runode,
    [
        PasteSelection,
        ClearScreen,
        /// 停在提示符上的 shell 换成一个新的，重新读用户配置，见 `Session::reload_shell`。
        ReloadShell,
        ScrollToTop,
        ScrollToBottom,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToSelection,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        /// 语音输入：按住说话、松开打进终端，或者点一下开始、再点一下结束。
        Dictate
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

use lifecycle::Opening;
pub(crate) use lifecycle::{PROVISIONAL_SIZE, remote_reconnected};
/// 写好的文件怎么处理：复制路径（`CopyPath`）、把路径粘贴进终端（`PastePath`）或者用系统默认
/// 程序打开（`Open`）。和快捷键配置里解析出来的是同一个类型，不用再转换。
pub use runode_config::keybind::ScreenFile;
/// 回到显示时主线程最多等这么久拿到宿主给的屏幕，见 `TerminalView::wait_for_screen`。
pub(crate) use screen::SHOW_WAIT;
pub use search::{SearchSelection, StartSearch};

/// 翻回回滚历史时右边那条位置条的宽度、离右边的距离和最短的长度。
const SCROLL_THUMB_WIDTH: f32 = 4.;
const SCROLL_THUMB_INSET: f32 = 2.;
const SCROLL_THUMB_MIN: f32 = 16.;
const MIN_FONT_SIZE: f32 = 6.;
const MAX_FONT_SIZE: f32 = 72.;
/// 程序没设置标题、也读不到前台进程时用的标题。
pub const DEFAULT_TITLE: &str = "Runode";
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    cell: gpui::Size<Pixels>,
    ascent: Pixels,
    descent: Pixels,
    /// 算它时窗口的缩放比例，变了就作废。
    scale: f32,
}

/// 字形缓存按（粗体、斜体）分成四张表，查找时直接用 `&str`，命中时不必为键分配 `String`。
/// 值放在 `Rc` 里：`ShapedLine` 内联着一整块装饰数组，每格每帧复制一份会占去绘制的大头。
type GlyphCache = [HashMap<String, Rc<ShapedLine>>; 4];

/// 终端视图通知外层（标签栏）的事件。
pub enum TerminalEvent {
    /// 运行中的程序改了标题（OSC 0/2）。
    TitleChanged,
    /// 宿主公布的会话状态变了：目录、前台程序，也包括标题和 agent。终端的输出不算。
    MetaChanged,
    /// 晚开的会话开好了，有了 `session_id`。
    SessionOpened,
    /// 焦点进入了这个终端（包括它的搜索栏）。
    Focused,
    /// 程序响铃（BEL）。
    Bell,
    /// 程序用 OSC 9、OSC 777 要求发桌面通知，`title` 可能为空。
    Notify { title: String, body: String },
    /// 程序用 CSI 8 t 要求把窗口改成能装下这么多行、列，0 是那一维不变。
    ResizeWindow { rows: u16, columns: u16 },
    /// 前台 agent 从工作中停了下来（干完了、等着用户回答或者退出了）。
    AgentFinished,
    /// 前台 agent 停下来等用户回答（要不要执行命令、选哪一项等）。从工作中直接变成这样时
    /// 紧跟在 `AgentFinished` 之后发。
    AgentBlocked,
    /// 远端主机上的终端里粘贴、剪贴板里只有图片：那边的程序读不到这台电脑的剪贴板，要先传过去。
    PasteImage(gpui::Image),
    /// 远端主机上的终端里粘贴从文件管理器复制的文件：本机的路径那边打不开，要先把文件传过去。
    PasteFiles(Vec<PathBuf>),
    /// 远端主机上的终端里要粘贴屏幕内容存成的文件：本机的文件那边读不到，要把这些文字传过去。
    PasteScreen(String),
    /// 本机的终端里要粘贴本机文件的路径：照终端前台 shell 的写法加引号再打进去。
    PastePath(PathBuf),
    /// 远端主机上的终端里 ⌘ 点了那台机器上的文件或目录（`dir`），本机打不开，要在窗口里看。
    OpenRemotePath { path: PathBuf, dir: bool },
    /// shell 已经退出，这个终端该关掉了。
    Exited,
}

/// 收宿主发来的事件的一端。读事件的任务和回到显示时主线程上的等待（见 `request_visible`、
/// `wait_for_screen`）轮流用它。
type Events = Rc<RefCell<UnboundedReceiver<LinkEvent>>>;

/// 从命令菜单开来跑的那条命令：菜单里的名字、命令行和跑它的目录。
struct TaskRun {
    name: SharedString,
    command: String,
    dir: PathBuf,
}

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
    /// `deferred` 建的视图开会话时 `start_dir` 不是目录的话改从哪里开。
    start_fallback: Option<PathBuf>,
    /// 恢复时要接回的 agent 对话：`deferred` 建的视图开会话时交给宿主。会话报出对话之前
    /// `SessionMeta::conversation` 还是空的，写存档时用它代替（`conversation`）。
    start_resume: Option<Conversation>,
    /// 新建时挑的 shell，为空时用宿主的默认 shell；会话断了在原目录重开时还用它，写存档时也记下。
    start_shell: Option<String>,
    /// 会话在哪台主机上，和到那台主机的连接。
    host: HostId,
    host_link: host_client::Link,
    /// 远端主机上正在后台开会话，见 `open_session`。
    opening: Option<Opening>,
    /// 要启动、还没开始开会话的 `deferred` 视图上等着会话开好的，开始开时交给 `Opening::waiting`。
    open_waiting: Vec<futures::channel::oneshot::Sender<Option<SessionId>>>,
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
    /// 上一帧画过的图片，按像素的版本（`FrameImage::generation`）存；帧里没有了就交还贴图。
    images: HashMap<u64, Arc<RenderImage>>,
    /// 输入法尚未上屏的预编辑文本，画在光标处。
    marked_text: Option<String>,
    /// 程序开着鼠标上报时，精确滚动（触控板）不足一行的余量；滚回滚历史时余量由
    /// `Session::scroll_smoothly` 记着，画成平滑滚动。
    scroll_remainder: f32,
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
    /// 按着 ⌘ 悬停时指针下的链接，画下划线、指针显示成手形；⌘ 点击打开它。别的平台上 ⌘ 换成 Ctrl。
    hovered_link: Option<Link>,
    /// 按着 ⌘ 时指针下的候选链接，以及后台查路径存在与否的进度；见 `LinkLookup`。
    link_lookup: Option<input::LinkLookup>,
    /// 这次左键按下是一次不带修饰键的单击，按在哪一格；松开时还在这一格就把光标挪过去。
    click_cell: Option<(i32, i32)>,
    cursor_blink: cursor_blink::CursorBlink,
    /// 接上的是提前启动的 shell 时它启动用的尺寸，第一次布局时比对过就清掉。
    adopted_size: Option<GridSize>,
    /// 要启动 shell，等下一次布局量出实际尺寸再启动。shell 读启动配置期间才收到尺寸变化时，
    /// 第一个提示符仍按旧尺寸画：zsh 的 PROMPT_SP 按旧列数补空格，终端比那窄时折行，
    /// 反白的 `%` 就留在了提示符上面一行。
    start_pending: bool,
    /// 等 shell 出了提示符再打进去的命令，见 `run_command`。
    pending_command: Option<String>,
    /// 从命令菜单开来跑的那条命令：分屏标题写它的名字，几个命令的终端分得开；再跑同一条命令时按它找回这个终端。
    task: Option<TaskRun>,
    /// 打开着的搜索栏输入框，以及对它事件的订阅。
    search_field: Option<(Entity<InputState>, Subscription)>,
    /// 开着的右键菜单。
    menu: Option<Menu>,
    /// 收宿主发来的输出和状态的任务，见 `read_events`。
    _reader: Task<()>,
    /// 前台 agent 上次换了种类或状态的时刻，agent 列表按它排同一状态里的先后。
    agent_changed_at: Instant,
    _hold_timeout: Option<Task<()>>,
    /// 图片动画到下一帧时重画的计时器。
    _image_frame: Option<Task<()>>,
    /// 拖选到网格外时运行的自动滚动计时器。
    _autoscroll: Option<Task<()>>,
    _config_watch: Subscription,
    /// 包住终端和搜索栏的外层：焦点进到其中任何一处都算这个终端获得了焦点。
    pane_focus: FocusHandle,
    _window_watches: lifecycle::WindowWatches,
    /// 正在录的或正在识别的语音输入。
    dictation: Option<dictation::Dictation>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

/// 丢掉视图只算不再看这个会话，会话在宿主里照旧跑着（宿主在 app 里时随 app 退出结束）。
/// 关标签、关分屏这类用户明确不要这个终端的路径先调 `end` 结束它，之后丢掉时不再发什么。
impl Drop for TerminalView {
    fn drop(&mut self) {
        if !self.ended
            && let Some(id) = self.id
        {
            self.host_link.detach(id);
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (foreground, background) = self.colors();
        // 屏幕上的字只有辅助工具在读时才拼。
        let a11y = window.is_a11y_active();
        let screen_text = a11y.then(|| {
            let image = rust_i18n::t!("terminal_image");
            self.screen.shown_mut().map(|session| screen_lines(&session.frame(), &image))
        });
        // 搜索栏和终端是兄弟节点，不在 `Terminal` 按键上下文里：在搜索栏里打字时，
        // ⌘← 之类映射给程序的快捷键不能生效。
        let search_bar = self.search_field.as_ref().map(|(field, _)| self.render_search_bar(field, cx));
        let terminal = div()
            .id("terminal")
            // 报给辅助工具：名字是标题，值是这一屏的字。
            .role(Role::Terminal)
            .aria_label(SharedString::from(self.title().to_owned()))
            .when_some(screen_text.flatten(), |terminal, text| terminal.aria_value(text))
            // 搜索栏开着时多一个 `searching` 标记，只在这时才让 Esc 关搜索而不发给程序。
            .key_context(if self.search_field.is_some() { "Terminal searching" } else { "Terminal" })
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_selection))
            .on_action(cx.listener(Self::clear_screen))
            .on_action(cx.listener(Self::reload_shell))
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
            .on_action(cx.listener(Self::dictate))
            .on_key_up(cx.listener(Self::dictation_key_up))
            .on_modifiers_changed(cx.listener(Self::dictation_modifiers_changed))
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
                    .cursor(if self.hovered_link.is_some() {
                        CursorStyle::PointingHand
                    } else if self.screen.live().is_some_and(Session::mouse_tracking) {
                        CursorStyle::Arrow
                    } else {
                        CursorStyle::IBeam
                    })
                    .child(TerminalElement { view: cx.entity() }),
            );
        let scrolled = self.screen.shown().and_then(Session::scrolled_back);
        let position = scrolled.map(|(top, len)| render_scroll_position(top, len, foreground));
        let lost = self.screen.is_lost().then(|| self.render_lost_bar(foreground, background, cx));
        // 和断开的提示都在底部，断开时只留那个。
        let jump =
            (scrolled.is_some() && lost.is_none()).then(|| self.render_jump_to_latest(foreground, background, cx));
        let size_owner = self.render_size_owner(foreground, background, cx);
        let link_target = self.render_link_target(foreground, background);
        let dictation = self.render_dictation(foreground, background);
        self.focus_menu(window, cx);
        let menu = self.render_context_menu(window, cx);
        div()
            .track_focus(&self.pane_focus)
            .relative()
            .size_full()
            .child(terminal)
            .children(position)
            .children(jump)
            .children(search_bar)
            .children(size_owner)
            .children(link_target)
            .children(dictation)
            .children(lost)
            .children(menu)
    }
}

/// 翻回了回滚历史时贴着右边画的一条细条：在全部行里的位置和视口占多少，看得出翻到了哪、下面还有多少。
/// 只是提示，不接鼠标，点击和滚轮照样落到终端上。`top`、`len` 是视口顶边的位置和视口的长度，都按占全部行的比例。
fn render_scroll_position(top: f32, len: f32, foreground: Rgb) -> gpui::Div {
    let color = gpui::Hsla::from(rgb(foreground.to_u32())).opacity(0.3);
    div().absolute().top_0().bottom_0().right(px(SCROLL_THUMB_INSET)).w(px(SCROLL_THUMB_WIDTH)).overflow_hidden().child(
        div()
            .absolute()
            .left_0()
            .right_0()
            .top(relative(top))
            .h(relative(len))
            .min_h(px(SCROLL_THUMB_MIN))
            .rounded_full()
            .bg(color),
    )
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
        let take_over = rust_i18n::t!("size_owner.take_over").into_owned();
        Some(
            div()
                .id("size-owner")
                .role(Role::Button)
                .aria_label(message.clone().into_owned())
                .aria_description(take_over.clone())
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
                .text_size(FontSize::Small)
                .text_color(fg.opacity(0.8))
                .cursor(CursorStyle::PointingHand)
                .hover(|label| label.text_color(fg))
                .child(message.into_owned())
                .child(div().text_color(fg.opacity(0.5)).child(take_over))
                .on_press(cx, |view, window, cx| {
                    window.focus(&view.focus_handle, cx);
                    view.report_focus(true);
                }),
        )
    }

    /// 按着 ⌘ 悬停在网址上时左下角显示它的真实目标（右下角让给尺寸提示）：OSC 8 的链接显示的字可以
    /// 和打开的 URI 毫不相干，点之前要让用户看得到会打开什么。屏幕上认出的路径看到的就是打开的，不显示。
    fn render_link_target(&self, foreground: Rgb, background: Rgb) -> Option<gpui::Stateful<gpui::Div>> {
        if self.screen.is_lost() {
            return None;
        }
        let LinkTarget::Url(uri) = &self.hovered_link.as_ref()?.target else {
            return None;
        };
        let fg = hsla(foreground);
        Some(
            div()
                .id("link-target")
                .aria_static_text(uri.clone())
                .absolute()
                .bottom(px(6.))
                .left(px(8.))
                .max_w(relative(0.6))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .bg(hsla(background.mix(foreground, 0.1)))
                .border_1()
                .border_color(fg.opacity(0.15))
                .text_size(FontSize::Small)
                .text_color(fg.opacity(0.8))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(uri.clone()),
        )
    }

    /// 翻回了回滚历史时右下角的「回到最新」，按了滚到最底部。
    fn render_jump_to_latest(
        &self,
        foreground: Rgb,
        background: Rgb,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let fg = hsla(foreground);
        let label = rust_i18n::t!("terminal_jump_to_latest").into_owned();
        div()
            .id("terminal-jump-to-latest")
            .role(Role::Button)
            .aria_label(label.clone())
            .absolute()
            .bottom(px(10.))
            .right(px(14.))
            .px(px(10.))
            .py(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .bg(hsla(background.mix(foreground, 0.12)))
            .hover(|button| button.bg(hsla(background.mix(foreground, 0.2))))
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .occlude()
            .cursor(CursorStyle::PointingHand)
            .text_size(FontSize::Small)
            .text_color(fg)
            .child(svg().flex_none().path(ARROW_DOWN_ICON).size(px(12.)).text_color(fg))
            .child(label)
            .on_press(cx, |view, window, cx| view.scroll_to_bottom(&ScrollToBottom, window, cx))
    }

    /// 和宿主断开后盖在底部的提示：画面停在最后一屏，以及「在原目录重开」；远端主机的显示连接状态和
    /// 「立即重连」，连上后自动接回。
    fn render_lost_bar(&self, foreground: Rgb, background: Rgb, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let fg = hsla(foreground);
        let (message, reopen) = match &self.host {
            HostId::Local => {
                (rust_i18n::t!("host_lost.message").into_owned(), rust_i18n::t!("host_lost.reopen").into_owned())
            }
            remote => {
                let status = crate::remote_hosts::status_text(&host_client::hosts::status_of(remote));
                let host = crate::remote_hosts::display_name(remote, cx);
                let message = rust_i18n::t!("ssh.lost", host = host, status = status).into_owned();
                (message, rust_i18n::t!("ssh.reconnect").into_owned())
            }
        };
        div()
            .id("host-lost")
            .role(Role::Alert)
            .aria_label(message.clone())
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
            .text_size(FontSize::Body)
            .text_color(fg)
            .cursor(CursorStyle::Arrow)
            .child(div().flex_1().min_w_0().child(message))
            .child(
                div()
                    .id("host-lost-reopen")
                    .role(Role::Button)
                    .aria_label(reopen.clone())
                    .flex_none()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(fg.opacity(0.1))
                    .hover(|button| button.bg(fg.opacity(0.2)))
                    .child(reopen)
                    .on_press(cx, |view, window, cx| view.reopen(window, cx)),
            )
    }
}

/// 视口里这一屏的字，每行一行：宽字符的后半格跳过，空白格当空格，行尾空白和最后的空行去掉。
/// 图片画出来的内容读不到，在它露出来的第一行末尾写一个 `image`，让辅助工具知道那里有图。
fn screen_lines(frame: &Frame, image: &str) -> String {
    let mut text = String::new();
    for y in 0..frame.rows {
        let start = text.len();
        for cell in frame.row(y).iter().filter(|cell| !cell.spacer) {
            text.push_str(if cell.text.is_empty() { " " } else { &cell.text });
        }
        text.truncate(start + text[start..].trim_end().len());
        let top = |shown: &FrameImage| shown.y.max(0.).floor() as u16;
        for _ in frame.images.iter().filter(|shown| shown.y + shown.height > 0. && top(shown) == y) {
            if text.len() > start {
                text.push(' ');
            }
            text.push_str(image);
        }
        text.push('\n');
    }
    text.truncate(text.trim_end().len());
    text
}

#[cfg(test)]
mod tests {
    use runode_types::frame::{Cell, ImageLayer};

    use super::*;

    #[test]
    fn screen_lines_skip_spacers_and_trailing_blanks() {
        let cell = |text: &str, spacer: bool| Cell { text: text.into(), spacer, ..Cell::default() };
        let cells = vec![
            cell("a", false),
            cell("", false),
            cell("中", false),
            cell("", true),
            cell("", false),
            cell("", false),
            cell("", false),
            cell("", false),
            cell("", false),
            cell("", false),
        ];
        let frame = Frame { cols: 5, rows: 2, cells, ..Frame::default() };
        assert_eq!(screen_lines(&frame, "[image]"), "a 中");
    }

    /// 图片在它露出来的第一行末尾标出来；上半截滚出视口的标在第一行，整个滚出去的不标。
    #[test]
    fn screen_lines_mark_where_images_are() {
        let image = |y: f32, height: f32| FrameImage {
            id: 1,
            generation: 1,
            x: 0.,
            y,
            width: 1.,
            height,
            source: [0, 0, 1, 1],
            layer: ImageLayer::AboveText,
        };
        let cells = vec![Cell { text: "a".into(), ..Cell::default() }; 3];
        let frame = Frame {
            cols: 1,
            rows: 3,
            cells,
            images: vec![image(-1.5, 2.), image(-2., 1.), image(2., 1.)],
            ..Frame::default()
        };
        assert_eq!(screen_lines(&frame, "[image]"), "a [image]\na\na [image]");
    }
}
