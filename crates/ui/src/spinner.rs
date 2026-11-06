//! 按几格字符转的圈。
//!
//! 每一格的字形排一次就缓存起来，转到哪格贴哪格的字形，不再每格重新排版。GPUI 把通知的视图连同外面
//! 各层视图都标成要重画，放在缓存视图里的转圈每动一格，整个缓存视图就得重新排版、重画；这种转圈经
//! `SpinnerLayer` 改画到缓存视图外面的一层上，动一格只重画那一层。

use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::LazyLock,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, ContentMask, Context, Element, ElementId, Entity, Font, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Render, ShapedLine, SharedString, Style, Task, Window,
    point, prelude::*, px, size,
};

/// 所有转圈共用的起点：同一种转圈在哪儿都转到同一格。
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

/// 每格至少停这么久：转一格整个窗口都要重新提交一帧，转得比这快的放慢到这个速度。
const MIN_FRAME_TIME: Duration = Duration::from_millis(250);

/// 缓存的字形超过这么多套时整个清空：只有字体、字号不断变化才会到。
const SHAPED_LIMIT: usize = 64;

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
            .map(|text| {
                let run = gpui::TextRun {
                    len: text.len(),
                    font: key.font.clone(),
                    color: Hsla::default(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                window.text_system().shape_line(SharedString::new_static(text), font_size, &[run], None)
            })
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

/// 此刻转到第几格，以及画面下一次变样的时刻；字一样的相邻几格算同一格，转圈只有一种字时没有下一次。
fn frame_at(frames: &[&str], frame_time: Duration, now: Instant) -> (usize, Option<Instant>) {
    let step_nanos = frame_time.as_nanos().max(1);
    let len = frames.len() as u128;
    let step = now.saturating_duration_since(*EPOCH).as_nanos() / step_nanos;
    let index = (step % len) as usize;
    let next = (1..len).map(|ahead| step + ahead).find(|next| frames[(next % len) as usize] != frames[index]);
    let at =
        next.and_then(|next| u64::try_from(next * step_nanos).ok()).map(|nanos| *EPOCH + Duration::from_nanos(nanos));
    (index, at)
}

/// 画好位置、等着画的一个转圈。
#[derive(Clone)]
struct Spin {
    frames: &'static [&'static str],
    frame_time: Duration,
    shaped: Rc<Shaped>,
    bounds: Bounds<Pixels>,
    mask: ContentMask<Pixels>,
    color: Hsla,
}

impl Spin {
    /// 画此刻的那一格，字在 `bounds` 里居中；返回画面下一次变样的时刻。
    fn paint(&self, now: Instant, window: &mut Window) -> Option<Instant> {
        let (index, next) = frame_at(self.frames, self.frame_time, now);
        let line = &self.shaped.lines[index];
        let x = self.bounds.origin.x + (self.bounds.size.width - line.width) / 2.;
        let baseline = self.bounds.origin.y + (self.bounds.size.height - line.ascent - line.descent) / 2. + line.ascent;
        window.with_content_mask(Some(self.mask), |window| {
            for run in &line.runs {
                for glyph in &run.glyphs {
                    let origin = point(x + glyph.position.x, baseline);
                    let painted = if glyph.is_emoji {
                        window.paint_emoji(origin, run.font_id, glyph.id, line.font_size)
                    } else {
                        window.paint_glyph(origin, run.font_id, glyph.id, line.font_size, self.color)
                    };
                    painted.ok();
                }
            }
        });
        next
    }
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
    SpinnerSlot { id: id.into(), frames, frame_time }
}

/// 转圈单独是一个视图：不在 `SpinnerLayer::scope` 里时由它自己定时重画，只通知它自己，不当成外层
/// 视图（桌面 app 的 `WindowView`）的状态变了，缓存的侧栏、标签栏和右侧面板不跟着重画。
struct Spinner {
    frames: &'static [&'static str],
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
        Glyphs { frames: self.frames, frame_time: self.frame_time, owner: cx.entity() }
    }
}

/// 在 `id` 处画 `Spinner`，连续几帧都画着时沿用同一个（存在元素状态里）。不用 `use_keyed_state`：
/// 它把状态的通知转给外层视图，转圈每动一格又成了外层的状态变了。
#[derive(IntoElement)]
struct SpinnerSlot {
    id: ElementId,
    frames: &'static [&'static str],
    frame_time: Duration,
}

impl RenderOnce for SpinnerSlot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, frames, frame_time } = self;
        let spinner = window.with_global_id(id, |id, window| {
            window.with_element_state(id, |state: Option<Entity<Spinner>>, _| {
                let spinner =
                    state.unwrap_or_else(|| cx.new(|_| Spinner { frames, frame_time, ticker: Ticker::default() }));
                (spinner.clone(), spinner)
            })
        });
        // 同一个位置上换了一种转法（agent 换了种类）时换成新的几格。
        spinner.update(cx, |spinner, _| {
            spinner.frames = frames;
            spinner.frame_time = frame_time;
        });
        spinner
    }
}

/// 按文字样式排好各格、占好地方的转圈：在 `SpinnerLayer::scope` 里时把自己登记给那一层，否则自己画、
/// 由 `owner` 定时重画。
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
            frames,
            frame_time: self.frame_time.max(MIN_FRAME_TIME),
            shaped,
            bounds,
            mask: window.content_mask(),
            color: window.text_style().color,
        };
        if let Some(scope) = SCOPES.with_borrow(|scopes| scopes.last().cloned()) {
            scope.borrow_mut().push(spin);
            return;
        }
        if let Some(at) = spin.paint(Instant::now(), window) {
            self.owner.update(cx, |spinner, cx| spinner.tick_at(at, cx));
        }
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
}
