//! agent 的状态标记和提醒：在 agent 自己的状态之外记下「干完了、用户还没看」（done），按
//! 等回答、done、工作中、空闲的优先级汇总到标签、标题栏和侧栏；用户没在看时发通知、出提示音；
//! 列出所有窗口里的 agent，以及跳到某个 agent 的分屏。各家 agent 的 logo 在 `logo`。

pub(super) mod alert;
pub(crate) mod logo;

use std::time::Instant;

use gpui::{App, Context, Entity, EntityId, SharedString, Window, WindowHandle};
use runode_shared_types::agent::{Agent, AgentKind, AgentState};

use super::{
    NextAgent, WindowView,
    model::{Tab, Workspace},
};
use crate::terminal_view::TerminalView;
use alert::{AgentAlert, Alert};

/// 标记上显示的状态，按优先级从高到低排列，汇总时取最靠前的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Status {
    /// 等用户回答。
    Blocked,
    /// 干完了，停在空闲，用户还没看过那个分屏。
    Done,
    Working,
    Idle,
}

impl Status {
    /// 要用户去处理：等回答的和干完了没看的。
    pub(super) fn needs_attention(self) -> bool {
        matches!(self, Self::Blocked | Self::Done)
    }

    /// 给人看的状态名，agent 列表里显示，也能拿来过滤。
    pub(super) fn label(self) -> SharedString {
        match self {
            Self::Blocked => rust_i18n::t!("agent.status.blocked"),
            Self::Done => rust_i18n::t!("agent.status.done"),
            Self::Working => rust_i18n::t!("agent.status.working"),
            Self::Idle => rust_i18n::t!("agent.status.idle"),
        }
        .into_owned()
        .into()
    }

    /// 状态的英文关键词，界面是别的语言时也能用它过滤。
    fn keyword(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Working => "working",
            Self::Idle => "idle",
        }
    }
}

/// 一个分屏或者一组分屏的 agent 标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Mark {
    pub(super) kind: AgentKind,
    pub(super) status: Status,
}

impl Mark {
    /// `done` 是这个分屏记着干完了没看；等回答优先于它，agent 已经不空闲了它也不算。
    pub(super) fn new(agent: Agent, done: bool) -> Self {
        let status = match agent.state {
            AgentState::Blocked => Status::Blocked,
            AgentState::Idle if done => Status::Done,
            AgentState::Idle => Status::Idle,
            AgentState::Working => Status::Working,
        };
        Self { kind: agent.kind, status }
    }
}

/// 汇总几个标记：取优先级最高的，同级的取先出现的，几种 agent 同时在时标记不会来回跳。
pub(super) fn summarize(marks: impl IntoIterator<Item = Mark>) -> Option<Mark> {
    marks.into_iter().fold(None, |best, mark| match best {
        Some(best) if best.status <= mark.status => Some(best),
        _ => Some(mark),
    })
}

/// 分屏的通知标识，点通知时凭它找回分屏。实体的编号只在一次运行里唯一，所以带上进程号：
/// 上次运行留在通知中心里的通知点了不会跳到恰好同号的另一个分屏。
pub(super) fn notification_tag(pane: EntityId) -> String {
    format!("{}{}", tag_prefix(), pane.as_u64())
}

fn tag_prefix() -> String {
    format!("agent-{}-", std::process::id())
}

/// 收回这些分屏发过的通知，分屏要关掉时用。
pub(super) fn dismiss_alerts(panes: impl IntoIterator<Item = EntityId>, cx: &mut App) {
    for pane in panes {
        alert::dismiss(&notification_tag(pane), cx);
    }
}

impl Tab {
    /// 这个分屏的标记；前台不是 agent 时为 `None`。
    pub(super) fn pane_mark(&self, pane: EntityId, cx: &App) -> Option<Mark> {
        let agent = self.panes.get(&pane)?.0.read(cx).agent()?;
        Some(Mark::new(agent, self.done.contains(&pane)))
    }

    /// 标签上的标记：汇总各个分屏，同级时先看当前分屏，和标签的标题对得上。
    pub(super) fn mark(&self, cx: &App) -> Option<Mark> {
        let others = self.root.leaves().into_iter().filter(|id| *id != self.focused);
        summarize(std::iter::once(self.focused).chain(others).filter_map(|id| self.pane_mark(id, cx)))
    }
}

impl Workspace {
    /// 侧栏上的标记：按标签的顺序汇总各个标签的标记。
    pub(super) fn mark(&self, cx: &App) -> Option<Mark> {
        summarize(self.tabs.iter().filter_map(|tab| tab.mark(cx)))
    }
}

/// agent 列表里的一行：哪个窗口的哪个分屏，以及显示和排序用的信息。
pub(super) struct AgentEntry {
    pub(super) window: WindowHandle<WindowView>,
    pub(super) pane: EntityId,
    pub(super) view: Entity<TerminalView>,
    pub(super) mark: Mark,
    /// 这个分屏的 agent 上次换状态的时刻，同一状态里新的排前面。
    pub(super) changed_at: Instant,
    pub(super) workspace_name: SharedString,
    pub(super) tab_title: SharedString,
}

impl AgentEntry {
    /// 过滤时比对的文字：agent 的名字和短名、状态、workspace 名和标签标题；目录由调用方另给。
    pub(super) fn haystack(&self) -> [SharedString; 6] {
        [
            self.kind_name(),
            self.mark.kind.label().into(),
            self.mark.status.label(),
            self.mark.status.keyword().into(),
            self.workspace_name.clone(),
            self.tab_title.clone(),
        ]
    }

    pub(super) fn kind_name(&self) -> SharedString {
        self.mark.kind.display_name().into()
    }
}

/// 按状态的优先级排，同一状态里最近变过的排前面；再相同的保持窗口、workspace 和标签的顺序。
pub(super) fn sort_entries<T>(entries: &mut [T], key: impl Fn(&T) -> (Status, Instant)) {
    entries.sort_by(|a, b| {
        let (a, b) = (key(a), key(b));
        a.0.cmp(&b.0).then(b.1.cmp(&a.1))
    });
}

/// `query` 按空白分成几个词，每个词（不分大小写）都出现在 `fields` 的某一项里才算匹配。
pub(super) fn matches_query<S: AsRef<str>>(fields: &[S], query: &str) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.as_ref().to_lowercase()).collect();
    query.split_whitespace().map(str::to_lowercase).all(|term| fields.iter().any(|field| field.contains(&term)))
}

/// 切到这个窗口里的这个分屏：激活应用和窗口，切到它所在的 workspace 和标签并聚焦它。推迟到
/// 当前的更新结束后再做，从这个窗口自己的回调里调用也不会重复借用。
pub(super) fn reveal(target: WindowHandle<WindowView>, pane: EntityId, cx: &mut App) {
    cx.defer(move |cx| {
        cx.activate(true);
        target
            .update(cx, |view, window, cx| {
                window.activate_window();
                view.show_pane(pane, window, cx);
            })
            .ok();
    });
}

fn pane_from_tag(tag: &str) -> Option<EntityId> {
    tag.strip_prefix(&tag_prefix())?.parse::<u64>().ok().map(EntityId::from)
}

/// 点了通知：按通知标识找到分屏所在的窗口，跳过去。分屏已经关掉了就什么都不做。
pub(crate) fn reveal_notified(tag: &str, cx: &mut App) {
    let Some(pane) = pane_from_tag(tag) else {
        return;
    };
    let target = cx.windows().into_iter().filter_map(|window| window.downcast::<WindowView>()).find(|window| {
        window
            .read(cx)
            .is_ok_and(|view| view.workspaces.iter().any(|w| w.tabs.iter().any(|t| t.panes.contains_key(&pane))))
    });
    if let Some(target) = target {
        reveal(target, pane, cx);
    }
}

impl WindowView {
    /// 用户正看着这个分屏：窗口在前台，它所在的标签正显示着，焦点在它上面。
    fn is_watching_pane(&self, pane: EntityId, wi: usize, ti: usize, window: &Window) -> bool {
        window.is_window_active() && self.is_shown(wi, ti) && self.workspaces[wi].tabs[ti].focused == pane
    }

    /// 前台 agent 从工作中停了下来。停在空闲、用户又没在看时记为 done 并提醒；否则（用户看着、
    /// 在等回答或者退出了）不算 done。
    pub(super) fn agent_finished(
        &mut self,
        pane: EntityId,
        wi: usize,
        ti: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let watching = self.is_watching_pane(pane, wi, ti, window);
        let tab = &mut self.workspaces[wi].tabs[ti];
        let Some(agent) = tab.panes[&pane].0.read(cx).agent() else {
            tab.done.remove(&pane);
            cx.notify();
            return;
        };
        if !watching && agent.state == AgentState::Idle {
            tab.done.insert(pane);
            self.alert(pane, wi, ti, agent.kind, Alert::Done, cx);
        } else {
            tab.done.remove(&pane);
        }
        cx.notify();
    }

    /// 前台 agent 停下来等用户回答；用户没在看时提醒。
    pub(super) fn agent_blocked(
        &mut self,
        pane: EntityId,
        wi: usize,
        ti: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let watching = self.is_watching_pane(pane, wi, ti, window);
        let tab = &mut self.workspaces[wi].tabs[ti];
        tab.done.remove(&pane);
        if !watching && let Some(agent) = tab.panes[&pane].0.read(cx).agent() {
            self.alert(pane, wi, ti, agent.kind, Alert::Blocked, cx);
        }
        cx.notify();
    }

    /// agent 不再停在空闲时（又开始工作或者退出了），之前记的 done 作废，「干完了」的通知
    /// 也一并收回。
    pub(super) fn forget_stale_done(&mut self, pane: EntityId, wi: usize, ti: usize, cx: &mut App) {
        let tab = &mut self.workspaces[wi].tabs[ti];
        if tab.done.contains(&pane) && tab.pane_mark(pane, cx).is_none_or(|mark| mark.status != Status::Done) {
            tab.done.remove(&pane);
            alert::dismiss(&notification_tag(pane), cx);
        }
    }

    fn alert(&self, pane: EntityId, wi: usize, ti: usize, kind: AgentKind, alert: Alert, cx: &mut Context<Self>) {
        let workspace = &self.workspaces[wi];
        let tab = workspace.tabs[ti].focused_view().read(cx).title().to_owned();
        let alert = AgentAlert { kind, alert, tag: notification_tag(pane), workspace: workspace.name.to_string(), tab };
        alert::alert(alert, cx);
    }

    /// 用户看到了当前分屏（窗口在前台）：清掉它的 done，收回它的通知。
    pub(super) fn mark_seen(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !window.is_window_active() {
            return;
        }
        let tab = self.tab_mut();
        let pane = tab.focused;
        if tab.done.remove(&pane) {
            cx.notify();
        }
        alert::dismiss(&notification_tag(pane), cx);
    }

    /// 这个窗口里所有前台是 agent 的分屏。
    fn push_entries(&self, handle: WindowHandle<WindowView>, out: &mut Vec<AgentEntry>, cx: &App) {
        for workspace in &self.workspaces {
            for tab in &workspace.tabs {
                let title: SharedString = tab.focused_view().read(cx).title().to_owned().into();
                for pane in tab.root.leaves() {
                    let Some(mark) = tab.pane_mark(pane, cx) else {
                        continue;
                    };
                    out.push(AgentEntry {
                        window: handle,
                        pane,
                        view: tab.panes[&pane].0.clone(),
                        mark,
                        changed_at: tab.panes[&pane].0.read(cx).agent_changed_at(),
                        workspace_name: workspace.name.clone(),
                        tab_title: title.clone(),
                    });
                }
            }
        }
    }

    /// 所有窗口里的 agent 分屏，按 `sort_entries` 排好。这个窗口正在更新、从窗口表里读不到，
    /// 直接用 `self`。
    pub(super) fn agent_entries(&self, window: &Window, cx: &App) -> Vec<AgentEntry> {
        let own = window.window_handle();
        let mut entries = Vec::new();
        for handle in cx.windows() {
            let Some(view) = handle.downcast::<WindowView>() else {
                continue;
            };
            if handle == own {
                self.push_entries(view, &mut entries, cx);
            } else if let Ok(other) = view.read(cx) {
                other.push_entries(view, &mut entries, cx);
            }
        }
        sort_entries(&mut entries, |entry| (entry.mark.status, entry.changed_at));
        entries
    }

    /// 切到这个窗口里的这个分屏所在的 workspace 和标签，并聚焦它；agent 列表开着时关掉。
    pub(super) fn show_pane(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate(pane) else {
            return;
        };
        self.agent_picker = None;
        let workspace = &mut self.workspaces[wi];
        let tab = &mut workspace.tabs[ti];
        if tab.focused != pane {
            tab.focused = pane;
            tab.zoomed = false;
        }
        workspace.active = ti;
        self.activate_workspace(wi, window, cx);
    }

    /// 跳到下一个要处理的 agent：先等回答的，再干完了没看的。当前分屏就在里面时跳到它后面那个，
    /// 连按几下挨个看过去。
    pub(super) fn next_agent(&mut self, _: &NextAgent, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<_> =
            self.agent_entries(window, cx).into_iter().filter(|entry| entry.mark.status.needs_attention()).collect();
        if entries.is_empty() {
            return;
        }
        let own = window.window_handle();
        let focused = self.tab().focused;
        let at = entries.iter().position(|entry| entry.window.window_id() == own.window_id() && entry.pane == focused);
        let next = &entries[at.map_or(0, |at| (at + 1) % entries.len())];
        reveal(next.window, next.pane, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn mark(kind: AgentKind, status: Status) -> Mark {
        Mark { kind, status }
    }

    #[test]
    fn blocked_wins_over_done_and_done_needs_idle() {
        let blocked = Agent { kind: AgentKind::Claude, state: AgentState::Blocked };
        let idle = Agent { kind: AgentKind::Claude, state: AgentState::Idle };
        let working = Agent { kind: AgentKind::Claude, state: AgentState::Working };
        assert_eq!(Mark::new(blocked, true).status, Status::Blocked);
        assert_eq!(Mark::new(idle, true).status, Status::Done);
        assert_eq!(Mark::new(idle, false).status, Status::Idle);
        assert_eq!(Mark::new(working, true).status, Status::Working);
    }

    #[test]
    fn summary_takes_the_most_urgent_and_the_first_among_equals() {
        let idle = mark(AgentKind::Pi, Status::Idle);
        let working = mark(AgentKind::Codex, Status::Working);
        let done = mark(AgentKind::Claude, Status::Done);
        let blocked = mark(AgentKind::Gemini, Status::Blocked);
        assert_eq!(summarize([idle, working, done]), Some(done));
        assert_eq!(summarize([done, blocked, working]), Some(blocked));
        assert_eq!(summarize([idle, working]), Some(working));
        let other_working = mark(AgentKind::Claude, Status::Working);
        assert_eq!(summarize([working, other_working]), Some(working));
        assert_eq!(summarize([]), None);
    }

    #[test]
    fn entries_sort_by_status_then_most_recent_change() {
        let now = Instant::now();
        let at = |secs| now - Duration::from_secs(secs);
        let mut entries = vec![
            ("idle", Status::Idle, at(1)),
            ("old done", Status::Done, at(50)),
            ("working", Status::Working, at(5)),
            ("new done", Status::Done, at(2)),
            ("blocked", Status::Blocked, at(100)),
        ];
        sort_entries(&mut entries, |(_, status, at)| (*status, *at));
        let names: Vec<_> = entries.iter().map(|(name, ..)| *name).collect();
        assert_eq!(names, ["blocked", "new done", "old done", "working", "idle"]);
    }

    #[test]
    fn every_term_must_match_some_field() {
        let fields = ["Claude Code", "claude", "runode", "~/dev/runode"];
        assert!(matches_query(&fields, ""));
        assert!(matches_query(&fields, "CLAUDE"));
        assert!(matches_query(&fields, "claude dev/run"));
        assert!(!matches_query(&fields, "claude codex"));
    }

    #[test]
    fn notification_tags_round_trip() {
        let pane = EntityId::from((1u64 << 32) | 42);
        assert_eq!(pane_from_tag(&notification_tag(pane)), Some(pane));
        // 别的进程发的通知不认。
        assert_eq!(pane_from_tag(&format!("agent-{}-42", std::process::id().wrapping_add(1))), None);
        assert_eq!(pane_from_tag("agent-42"), None);
        assert_eq!(pane_from_tag("agent-x"), None);
        assert_eq!(pane_from_tag("other-1"), None);
    }
}
