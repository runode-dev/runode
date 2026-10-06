use std::{cell::Cell, collections::HashSet, rc::Rc};

use runode_protocol::SessionId;
use runode_shared_types::agent::{Agent, AgentKind, AgentState};

use super::*;
use crate::session_host::Attached;

const SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
const BIG: GridSize = GridSize { cols: 120, rows: 40, cell_width_px: 8, cell_height_px: 16 };

/// 假的界面 VT：记下喂了什么、改了什么；`alive` 数着还没丢掉的有几份。
struct Fake {
    fed: Vec<u8>,
    resized: Vec<GridSize>,
    requested: Vec<GridSize>,
    themes: usize,
    meta: SessionMeta,
    alive: Rc<Cell<usize>>,
}

impl Fake {
    fn new(data: &[u8], alive: &Rc<Cell<usize>>) -> Self {
        alive.set(alive.get() + 1);
        Self {
            fed: data.to_vec(),
            resized: Vec::new(),
            requested: Vec::new(),
            themes: 0,
            meta: SessionMeta::default(),
            alive: alive.clone(),
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.alive.set(self.alive.get() - 1);
    }
}

impl Vt for Fake {
    fn feed(&mut self, data: &[u8]) {
        self.fed.extend_from_slice(data);
    }

    fn apply_resized(&mut self, size: GridSize) {
        self.resized.push(size);
    }

    fn apply_theme(&mut self, _: &TermSettings) {
        self.themes += 1;
    }

    fn apply_meta(&mut self, meta: SessionMeta) {
        self.meta = meta;
    }

    fn resize(&mut self, size: GridSize) {
        self.requested.push(size);
    }
}

/// 一个视图的状态机加上建假 VT 的办法。
struct Harness {
    state: ScreenState<Fake>,
    alive: Rc<Cell<usize>>,
    now: Instant,
}

impl Harness {
    /// 正看着，界面这份 VT 里是 `data`。
    fn live(data: &[u8]) -> Self {
        let alive = Rc::new(Cell::new(0));
        let state = ScreenState::new_live(Fake::new(data, &alive), 1, SessionMeta::default(), SIZE);
        Self { state, alive, now: Instant::now() }
    }

    fn apply(&mut self, events: Vec<LinkEvent>) -> Changes {
        let alive = self.alive.clone();
        let mut build = |screen: HostScreen| Ok(Fake::new(&screen.data, &alive));
        self.state.apply(events, &mut build, self.now)
    }

    fn later(&mut self, by: Duration) -> Instant {
        self.now += by;
        self.now
    }

    fn fed(&self) -> &[u8] {
        &self.state.live().expect("live").fed
    }
}

fn screen(mode: AttachMode, channel: u32, meta: SessionMeta, data: &[u8]) -> LinkEvent {
    LinkEvent::Screen(HostScreen {
        attached: Attached { id: SessionId(1), channel, size: SIZE, mode, meta, settings: None },
        data: data.to_vec(),
    })
}

fn msg(message: HostMsg) -> LinkEvent {
    LinkEvent::Msg(message)
}

fn output(data: &[u8]) -> LinkEvent {
    LinkEvent::Output(data.to_vec())
}

fn agent(state: AgentState) -> SessionMeta {
    SessionMeta { agent: Some(Agent { kind: AgentKind::Claude, state }), ..SessionMeta::default() }
}

fn meta(meta: SessionMeta) -> LinkEvent {
    msg(HostMsg::Meta { id: SessionId(1), meta })
}

fn name(screen: &Screen<Fake>) -> &'static str {
    match screen {
        Screen::Attaching { keep: Some(_), .. } => "Attaching(keep)",
        Screen::Attaching { keep: None, .. } => "Attaching",
        Screen::Live { .. } => "Live",
        Screen::Hidden => "Hidden",
        Screen::Lost { session: Some(_) } => "Lost(screen)",
        Screen::Lost { session: None } => "Lost",
    }
}

const SNAPSHOT: Option<Attach> = Some(Attach { size: None, mode: AttachMode::Snapshot });
const META_ONLY: Option<Attach> = Some(Attach { size: None, mode: AttachMode::MetaOnly });

#[test]
fn transitions() {
    // 正看着 → 收到 Resync → 冻结着重新要屏幕 → 新屏幕到了接着看。
    let mut h = Harness::live(b"a");
    let changes = h.apply(vec![msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() })]);
    assert_eq!(changes.attach, SNAPSHOT);
    assert_eq!(name(h.state.screen()), "Attaching(keep)");
    h.apply(vec![screen(AttachMode::Snapshot, 2, SessionMeta::default(), b"b")]);
    assert_eq!(name(h.state.screen()), "Live");
    assert_eq!(h.fed(), b"b");

    // 正看着 → 不显示满了宽限 → 只看状态 → 回到显示 → 等屏幕 → 看。
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    assert_eq!(h.state.tick(now), META_ONLY);
    assert_eq!(name(h.state.screen()), "Hidden");
    h.apply(vec![screen(AttachMode::MetaOnly, 3, SessionMeta::default(), b"")]);
    assert_eq!(name(h.state.screen()), "Hidden");
    assert_eq!(h.state.set_visible(true, h.now), Some(Attach { size: Some(SIZE), mode: AttachMode::Snapshot }));
    assert_eq!(name(h.state.screen()), "Attaching");
    h.apply(vec![screen(AttachMode::Snapshot, 4, SessionMeta::default(), b"c")]);
    assert_eq!(name(h.state.screen()), "Live");

    // 正看着 → 断开 → 冻结着最后一屏。
    assert!(h.apply(vec![LinkEvent::Lost]).lost);
    assert_eq!(name(h.state.screen()), "Lost(screen)");
    // 断开后又连上 → 冻结着要新屏幕 → 看。
    assert_eq!(h.state.reconnect(h.now), Some(Attach { size: Some(SIZE), mode: AttachMode::Snapshot }));
    assert_eq!(name(h.state.screen()), "Attaching(keep)");
    h.apply(vec![screen(AttachMode::Snapshot, 5, SessionMeta::default(), b"d")]);
    assert_eq!(h.fed(), b"d");

    // 只看状态时断开 → 没有屏幕可冻结；连上后接着只看状态。
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    h.apply(vec![LinkEvent::Lost]);
    assert_eq!(name(h.state.screen()), "Lost");
    assert_eq!(h.state.reconnect(h.now), META_ONLY);
    assert_eq!(name(h.state.screen()), "Hidden");
    // 在等屏幕时断开 → 没有屏幕。
    h.state.set_visible(true, h.now);
    h.apply(vec![LinkEvent::Lost]);
    assert_eq!(name(h.state.screen()), "Lost");
}

#[test]
fn frames_before_the_screen_are_dropped() {
    let mut h = Harness::live(b"");
    h.apply(vec![msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() })]);
    // 新屏幕之前漏过来的旧订阅的输出、改尺寸、换主题都不要；状态照收。
    let changes = h.apply(vec![
        output(b"stale"),
        msg(HostMsg::Resized { id: SessionId(1), size: BIG }),
        msg(HostMsg::ThemeApplied { id: SessionId(1), settings: TermSettings::default() }),
        meta(SessionMeta { title: Some("t".into()), ..SessionMeta::default() }),
    ]);
    assert!(!changes.fed && !changes.theme_applied);
    assert!(changes.title_changed);
    // 冻结着的那份也没被喂。
    let Screen::Attaching { keep: Some(keep), .. } = h.state.screen() else { panic!("attaching") };
    assert!(keep.fed.is_empty() && keep.resized.is_empty() && keep.themes == 0);
    // 屏幕和之后的输出在同一批里到：从屏幕接着喂。
    let changes = h.apply(vec![
        screen(AttachMode::Snapshot, 2, SessionMeta::default(), b"snap|"),
        output(b"new1|"),
        output(b"new2"),
        msg(HostMsg::Resized { id: SessionId(1), size: BIG }),
    ]);
    assert!(changes.fed && changes.replaced);
    assert_eq!(h.fed(), b"snap|new1|new2");
    assert_eq!(h.state.live().unwrap().resized, [BIG]);
    // 快照解出的 VT 不再另外套主题；之后宿主标出的换主题照常套。
    assert_eq!(h.state.live().unwrap().themes, 0);
    h.apply(vec![msg(HostMsg::ThemeApplied { id: SessionId(1), settings: TermSettings::default() })]);
    assert_eq!(h.state.live().unwrap().themes, 1);
}

#[test]
fn resync_keeps_the_old_screen_until_the_new_one_arrives() {
    let mut h = Harness::live(b"old");
    assert_eq!(h.alive.get(), 1);
    let changes = h.apply(vec![output(b"1"), msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() })]);
    assert_eq!(changes.attach, SNAPSHOT);
    // Resync 之前的输出喂进了旧的那份，冻结着接着画。
    assert_eq!(h.state.shown().unwrap().fed, b"old1");
    assert!(h.state.live().is_none(), "no input while resyncing");
    // 等的时候再来一次 Resync 不再重复要。
    let again = h.apply(vec![msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() })]);
    assert_eq!(again.attach, None);
    h.apply(vec![screen(AttachMode::Snapshot, 2, SessionMeta::default(), b"new")]);
    assert_eq!(h.fed(), b"new");
    assert_eq!(h.alive.get(), 1, "the frozen screen is gone");
}

#[test]
fn hiding_waits_for_the_grace_period_and_showing_cancels_it() {
    let mut h = Harness::live(b"");
    let start = h.now;
    assert_eq!(h.state.set_visible(false, start), None);
    assert_eq!(h.state.tick(start + HIDE_GRACE / 2), None);
    assert_eq!(name(h.state.screen()), "Live");
    // 宽限内切回来：不降级，也不用重新要屏幕。
    assert_eq!(h.state.set_visible(true, start + HIDE_GRACE / 2), None);
    assert_eq!(h.state.tick(start + HIDE_GRACE * 2), None);
    assert_eq!(name(h.state.screen()), "Live");
    // 再离开：从这次离开算宽限。
    let left = start + HIDE_GRACE * 2;
    h.state.set_visible(false, left);
    assert_eq!(h.state.tick(left + HIDE_GRACE - Duration::from_millis(1)), None);
    assert_eq!(h.state.tick(left + HIDE_GRACE), META_ONLY);
    assert_eq!(h.alive.get(), 0, "the view's VT is released");
    assert!(h.state.shown().is_none());
    // 已经降级了，不再重复。
    assert_eq!(h.state.tick(left + HIDE_GRACE * 3), None);
    // 只看状态时视图量出了新尺寸：回到显示时按它要屏幕。
    h.state.resize(BIG);
    assert_eq!(h.state.set_visible(true, left), Some(Attach { size: Some(BIG), mode: AttachMode::Snapshot }));
    // 只看状态那边的 Attached 先到（宿主按先后回）：连接那一层已经丢掉了，这里到的是新的。
    h.apply(vec![output(b"dropped"), screen(AttachMode::Snapshot, 3, SessionMeta::default(), b"x")]);
    assert_eq!(h.fed(), b"x");
    assert_eq!(h.alive.get(), 1);
}

#[test]
fn hiding_while_attaching_goes_back_to_meta_only() {
    let mut h = Harness::live(b"");
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    h.state.set_visible(true, now);
    // 屏幕还没到又切走了：宽限后改回只看状态。
    h.state.set_visible(false, now);
    let now = h.later(HIDE_GRACE);
    assert_eq!(h.state.tick(now), META_ONLY);
    h.apply(vec![screen(AttachMode::MetaOnly, 4, SessionMeta::default(), b"")]);
    assert_eq!(name(h.state.screen()), "Hidden");
}

#[test]
fn fifty_background_tabs_hold_no_view_vt() {
    let alive = Rc::new(Cell::new(0));
    let now = Instant::now();
    let mut states: Vec<ScreenState<Fake>> =
        (0..50).map(|i| ScreenState::new_live(Fake::new(b"", &alive), i, SessionMeta::default(), SIZE)).collect();
    assert_eq!(alive.get(), 50);
    for state in &mut states {
        state.set_visible(false, now);
    }
    let attaches: Vec<_> = states.iter_mut().filter_map(|state| state.tick(now + HIDE_GRACE)).collect();
    assert_eq!(attaches.len(), 50);
    assert!(attaches.iter().all(|attach| attach.mode == AttachMode::MetaOnly));
    assert_eq!(alive.get(), 0);
}

#[test]
fn the_first_meta_does_not_notify() {
    // 只看状态建的视图（存档恢复、后台会话）第一次拿到的状态：agent 正等着回答，不算刚停下来。
    let now = Instant::now();
    let mut state: ScreenState<Fake> = ScreenState::new_attaching(AttachMode::MetaOnly, SIZE, now);
    let alive = Rc::new(Cell::new(0));
    let mut build = |screen: HostScreen| Ok(Fake::new(&screen.data, &alive));
    let changes = state.apply(vec![screen(AttachMode::MetaOnly, 1, agent(AgentState::Blocked), b"")], &mut build, now);
    assert!(!changes.agent_finished && !changes.agent_blocked);
    assert!(changes.agent_changed && changes.title_changed);
    // 之后的变化照常通知，只看状态时也是。
    let changes =
        state.apply(vec![meta(agent(AgentState::Working)), meta(agent(AgentState::Blocked))], &mut build, now);
    assert!(changes.agent_finished && changes.agent_blocked);

    // 断开前在工作，重新连上时已经停了：断开期间的事不通知。
    let mut h = Harness::live(b"");
    h.apply(vec![meta(agent(AgentState::Working)), LinkEvent::Lost]);
    h.state.reconnect(h.now);
    let changes = h.apply(vec![screen(AttachMode::Snapshot, 2, agent(AgentState::Idle), b"")]);
    assert!(!changes.agent_finished && !changes.agent_blocked);
    assert_eq!(h.state.live().unwrap().meta, agent(AgentState::Idle), "the VT gets the meta too");
}

#[test]
fn a_stop_missed_while_attaching_again_still_notifies() {
    // 视图记着在工作，降成只看状态时 agent 停了：那条 `Meta` 夹在 `Attach` 和 `Attached` 之间被连接
    // 那一层丢掉了，`Attached` 带的状态和记着的比，照样通知。
    let mut h = Harness::live(b"");
    h.apply(vec![meta(agent(AgentState::Working))]);
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    let changes = h.apply(vec![screen(AttachMode::MetaOnly, 2, agent(AgentState::Blocked), b"")]);
    assert!(changes.agent_finished && changes.agent_blocked);
    // 回到显示时同样比；没在工作的不因为连上就通知。
    h.state.set_visible(true, now);
    let changes = h.apply(vec![screen(AttachMode::Snapshot, 3, agent(AgentState::Blocked), b"")]);
    assert!(!changes.agent_finished && !changes.agent_blocked);
    let changes = h.apply(vec![
        meta(agent(AgentState::Working)),
        msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() }),
    ]);
    assert!(!changes.agent_finished);
    let changes = h.apply(vec![screen(AttachMode::Snapshot, 4, agent(AgentState::Idle), b"")]);
    assert!(changes.agent_finished && !changes.agent_blocked);
}

#[test]
fn a_stale_screen_of_the_other_kind_is_ignored() {
    let mut h = Harness::live(b"");
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    h.apply(vec![screen(AttachMode::MetaOnly, 2, SessionMeta::default(), b"")]);
    h.state.set_visible(true, now);
    // 在要屏幕，到的却是只看状态的：是之前那次的，不能把视图拉回只看状态。
    let changes = h.apply(vec![screen(AttachMode::MetaOnly, 3, agent(AgentState::Idle), b"")]);
    assert!(!changes.replaced && !changes.agent_changed);
    assert_eq!(name(h.state.screen()), "Attaching");
    // 宿主给不了快照时退成 VT 重放，也算要到的屏幕。
    h.apply(vec![screen(AttachMode::VtReplay, 4, SessionMeta::default(), b"replay")]);
    assert_eq!(h.fed(), b"replay");
    // 反过来：在要只看状态，到的是屏幕。
    h.state.set_visible(false, now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    h.apply(vec![screen(AttachMode::Snapshot, 5, SessionMeta::default(), b"stale")]);
    assert_eq!(name(h.state.screen()), "Hidden");
    assert_eq!(h.alive.get(), 0);
}

#[test]
fn reopening_only_replaces_a_session_the_host_does_not_have() {
    let id = SessionId(1);
    let other: HashSet<SessionId> = [SessionId(2)].into();
    let with: HashSet<SessionId> = [id, SessionId(2)].into();
    assert_eq!(reopen_plan(id, Some(&with)), Reopen::Resume);
    assert_eq!(reopen_plan(id, Some(&other)), Reopen::Replace);
    // 列不出会话（宿主一时没回话）时不能当成没了：换上新会话会结束原来那个。
    assert_eq!(reopen_plan(id, None), Reopen::Resume);
}

#[test]
fn nothing_notifies_or_reaches_the_program_after_losing_the_host() {
    let mut h = Harness::live(b"last");
    h.apply(vec![meta(agent(AgentState::Working))]);
    assert!(h.state.live().is_some());
    h.apply(vec![LinkEvent::Lost]);
    // 输入只发给正看着的 VT：断开后没有。
    assert!(h.state.live_mut().is_none());
    assert_eq!(h.state.shown().unwrap().fed, b"last");
    // 之后漏过来的东西都不理。
    let changes = h.apply(vec![
        meta(agent(AgentState::Idle)),
        output(b"late"),
        msg(HostMsg::Bell { id: SessionId(1) }),
        msg(HostMsg::Exited { id: SessionId(1), status: None }),
        screen(AttachMode::Snapshot, 9, SessionMeta::default(), b"new"),
    ]);
    assert!(!changes.agent_finished && !changes.bell && !changes.exited && !changes.fed);
    assert_eq!(h.state.shown().unwrap().fed, b"last");
    assert!(h.state.is_lost());
}

#[test]
fn bells_ring_with_or_without_the_view_vt() {
    let mut h = Harness::live(b"");
    assert!(h.apply(vec![output(b"\x07"), msg(HostMsg::Bell { id: SessionId(1) })]).bell);
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    h.apply(vec![screen(AttachMode::MetaOnly, 2, SessionMeta::default(), b"")]);
    assert!(h.apply(vec![msg(HostMsg::Bell { id: SessionId(1) })]).bell);
    // 只有宿主的 Bell 算：没有它的输出里有 BEL 也不响（界面这份 VT 的响铃不看）。
    h.state.set_visible(true, h.now);
    h.apply(vec![screen(AttachMode::Snapshot, 3, SessionMeta::default(), b"\x07")]);
    assert!(!h.apply(vec![output(b"\x07")]).bell);
}

#[test]
fn a_failed_attach_means_the_session_is_gone() {
    let error = HostMsg::Error { req: None, id: Some(SessionId(1)), message: "no session".into() };
    // 平常连不成（会话被命令行结束了，它的 `Exited` 在连的时候被丢掉了）：按 shell 退出，关掉终端。
    let mut h = Harness::live(b"");
    h.apply(vec![msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() })]);
    let changes = h.apply(vec![msg(error.clone())]);
    assert!(changes.exited && !changes.lost);
    // 只看状态时也是。
    let mut h = Harness::live(b"");
    h.state.set_visible(false, h.now);
    let now = h.later(HIDE_GRACE);
    h.state.tick(now);
    assert!(h.apply(vec![msg(error.clone())]).exited);
    // 断开后重新连上时连不成：回到断开，用户可以再点重开。
    let mut h = Harness::live(b"last");
    h.apply(vec![LinkEvent::Lost]);
    h.state.reconnect(h.now);
    let changes = h.apply(vec![msg(error.clone())]);
    assert!(changes.lost && !changes.exited);
    assert_eq!(name(h.state.screen()), "Lost(screen)");
    // 不在连的时候的错误只记日志。
    let mut h = Harness::live(b"");
    let changes = h.apply(vec![msg(error)]);
    assert!(!changes.lost && !changes.exited);
    assert_eq!(name(h.state.screen()), "Live");
}

#[test]
fn an_unbuildable_screen_counts_as_lost() {
    let mut h = Harness::live(b"old");
    h.apply(vec![msg(HostMsg::Resync { id: SessionId(1), reason: "slow".into() })]);
    let mut build = |_: HostScreen| -> anyhow::Result<Fake> { Err(anyhow::anyhow!("bad snapshot")) };
    let changes = h.state.apply(vec![screen(AttachMode::Snapshot, 2, SessionMeta::default(), b"")], &mut build, h.now);
    assert!(changes.lost);
    assert_eq!(h.state.shown().unwrap().fed, b"old");
}

#[test]
fn a_view_attached_while_hidden_learns_its_size_from_the_host() {
    let now = Instant::now();
    let mut state: ScreenState<Fake> = ScreenState::new_attaching(AttachMode::MetaOnly, SIZE, now);
    assert!(!state.visible());
    let alive = Rc::new(Cell::new(0));
    let mut build = |screen: HostScreen| Ok(Fake::new(&screen.data, &alive));
    let attached = HostScreen {
        attached: Attached {
            id: SessionId(1),
            channel: 1,
            size: BIG,
            mode: AttachMode::MetaOnly,
            meta: SessionMeta::default(),
            settings: None,
        },
        data: Vec::new(),
    };
    state.apply(vec![LinkEvent::Screen(attached)], &mut build, now);
    assert_eq!(state.last_size(), BIG);
    assert_eq!(state.set_visible(true, now), Some(Attach { size: Some(BIG), mode: AttachMode::Snapshot }));
    // 新屏幕到了以后视图量出的尺寸照常请宿主改。
    state.apply(vec![screen(AttachMode::Snapshot, 2, SessionMeta::default(), b"")], &mut build, now);
    state.resize(SIZE);
    assert_eq!(state.live().unwrap().requested, [SIZE]);
}

#[test]
fn an_unopened_view_keeps_its_own_meta_until_the_host_answers() {
    let alive = Rc::new(Cell::new(0));
    let now = Instant::now();
    let own =
        SessionMeta { fallback_title: Some("proj".into()), cwd: Some("/tmp/proj".into()), ..SessionMeta::default() };
    let mut h = Harness { state: ScreenState::new_unopened(own.clone(), SIZE, now), alive, now };
    assert_eq!(name(h.state.screen()), "Hidden");
    assert!(!h.state.visible());
    assert_eq!(h.state.meta(), &own);
    // 看不见放多久都没有可丢的。
    assert_eq!(h.state.tick(h.now + HIDE_GRACE * 10), None);
    // 回到显示时要一份按视图尺寸的屏幕；等的时候还是自己给的状态，标签上的名字不会闪成默认的。
    assert_eq!(h.state.set_visible(true, h.now), Some(Attach { size: Some(SIZE), mode: AttachMode::Snapshot }));
    assert_eq!(name(h.state.screen()), "Attaching");
    assert_eq!(h.state.meta(), &own);
    // 宿主给的状态是第一次给的，不通知。
    let host =
        SessionMeta { fallback_title: Some("proj".into()), cwd: Some("/tmp/proj".into()), ..agent(AgentState::Idle) };
    let changes = h.apply(vec![screen(AttachMode::Snapshot, 3, host.clone(), b"$ ")]);
    assert_eq!(name(h.state.screen()), "Live");
    assert_eq!(h.state.meta(), &host);
    assert!(!changes.agent_finished && !changes.agent_blocked);
    assert_eq!(h.fed(), b"$ ");
    assert_eq!(h.alive.get(), 1);
}

#[test]
fn an_unopened_view_started_while_hidden_only_watches_the_state() {
    let alive = Rc::new(Cell::new(0));
    let now = Instant::now();
    let mut h = Harness { state: ScreenState::new_unopened(SessionMeta::default(), SIZE, now), alive, now };
    h.state.resize(BIG);
    // 看不见时开的会话只看状态：宿主给的是只看状态的那种，不建界面这份 VT。
    let host = SessionMeta { fallback_title: Some("x".into()), ..SessionMeta::default() };
    let changes = h.apply(vec![screen(AttachMode::MetaOnly, 1, host.clone(), b"")]);
    assert!(changes.title_changed);
    assert_eq!(name(h.state.screen()), "Hidden");
    assert_eq!(h.state.meta(), &host);
    assert_eq!(h.alive.get(), 0);
    // 量过的尺寸不跟着宿主给的走；回到显示时按它要屏幕。
    assert_eq!(h.state.last_size(), BIG);
    assert_eq!(h.state.set_visible(true, h.now), Some(Attach { size: Some(BIG), mode: AttachMode::Snapshot }));
}
