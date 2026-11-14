//! 会动的小标记：按几格字符转的圈（`spinner`），画出来的盲文格（`braille`）和闪烁的光标
//! （`blinking_cursor`），以及按秒走的倒计时（`countdown`）。
//!
//! 每一格的字形排一次就缓存起来，转到哪格贴哪格的字形，不再每格重新排版。GPUI 把通知的视图连同外面
//! 各层视图都标成要重画，放在缓存视图里的转圈每动一格，整个缓存视图就得重新排版、重画；这种转圈经
//! `SpinnerLayer` 改画到缓存视图外面的一层上，动一格只重画那一层。打开了 `ReduceMotion` 时转圈都只画
//! 静止的那一格，倒计时是信息不是动效，照样走。

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::LazyLock,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, ContentMask, Context, Element, ElementId, Entity, Font, Global,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Render, ShapedLine, SharedString, Style,
    Task, Window, point, prelude::*, px, size,
};

/// 所有转圈共用的起点：同一种转圈在哪儿都转到同一格。
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// 每格至少停这么久：转一格整个窗口都要重新提交一帧，转得比这快的放慢到这个速度。
const MIN_FRAME_TIME: Duration = Duration::from_millis(250);

/// 缓存的字形超过这么多套时整个清空：只有字体、字号不断变化才会到。
const SHAPED_LIMIT: usize = 64;

/// 盲文格和光标按这么多格见方的坐标系定位置，画的时候按实际边长等比缩放。
const GRID: f32 = 24.;

/// 盲文格六个点的圆心，从左上起顺时针；掩码的第 i 位对应第 i 个点。
const BRAILLE_DOTS: [(f32, f32); 6] = [(7.5, 4.), (16.5, 4.), (16.5, 12.), (16.5, 20.), (7.5, 20.), (7.5, 12.)];
const BRAILLE_DOT_RADIUS: f32 = 2.5;
/// 暗着的点的不透明度。
const BRAILLE_DIM: f32 = 0.22;
/// 盲文格每一格亮的点：第 k 格亮第 k、k−1、k−2 个点，三个亮点顺时针走一圈。
const BRAILLE_FRAMES: [u8; 6] = [0b11_0001, 0b10_0011, 0b00_0111, 0b00_1110, 0b01_1100, 0b11_1000];
/// 盲文格静止时亮的点：从左上起顺时针的前三个。
const BRAILLE_STILL: u8 = 0b00_0111;

/// 光标在坐标系里的左、上、右、下和圆角。
const CURSOR_RECT: (f32, f32, f32, f32) = (5.5, 1., 18.5, 23.);
const CURSOR_RADIUS: f32 = 1.5;
/// 光标闪烁时每一格的不透明度：亮的久、暗的短。
const CURSOR_FRAMES: [f32; 5] = [1., 1., 1., 0.3, 0.3];

/// 系统设置里要求减少动态效果：为真时会动的标记都只画静止的那一格，不再定时重画。由 app 设，没设时照常动。
pub struct ReduceMotion(pub bool);

impl Global for ReduceMotion {}

fn reduce_motion(cx: &App) -> bool {
    cx.try_global::<ReduceMotion>().is_some_and(|reduce| reduce.0)
}

thread_local! {
    /// 每套转圈在每种字体、字号下排好的字形。
    static SHAPED: RefCell<HashMap<ShapeKey, Rc<Shaped>>> = RefCell::new(HashMap::new());
    /// 正在画的 `SpinnerLayer::scope` 里登记转圈的那一层；嵌套时取最里面的。
    static SCOPES: RefCell<Vec<Rc<RefCell<Vec<Spin>>>>> = const { RefCell::new(Vec::new()) };
}

#[derive(PartialEq, Eq, Hash)]
struct ShapeKey {
    frames: &'static [&'static str],
    font: Font,
    font_size: u32,
}

/// 一套转圈排好的各格字形，`width` 是最宽的那格。
struct Shaped {
    lines: Vec<ShapedLine>,
    width: Pixels,
}

fn shaped(frames: &'static [&'static str], font: Font, font_size: Pixels, window: &Window) -> Rc<Shaped> {
    let key = ShapeKey { frames, font, font_size: f32::from(font_size).to_bits() };
    SHAPED.with_borrow_mut(|cache| {
        if let Some(shaped) = cache.get(&key) {
            return shaped.clone();
        }
        let lines: Vec<ShapedLine> = frames
            .iter()
            .map(|text| shape_text(SharedString::new_static(text), key.font.clone(), font_size, window))
            .collect();
        let width = lines.iter().map(|line| line.width).fold(px(0.), Pixels::max);
        if cache.len() >= SHAPED_LIMIT {
            cache.clear();
        }
        let shaped = Rc::new(Shaped { lines, width });
        cache.insert(key, shaped.clone());
        shaped
    })
}

/// 此刻转到第几格，以及画面下一次变样的时刻；一样的相邻几格算同一格，只有一种格时没有下一次。
fn frame_at<T: PartialEq>(frames: &[T], frame_time: Duration, now: Instant) -> (usize, Option<Instant>) {
    let step_nanos = frame_time.as_nanos().max(1);
    let len = frames.len() as u128;
    let step = now.saturating_duration_since(*EPOCH).as_nanos() / step_nanos;
    let index = (step % len) as usize;
    let next = (1..len).map(|ahead| step + ahead).find(|next| frames[(next % len) as usize] != frames[index]);
    let at =
        next.and_then(|next| u64::try_from(next * step_nanos).ok()).map(|nanos| *EPOCH + Duration::from_nanos(nanos));
    (index, at)
}

/// 倒计时的宽按这段字量，各位数字一样宽时装得下 99 分 59 秒以内的「分:秒」。
const COUNTDOWN_WIDEST: [&str; 1] = ["00:00"];

/// 秒数写成「分:秒」。
fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// `now` 时离 `until` 还剩几秒（不足一秒的算一秒，到点时为零），以及剩下的秒数下一次变的时刻；到点后
/// 没有下一次。
fn countdown_at(until: Instant, now: Instant) -> (u64, Option<Instant>) {
    let left = until.saturating_duration_since(now);
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    let next = secs.checked_sub(1).and_then(|whole| until.checked_sub(Duration::from_secs(whole)));
    (secs, next)
}

/// 每一格画什么。各种的格数都不为零。
#[derive(Clone)]
enum Look {
    /// 一格一段字，排好的字形在 `shaped` 里。
    Glyphs { frames: &'static [&'static str], shaped: Rc<Shaped> },
    /// 一格是盲文格六个点的亮灭掩码。
    Dots(&'static [u8]),
    /// 一格是光标的不透明度。
    Bar(&'static [f32]),
    /// 一格是离 `until` 还剩的秒数，画的时候按 `font`、`font_size` 排成「分:秒」；不受 `ReduceMotion`
    /// 管，`Spin::still` 恒为假。
    Countdown { until: Instant, font: Font, font_size: Pixels },
}

impl Look {
    fn frame_at(&self, frame_time: Duration, now: Instant) -> (usize, Option<Instant>) {
        match self {
            Self::Glyphs { frames, .. } => frame_at(frames, frame_time, now),
            Self::Dots(frames) => frame_at(frames, frame_time, now),
            Self::Bar(frames) => frame_at(frames, frame_time, now),
            Self::Countdown { until, .. } => {
                let (secs, next) = countdown_at(*until, now);
                (usize::try_from(secs).unwrap_or(usize::MAX), next)
            }
        }
    }
}

/// 不动时画的那一格：盲文格亮 `BRAILLE_STILL` 的那格（没有时第一格），别的都是第一格。
fn still_frame(look: &Look) -> usize {
    match look {
        Look::Dots(frames) => frames.iter().position(|mask| *mask == BRAILLE_STILL).unwrap_or(0),
        Look::Glyphs { .. } | Look::Bar(_) | Look::Countdown { .. } => 0,
    }
}

/// 画好位置、等着画的一个转圈。
#[derive(Clone)]
struct Spin {
    look: Look,
    frame_time: Duration,
    /// 只画静止的那一格，不排下一次。
    still: bool,
    bounds: Bounds<Pixels>,
    mask: ContentMask<Pixels>,
    color: Hsla,
}

impl Spin {
    /// 画此刻的那一格，在 `bounds` 里居中；返回画面下一次变样的时刻。
    fn paint(&self, now: Instant, window: &mut Window) -> Option<Instant> {
        let (index, next) =
            if self.still { (still_frame(&self.look), None) } else { self.look.frame_at(self.frame_time, now) };
        window.with_content_mask(Some(self.mask), |window| match &self.look {
            Look::Glyphs { shaped, .. } => paint_glyphs(&shaped.lines[index], self.bounds, self.color, window),
            Look::Dots(frames) => paint_dots(frames[index], self.bounds, self.color, window),
            Look::Bar(frames) => paint_bar(frames[index], self.bounds, self.color, window),
            Look::Countdown { font, font_size, .. } => {
                let line = shape_text(clock(index as u64).into(), font.clone(), *font_size, window);
                paint_glyphs(&line, self.bounds, self.color, window);
            }
        });
        next
    }
}

/// 按 `font`、`font_size` 排一段字，颜色在画的时候给。
fn shape_text(text: SharedString, font: Font, font_size: Pixels, window: &Window) -> ShapedLine {
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(text, font_size, &[run], None)
}

fn paint_glyphs(line: &ShapedLine, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let x = bounds.origin.x + (bounds.size.width - line.width) / 2.;
    let baseline = bounds.origin.y + (bounds.size.height - line.ascent - line.descent) / 2. + line.ascent;
    for run in &line.runs {
        for glyph in &run.glyphs {
            let origin = point(x + glyph.position.x, baseline);
            let painted = if glyph.is_emoji {
                window.paint_emoji(origin, run.font_id, glyph.id, line.font_size)
            } else {
                window.paint_glyph(origin, run.font_id, glyph.id, line.font_size, color)
            };
            painted.ok();
        }
    }
}

/// `bounds` 里居中的那个正方形，以及坐标系一格折成多少点。
fn grid(bounds: Bounds<Pixels>) -> (gpui::Point<Pixels>, f32) {
    let side = bounds.size.width.min(bounds.size.height);
    let origin = bounds.origin + point((bounds.size.width - side) / 2., (bounds.size.height - side) / 2.);
    (origin, f32::from(side) / GRID)
}

fn paint_dots(mask: u8, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let (origin, k) = grid(bounds);
    let radius = px(BRAILLE_DOT_RADIUS * k);
    for (i, (x, y)) in BRAILLE_DOTS.iter().enumerate() {
        let lit = mask & (1 << i) != 0;
        let center = origin + point(px(x * k), px(y * k));
        let dot = Bounds::centered_at(center, size(radius * 2., radius * 2.));
        window.paint_quad(gpui::fill(dot, if lit { color } else { color.opacity(BRAILLE_DIM) }).corner_radii(radius));
    }
}

fn paint_bar(opacity: f32, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let (origin, k) = grid(bounds);
    let (left, top, right, bottom) = CURSOR_RECT;
    let rect =
        Bounds::from_corners(origin + point(px(left * k), px(top * k)), origin + point(px(right * k), px(bottom * k)));
    window.paint_quad(gpui::fill(rect, color.opacity(opacity)).corner_radii(px(CURSOR_RADIUS * k)));
}

/// 定时叫视图重画：转圈下一格到了时通知一次，同一时刻只排一个。
#[derive(Default)]
struct Ticker {
    due: Option<Instant>,
    _task: Option<Task<()>>,
}

trait Ticking: 'static + Sized {
    fn ticker(&mut self) -> &mut Ticker;

    /// 到 `at` 时通知这个视图重画；已经排了更早的就不再排。
    fn tick_at(&mut self, at: Instant, cx: &mut Context<Self>) {
        if self.ticker().due.is_some_and(|due| due <= at) {
            return;
        }
        let delay = at.saturating_duration_since(Instant::now());
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |view, cx| {
                view.ticker().due = None;
                cx.notify();
            })
            .ok();
        });
        *self.ticker() = Ticker { due: Some(at), _task: Some(task) };
    }
}

/// 在 `id` 处画一个按 `frames` 转的圈，每格停 `frame_time`，至少 `MIN_FRAME_TIME`。字体、字号和颜色
/// 跟着外面的文字样式，占的宽是最宽那格的宽，每格在里面居中。
pub fn spinner(id: impl Into<ElementId>, frames: &'static [&'static str], frame_time: Duration) -> impl IntoElement {
    SpinnerSlot { id: id.into(), source: Source::Glyphs(frames), frame_time }
}

/// 在 `id` 处画一个 `side` 见方的盲文格，六个点里亮着的三个顺时针走，每格停 `MIN_FRAME_TIME`。颜色跟着
/// 外面的文字颜色。
pub fn braille(id: impl Into<ElementId>, side: f32) -> impl IntoElement {
    SpinnerSlot { id: id.into(), source: Source::Shape(Look::Dots(&BRAILLE_FRAMES), side), frame_time: MIN_FRAME_TIME }
}

/// `side` 见方、不动的盲文格，亮 `braille` 静止时亮的那几个点。不是视图，不定时重画；颜色跟着外面的文字颜色。
pub fn braille_still(side: f32) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        |bounds, (), window, _| paint_dots(BRAILLE_STILL, bounds, window.text_style().color, window),
    )
    .flex_none()
    .size(px(side))
}

/// 在 `id` 处画一个 `side` 见方、闪烁着的竖条光标，每格停 `MIN_FRAME_TIME`。颜色跟着外面的文字颜色。
pub fn blinking_cursor(id: impl Into<ElementId>, side: f32) -> impl IntoElement {
    SpinnerSlot { id: id.into(), source: Source::Shape(Look::Bar(&CURSOR_FRAMES), side), frame_time: MIN_FRAME_TIME }
}

/// 在 `id` 处画离 `until` 还剩多久，写成「分:秒」，每秒变一次，到点后停在「0:00」。字体、字号和颜色跟着
/// 外面的文字样式，占的宽按 `COUNTDOWN_WIDEST` 量，字在里面居中。辅助工具读不到它，调用方另给文字。
pub fn countdown(id: impl Into<ElementId>, until: Instant) -> impl IntoElement {
    SpinnerSlot { id: id.into(), source: Source::Countdown(until), frame_time: COUNTDOWN_STEP }
}

/// 倒计时每格停的时间。
const COUNTDOWN_STEP: Duration = Duration::from_secs(1);

/// `Spinner` 画什么：按文字样式排的几格字，`side` 见方画出来的形状（`Look::Dots`、`Look::Bar`），或者
/// 到某一刻为止的倒计时。
#[derive(Clone)]
enum Source {
    Glyphs(&'static [&'static str]),
    Shape(Look, f32),
    Countdown(Instant),
}

/// 转圈单独是一个视图：不在 `SpinnerLayer::scope` 里时由它自己定时重画，只通知它自己，不当成外层
/// 视图（桌面 app 的 `WindowView`）的状态变了，缓存的侧栏、标签栏和右侧面板不跟着重画。
struct Spinner {
    source: Source,
    frame_time: Duration,
    ticker: Ticker,
}

impl Ticking for Spinner {
    fn ticker(&mut self) -> &mut Ticker {
        &mut self.ticker
    }
}

impl Render for Spinner {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity();
        match &self.source {
            Source::Glyphs(frames) => Glyphs { frames, frame_time: self.frame_time, owner }.into_any_element(),
            Source::Shape(look, side) => Shape { look: look.clone(), side: *side, owner }.into_any_element(),
            Source::Countdown(until) => Countdown { until: *until, owner }.into_any_element(),
        }
    }
}

/// 在 `id` 处画 `Spinner`，连续几帧都画着时沿用同一个（存在元素状态里）。不用 `use_keyed_state`：
/// 它把状态的通知转给外层视图，转圈每动一格又成了外层的状态变了。
#[derive(IntoElement)]
struct SpinnerSlot {
    id: ElementId,
    source: Source,
    frame_time: Duration,
}

impl RenderOnce for SpinnerSlot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, source, frame_time } = self;
        let spinner = window.with_global_id(id, |id, window| {
            window.with_element_state(id, |state: Option<Entity<Spinner>>, _| {
                let spinner = state.unwrap_or_else(|| {
                    cx.new(|_| Spinner { source: source.clone(), frame_time, ticker: Ticker::default() })
                });
                (spinner.clone(), spinner)
            })
        });
        // 同一个位置上换了一种转法（agent 换了种类、状态）时换成新的几格。
        spinner.update(cx, |spinner, _| {
            spinner.source = source;
            spinner.frame_time = frame_time;
        });
        spinner
    }
}

/// 排好位置的转圈交出去画：在 `SpinnerLayer::scope` 里时登记给那一层，否则自己画、由 `owner` 定时重画。
fn submit(spin: Spin, owner: &Entity<Spinner>, window: &mut Window, cx: &mut App) {
    if let Some(scope) = SCOPES.with_borrow(|scopes| scopes.last().cloned()) {
        scope.borrow_mut().push(spin);
        return;
    }
    if let Some(at) = spin.paint(Instant::now(), window) {
        owner.update(cx, |spinner, cx| spinner.tick_at(at, cx));
    }
}

/// 按文字样式排好各格、占好地方的转圈。
struct Glyphs {
    frames: &'static [&'static str],
    frame_time: Duration,
    owner: Entity<Spinner>,
}

impl IntoElement for Glyphs {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Glyphs {
    type RequestLayoutState = Option<Rc<Shaped>>;
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
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = window.text_style();
        let rem = window.rem_size();
        let line_height = text.line_height_in_pixels(rem);
        // 没有帧时没东西可画，只占一行高。
        let shaped =
            (!self.frames.is_empty()).then(|| shaped(self.frames, text.font(), text.font_size.to_pixels(rem), window));
        let width = shaped.as_ref().map_or(px(0.), |shaped| shaped.width);
        let style = Style { flex_shrink: 0., size: size(width.into(), line_height.into()), ..Style::default() };
        (window.request_layout(style, [], cx), shaped)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        shaped: &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(shaped) = shaped.clone() else {
            return;
        };
        // 每格停零秒时帧率没有上限，只画第一格。
        let frames: &'static [&'static str] = if self.frame_time.is_zero() { &self.frames[..1] } else { self.frames };
        let spin = Spin {
            look: Look::Glyphs { frames, shaped },
            frame_time: self.frame_time.max(MIN_FRAME_TIME),
            still: reduce_motion(cx),
            bounds,
            mask: window.content_mask(),
            color: window.text_style().color,
        };
        submit(spin, &self.owner, window, cx);
    }
}

/// `side` 见方、不量字的转圈，颜色取外面的文字颜色。
struct Shape {
    look: Look,
    side: f32,
    owner: Entity<Spinner>,
}

impl IntoElement for Shape {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Shape {
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
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let side = px(self.side);
        let style = Style { flex_shrink: 0., size: size(side.into(), side.into()), ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let spin = Spin {
            look: self.look.clone(),
            frame_time: MIN_FRAME_TIME,
            still: reduce_motion(cx),
            bounds,
            mask: window.content_mask(),
            color: window.text_style().color,
        };
        submit(spin, &self.owner, window, cx);
    }
}

/// 按文字样式占好地方的倒计时。
struct Countdown {
    until: Instant,
    owner: Entity<Spinner>,
}

impl IntoElement for Countdown {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Countdown {
    type RequestLayoutState = (Font, Pixels);
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
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = window.text_style();
        let rem = window.rem_size();
        let line_height = text.line_height_in_pixels(rem);
        let (font, font_size) = (text.font(), text.font_size.to_pixels(rem));
        let width = shaped(&COUNTDOWN_WIDEST, font.clone(), font_size, window).width;
        let style = Style { flex_shrink: 0., size: size(width.into(), line_height.into()), ..Style::default() };
        (window.request_layout(style, [], cx), (font, font_size))
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (font, font_size): &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let spin = Spin {
            look: Look::Countdown { until: self.until, font: font.clone(), font_size: *font_size },
            frame_time: COUNTDOWN_STEP,
            still: false,
            bounds,
            mask: window.content_mask(),
            color: window.text_style().color,
        };
        submit(spin, &self.owner, window, cx);
    }
}

/// 缓存视图里的转圈改画到视图外面的一层：用这一层的视图每帧都在 render 里 `clear`、把画的东西包进
/// `scope`，再把 `overlay` 放进这个视图外面、紧挨着它画在它后面的地方。缓存视图照搬上一帧时不重画
/// 它里面的转圈，登记的位置沿用上一次的；转圈动一格只通知 `overlay`。
#[derive(Clone)]
pub struct SpinnerLayer {
    spins: Rc<RefCell<Vec<Spin>>>,
    overlay: Entity<Overlay>,
}

impl SpinnerLayer {
    pub fn new(cx: &mut App) -> Self {
        let spins = Rc::new(RefCell::new(Vec::new()));
        let overlay = cx.new(|_| Overlay { spins: spins.clone(), ticker: Ticker::default() });
        Self { spins, overlay }
    }

    /// 缓存视图重画了：上次登记的转圈作废，接着画的时候重新登记。
    pub fn clear(&self) {
        self.spins.borrow_mut().clear();
    }

    /// 画 `child` 时，里面的转圈登记给这一层，不自己画。
    pub fn scope(&self, child: impl IntoElement) -> AnyElement {
        Scope { spins: self.spins.clone(), child: child.into_any_element() }.into_any_element()
    }

    /// 画登记了的转圈的那一层，不占地方。
    pub fn overlay(&self) -> AnyElement {
        self.overlay.clone().into_any_element()
    }
}

struct Scope {
    spins: Rc<RefCell<Vec<Spin>>>,
    child: AnyElement,
}

impl IntoElement for Scope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scope {
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
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        SCOPES.with_borrow_mut(|scopes| scopes.push(self.spins.clone()));
        self.child.paint(window, cx);
        SCOPES.with_borrow_mut(|scopes| scopes.pop());
    }
}

/// `SpinnerLayer` 画转圈的那一层，自己定时重画。
struct Overlay {
    spins: Rc<RefCell<Vec<Spin>>>,
    ticker: Ticker,
}

impl Ticking for Overlay {
    fn ticker(&mut self) -> &mut Ticker {
        &mut self.ticker
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let spins = self.spins.clone();
        let overlay = cx.entity();
        gpui::canvas(
            |_, _, _| {},
            move |_, _, window, cx| {
                let now = Instant::now();
                let next = spins.borrow().iter().filter_map(|spin| spin.paint(now, window)).min();
                if let Some(at) = next {
                    overlay.update(cx, |overlay, cx| overlay.tick_at(at, cx));
                }
            },
        )
        .absolute()
        .size_0()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAMES: [&str; 4] = ["a", "b", "b", "c"];

    fn at(steps: u32) -> Instant {
        *EPOCH + Duration::from_millis(100) * steps + Duration::from_millis(1)
    }

    #[test]
    fn frames_follow_the_shared_clock() {
        assert_eq!(frame_at(&FRAMES, Duration::from_millis(100), at(0)).0, 0);
        assert_eq!(frame_at(&FRAMES, Duration::from_millis(100), at(5)).0, 1);
    }

    #[test]
    fn the_next_change_skips_repeated_frames() {
        let (index, next) = frame_at(&FRAMES, Duration::from_millis(100), at(1));
        assert_eq!(index, 1);
        assert_eq!(next, Some(*EPOCH + Duration::from_millis(300)));
    }

    #[test]
    fn a_single_frame_never_changes() {
        assert_eq!(frame_at(&["a", "a"], Duration::from_millis(100), at(3)).1, None);
    }

    #[test]
    fn braille_masks_light_three_in_a_row() {
        let dots = BRAILLE_DOTS.len();
        assert_eq!(BRAILLE_FRAMES.len(), dots);
        for (k, mask) in BRAILLE_FRAMES.iter().enumerate() {
            assert_eq!(mask.count_ones(), 3, "frame {k}");
            assert_eq!(u32::from(*mask) >> dots, 0, "frame {k}");
            // 亮的三点首尾相接：从第 k 个点往回数三个。
            let run: u8 = (0..3).map(|back| 1 << ((k + dots - back) % dots)).sum();
            assert_eq!(*mask, run, "frame {k}");
        }
    }

    #[test]
    fn reduced_motion_paints_the_still_frame() {
        assert_eq!(BRAILLE_FRAMES[still_frame(&Look::Dots(&BRAILLE_FRAMES))], BRAILLE_STILL);
        assert_eq!(BRAILLE_STILL.count_ones(), 3);
        assert_eq!(CURSOR_FRAMES[still_frame(&Look::Bar(&CURSOR_FRAMES))], 1.);
        let shaped = Rc::new(Shaped { lines: Vec::new(), width: px(0.) });
        assert_eq!(still_frame(&Look::Glyphs { frames: &FRAMES, shaped }), 0);
    }

    #[test]
    fn the_clock_reads_as_minutes_and_seconds() {
        assert_eq!(clock(600), "10:00");
        assert_eq!(clock(65), "1:05");
        assert_eq!(clock(0), "0:00");
    }

    #[test]
    fn the_countdown_changes_on_whole_seconds_left() {
        let until = *EPOCH + Duration::from_secs(10);
        let (half, secs) = (Duration::from_millis(500), Duration::from_secs);
        assert_eq!(countdown_at(until, until - secs(2) - half), (3, Some(until - secs(2))));
        assert_eq!(countdown_at(until, until - secs(2)), (2, Some(until - secs(1))));
        assert_eq!(countdown_at(until, until - half), (1, Some(until)));
        assert_eq!(countdown_at(until, until), (0, None));
        assert_eq!(countdown_at(until, until + half), (0, None));
    }

    #[test]
    fn the_cursor_blinks_mostly_lit() {
        let lit = CURSOR_FRAMES.iter().filter(|opacity| **opacity == 1.).count();
        assert_eq!((CURSOR_FRAMES.len(), lit), (5, 3));
        // 一圈里只变样两次：亮转暗、暗转亮，相邻一样的几格不另排重画。
        let step = MIN_FRAME_TIME;
        let (_, next) = frame_at(&CURSOR_FRAMES, step, *EPOCH + Duration::from_millis(1));
        assert_eq!(next, Some(*EPOCH + step * 3));
        let (_, next) = frame_at(&CURSOR_FRAMES, step, *EPOCH + step * 3 + Duration::from_millis(1));
        assert_eq!(next, Some(*EPOCH + step * 5));
    }
}
