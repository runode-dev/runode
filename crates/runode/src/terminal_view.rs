//! 单个终端会话的 GPUI 视图：输入分发和单元格绘制。

use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font,
    FontStyle, FontWeight, GlobalElementId, Hsla, KeyDownEvent, LayoutId, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, PromptLevel, Render, ScrollDelta,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Subscription, Task, TextRun, UTF16Selection,
    Window, actions, div, fill, font, point, prelude::*, px, relative, rgb, size,
};
use libghostty_vt::{key::Mods, mouse};

use crate::{
    config::{AppConfig, CellHeight, Config},
    keys,
    pty::{GridSize, PtyEvent},
    session::{Attrs, CursorShape, Frame, GridPoint, Paste as PasteResult, Rgb, SYNC_OUTPUT_TIMEOUT, Session},
    sprites,
};

actions!(
    runode,
    [Copy, Paste, IncreaseFontSize, DecreaseFontSize, ResetFontSize]
);

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
type GlyphCache = [HashMap<String, ShapedLine>; 4];

fn glyph_table(attrs: Attrs) -> usize {
    usize::from(attrs.bold) | usize::from(attrs.italic) << 1
}

/// 终端视图通知外层（标签栏）的事件。
pub enum TerminalEvent {
    /// 运行中的程序改了标题（OSC 0/2）。
    TitleChanged,
    /// 程序响铃（BEL）。
    Bell,
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
    /// 闪烁光标当前处于亮的一半周期。
    cursor_blink_visible: bool,
    _reader: Task<()>,
    _foreground_poll: Task<()>,
    _hold_timeout: Option<Task<()>>,
    /// 有焦点时才运行的闪烁计时器。
    _cursor_blink: Option<Task<()>>,
    /// 拖选到网格外时运行的自动滚动计时器。
    _autoscroll: Option<Task<()>>,
    _config_watch: Subscription,
    _appearance_watch: Subscription,
    _focus_watch: [Subscription; 2],
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    /// 启动一个 shell 会话并建好它的视图；shell 起不来时返回错误，不建视图。
    pub fn spawn(window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        // 临时尺寸；第一次布局时会按实际大小重设。
        let (session, rx) = Session::spawn(GridSize {
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
        })?;
        Ok(cx.new(|cx| Self::new(session, rx, window, cx)))
    }

    fn new(
        mut session: Session,
        mut rx: futures::channel::mpsc::UnboundedReceiver<PtyEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reader = cx.spawn_in(window, async move |this, cx| {
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
                    next = rx.try_recv().ok();
                }
                let updated = this.update_in(cx, |view, window, cx| {
                    if !output.is_empty() {
                        // 进出目录、启动或退出程序时通常都有输出，顺带重读前台进程。
                        let fallback_changed = view.session.refresh_fallback_title();
                        if view.session.feed(&output) || fallback_changed {
                            cx.emit(TerminalEvent::TitleChanged);
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

        let config = cx.global::<AppConfig>().0.clone();
        session.apply_config(&config);
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

        let foreground_poll = cx.spawn(async move |this, cx| {
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
        });

        let focus_handle = cx.focus_handle();
        let focus_watch = [
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
            cursor_blink_visible: true,
            _reader: reader,
            _foreground_poll: foreground_poll,
            _hold_timeout: None,
            _cursor_blink: None,
            _autoscroll: None,
            _config_watch: config_watch,
            _appearance_watch: appearance_watch,
            _focus_watch: focus_watch,
        }
    }

    /// 程序设置的标题；没设置时为前台进程的目录名或进程名，都没有时为 `DEFAULT_TITLE`。
    pub fn title(&self) -> &str {
        self.session
            .title
            .as_deref()
            .or(self.session.fallback_title.as_deref())
            .unwrap_or(DEFAULT_TITLE)
    }

    /// 当前的默认前景色和背景色，标签栏跟着终端配色走。
    pub fn colors(&mut self) -> (Rgb, Rgb) {
        let frame = self.session.frame();
        (frame.foreground, frame.background)
    }

    /// 让光标立即亮起，并从头开始计闪烁周期；没有焦点时不闪，也就不启动计时器。
    fn reset_cursor_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.cursor_blink_visible = true;
        if !self.focus_handle.is_focused(window) {
            return;
        }
        self._cursor_blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CURSOR_BLINK_INTERVAL).await;
                let updated = this.update(cx, |view, cx| {
                    view.cursor_blink_visible = !view.cursor_blink_visible;
                    if view.session.frame().cursor.is_some_and(|c| c.blinking) {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
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
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.selecting = true;
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
        if inside && self.session.mouse_tracking() {
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
            }
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
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if self.session.paste(&text, false) == PasteResult::Done {
            return;
        }
        // 可能直接执行命令的粘贴先让用户确认。
        let answer = window.prompt(
            PromptLevel::Warning,
            "粘贴的内容可能会直接执行命令",
            Some("内容含有换行或终端控制序列，粘贴后可能被当作命令立即运行。"),
            &["粘贴", "取消"],
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

    fn shape(&mut self, text: &str, attrs: Attrs, window: &Window) -> ShapedLine {
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
        let line = window.text_system().shape_line(
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
        );
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
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
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
            )
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
            let metrics = view.metrics(window);
            let cols = (f32::from(bounds.size.width) / f32::from(metrics.cell.width)).floor();
            let rows = (f32::from(bounds.size.height) / f32::from(metrics.cell.height)).floor();
            let scale = window.scale_factor();
            view.session.resize(GridSize {
                cols: cols.clamp(1., u16::MAX as f32) as u16,
                rows: rows.clamp(1., u16::MAX as f32) as u16,
                cell_width_px: (f32::from(metrics.cell.width) * scale).round() as u16,
                cell_height_px: (f32::from(metrics.cell.height) * scale).round() as u16,
            });
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

    // 闪烁到灭的一半时不画光标；没有焦点时光标不闪，总画空心框。
    let cursor_hidden = focused && !view.cursor_blink_visible && frame.cursor.is_some_and(|c| c.blinking);
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
