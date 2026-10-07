//! 视图屏幕的状态机：界面这份 VT 现在是什么样（在等宿主给屏幕、正看着、只看状态、和宿主断开了），
//! 宿主经连接发来的事件按状态怎么处理，什么时候要重新连上会话。不碰 GPUI，也不直接发消息：要
//! 视图经连接做的事放在返回值里（`Attach`），视图照着做。
//!
//! 两份 VT 一样的前提：界面这份从宿主给的屏幕开始，之后只喂那份屏幕之后的输出和标记。所以发出
//! `Attach` 到收到它的屏幕之间，输出、改尺寸和换主题一律丢掉（连接那一层已经丢掉了 `Attached`
//! 之前的，这里再挡住 `Attached` 和快照拼完之间漏过来的）；快照解出来的 VT 已经套着宿主的主题，
//! 之后不再另外 `apply_theme`。
//!
//! 后台标签不留界面这份 VT：离开显示 `HIDE_GRACE` 后改成只看状态（`AttachMode::MetaOnly`），回到
//! 显示时重新要一份屏幕。只看状态时标题、agent、响铃、命令历史照常从宿主来。
//!
//! 通知：第一次连上（建视图、断开后重新连上）时宿主给的状态（`Attached` 里带的，包括只看状态时
//! 第一次给的）是「现在的样子」，不是刚发生的变化，不发 agent 停下来的通知；和宿主断开以后也不发。
//! 之后在看屏幕和只看状态之间切换、`Resync` 时重新连上，`Attached` 带的状态和视图记着的比：发出
//! `Attach` 到收到 `Attached` 之间的 `Meta` 被连接那一层丢掉了，agent 恰好在这时停下来的话靠它通知。
//!
//! 尺寸归属：几个前端看同一个会话时，宿主按最近交互的那个前端（owner）的视图改会话的尺寸，用
//! `HostMsg::SizeOwner` 告诉各个带尺寸连着的前端是不是自己。这里记下别的前端是 owner 时它的设备名，
//! 视图据此把对不上的 VT 裁切或留白着画（`Crop`）。视图照旧把自己量出的尺寸报给宿主（宿主记着，轮到
//! 这边当 owner 时用），但只在尺寸真的变了时报：收到 owner 的 `Resized` 不会让视图再发一遍自己的，
//! 两个前端不会来回抢。只有一个前端（没收到过 `SizeOwner`，或者说是自己）时一切照旧。

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use runode_protocol::{AttachMode, FinishedCommand, HostMsg, SessionId};
use runode_shared_types::{agent::Agent, grid::GridSize, session::SessionMeta, settings::TermSettings};

use crate::host_client::{LinkEvent, Screen as HostScreen};

/// 离开显示这么久后丢掉界面这份 VT、只看状态；这期间切回来就什么都不用做。
pub(super) const HIDE_GRACE: Duration = Duration::from_secs(5);
/// 回到显示时主线程最多等这么久拿到宿主给的屏幕；等不到先画背景，到了再补上。
pub(crate) const SHOW_WAIT: Duration = Duration::from_millis(30);

/// 界面这份 VT 要状态机做的事。`Session` 实现它；测试里用假的。
pub(super) trait Vt {
    /// 喂宿主转来的输出。
    fn feed(&mut self, data: &[u8]);
    /// 在宿主改尺寸的位置改 VT 的尺寸。
    fn apply_resized(&mut self, size: GridSize);
    /// 在宿主换主题的位置换主题。
    fn apply_theme(&mut self, settings: &TermSettings);
    /// 写入宿主公布的状态（补全要用里面的目录、shell 的名字）。
    fn apply_meta(&mut self, meta: SessionMeta);
    /// 视图的尺寸变了，请宿主改。
    fn resize(&mut self, size: GridSize);
}

impl Vt for runode_terminal::session::Session {
    fn feed(&mut self, data: &[u8]) {
        Self::feed(self, data);
    }

    fn apply_resized(&mut self, size: GridSize) {
        Self::apply_resized(self, size);
    }

    fn apply_theme(&mut self, settings: &TermSettings) {
        Self::apply_theme(self, settings);
    }

    fn apply_meta(&mut self, meta: SessionMeta) {
        Self::apply_meta(self, meta);
    }

    fn resize(&mut self, size: GridSize) {
        Self::resize(self, size);
    }
}

/// 界面这份 VT 现在的样子。
pub(super) enum Screen<S> {
    /// 发出了要屏幕的 `Attach`，还没等到。`keep` 是重新连上（`Resync`、断开后重连）前那份，冻结
    /// 着接着画，新的到了再换；从只看状态回到显示时没有，先画背景。
    Attaching { since: Instant, keep: Option<S> },
    /// 正看着：输出喂给 `session`。`channel` 是宿主给这次订阅的通道。
    Live { session: S, channel: u32 },
    /// 只看状态，没有界面这份 VT。
    Hidden,
    /// 和宿主断开了。显示着的冻结在最后一屏（`session`），只看状态的没有屏幕，画占位。
    Lost { session: Option<S> },
}

/// 别的前端的视图在决定这个会话的尺寸，见 `HostMsg::SizeOwner`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SizeOwner {
    /// 那个前端在 `Hello` 里报的设备名；没报时为空。
    pub(super) device: Option<String>,
}

/// 要视图经连接重新连上会话：`Link::reattach(id, size, mode)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Attach {
    pub(super) size: Option<GridSize>,
    pub(super) mode: AttachMode,
}

/// 处理完一批事件后视图要跟着做的事。
#[derive(Debug, Default)]
pub(super) struct Changes {
    /// 喂了输出：屏幕上的输入可能变了，光标要亮起来。
    pub(super) fed: bool,
    /// 换了界面这份 VT（新的屏幕到了，或者丢掉了）：选区、搜索、补全都跟着旧的 VT 没了。
    pub(super) replaced: bool,
    /// 标题（含 `fallback_title`）或 agent 变了。
    pub(super) title_changed: bool,
    /// 前台 agent 换了种类或状态。
    pub(super) agent_changed: bool,
    /// 前台 agent 从工作中停了下来，要通知外层 `AgentFinished`。
    pub(super) agent_finished: bool,
    /// 前台 agent 刚停下来等用户回答，要通知外层 `AgentBlocked`。
    pub(super) agent_blocked: bool,
    /// 换了主题：输入的高亮取自调色板，要重算。
    pub(super) theme_applied: bool,
    /// shell 跑完的命令，按先后。
    pub(super) commands: Vec<FinishedCommand>,
    /// 程序响过铃。
    pub(super) bell: bool,
    /// shell 退出了。
    pub(super) exited: bool,
    /// 和宿主断开了（这一批里刚断开）。
    pub(super) lost: bool,
    /// 要重新连上会话（收到 `Resync`）。
    pub(super) attach: Option<Attach>,
}

/// 一个视图的屏幕状态和宿主公布的会话状态。
pub(super) struct ScreenState<S> {
    screen: Screen<S>,
    /// 宿主最近一次公布的会话状态，不管有没有界面这份 VT 都在。
    meta: SessionMeta,
    /// 视图最近一次量出的尺寸；还没量过时是宿主给的会话尺寸。
    last_size: GridSize,
    /// 量过视图的尺寸，`last_size` 不再跟着宿主给的走。
    sized: bool,
    /// shell 已经退出。
    exited: bool,
    /// 发出去、还没等到屏幕的 `Attach` 要的模式。
    attaching: Option<AttachMode>,
    /// 视图在窗口里显示着。
    visible: bool,
    /// 从什么时候起不显示，见 `tick`。
    hidden_since: Option<Instant>,
    /// `meta` 是宿主给过的（不是建视图时的空白），`Attached` 带的状态可以和它比，见 `screen_arrived`。
    meta_known: bool,
    /// 正在断开后重新连上：这时连不成多半是宿主还没准备好，按断开处理，用户可以再点重开；平常连不成
    /// 是会话已经没了，按 shell 退出处理。
    reconnecting: bool,
    /// 别的前端在决定尺寸时是它；是这边、或者没收到过 `SizeOwner`（只有这一个前端）时为空。只看状态和
    /// 断开时不带尺寸，宿主不再告诉这边，清掉。
    size_owner: Option<SizeOwner>,
}

/// 和宿主断开后点了「在原目录重开」、重新连上宿主以后怎么办。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reopen {
    /// 接着用原来的会话（重新连上它）。宿主列不出会话时也这样：会话真没了宿主会回错，视图回到断开，
    /// 用户可以再点。
    Resume,
    /// 宿主确实没有这个会话了：在原来的目录开一个新的换上。
    Replace,
}

/// 重新连上宿主以后，按宿主列出的还活着的会话（列不出时为 `None`）决定会话 `id` 怎么办。列不出时
/// 不能当成会话都没了：换上新会话会结束原来那个，宿主只是一时没回话的话，用户还活着的会话（可能
/// 有 agent 在跑）就被结束掉了。
pub(super) fn reopen_plan(id: SessionId, alive: Option<&HashSet<SessionId>>) -> Reopen {
    match alive {
        Some(alive) if !alive.contains(&id) => Reopen::Replace,
        _ => Reopen::Resume,
    }
}

impl<S: Vt> ScreenState<S> {
    /// 各构造函数共用的起点：显示着，没在连、没量过尺寸，`meta` 不是宿主给的。
    fn base(screen: Screen<S>, meta: SessionMeta, size: GridSize) -> Self {
        Self {
            screen,
            meta,
            last_size: size,
            sized: false,
            exited: false,
            attaching: None,
            visible: true,
            hidden_since: None,
            meta_known: false,
            reconnecting: false,
            size_owner: None,
        }
    }

    /// 已经拿到宿主给的屏幕、建好了界面这份 VT：正看着。
    pub(super) fn new_live(mut session: S, channel: u32, meta: SessionMeta, size: GridSize) -> Self {
        session.apply_meta(meta.clone());
        Self { meta_known: true, ..Self::base(Screen::Live { session, channel }, meta, size) }
    }

    /// 发出了 `mode` 的 `Attach`、还没等到屏幕：要屏幕时是 `Attaching`（视图显示着），只看状态时是
    /// `Hidden`（视图不显示）。
    pub(super) fn new_attaching(mode: AttachMode, size: GridSize, now: Instant) -> Self {
        let meta_only = mode == AttachMode::MetaOnly;
        let screen = if meta_only { Screen::Hidden } else { Screen::Attaching { since: now, keep: None } };
        Self {
            attaching: Some(mode),
            visible: !meta_only,
            hidden_since: meta_only.then_some(now),
            ..Self::base(screen, SessionMeta::default(), size)
        }
    }

    /// 宿主里还没有会话（恢复布局时看不见、又从没启动过的终端，见 `TerminalView::deferred`）：不显示，
    /// 没有界面这份 VT，状态是视图自己按起始目录给的 `meta`。开了会话以后照常连：回到显示时
    /// `set_visible` 给出要屏幕的 `Attach`；没显示就要启动时只看状态。宿主给了状态再换掉 `meta`。
    pub(super) fn new_unopened(meta: SessionMeta, size: GridSize, now: Instant) -> Self {
        Self { visible: false, hidden_since: Some(now), ..Self::base(Screen::Hidden, meta, size) }
    }

    #[cfg(test)]
    pub(super) fn screen(&self) -> &Screen<S> {
        &self.screen
    }

    pub(super) fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    pub(super) fn last_size(&self) -> GridSize {
        self.last_size
    }

    pub(super) fn visible(&self) -> bool {
        self.visible
    }

    /// 在等宿主给屏幕（显示着、还没画出来）。
    pub(super) fn is_attaching(&self) -> bool {
        matches!(self.screen, Screen::Attaching { .. })
    }

    /// 正看着时宿主给这次订阅的通道。
    pub(super) fn channel(&self) -> Option<u32> {
        match self.screen {
            Screen::Live { channel, .. } => Some(channel),
            _ => None,
        }
    }

    pub(super) fn is_lost(&self) -> bool {
        matches!(self.screen, Screen::Lost { .. })
    }

    /// 别的前端的视图在决定这个会话的尺寸时是它，见 `SizeOwner`。
    pub(super) fn size_owner(&self) -> Option<&SizeOwner> {
        self.size_owner.as_ref()
    }

    /// 正看着时的界面这份 VT。发给程序的输入、要宿主做的事只在这时才有：重新连上的过程中和
    /// 断开以后没有，按键、粘贴之类一律丢掉。
    pub(super) fn live(&self) -> Option<&S> {
        match &self.screen {
            Screen::Live { session, .. } => Some(session),
            _ => None,
        }
    }

    pub(super) fn live_mut(&mut self) -> Option<&mut S> {
        match &mut self.screen {
            Screen::Live { session, .. } => Some(session),
            _ => None,
        }
    }

    /// 画在屏幕上的那份 VT：正看着的，或者重新连上、断开时冻结着的。选区、滚动回滚历史、搜索
    /// 这些只在界面里的操作用它，不发东西给程序。
    pub(super) fn shown(&self) -> Option<&S> {
        match &self.screen {
            Screen::Live { session, .. } => Some(session),
            Screen::Attaching { keep, .. } => keep.as_ref(),
            Screen::Lost { session } => session.as_ref(),
            Screen::Hidden => None,
        }
    }

    pub(super) fn shown_mut(&mut self) -> Option<&mut S> {
        match &mut self.screen {
            Screen::Live { session, .. } => Some(session),
            Screen::Attaching { keep, .. } => keep.as_mut(),
            Screen::Lost { session } => session.as_mut(),
            Screen::Hidden => None,
        }
    }

    /// 视图量出了尺寸：记下来，正看着时请宿主改。没在看时等回到显示时随 `Attach` 一起给宿主。每次
    /// 布局都会调，和上次量的一样时什么都不做：宿主按别的前端的尺寸改了 VT（`Resized`）以后，这边
    /// 不能因为两边对不上就再请一遍。
    pub(super) fn resize(&mut self, size: GridSize) {
        if self.sized && size == self.last_size {
            return;
        }
        self.last_size = size;
        self.sized = true;
        if let Some(session) = self.live_mut() {
            session.resize(size);
        }
    }

    /// 视图开始或不再显示。回到显示时要是已经丢了界面这份 VT，返回要发的 `Attach`（按视图的尺寸
    /// 要一份屏幕）；不显示时只记下时刻，`HIDE_GRACE` 后由 `tick` 降级。
    pub(super) fn set_visible(&mut self, visible: bool, now: Instant) -> Option<Attach> {
        if visible == self.visible {
            return None;
        }
        self.visible = visible;
        if !visible {
            self.hidden_since = Some(now);
            return None;
        }
        self.hidden_since = None;
        // shell 已经退出的会话不再要屏幕，视图马上就关。
        if self.exited || !matches!(self.screen, Screen::Hidden) {
            return None;
        }
        self.screen = Screen::Attaching { since: now, keep: None };
        self.attaching = Some(AttachMode::Snapshot);
        Some(Attach { size: Some(self.last_size), mode: AttachMode::Snapshot })
    }

    /// 不显示满了 `HIDE_GRACE` 时丢掉界面这份 VT、改成只看状态，返回要发的 `Attach`。
    pub(super) fn tick(&mut self, now: Instant) -> Option<Attach> {
        let due = self.hidden_since.is_some_and(|since| now.duration_since(since) >= HIDE_GRACE);
        if self.visible || !due || !matches!(self.screen, Screen::Live { .. } | Screen::Attaching { .. }) {
            return None;
        }
        self.screen = Screen::Hidden;
        self.attaching = Some(AttachMode::MetaOnly);
        Some(Attach { size: None, mode: AttachMode::MetaOnly })
    }

    /// 和宿主断开后又连上了、会话还在：重新连上它。显示着的要一份屏幕，断开前的那一屏冻结着画到
    /// 新的到了为止；不显示的只看状态。返回要发的 `Attach`（视图要用 `Link::attach` 重新登记，断开时
    /// 原来的登记已经没了）。没断开时返回 `None`。
    pub(super) fn reconnect(&mut self, now: Instant) -> Option<Attach> {
        let Screen::Lost { session } = &mut self.screen else {
            return None;
        };
        let keep = session.take();
        self.reconnecting = true;
        // 断开期间的变化不通知，重新连上时宿主给的状态当作第一次给的。
        self.meta_known = false;
        if self.visible {
            self.screen = Screen::Attaching { since: now, keep };
            self.attaching = Some(AttachMode::Snapshot);
            Some(Attach { size: Some(self.last_size), mode: AttachMode::Snapshot })
        } else {
            self.screen = Screen::Hidden;
            self.attaching = Some(AttachMode::MetaOnly);
            Some(Attach { size: None, mode: AttachMode::MetaOnly })
        }
    }

    /// 按先后处理宿主发来的一批事件。新的屏幕（快照或 VT 重放）用 `build` 建成界面这份 VT。连着的
    /// 几块输出合成一次喂进去。
    pub(super) fn apply(
        &mut self,
        events: impl IntoIterator<Item = LinkEvent>,
        build: &mut dyn FnMut(HostScreen) -> anyhow::Result<S>,
        now: Instant,
    ) -> Changes {
        let mut changes = Changes::default();
        let mut pending: Vec<Vec<u8>> = Vec::new();
        for event in events {
            if let LinkEvent::Output(data) = event {
                // 没在看（在等新的屏幕、只看状态、断开了）时的输出接不上界面这份 VT。
                if matches!(self.screen, Screen::Live { .. }) {
                    pending.push(data);
                }
                continue;
            }
            changes.fed |= self.feed(&mut pending);
            match event {
                LinkEvent::Output(_) => unreachable!("handled above"),
                LinkEvent::Screen(screen) => self.screen_arrived(screen, build, &mut changes),
                LinkEvent::Lost => self.lose(&mut changes),
                LinkEvent::Msg(message) => self.message(message, now, &mut changes),
            }
        }
        changes.fed |= self.feed(&mut pending);
        changes
    }

    /// 把攒着的几块输出一次喂给 VT，返回是否喂了。
    fn feed(&mut self, pending: &mut Vec<Vec<u8>>) -> bool {
        let Some(session) = self.live_mut() else {
            pending.clear();
            return false;
        };
        match pending.len() {
            0 => return false,
            1 => session.feed(&pending[0]),
            _ => session.feed(&pending.concat()),
        }
        pending.clear();
        true
    }

    /// 宿主给了屏幕：只看状态的只记下状态，要屏幕的建好界面这份 VT 开始看。建不出来时按断开处理，
    /// 冻结的那份接着画。
    fn screen_arrived(
        &mut self,
        screen: HostScreen,
        build: &mut dyn FnMut(HostScreen) -> anyhow::Result<S>,
        changes: &mut Changes,
    ) {
        if self.is_lost() {
            return;
        }
        let attached = &screen.attached;
        let (mode, channel, size) = (attached.mode, attached.channel, attached.size);
        // 只认最近一次 `Attach` 要的那种：要屏幕时到了只看状态的（或者反过来）是之前那次的，不管它。
        // 连接那一层已经只交最近一次的，这里再挡一道。
        if let Some(wanted) = self.attaching
            && (wanted == AttachMode::MetaOnly) != (mode == AttachMode::MetaOnly)
        {
            tracing::debug!("ignored a {mode:?} screen while waiting for {wanted:?}");
            return;
        }
        if !self.sized {
            self.last_size = size;
        }
        self.attaching = None;
        self.reconnecting = false;
        // 第一次连上时的状态是现在的样子，不是刚发生的变化，不通知；之后重新连上时和记着的比，agent
        // 从工作中停了下来照样通知（这个变化的 `Meta` 被连接那一层丢掉了）。
        let notify = self.meta_known && self.meta.agent.is_some_and(Agent::is_working);
        self.set_meta(attached.meta.clone(), notify, changes);
        self.meta_known = true;
        changes.replaced = true;
        if mode == AttachMode::MetaOnly {
            self.screen = Screen::Hidden;
            self.size_owner = None;
            return;
        }
        let keep = match std::mem::replace(&mut self.screen, Screen::Hidden) {
            Screen::Attaching { since, keep } => {
                tracing::debug!("the screen arrived {:?} after asking", since.elapsed());
                keep
            }
            Screen::Lost { session: keep } => keep,
            Screen::Live { session, .. } => Some(session),
            Screen::Hidden => None,
        };
        match build(screen) {
            Ok(mut session) => {
                session.apply_meta(self.meta.clone());
                // 回到显示时按视图的尺寸要的屏幕，宿主已经改好；没改成的（比如重新连上时没给尺寸、
                // 等的时候视图又变了）这里补上。
                if self.sized && size != self.last_size {
                    session.resize(self.last_size);
                }
                self.screen = Screen::Live { session, channel };
            }
            Err(err) => {
                tracing::warn!("failed to build the screen the host sent: {err:#}");
                self.screen = Screen::Lost { session: keep };
                changes.lost = true;
            }
        }
    }

    fn lose(&mut self, changes: &mut Changes) {
        let session = match std::mem::replace(&mut self.screen, Screen::Hidden) {
            Screen::Live { session, .. } => Some(session),
            Screen::Attaching { keep, .. } => keep,
            Screen::Hidden => None,
            Screen::Lost { session } => {
                self.screen = Screen::Lost { session };
                return;
            }
        };
        self.screen = Screen::Lost { session };
        self.attaching = None;
        self.reconnecting = false;
        self.size_owner = None;
        changes.lost = true;
    }

    fn message(&mut self, message: HostMsg, now: Instant, changes: &mut Changes) {
        if self.is_lost() {
            return;
        }
        match message {
            HostMsg::Meta { meta, .. } => self.set_meta(meta, true, changes),
            HostMsg::Resized { size, .. } => {
                if let Some(session) = self.live_mut() {
                    session.apply_resized(size);
                }
            }
            // 套标记里带的那份：宿主在这里套的就是它，全局配置可能已经又变了，主题也可能是
            // 别的前端换的。
            HostMsg::ThemeApplied { settings, .. } => {
                if let Some(session) = self.live_mut() {
                    session.apply_theme(&settings);
                    changes.theme_applied = true;
                }
            }
            // 只认带着尺寸看着时的：只看状态时宿主不该发，发了也用不上。
            HostMsg::SizeOwner { mine, owner, .. } => {
                let follows = !mine && !matches!(self.screen, Screen::Hidden);
                self.size_owner = follows.then_some(SizeOwner { device: owner });
            }
            HostMsg::CommandFinished { command, .. } => changes.commands.push(command),
            HostMsg::Bell { .. } => changes.bell = true,
            HostMsg::Exited { .. } => {
                self.exited = true;
                changes.exited = true;
            }
            // 宿主没法再保证两份 VT 一样（这边读得太慢）：冻结着现在这份，重新要一份屏幕。
            HostMsg::Resync { reason, .. } => {
                tracing::info!("resyncing: {reason}");
                match std::mem::replace(&mut self.screen, Screen::Hidden) {
                    Screen::Live { session, .. } => {
                        self.screen = Screen::Attaching { since: now, keep: Some(session) };
                        self.attaching = Some(AttachMode::Snapshot);
                        changes.attach = Some(Attach { size: None, mode: AttachMode::Snapshot });
                    }
                    other => self.screen = other,
                }
            }
            // 正连着时的错误是这次没连成。断开后重新连上时按断开处理，用户可以再点重开；平常是会话
            // 已经没了（比如被命令行结束了，它的 `Exited` 在连的时候被连接那一层丢掉了），按 shell
            // 退出处理，关掉这个终端。
            HostMsg::Error { message, .. } if self.attaching.is_some() => {
                tracing::warn!("failed to attach: {message}");
                if self.reconnecting {
                    self.reconnecting = false;
                    self.lose(changes);
                } else {
                    self.attaching = None;
                    self.exited = true;
                    changes.exited = true;
                }
            }
            HostMsg::Error { message, .. } => tracing::warn!("the host reported: {message}"),
            _ => {}
        }
    }

    /// 写入宿主公布的状态；`notify` 时按 agent 状态的变化决定要不要通知外层。
    fn set_meta(&mut self, meta: SessionMeta, notify: bool, changes: &mut Changes) {
        let before = self.meta.agent;
        let after = meta.agent;
        if meta.title != self.meta.title || meta.fallback_title != self.meta.fallback_title || before != after {
            changes.title_changed = true;
        }
        if before != after {
            changes.agent_changed = true;
        }
        if notify {
            changes.agent_finished |= agent_finished(before, after);
            changes.agent_blocked |= agent_blocked(before, after);
        }
        if let Some(session) = self.live_mut() {
            session.apply_meta(meta.clone());
        }
        self.meta = meta;
    }
}

/// 前台 agent 本来在工作、现在不在工作了（干完了、等着用户回答或者退出了）。
fn agent_finished(before: Option<Agent>, after: Option<Agent>) -> bool {
    before.is_some_and(Agent::is_working) && !after.is_some_and(Agent::is_working)
}

/// 前台 agent 刚停下来等用户回答。
fn agent_blocked(before: Option<Agent>, after: Option<Agent>) -> bool {
    !before.is_some_and(Agent::is_blocked) && after.is_some_and(Agent::is_blocked)
}

#[cfg(test)]
mod tests;
