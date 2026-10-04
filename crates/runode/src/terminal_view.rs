//! 单个终端会话的 GPUI 视图：输入分发和单元格绘制。

use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

use futures::StreamExt as _;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementId, ElementInputHandler, EntityInputHandler,
    FocusHandle, Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, KeyDownEvent,
    LayoutId, Pixels, Point, PromptLevel, Render, ScrollDelta, ScrollWheelEvent, ShapedLine, SharedString,
    Style, Subscription, Task, TextRun, UTF16Selection, Window, actions, div, fill, font, point, prelude::*, px,
    relative, rgb, size,
};
use libghostty_vt::key::Mods;

use crate::{
    config::{AppConfig, CellHeight, Config},
    keys,
    pty::{GridSize, PtyEvent},
    session::{Attrs, CursorShape, Frame, Paste as PasteResult, Rgb, SYNC_OUTPUT_TIMEOUT, Session},
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
/// 透明标题栏的高度：终端内容从它下面开始，这一条用来拖动窗口。
const TITLEBAR_HEIGHT: f32 = 28.;
/// 光标闪烁时亮、灭各持续的时长。
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    cell: gpui::Size<Pixels>,
    ascent: Pixels,
    descent: Pixels,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GlyphKey {
    text: String,
    bold: bool,
    italic: bool,
}

pub struct TerminalView {
    session: Session,
    config: Arc<Config>,
    focus_handle: FocusHandle,
    font: Font,
    font_size: Pixels,
    metrics: Option<Metrics>,
    /// 按文本和样式缓存的字形排版结果；颜色在绘制时再上。
    glyphs: HashMap<GlyphKey, ShapedLine>,
    /// 输入法尚未上屏的预编辑文本，画在光标处。
    marked_text: Option<String>,
    /// 精确滚动（触控板）不足一行的余量。
    scroll_remainder: f32,
    /// 光标单元格上次绘制的位置，供输入法候选窗定位。
    cursor_bounds: Option<Bounds<Pixels>>,
    /// 单元格网格的原点，用于把指针位置换算成单元格。
    grid_origin: Point<Pixels>,
    /// 闪烁光标当前处于亮的一半周期。
    cursor_blink_visible: bool,
    _reader: Task<()>,
    _hold_timeout: Option<Task<()>>,
    /// 有焦点时才运行的闪烁计时器。
    _cursor_blink: Option<Task<()>>,
    _config_watch: Subscription,
    _focus_watch: [Subscription; 2],
}

impl TerminalView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<Self> {
        // 临时尺寸；第一次布局时会按实际大小重设。
        let (mut session, mut rx) = Session::spawn(GridSize {
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
        })?;

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
                        if view.session.feed(&output) {
                            let title = view.session.title.as_deref().unwrap_or("runode");
                            window.set_window_title(title);
                        }
                        // 有输出（包括键入的回显）时光标先亮起，免得打字时看不到它。
                        view.reset_cursor_blink(window, cx);
                    }
                    if view.session.take_bell() {
                        window.play_system_bell();
                    }
                    if exited {
                        view.session.exited = true;
                        window.remove_window();
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
        let config_watch = cx.observe_global_in::<AppConfig>(window, |view, window, cx| {
            view.config = cx.global::<AppConfig>().0.clone();
            view.session.apply_config(&view.config);
            view.font = resolve_font(&view.config.font_family, window);
            view.font_size = px(view.config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
            // 字体、字号或行高调整都可能变了，单元格尺寸和字形缓存一律作废。
            view.metrics = None;
            view.glyphs.clear();
            cx.notify();
        });

        let focus_handle = cx.focus_handle();
        let focus_watch = [
            cx.on_focus(&focus_handle, window, |view, window, cx| {
                view.reset_cursor_blink(window, cx);
            }),
            cx.on_blur(&focus_handle, window, |view, _, _| view._cursor_blink = None),
        ];

        Ok(Self {
            session,
            font: resolve_font(&config.font_family, window),
            font_size: px(config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)),
            config,
            focus_handle,
            metrics: None,
            glyphs: HashMap::new(),
            marked_text: None,
            scroll_remainder: 0.,
            cursor_bounds: None,
            grid_origin: Point::default(),
            cursor_blink_visible: true,
            _reader: reader,
            _hold_timeout: None,
            _cursor_blink: None,
            _config_watch: config_watch,
            _focus_watch: focus_watch,
        })
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
        let local = event.position - self.grid_origin;
        let cell = (
            (f32::from(local.x) / f32::from(metrics.cell.width)).max(0.) as u16,
            (f32::from(local.y) / f32::from(metrics.cell.height)).max(0.) as u16,
        );
        let mut mods = Mods::empty();
        if event.modifiers.shift {
            mods |= Mods::SHIFT;
        }
        if event.modifiers.control {
            mods |= Mods::CTRL;
        }
        if event.modifiers.alt {
            mods |= Mods::ALT;
        }
        self.session.scroll(whole as isize, cell, mods);
        cx.notify();
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
        // 选择功能后续再做；在那之前先复制整个可见屏幕。
        let frame = self.session.frame();
        let mut text = String::new();
        for y in 0..frame.rows {
            let line: String = frame
                .row(y)
                .iter()
                .filter(|c| !c.spacer)
                .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
                .collect();
            text.push_str(line.trim_end());
            text.push('\n');
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text.trim_end().to_owned()));
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
        self.glyphs.clear();
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
        let key = GlyphKey {
            text: text.to_owned(),
            bold: attrs.bold,
            italic: attrs.italic,
        };
        if let Some(line) = self.glyphs.get(&key) {
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
            SharedString::from(key.text.clone()),
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
        if self.glyphs.len() > 8192 {
            self.glyphs.clear();
        }
        self.glyphs.insert(key, line.clone());
        line
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = self.session.frame().background;
        // 标题栏透明后内容铺到红绿灯下面，顶部这条要能拖动窗口、双击缩放。
        // 全屏时没有红绿灯，不留这一条。
        let titlebar = (!window.is_fullscreen()).then(|| {
            div()
                .id("titlebar")
                .h(px(TITLEBAR_HEIGHT))
                .flex_none()
                .on_mouse_down(gpui::MouseButton::Left, |event, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
        });
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
            .children(titlebar)
            .child(
                div()
                    .flex_1()
                    .pl(px(self.config.window_padding_x.0))
                    .pr(px(self.config.window_padding_x.1))
                    .pt(px(self.config.window_padding_y.0))
                    .pb(px(self.config.window_padding_y.1))
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

fn hsla(color: Rgb) -> Hsla {
    rgb(color.to_u32()).into()
}

/// `fg` 与 `bg` 的中间色，用于暗淡（SGR 2）文字。
fn faint(fg: Rgb, bg: Rgb) -> Rgb {
    let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    Rgb(mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
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
    // 有焦点时块状光标是实心的，光标下的字形改用背景色画，保证仍然看得清。
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
                if filled_cursor.is_some_and(|c| c.x == x && c.y == y) {
                    fg = frame.background;
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
