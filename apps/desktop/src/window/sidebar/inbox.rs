//! 侧栏顶上的「需要你」：本窗口里在等用户回答的 agent 分屏，一个分屏一行，等得最久的在上面，按下跳到那个分屏。
//! 本窗口有 agent、却没有一个在等时是一个虚线框的空状态，一个 agent 都没有时整段不画。

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, FontWeight, Role, Window, div, prelude::*, px, svg};

use super::project_group::project_name;
use crate::{
    agent_prompts,
    git_panel::ago::waited,
    ui::{
        a11y::{Press as _, StaticText as _},
        font_size::{FontSize, scaled},
        icons::{CHECK_ICON, EYE_ICON},
        theme::ActiveTheme as _,
    },
    window::{
        AGENT_MARK_SIZE_SMALL, WindowView,
        agents::{self, AgentEntry, Status},
        titlebar::{agent_mark, pane_label},
    },
};

/// 卡片里一行的左右留白，行间分隔线左右也缩进这么多。
const ROW_INSET: f32 = 11.;
/// 一行里标记、名字、等了多久那一行的高度。
const ROW_LINE_HEIGHT: f32 = 22.;
/// 标记和名字的间距。
const MARK_GAP: f32 = 8.;
/// 卡片和空状态虚线框的圆角。
const CARD_RADIUS: f32 = 12.;
/// 卡片最多占窗口高度的这个比例，行多了在卡片里滚动，下面的项目列表不会被挤没。
const MAX_HEIGHT_RATIO: f32 = 0.4;
/// 重画只为了让「等了 N 分钟」跟上，按这个间隔取整。
const REDRAW_STEP: Duration = Duration::from_secs(60);

/// 只留等回答的，等得最久（`key` 给出的时刻最早）的排前面；时刻相同的保持原来的先后。
pub(in crate::window) fn waiting_oldest_first<T>(entries: Vec<T>, key: impl Fn(&T) -> (Status, Instant)) -> Vec<T> {
    let mut waiting: Vec<T> = entries.into_iter().filter(|entry| key(entry).0 == Status::Blocked).collect();
    waiting.sort_by_key(|entry| key(entry).1);
    waiting
}

/// 从 `since` 起等到 `now`，下一个满整分钟的时刻；正好满整分钟时取再下一个。
fn next_whole_minute(since: Instant, now: Instant) -> Instant {
    let step = REDRAW_STEP.as_secs();
    let passed = now.saturating_duration_since(since).as_secs() / step;
    since + Duration::from_secs((passed + 1) * step)
}

/// 各行分别从 `waits` 里的时刻起等，到 `now` 之后最早有一行满整分钟的时刻；没有在等的为 `None`。
fn next_redraw(waits: impl IntoIterator<Item = Instant>, now: Instant) -> Option<Instant> {
    waits.into_iter().map(|since| next_whole_minute(since, now)).min()
}

impl WindowView {
    /// 侧栏的「需要你」：有等回答的分屏时是标题和一张卡片，没有时是空状态；本窗口一个 agent 都没有时为 `None`。
    /// 顺带排好下一次重画。
    pub(super) fn render_inbox(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entries = self.window_entries(window, cx);
        let any_agent = !entries.is_empty();
        let waiting = waiting_oldest_first(entries, |entry| (entry.mark.status, entry.changed_at));
        self.schedule_inbox_redraw(waiting.iter().map(|entry| entry.changed_at), cx);
        if waiting.is_empty() {
            return any_agent.then(|| render_empty(cx));
        }
        let theme = cx.theme();
        let (surface, border, warning_text) = (theme.surface, theme.border, theme.warning_text);
        let title = rust_i18n::t!("inbox.title").into_owned();
        let count = waiting.len();
        let now = Instant::now();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(count * 2);
        for (ix, entry) in waiting.iter().enumerate() {
            if ix > 0 {
                rows.push(div().flex_none().h(px(1.)).mx(px(ROW_INSET)).bg(border).into_any_element());
            }
            rows.push(self.render_inbox_row(entry, now, window, cx));
        }
        let max_height = f32::from(window.viewport_size().height) * MAX_HEIGHT_RATIO;
        let header = div()
            .flex_none()
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(FontSize::Body)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(warning_text)
            .child(title.clone())
            .child(div().font_weight(FontWeight::MEDIUM).child(count.to_string()));
        let card = div()
            .id("inbox-rows")
            .flex_none()
            .max_h(px(max_height))
            .flex()
            .flex_col()
            .rounded(px(CARD_RADIUS))
            .bg(surface)
            .border_1()
            .border_color(border)
            .overflow_hidden()
            .overflow_y_scroll()
            .children(rows);
        Some(
            div()
                .id("inbox")
                .role(Role::List)
                .aria_label(title)
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(header)
                .child(card)
                .into_any_element(),
        )
    }

    /// 收件箱里的一行：标记、名字和等了多久，下面一行写哪家 agent 在哪个项目里等，按下跳到那个分屏。没在看的
    /// 分屏认出了提问时再往下画提问和回答的按钮（`agent_prompts::render_prompt`）。
    fn render_inbox_row(
        &self,
        entry: &AgentEntry,
        now: Instant,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane = entry.pane;
        let target = entry.window;
        let located = self.locate(pane);
        let watching = located.is_some_and(|(wi, ti)| self.is_watching_pane(pane, wi, ti, window, cx));
        let place = located.map_or_else(String::new, |(wi, _)| {
            let workspace = &self.workspaces[wi];
            project_name(&workspace.host, workspace.project_dir())
        });
        let name = pane_label(entry.view.read(cx)).0;
        let secs = now.saturating_duration_since(entry.changed_at).as_secs();
        // 头一分钟另有说法：行里放不下「等了」再接一句不到一分钟的话。
        let waited_text = match (watching, secs < 60) {
            (true, true) => rust_i18n::t!("inbox.watching_just"),
            (false, true) => rust_i18n::t!("inbox.waited_just"),
            (true, false) => rust_i18n::t!("inbox.watching", time = waited(secs)),
            (false, false) => rust_i18n::t!("inbox.waited", time = waited(secs)),
        }
        .into_owned();
        let detail = rust_i18n::t!("inbox.detail_blocked", agent = entry.kind_name(), place = place).into_owned();
        // 正看着的分屏里提问就在眼前，不再放一份。
        let asking = if watching {
            None
        } else {
            let view = entry.view.read(cx);
            view.session_id()
                .map(|id| (view.host().clone(), id))
                .and_then(|key| agent_prompts::prompt(&key, cx).map(|prompt| (key, prompt)))
        };
        let theme = cx.theme();
        let (foreground, subtle, hover_bg) = (theme.foreground, theme.subtle_foreground, theme.accent);
        let mark = agent_mark(Some(entry.mark), ("inbox-mark", pane), AGENT_MARK_SIZE_SMALL, !watching, cx);
        let reveal = move |_: &mut Self, _: &mut Window, cx: &mut Context<Self>| agents::reveal(target, pane, cx);
        let title_line = div()
            .h(scaled(ROW_LINE_HEIGHT))
            .flex()
            .items_center()
            .gap(px(MARK_GAP))
            .child(mark)
            .child(
                div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_color(foreground).child(name.clone()),
            )
            .child(
                div().flex_none().ml_auto().text_size(FontSize::Small).text_color(subtle).child(waited_text.clone()),
            );
        let detail_line = div()
            .pl(px(AGENT_MARK_SIZE_SMALL + MARK_GAP))
            .text_size(FontSize::Body)
            .text_color(subtle)
            .truncate()
            .child(detail.clone());
        let row = div()
            .id(("inbox", pane))
            .aria_label(name.clone())
            .aria_description([detail.as_str(), waited_text.as_str()].join(rust_i18n::t!("a11y.separator").as_ref()))
            .flex_none()
            .flex()
            .flex_col()
            .px(px(ROW_INSET))
            .py(px(8.));
        let Some((key, prompt)) = asking else {
            return row
                .role(Role::Button)
                .hover(move |row| row.bg(hover_bg))
                .child(title_line)
                .child(detail_line)
                .on_press(cx, reveal)
                .into_any_element();
        };
        // 行里另有回答的按钮，整行成了一组，跳到分屏的是标题那一行。
        let show = rust_i18n::t!("inbox.show_terminal").into_owned();
        let asked = agent_prompts::render_prompt(&key, &prompt, &name, EYE_ICON, show, reveal, cx);
        row.role(Role::Group)
            .child(
                title_line
                    .id(("inbox-open", pane))
                    .role(Role::Button)
                    .aria_label(name)
                    .cursor_pointer()
                    .on_press(cx, reveal),
            )
            .child(detail_line)
            .child(asked.mt(px(8.)))
            .into_any_element()
    }

    /// 有等回答的分屏（各从 `waits` 里的时刻起等）时，到最早有一行满下一个整分钟的时刻通知一次，每行的
    /// 「等了 N 分钟」才都跟得上；已经约好的不晚于这个时刻就留着，同一时刻只排一个。没有等回答的时取消约好的。
    fn schedule_inbox_redraw(&mut self, waits: impl IntoIterator<Item = Instant>, cx: &mut Context<Self>) {
        let now = Instant::now();
        let Some(due) = next_redraw(waits, now) else {
            self.inbox_redraw = None;
            return;
        };
        if self.inbox_redraw.as_ref().is_some_and(|(at, _)| *at <= due) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(due.saturating_duration_since(now)).await;
            this.update(cx, |this, cx| {
                this.inbox_redraw = None;
                cx.notify();
            })
            .ok();
        });
        self.inbox_redraw = Some((due, task));
    }
}

/// 本窗口有 agent、但没有一个在等回答：一个虚线框，对勾和一句话。
fn render_empty(cx: &mut Context<WindowView>) -> AnyElement {
    let theme = cx.theme();
    let (control_border, subtle, success) = (theme.control_border, theme.subtle_foreground, theme.success);
    let text = rust_i18n::t!("inbox.empty").into_owned();
    div()
        .id("inbox-empty")
        .aria_static_text(text.clone())
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(ROW_INSET))
        .py(px(10.))
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_dashed()
        .border_color(control_border)
        .text_color(subtle)
        .child(svg().path(CHECK_ICON).flex_none().size(px(14.)).text_color(success))
        .child(div().min_w_0().truncate().child(text))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_blocked_and_puts_the_longest_wait_first() {
        let now = Instant::now();
        let at = |secs| now - Duration::from_secs(secs);
        let entries = vec![
            ("working", Status::Working, at(500)),
            ("new blocked", Status::Blocked, at(10)),
            ("done", Status::Done, at(300)),
            ("old blocked", Status::Blocked, at(200)),
            ("idle", Status::Idle, at(1000)),
            ("mid blocked", Status::Blocked, at(90)),
        ];
        let waiting = waiting_oldest_first(entries, |(_, status, at)| (*status, *at));
        let names: Vec<_> = waiting.iter().map(|(name, ..)| *name).collect();
        assert_eq!(names, ["old blocked", "mid blocked", "new blocked"]);
    }

    #[test]
    fn redraws_when_the_wait_reaches_the_next_whole_minute() {
        let since = Instant::now();
        let after = |secs| since + Duration::from_secs(secs);
        assert_eq!(next_whole_minute(since, since), after(60));
        assert_eq!(next_whole_minute(since, after(59)), after(60));
        assert_eq!(next_whole_minute(since, after(60)), after(120));
        assert_eq!(next_whole_minute(since, after(3599)), after(3600));
    }

    #[test]
    fn redraws_at_the_earliest_whole_minute_of_any_row() {
        let start = Instant::now();
        let after = |secs| start + Duration::from_secs(secs);
        // 先等的那行下一个整分钟在 120 秒，晚半分钟开始等的那行在 90 秒。
        assert_eq!(next_redraw([after(0), after(30)], after(70)), Some(after(90)));
        assert_eq!(next_redraw([], after(70)), None);
    }
}
