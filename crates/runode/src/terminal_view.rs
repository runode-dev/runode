//! 单个终端会话的 GPUI 视图：输入分发和单元格绘制。

use std::{
    collections::HashMap,
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{FutureExt as _, StreamExt as _};
use gpui::{
    Action, App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    FontStyle, FontWeight, GlobalElementId, Hsla, KeyDownEvent, Keystroke, LayoutId, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, PromptLevel, Render, ScrollDelta,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Subscription, Task, TextRun, UTF16Selection,
    Window, actions, div, fill, font, point, prelude::*, px, relative, rgb, size,
};
use libghostty_vt::{key::Mods, mouse, selection::Adjustment};

use crate::{
    agent::Agent,
    config::{AppConfig, CellHeight, Config},
    keys,
    prespawn::Prespawned,
    pty::{GridSize, PtyEvent},
    search_bar::{
        EndSearch, SearchField, SearchFieldEvent, SearchNext, SearchPrevious, SearchSelection,
        StartSearch,
    },
    session::{
        Attrs, CursorShape, Frame, GridPoint, Paste as PasteResult, Rgb, SYNC_OUTPUT_TIMEOUT, Session,
        ViewportScroll,
    },
    sprites,
};

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

/// 配置的字体都不可用时使用的等宽字体，macOS 自带。
const FALLBACK_FONT_FAMILY: &str = "Menlo";
/// 字体的下划线粗细（em 的比例），自绘字符的线宽由它算出。GPUI 不公开字体的下划线粗细，
/// 这里取 Hack 与 Menlo 的 post 表数值，两者都是 90/2048。
const UNDERLINE_THICKNESS_EM: f32 = 90. / 2048.;
const MIN_FONT_SIZE: f32 = 6.;
const MAX_FONT_SIZE: f32 = 72.;
/// 程序没设置标题、也读不到前台进程时用的标题。
pub const DEFAULT_TITLE: &str = "Runode";
/// 重新读取前台进程的间隔：不输出的程序（比如 `sleep`）启动后，标签名也能跟上。
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 视图建好时伪终端的临时尺寸；第一次布局时会按实际大小重设。
const PROVISIONAL_SIZE: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width_px: 8,
    cell_height_px: 16,
};
/// 建视图时最多先喂进去这么多已经到达的输出，余下的照常交给读输出的任务：shell 一启动就
/// 大量输出时，第一帧不能等它们全部处理完。
const EARLY_OUTPUT_LIMIT: usize = 64 * 1024;
/// 一次最多合并这么多排队的输出再交给 VT：积压很多时不必为它们另拼一整块大缓冲。
const MAX_OUTPUT_BATCH: usize = 1024 * 1024;
/// 有输出时重读前台进程的最短间隔：大量输出时不必每批都做几次系统调用，推迟的那次到点补上。
const FOREGROUND_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
/// 拖选到网格外时自动滚动的间隔，每次滚一行。
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);
/// 光标闪烁时亮、灭各持续的时长。
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    cell: gpui::Size<Pixels>,
    ascent: Pixels,
    descent: Pixels,
}

/// 字形缓存按（粗体、斜体）分成四张表，查找时直接用 `&str`，命中时不必为键分配 `String`。
/// 值放在 `Rc` 里：`ShapedLine` 内联着一整块装饰数组，每格每帧复制一份会占去绘制的大头。
type GlyphCache = [HashMap<String, Rc<ShapedLine>>; 4];

fn glyph_table(attrs: Attrs) -> usize {
    usize::from(attrs.bold) | usize::from(attrs.italic) << 1
}

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
    /// 精确滚动（触控板）不足一行的余量。
    scroll_remainder: f32,
    /// 光标单元格上次绘制的位置，供输入法候选窗定位。
    cursor_bounds: Option<Bounds<Pixels>>,
    /// 单元格网格的原点，用于把指针位置换算成单元格。
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
    /// 打开着的搜索栏输入框，以及对它事件的订阅。
    search_field: Option<(Entity<SearchField>, Subscription)>,
    _reader: Task<()>,
    _foreground_poll: Task<()>,
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

impl TerminalView {
    /// 启动一个 shell 会话并建好它的视图；shell 起不来时返回错误，不建视图。
    /// `cwd` 为 `None` 时 shell 从家目录开始。
    pub fn spawn(
        cwd: Option<&std::path::Path>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let integration = cx.global::<AppConfig>().0.shell_integration;
        let (session, rx) = Session::spawn(PROVISIONAL_SIZE, cwd, integration)?;
        Ok(cx.new(|cx| Self::new(session, rx, window, cx)))
    }

    /// 定时重读前台进程，不输出的程序（比如 `sleep`）启动后标签名也能跟上。
    fn poll_foreground(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FOREGROUND_POLL_INTERVAL).await;
                let updated = this.update(cx, |view, cx| {
                    if view.session.refresh_fallback_title() {
                        cx.emit(TerminalEvent::TitleChanged);
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        })
    }

    /// 接上启动时提前拉起的 shell（见 `prespawn`）并建好它的视图。
    pub fn adopt(shell: Prespawned, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let session = Session::with_pty(shell.size, shell.pty)?;
        Ok(cx.new(|cx| Self::new(session, shell.rx, window, cx)))
    }

    fn new(
        mut session: Session,
        mut rx: futures::channel::mpsc::UnboundedReceiver<PtyEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = cx.global::<AppConfig>().0.clone();
        session.apply_config(&config);
        // 提前启动的 shell 多半已经输出了提示符：现在就喂进去，第一帧就画得出来，
        // 不用等下面读输出的任务排上主线程。已经读到的退出留给那个任务照常处理。
        let mut exited_early = None;
        let mut early = Vec::new();
        while early.len() < EARLY_OUTPUT_LIMIT
            && let Ok(event) = rx.try_recv()
        {
            match event {
                PtyEvent::Output(data) => early.extend_from_slice(&data),
                PtyEvent::Exited => exited_early = Some(PtyEvent::Exited),
            }
        }
        if !early.is_empty() {
            session.feed(&early);
        }

        let reader = cx.spawn_in(window, async move |this, cx| {
            let mut rx = futures::stream::iter(exited_early).chain(rx);
            while let Some(first) = rx.next().await {
                // 把已排队的输出合并成一次 VT 写入和一次重绘。
                let mut output = Vec::new();
                let mut exited = false;
                let mut next = Some(first);
                while let Some(event) = next.take() {
                    match event {
                        PtyEvent::Output(data) => output.extend_from_slice(&data),
                        PtyEvent::Exited => exited = true,
                    }
                    if output.len() < MAX_OUTPUT_BATCH {
                        next = rx.next().now_or_never().flatten();
                    }
                }
                let updated = this.update_in(cx, |view, window, cx| {
                    if !output.is_empty() {
                        let was_working = view.session.agent.is_some_and(Agent::is_working);
                        // 进出目录、启动或退出程序时通常都有输出，顺带重读前台进程。
                        let fallback_changed = view.refresh_foreground_soon(cx);
                        if view.session.feed(&output) || fallback_changed {
                            cx.emit(TerminalEvent::TitleChanged);
                        }
                        if was_working && !view.session.agent.is_some_and(Agent::is_working) {
                            cx.emit(TerminalEvent::AgentFinished);
                        }
                        // 有输出（包括键入的回显）时光标先亮起，免得打字时看不到它。
                        view.reset_cursor_blink(window, cx);
                    }
                    if view.session.take_bell() {
                        cx.emit(TerminalEvent::Bell);
                    }
                    if exited {
                        view.session.exited = true;
                        cx.emit(TerminalEvent::Exited);
                    }
                    if view.session.render_held() {
                        view.schedule_hold_timeout(cx);
                    }
                    cx.notify();
                });
                if updated.is_err() || exited {
                    break;
                }
            }
        });

        // shell 已经在起始目录里跑起来了，不等第一次输出，新标签一出现就有名字。
        session.refresh_fallback_title();
        let config_watch = cx.observe_global_in::<AppConfig>(window, |view, window, cx| {
            view.config = cx.global::<AppConfig>().0.clone();
            view.session.apply_config(&view.config);
            view.font = resolve_font(&view.config.font_family, window);
            view.font_size = px(view.config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
            // 字体、字号或行高调整都可能变了，单元格尺寸和字形缓存一律作废。
            view.metrics = None;
            view.glyphs.iter_mut().for_each(HashMap::clear);
            cx.notify();
        });
        let appearance_watch =
            cx.observe_window_appearance(window, |_, _, cx| crate::config::follow_appearance(cx));

        let foreground_poll = Self::poll_foreground(cx);

        let focus_handle = cx.focus_handle();
        let pane_focus = cx.focus_handle();
        let focus_watch = [
            cx.on_focus_in(&pane_focus, window, |_, _, cx| cx.emit(TerminalEvent::Focused)),
            cx.on_focus(&focus_handle, window, |view, window, cx| {
                view.reset_cursor_blink(window, cx);
            }),
            cx.on_blur(&focus_handle, window, |view, _, _| view._cursor_blink = None),
        ];

        Self {
            session,
            font: resolve_font(&config.font_family, window),
            font_size: px(config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)),
            config,
            focus_handle,
            metrics: None,
            glyphs: Default::default(),
            marked_text: None,
            scroll_remainder: 0.,
            cursor_bounds: None,
            grid_origin: Point::default(),
            selecting: false,
            reporting_press: false,
            click_cell: None,
            cursor_blink_visible: true,
            cursor_blink_since: Instant::now(),
            search_field: None,
            _reader: reader,
            _foreground_poll: foreground_poll,
            foreground_read_at: Instant::now(),
            _foreground_refresh: None,
            _hold_timeout: None,
            _cursor_blink: None,
            _autoscroll: None,
            _config_watch: config_watch,
            _appearance_watch: appearance_watch,
            pane_focus,
            _focus_watch: focus_watch,
        }
    }

    /// shell 当前所在的目录，新建标签或分屏时沿用。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.session.cwd()
    }

    /// 程序设置的标题；没设置时为前台进程的目录名或进程名，都没有时为 `DEFAULT_TITLE`。
    pub fn title(&self) -> &str {
        self.session
            .title
            .as_deref()
            .or(self.session.fallback_title.as_deref())
            .unwrap_or(DEFAULT_TITLE)
    }

    /// 前台 agent 在标题里报告的状态；不是 agent 在前台时为 `None`。
    pub fn agent(&self) -> Option<Agent> {
        self.session.agent
    }

    /// 当前的默认前景色和背景色，标签栏跟着终端配色走。
    pub fn colors(&mut self) -> (Rgb, Rgb) {
        let frame = self.session.frame();
        (frame.foreground, frame.background)
    }

    /// 有输出时重读前台进程，返回程序没设置标题时用的名字或 agent 状态是否变了。离上次读
    /// 不到 `FOREGROUND_REFRESH_INTERVAL` 时推迟到间隔满了再读，那时有变化再通知外层。
    fn refresh_foreground_soon(&mut self, cx: &mut Context<Self>) -> bool {
        if self._foreground_refresh.is_some() {
            return false;
        }
        let elapsed = self.foreground_read_at.elapsed();
        if elapsed >= FOREGROUND_REFRESH_INTERVAL {
            self.foreground_read_at = Instant::now();
            return self.session.refresh_fallback_title();
        }
        let timer = cx.background_executor().timer(FOREGROUND_REFRESH_INTERVAL - elapsed);
        self._foreground_refresh = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |view, cx| {
                view._foreground_refresh = None;
                view.foreground_read_at = Instant::now();
                let was_working = view.session.agent.is_some_and(Agent::is_working);
                if view.session.refresh_fallback_title() {
                    cx.emit(TerminalEvent::TitleChanged);
                }
                if was_working && !view.session.agent.is_some_and(Agent::is_working) {
                    cx.emit(TerminalEvent::AgentFinished);
                }
            })
            .ok();
        }));
        false
    }

    /// 让光标立即亮起，并从头开始计闪烁周期；没有焦点时不闪，也就不启动计时器。
    /// 每批输出都会调用，所以计时器只建一次，重新计周期只是改 `cursor_blink_since`。
    fn reset_cursor_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.cursor_blink_visible = true;
        self.cursor_blink_since = Instant::now();
        if self._cursor_blink.is_some() || !self.focus_handle.is_focused(window) {
            return;
        }
        self._cursor_blink = Some(cx.spawn(async move |this, cx| {
            let mut wait = CURSOR_BLINK_INTERVAL;
            loop {
                cx.background_executor().timer(wait).await;
                let next = this.update(cx, |view, cx| {
                    // 等的时候又重新计了周期，就等到这个周期结束。
                    let elapsed = view.cursor_blink_since.elapsed();
                    if elapsed < CURSOR_BLINK_INTERVAL {
                        return CURSOR_BLINK_INTERVAL - elapsed;
                    }
                    view.cursor_blink_visible = !view.cursor_blink_visible;
                    view.cursor_blink_since = Instant::now();
                    if view.session.frame().cursor.is_some_and(|c| c.blinking) {
                        cx.notify();
                    }
                    CURSOR_BLINK_INTERVAL
                });
                match next {
                    Ok(next) => wait = next,
                    Err(_) => break,
                }
            }
        }));
    }

    /// 同步输出的冻结超时后重绘一次，避免程序一直不释放导致画面卡死。
    fn schedule_hold_timeout(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().timer(SYNC_OUTPUT_TIMEOUT);
        self._hold_timeout = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |_, cx| cx.notify()).ok();
        }));
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 输入法正在组字时，按键归输入法处理。
        if self.marked_text.is_some() {
            return;
        }
        // 在 shell 提示符上选中了一段命令时，退格和 Delete 删掉选中的文字。
        let keystroke = &event.keystroke;
        if matches!(keystroke.key.as_str(), "backspace" | "delete")
            && !keystroke.modifiers.modified()
            && self.session.delete_selection()
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        // Shift 加方向键等在有选区时用来扩展选区，没有选区时照常发给程序。
        if let Some(adjustment) = selection_adjustment(&event.keystroke)
            && self.session.adjust_selection(adjustment)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(input) = keys::translate(&event.keystroke) else {
            return;
        };
        if self.session.key(&input) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / f32::from(metrics.cell.height),
        };
        // 滚轮增量为正表示内容向下移动，即往回滚到历史输出。
        self.scroll_remainder -= lines;
        let whole = self.scroll_remainder.trunc();
        self.scroll_remainder -= whole;
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        self.session.scroll(whole as isize, at, mouse_mods(&event.modifiers));
        cx.notify();
    }

    /// 窗口坐标换算成网格位置；单元格尺寸还没量出来时为 `None`。
    fn grid_point(&self, position: Point<Pixels>) -> Option<GridPoint> {
        let metrics = self.metrics?;
        let local = position - self.grid_origin;
        Some(GridPoint {
            x: f32::from(local.x) / f32::from(metrics.cell.width),
            y: f32::from(local.y) / f32::from(metrics.cell.height),
        })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        // 激活窗口的那一下只用来激活，不选择也不上报。
        if event.first_mouse {
            return;
        }
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        // 程序开了鼠标上报时按键归程序，按住 Shift 照常选择。
        if self.session.mouse_tracking() && !event.modifiers.shift {
            if let Some(button) = mouse_button(event.button) {
                let mods = mouse_mods(&event.modifiers);
                self.session.mouse_report(mouse::Action::Press, Some(button), at, mods);
                self.reporting_press = true;
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.selecting = true;
        let plain = !event.modifiers.modified() && event.click_count == 1;
        self.click_cell = plain.then(|| grid_cell(at));
        self.session.select_press(at, double_click_interval());
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, inside: bool, cx: &mut Context<Self>) {
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        if self.selecting {
            // 漏掉了松开事件时按松开处理，免得之后的移动还在扩展选区。
            if event.pressed_button != Some(MouseButton::Left) {
                self.finish_selecting(at, cx);
                return;
            }
            // Option 拖出矩形块。
            if self.session.select_drag(at, event.modifiers.alt) {
                self.start_autoscroll(cx);
            } else {
                self._autoscroll = None;
            }
            cx.notify();
            return;
        }
        // 没按键的移动只报给指针下的终端；按着键的拖动只报给按下时所在的终端。
        let ours = if event.pressed_button.is_some() { self.reporting_press } else { inside };
        if ours && self.session.mouse_tracking() {
            let pressed = event.pressed_button.and_then(mouse_button);
            let mods = mouse_mods(&event.modifiers);
            self.session.mouse_report(mouse::Action::Motion, pressed, at, mods);
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(at) = self.grid_point(event.position) else {
            return;
        };
        if self.selecting {
            if event.button == MouseButton::Left {
                self.finish_selecting(at, cx);
                // 原地单击、没选出东西：在 shell 提示符上时把光标挪到点击处。
                if self.click_cell.take() == Some(grid_cell(at)) && self.session.selection_text().is_none() {
                    self.session.click_to_move(at);
                }
            }
            return;
        }
        if !std::mem::take(&mut self.reporting_press) {
            return;
        }
        if self.session.mouse_tracking()
            && let Some(button) = mouse_button(event.button)
        {
            let mods = mouse_mods(&event.modifiers);
            self.session.mouse_report(mouse::Action::Release, Some(button), at, mods);
        }
    }

    fn finish_selecting(&mut self, at: GridPoint, cx: &mut Context<Self>) {
        self.selecting = false;
        self._autoscroll = None;
        self.session.select_release(at);
        cx.notify();
    }

    /// 拖到网格上下边以外时定时滚动视口、扩展选区，直到拖回网格里或松开左键。
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self._autoscroll.is_some() {
            return;
        }
        self._autoscroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_INTERVAL).await;
                let updated = this.update(cx, |view, cx| {
                    if view.session.select_autoscroll() {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        }));
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste_text(text, window, cx);
        }
    }

    fn paste_selection(&mut self, _: &PasteSelection, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selection_text() {
            self.paste_text(text, window, cx);
        }
    }

    fn paste_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.paste(&text, false) == PasteResult::Done {
            return;
        }
        // 可能直接执行命令的粘贴先让用户确认。
        let detail = rust_i18n::t!("paste.detail");
        let answer = window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("paste.title"),
            Some(&detail),
            &[&*rust_i18n::t!("paste.confirm"), &*rust_i18n::t!("paste.cancel")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |view, _| {
                    view.session.paste(&text, true);
                })
                .ok();
            }
        })
        .detach();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn clear_screen(&mut self, _: &ClearScreen, _: &mut Window, cx: &mut Context<Self>) {
        self.session.clear_screen();
        cx.notify();
    }

    fn start_search(&mut self, _: &StartSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(None, window, cx);
    }

    fn search_selection(&mut self, _: &SearchSelection, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selection_text() {
            // 搜索只在一行里找，多行选区只取第一行。
            let line = text.lines().next().unwrap_or_default().to_owned();
            self.open_search(Some(line), window, cx);
        }
    }

    /// 打开搜索栏并把焦点移过去；给了 `query` 时用它替换搜索词并立即搜索。
    fn open_search(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let field = match &self.search_field {
            Some((field, _)) => field.clone(),
            None => {
                let field = cx.new(|cx| SearchField::new(String::new(), cx));
                let events = cx.subscribe_in(&field, window, Self::handle_search_event);
                self.search_field = Some((field.clone(), events));
                field
            }
        };
        if let Some(query) = query {
            self.session.search(&query);
            field.update(cx, |field, cx| field.set_query(query, cx));
        }
        window.focus(&field.focus_handle(cx), cx);
        cx.notify();
    }

    fn handle_search_event(
        &mut self,
        _: &Entity<SearchField>,
        event: &SearchFieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SearchFieldEvent::Changed(query) => self.session.search(query),
            SearchFieldEvent::Next => self.session.search_step(false),
            SearchFieldEvent::Previous => self.session.search_step(true),
            SearchFieldEvent::Dismiss => self.close_search(window, cx),
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_field = None;
        self.session.end_search();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 搜索栏开着时切到下一个或上一个匹配；没开时这些键不做事。
    fn search_next(&mut self, _: &SearchNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.session.search_step(false);
            cx.notify();
        }
    }

    fn search_previous(&mut self, _: &SearchPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.session.search_step(true);
            cx.notify();
        }
    }

    fn end_search(&mut self, _: &EndSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.close_search(window, cx);
        }
    }

    /// 右上角的搜索栏：输入框、匹配进度、上下切换和关闭按钮。
    fn render_search_bar(&self, field: &Entity<SearchField>, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let frame = self.session.peek_colors();
        let fg = hsla(frame.0);
        let bar_bg = hsla(frame.1.mix(frame.0, 0.1));
        let status = match self.session.search_status() {
            Some((_, 0)) if !field.read(cx).query().is_empty() => rust_i18n::t!("search.no_results").into_owned(),
            Some((Some(selected), total)) => format!("{}/{total}", selected + 1),
            Some((None, total)) if total > 0 => format!("-/{total}"),
            _ => String::new(),
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_none()
                .size(px(20.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(fg.opacity(0.7))
                .hover(|button| button.bg(fg.opacity(0.15)).text_color(fg))
                .child(label)
        };
        div()
            .id("search-bar")
            .absolute()
            .top(px(8.))
            .right(px(16.))
            .w(px(320.))
            .h(px(32.))
            .pl(px(10.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(6.))
            .bg(bar_bg)
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .occlude()
            .text_size(px(12.))
            .text_color(fg)
            .cursor(CursorStyle::Arrow)
            // 搜索词常从终端里复制而来，带着提示符图标之类的字符，界面字体没有这些字形，
            // 输入框用终端的字体。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .font_family(self.font.family.clone())
                    .cursor(CursorStyle::IBeam)
                    .child(field.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(36.))
                    .text_right()
                    .text_color(fg.opacity(0.6))
                    .child(status),
            )
            .child(button("search-previous", "↑").on_click(cx.listener(|view, _, _, cx| {
                view.session.search_step(true);
                cx.notify();
            })))
            .child(button("search-next", "↓").on_click(cx.listener(|view, _, _, cx| {
                view.session.search_step(false);
                cx.notify();
            })))
            .child(button("search-close", "×").on_click(cx.listener(|view, _, window, cx| {
                view.close_search(window, cx);
            })))
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.session.select_all();
        cx.notify();
    }

    fn scroll_to_top(&mut self, _: &ScrollToTop, _: &mut Window, cx: &mut Context<Self>) {
        self.session.scroll_viewport(ViewportScroll::Top);
        cx.notify();
    }

    fn scroll_to_bottom(&mut self, _: &ScrollToBottom, _: &mut Window, cx: &mut Context<Self>) {
        self.session.scroll_viewport(ViewportScroll::Bottom);
        cx.notify();
    }

    fn scroll_page_up(&mut self, _: &ScrollPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.session.scroll_viewport(ViewportScroll::Page(-1));
        cx.notify();
    }

    fn scroll_page_down(&mut self, _: &ScrollPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.session.scroll_viewport(ViewportScroll::Page(1));
        cx.notify();
    }

    fn scroll_to_selection(&mut self, _: &ScrollToSelection, _: &mut Window, cx: &mut Context<Self>) {
        self.session.scroll_to_selection();
        cx.notify();
    }

    fn jump_to_prompt(&mut self, action: &JumpToPrompt, _: &mut Window, cx: &mut Context<Self>) {
        self.session.jump_to_prompt(action.0 < 0);
        cx.notify();
    }

    fn send_text(&mut self, action: &SendText, _: &mut Window, cx: &mut Context<Self>) {
        self.session.send_text(action.0.as_bytes());
        cx.notify();
    }

    fn write_screen_file(&mut self, action: &WriteScreenFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.session.screen_text() else {
            return;
        };
        let path = match write_screen_file(&text) {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("failed to write the screen file: {err:#}");
                return;
            }
        };
        match action.0 {
            ScreenFile::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
            }
            ScreenFile::PastePath => self.paste_text(path.display().to_string(), window, cx),
            ScreenFile::Open => cx.open_with_system(&path),
        }
    }

    fn increase_font_size(&mut self, _: &IncreaseFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(f32::from(self.font_size) + 1., cx);
    }

    fn decrease_font_size(&mut self, _: &DecreaseFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(f32::from(self.font_size) - 1., cx);
    }

    fn reset_font_size(&mut self, _: &ResetFontSize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(self.config.font_size, cx);
    }

    /// 字号一变，单元格尺寸和已排版的字形都要作废；下一次 prepaint 会按新单元格
    /// 重新计算行列数并调整终端尺寸。
    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        let size = px(size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        if size == self.font_size {
            return;
        }
        self.font_size = size;
        self.metrics = None;
        self.glyphs.iter_mut().for_each(HashMap::clear);
        cx.notify();
    }

    fn metrics(&mut self, window: &Window) -> Metrics {
        if let Some(metrics) = self.metrics {
            return metrics;
        }
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&self.font);
        let scale = window.scale_factor();
        // 对齐到设备像素，相邻单元格背景之间才不会出现缝隙。
        let snap = |v: f32| (v * scale).round() / scale;
        let width = text_system
            .advance(font_id, self.font_size, 'M')
            .map_or(f32::from(self.font_size) * 0.6, |a| f32::from(a.width));
        let ascent = text_system.ascent(font_id, self.font_size);
        let descent = text_system.descent(font_id, self.font_size).abs();
        // 行高取字体本身的 ascent + descent，再按配置的 adjust-cell-height 增减。
        let natural = f32::from(ascent) + f32::from(descent);
        let height = match self.config.adjust_cell_height {
            Some(CellHeight::Pixels(delta)) => natural + delta,
            Some(CellHeight::Percent(percent)) => natural * (1. + percent / 100.),
            None => natural,
        }
        .max(1.);
        let metrics = Metrics {
            cell: size(px(snap(width)), px(snap(height).ceil())),
            ascent,
            descent,
        };
        self.metrics = Some(metrics);
        metrics
    }

    fn shape(&mut self, text: &str, attrs: Attrs, window: &Window) -> Rc<ShapedLine> {
        let table = glyph_table(attrs);
        if let Some(line) = self.glyphs[table].get(text) {
            return line.clone();
        }
        let mut font = self.font.clone();
        if attrs.bold {
            font.weight = FontWeight::BOLD;
        }
        if attrs.italic {
            font.style = FontStyle::Italic;
        }
        let line = Rc::new(window.text_system().shape_line(
            SharedString::from(text.to_owned()),
            self.font_size,
            &[TextRun {
                len: text.len(),
                font,
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        ));
        // 只有异常输出才会让缓存无限增长；直接清空，不做逐项淘汰。
        if self.glyphs.iter().map(HashMap::len).sum::<usize>() > 8192 {
            self.glyphs.iter_mut().for_each(HashMap::clear);
        }
        self.glyphs[table].insert(text.to_owned(), line.clone());
        line
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = self.session.frame().background;
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

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.marked_text.as_deref()?;
        let utf16: Vec<u16> = marked.encode_utf16().collect();
        let range = range.start.min(utf16.len())..range.end.min(utf16.len());
        actual_range.replace(range.clone());
        Some(String::from_utf16_lossy(&utf16[range]))
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked_text
            .as_deref()
            .map_or(0, |t| t.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text
            .as_deref()
            .map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.session.commit_text(text);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

struct TerminalElement {
    view: gpui::Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        style.size.height = relative(1.).into();
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
    ) {
        self.view.update(cx, |view, _| {
            let first_layout = view.metrics.is_none();
            let metrics = view.metrics(window);
            let cols = (f32::from(bounds.size.width) / f32::from(metrics.cell.width)).floor();
            let rows = (f32::from(bounds.size.height) / f32::from(metrics.cell.height)).floor();
            let scale = window.scale_factor();
            let size = GridSize {
                cols: cols.clamp(1., u16::MAX as f32) as u16,
                rows: rows.clamp(1., u16::MAX as f32) as u16,
                cell_width_px: (f32::from(metrics.cell.width) * scale).round() as u16,
                cell_height_px: (f32::from(metrics.cell.height) * scale).round() as u16,
            };
            // 进程里第一个量出尺寸的终端就是启动时那个，记下来，下次启动好提前拉起 shell。
            if first_layout {
                crate::prespawn::remember(&view.config, size);
            }
            view.session.resize(size);
            view.grid_origin = bounds.origin;
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        // 移动和松开挂在窗口上：拖到网格外甚至窗口外时也要收到。
        window.on_mouse_event({
            let view = self.view.clone();
            move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let inside = bounds.contains(&event.position);
                    view.update(cx, |view, cx| view.mouse_move(event, inside, cx));
                }
            }
        });
        window.on_mouse_event({
            let view = self.view.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |view, cx| view.mouse_up(event, cx));
                }
            }
        });
        let focused = focus_handle.is_focused(window);
        self.view.update(cx, |view, _| {
            let metrics = view.metrics(window);
            // 绘制时要同时用到帧和 `&mut view`（字形缓存），所以先把帧取出来，画完再放回。
            let frame = view.session.take_frame();
            paint_frame(view, &frame, bounds.origin, metrics, focused, window);
            view.session.restore_frame(frame);
        });
    }
}

/// 依次尝试配置的字体族，取第一个系统里有的。字体族整个找不到时 GPUI 会
/// 落到 Helvetica 这类非等宽字体，所以要先确认能解析成它自己。
fn resolve_font(families: &[String], window: &Window) -> Font {
    let text_system = window.text_system();
    let family = families
        .iter()
        .find(|family| {
            let id = text_system.resolve_font(&font(family.as_str()));
            text_system
                .get_font_for_id(id)
                .is_some_and(|f| f.family.as_ref() == family.as_str())
        })
        .map_or(FALLBACK_FONT_FAMILY, String::as_str);
    tracing::debug!("terminal font: {family}");
    font(family.to_owned())
}

fn mouse_mods(modifiers: &Modifiers) -> Mods {
    let mut mods = Mods::empty();
    if modifiers.shift {
        mods |= Mods::SHIFT;
    }
    if modifiers.control {
        mods |= Mods::CTRL;
    }
    if modifiers.alt {
        mods |= Mods::ALT;
    }
    mods
}

/// 系统设置里的双击间隔，决定两次按下算不算连击。
fn double_click_interval() -> Duration {
    #[cfg(target_os = "macos")]
    return Duration::from_secs_f64(objc2_app_kit::NSEvent::doubleClickInterval());
    #[cfg(not(target_os = "macos"))]
    Duration::from_millis(500)
}

fn mouse_button(button: MouseButton) -> Option<mouse::Button> {
    match button {
        MouseButton::Left => Some(mouse::Button::Left),
        MouseButton::Right => Some(mouse::Button::Right),
        MouseButton::Middle => Some(mouse::Button::Middle),
        _ => None,
    }
}

/// 网格位置所在的单元格。
fn grid_cell(at: GridPoint) -> (i32, i32) {
    (at.x.floor() as i32, at.y.floor() as i32)
}

/// 只按着 Shift 的方向、翻页、Home/End 键对应的选区调整。
fn selection_adjustment(keystroke: &Keystroke) -> Option<Adjustment> {
    let mods = &keystroke.modifiers;
    if !mods.shift || mods.control || mods.alt || mods.platform {
        return None;
    }
    Some(match keystroke.key.as_str() {
        "left" => Adjustment::Left,
        "right" => Adjustment::Right,
        "up" => Adjustment::Up,
        "down" => Adjustment::Down,
        "pageup" => Adjustment::PageUp,
        "pagedown" => Adjustment::PageDown,
        "home" => Adjustment::Home,
        "end" => Adjustment::End,
        _ => return None,
    })
}

/// 屏幕内容写到临时目录下一个新文件里，返回它的路径。
fn write_screen_file(text: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("runode");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!("screen-{}-{stamp}.txt", std::process::id()));
    // 屏幕上可能有密钥之类的内容，只让自己读写。
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&path)?, text.as_bytes())?;
    Ok(path)
}

pub fn hsla(color: Rgb) -> Hsla {
    rgb(color.to_u32()).into()
}

/// `fg` 与 `bg` 的中间色，用于暗淡（SGR 2）文字。
fn faint(fg: Rgb, bg: Rgb) -> Rgb {
    fg.mix(bg, 0.5)
}

fn paint_frame(
    view: &mut TerminalView,
    frame: &Frame,
    origin: Point<Pixels>,
    metrics: Metrics,
    focused: bool,
    window: &mut Window,
) {
    let cw = metrics.cell.width;
    let ch = metrics.cell.height;
    let cell_origin = |x: u16, y: u16| origin + point(cw * f32::from(x), ch * f32::from(y));
    let grid = Bounds::new(
        origin,
        size(cw * f32::from(frame.cols), ch * f32::from(frame.rows)),
    );

    let scale = window.scale_factor();
    let sprite_metrics = sprites::Metrics::new(
        f32::from(cw),
        f32::from(ch),
        f32::from(view.font_size) * UNDERLINE_THICKNESS_EM,
        scale,
    );

    // 闪烁到灭的一半时不画光标；没有焦点时光标不闪，总画空心框。光标落在选区里时也不画，
    // 免得盖住那一格的选区颜色。
    let cursor_hidden = frame.cursor.is_some_and(|c| {
        (focused && !view.cursor_blink_visible && c.blinking)
            || frame.row(c.y).get(usize::from(c.x)).is_some_and(|cell| cell.selected)
    });
    // 有焦点时块状光标是实心的，光标下的字形改用光标文字色（默认背景色）画，保证仍然看得清。
    let filled_cursor = frame.cursor.filter(|c| {
        focused && !cursor_hidden && c.shape == CursorShape::Block && view.marked_text.is_none()
    });

    window.paint_layer(grid, |window| {
        // 背景：每行把同色的相邻单元格合并成一块画。
        for y in 0..frame.rows {
            let row = frame.row(y);
            let mut x = 0usize;
            while x < row.len() {
                let Some(bg) = row[x].bg else {
                    x += 1;
                    continue;
                };
                let start = x;
                while x < row.len() && row[x].bg == Some(bg) {
                    x += 1;
                }
                window.paint_quad(fill(
                    Bounds::new(
                        cell_origin(start as u16, y),
                        size(cw * (x - start) as f32, ch),
                    ),
                    hsla(bg),
                ));
            }
        }

        if let Some(cursor) = filled_cursor {
            let width = if cursor.wide { cw * 2. } else { cw };
            window.paint_quad(fill(
                Bounds::new(cell_origin(cursor.x, cursor.y), size(width, ch)),
                hsla(cursor.color),
            ));
        }

        // 字形和装饰线。
        let baseline = (ch - metrics.ascent - metrics.descent) / 2. + metrics.ascent;
        for y in 0..frame.rows {
            for (x, cell) in frame.row(y).iter().enumerate() {
                let x = x as u16;
                if cell.spacer {
                    continue;
                }
                let bg = cell.bg.unwrap_or(frame.background);
                let mut fg = if cell.attrs.faint {
                    faint(cell.fg, bg)
                } else {
                    cell.fg
                };
                if let Some(cursor) = filled_cursor.filter(|c| c.x == x && c.y == y) {
                    fg = cursor.text;
                }
                let position = cell_origin(x, y);
                let width = if cell.wide { cw * 2. } else { cw };
                if cell.attrs.underline {
                    window.paint_quad(fill(
                        Bounds::new(position + point(px(0.), ch - px(2.)), size(width, px(1.))),
                        hsla(fg),
                    ));
                }
                if cell.attrs.strikethrough {
                    window.paint_quad(fill(
                        Bounds::new(position + point(px(0.), ch / 2.), size(width, px(1.))),
                        hsla(fg),
                    ));
                }
                if cell.text.is_empty() || cell.text == " " {
                    continue;
                }
                // 方框线、块元素、Powerline 等符号自绘，铺满单元格，不用字体字形。
                if !cell.wide
                    && let Some(shapes) = sprites::shapes(&cell.text, sprite_metrics)
                {
                    sprites::paint(&shapes, position, scale, hsla(fg), window);
                    continue;
                }
                let line = view.shape(&cell.text, cell.attrs, window);
                paint_glyphs(&line, position + point(px(0.), baseline), hsla(fg), window);
            }
        }
    });

    // 盖在文字上方的光标形状，以及输入法预编辑文本。
    let mut cursor_bounds = None;
    if let Some(cursor) = frame.cursor {
        let position = cell_origin(cursor.x, cursor.y);
        let width = if cursor.wide { cw * 2. } else { cw };
        cursor_bounds = Some(Bounds::new(position, size(width, ch)));
        let color = hsla(cursor.color);
        window.paint_layer(grid, |window| {
            if let Some(text) = view.marked_text.clone() {
                let line = view.shape(&text, Attrs::default(), window);
                let area = Bounds::new(position, size(line.width.max(cw), ch));
                window.paint_quad(fill(area, hsla(frame.background)));
                window.paint_quad(fill(
                    Bounds::new(position + point(px(0.), ch - px(2.)), size(area.size.width, px(1.))),
                    hsla(frame.foreground),
                ));
                let baseline = (ch - metrics.ascent - metrics.descent) / 2. + metrics.ascent;
                paint_glyphs(&line, position + point(px(0.), baseline), hsla(frame.foreground), window);
                return;
            }
            if cursor_hidden {
                return;
            }
            // 竖条、下划线和空心框的线宽都是一个设备像素。
            let line = px(1. / scale);
            let quads: &[Bounds<Pixels>] = match (focused, cursor.shape) {
                (true, CursorShape::Block) => &[],
                // 骑在单元格左边线上，落在两个字符之间而不是贴着右边的字符。
                (true, CursorShape::Bar) => &[Bounds::new(position - point(line, px(0.)), size(line, ch))],
                // 和文字下划线同一高度。
                (true, CursorShape::Underline) => {
                    &[Bounds::new(position + point(px(0.), ch - px(2.)), size(width, line))]
                }
                // 没有焦点或明确要求空心时：只画轮廓。
                _ => &[
                    Bounds::new(position, size(width, line)),
                    Bounds::new(position + point(px(0.), ch - line), size(width, line)),
                    Bounds::new(position, size(line, ch)),
                    Bounds::new(position + point(width - line, px(0.)), size(line, ch)),
                ],
            };
            for quad in quads {
                window.paint_quad(fill(*quad, color));
            }
        });
    }
    view.cursor_bounds = cursor_bounds;
}

/// 在 `baseline_origin`（x 为单元格左边缘，y 在基线上）绘制一行已排版字形，
/// 不像 `ShapedLine::paint` 那样每次调用都新建一层。
fn paint_glyphs(line: &ShapedLine, baseline_origin: Point<Pixels>, color: Hsla, window: &mut Window) {
    for run in &line.runs {
        for glyph in &run.glyphs {
            let position = baseline_origin + point(glyph.position.x, px(0.));
            let painted = if glyph.is_emoji {
                window.paint_emoji(position, run.font_id, glyph.id, line.font_size)
            } else {
                window.paint_glyph(position, run.font_id, glyph.id, line.font_size, color)
            };
            if let Err(err) = painted {
                tracing::debug!("glyph paint failed: {err}");
            }
        }
    }
}
