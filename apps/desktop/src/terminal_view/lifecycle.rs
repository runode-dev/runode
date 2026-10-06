//! 终端视图的生命周期：在宿主里开会话、连上它、启动 shell、处理宿主发来的输出和状态，在显示
//! 和不显示之间切换，和宿主断开后重开，以及光标闪烁和同步输出的计时器。前台进程的轮询和 agent
//! 状态的判断在宿主里，结果随 `HostMsg::Meta` 到达。
//!
//! 和宿主之间经 `session_host::link()` 说话：连上会话时宿主给一份当时的屏幕（同一个构建时是
//! 快照，否则是 VT 重放），之后的输出和标记按先后到达；事件按界面这份 VT 现在的样子怎么处理见
//! `ScreenState`。响铃只认宿主的 `HostMsg::Bell`，不看界面这份 VT 的 `on_bell`：两边认的是同一个
//! BEL，都认会响两次；宿主那份在视图只看状态时也认得到，VT 重放出来的屏幕也不会再响一遍。

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    task::Poll,
    thread,
    time::{Duration, Instant},
};

use futures::{StreamExt as _, channel::mpsc::UnboundedReceiver, task::ArcWake};
use gpui::{App, AppContext as _, Context, Entity, Font, Global, Point, SharedString, Task, Window, font, px};
use runode_protocol::{AttachMode, ClientMsg, HostMsg, SessionId};
use runode_shared_types::{
    agent::Agent,
    color::Rgb,
    grid::GridSize,
    session::{Driver, SessionMeta},
    settings::TermSettings,
};
use runode_terminal::{
    history,
    session::{self, Request, SYNC_OUTPUT_TIMEOUT, Session},
};

use super::{
    DEFAULT_TITLE, Events, MAX_FONT_SIZE, MIN_FONT_SIZE, TerminalEvent, TerminalView,
    screen::{Attach, Changes, HIDE_GRACE, SHOW_WAIT, ScreenState},
};
use crate::{
    config::AppConfig,
    prespawn::Prespawned,
    session_host::{self, LinkEvent, Screen, SpawnOptions},
};

/// 配置的字体都不可用时使用的等宽字体，macOS 自带。
const FALLBACK_FONT_FAMILY: &str = "Menlo";
/// 视图建好时伪终端的临时尺寸；第一次布局时会按实际大小重设，shell 等到那之后才启动。
const PROVISIONAL_SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
/// 建视图时最多先喂进去这么多已经到达的输出，余下的照常交给读输出的任务：shell 一启动就
/// 大量输出时，第一帧不能等它们全部处理完。
const EARLY_OUTPUT_LIMIT: usize = 64 * 1024;
/// 一次最多合并这么多排队的输出再交给 VT：积压很多时不必为它们另拼一整块大缓冲。
const MAX_OUTPUT_BATCH: usize = 1024 * 1024;
/// 光标闪烁时亮、灭各持续的时长。
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);
/// 连上会话时最多等这么久宿主给的第一份屏幕。
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// 和宿主断开后有个终端点了「在原目录重开」、重新连上了宿主：第几次重连，以及那时宿主里还活着
/// 的会话。断开着的视图见了，会话还在的就重新连上，见 `TerminalView::host_reconnected`。
#[derive(Default)]
struct HostReconnected {
    generation: u64,
    alive: Rc<HashSet<SessionId>>,
}

impl Global for HostReconnected {}

/// 界面这边的 `Session` 要宿主做的事，经连接发给会话 `id`。
fn sender(id: SessionId) -> session::Sender {
    Box::new(move |request| {
        let link = session_host::link();
        match request {
            Request::Input(data) => link.input(id, &data),
            Request::Resize(size) => link.send(ClientMsg::Resize { id, size }),
            Request::ClearScreen => link.send(ClientMsg::ClearScreen { id }),
        }
    })
}

/// 按宿主给的屏幕建界面这边的 `Session`：快照直接解出来（已经套着宿主的主题）；VT 重放按宿主
/// 给的尺寸和主题（没给时用 `settings`）新建一份 VT 再喂。界面自己的那部分配置按 `settings` 套上。
fn build_session(id: SessionId, screen: Screen, settings: &TermSettings) -> anyhow::Result<Session> {
    let Screen { attached, data } = screen;
    let mut session = match attached.mode {
        AttachMode::Snapshot => Session::from_snapshot(&data, sender(id))?,
        AttachMode::VtReplay | AttachMode::MetaOnly => {
            let theme = attached.settings.as_ref().unwrap_or(settings);
            let mut session = Session::new(attached.size, theme, sender(id))?;
            session.feed(&data);
            session
        }
    };
    session.apply_config(settings);
    Ok(session)
}

/// 已经连上、拿到第一份屏幕：建好界面这份 VT，正看着。
fn live(id: SessionId, screen: Screen, settings: &TermSettings) -> anyhow::Result<ScreenState<Session>> {
    let (channel, meta, size) = (screen.attached.channel, screen.attached.meta.clone(), screen.attached.size);
    let session = build_session(id, screen, settings)?;
    Ok(ScreenState::new_live(session, channel, meta, size))
}

/// 连上宿主里的会话 `id`，等它给第一份屏幕，建好界面这边的 VT，返回它和收之后的事件的一端。
/// 连不上时结束这个会话（用于刚开的会话）。
fn connect(
    id: SessionId,
    settings: &TermSettings,
) -> anyhow::Result<(ScreenState<Session>, UnboundedReceiver<LinkEvent>)> {
    let link = session_host::link();
    let connected = link
        .attach_now(id, None, AttachMode::Snapshot, ATTACH_TIMEOUT)
        .and_then(|(screen, rx)| Ok((live(id, screen, settings)?, rx)));
    if connected.is_err() {
        link.kill(id);
    }
    connected
}

/// 唤醒时叫醒等着的线程，主线程上等宿主给屏幕时用，见 `TerminalView::wait_for_screen`。
struct Unpark(thread::Thread);

impl ArcWake for Unpark {
    fn wake_by_ref(arc: &Arc<Self>) {
        arc.0.unpark();
    }
}

impl TerminalView {
    /// 建好视图，在 `cwd` 下启动 shell，`cwd` 为 `None` 时从家目录开始；shell 等第一次布局后
    /// 才启动，见 `start`。伪终端开不了时返回错误，不建视图。
    pub fn spawn(cwd: Option<&Path>, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let view = Self::unstarted(cwd, window, cx)?;
        view.update(cx, |view, cx| view.start(cx));
        Ok(view)
    }

    /// 建好视图但先不启动 shell，等 `start` 时再在 `cwd` 下启动。恢复布局时看不见的终端用它，
    /// 不切过去就不占进程。
    pub fn unstarted(cwd: Option<&Path>, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let config = cx.global::<AppConfig>().0.clone();
        let id = session_host::link().spawn(SpawnOptions {
            size: PROVISIONAL_SIZE,
            cwd: cwd.map(Into::into),
            integration: config.shell_integration,
            start: false,
            shell: None,
            settings: None,
            env: Vec::new(),
        })?;
        let (screen, rx) = connect(id, &config.term_settings())?;
        Ok(cx.new(|cx| Self::new(id, screen, false, rx, window, cx)))
    }

    /// 用宿主里已有的会话 `id` 建视图（存档恢复、接上后台会话）。`cwd` 是调用方记着的目录，宿主
    /// 还没报告目录时当作它的目录。`visible` 为假时视图只看状态（`AttachMode::MetaOnly`），等
    /// `set_visible` 再看屏幕。会话已经启动过，不再 `start`。连不上时返回错误，不结束会话。
    pub fn reattach(
        id: SessionId,
        cwd: Option<&Path>,
        visible: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let config = cx.global::<AppConfig>().0.clone();
        let link = session_host::link();
        if !link.connected() {
            anyhow::bail!("not connected to the host");
        }
        let (screen, rx) = if visible {
            let (screen, rx) = link.attach_now(id, None, AttachMode::Snapshot, ATTACH_TIMEOUT)?;
            (live(id, screen, &config.term_settings())?, rx)
        } else {
            let rx = link.attach(id, None, AttachMode::MetaOnly);
            (ScreenState::new_attaching(AttachMode::MetaOnly, PROVISIONAL_SIZE, Instant::now()), rx)
        };
        let start_dir = cwd.map(Path::to_path_buf);
        Ok(cx.new(|cx| {
            let mut view = Self::new(id, screen, true, rx, window, cx);
            view.start_dir = start_dir;
            view
        }))
    }

    pub fn started(&self) -> bool {
        self.started
    }

    /// 启动 `unstarted` 建的视图的 shell：等下一次布局量出实际尺寸再启动，见 `start_pending`。
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if !self.started {
            self.start_pending = true;
            cx.notify();
        }
    }

    /// 按已经设好的实际尺寸启动 shell。启动不了时宿主发 `HostMsg::Exited`，按 shell 已退出
    /// 处理，关掉这个终端。
    pub(super) fn start_now(&mut self, _cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        session_host::link().send(ClientMsg::Start { id: self.id, integration: self.config.shell_integration });
    }

    /// 收宿主发来的事件的任务：把排队的事件合并成一批处理，再重绘。收事件的一端在视图里，
    /// 只在取事件时借用：回到显示时主线程要直接从它取（见 `wait_for_screen`）。
    fn read_events(events: Events, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let next = futures::future::poll_fn(|task| events.borrow_mut().poll_next_unpin(task)).await;
                let Some(first) = next else { break };
                // 把已排队的输出合并成一次 VT 写入和一次重绘。
                let mut bytes = output_len(&first);
                let mut batch = vec![first];
                while bytes < MAX_OUTPUT_BATCH
                    && let Ok(event) = events.borrow_mut().try_recv()
                {
                    bytes += output_len(&event);
                    batch.push(event);
                }
                let ended = batch.iter().any(ends);
                let updated = this.update_in(cx, |view, window, cx| {
                    view.handle_link_events(batch, window, cx);
                    view.ring_bell(cx);
                });
                if updated.is_err() || ended {
                    break;
                }
            }
        })
    }

    /// 按先后处理宿主发来的一批事件（见 `ScreenState::apply`），再把变化转给外层：标题、agent、
    /// 退出等事件，命令历史，要重新连上的就连，然后重绘。响铃留给调用方用 `ring_bell` 转发，
    /// 见 `new`；这一批里有退出时在退出之前转发。
    fn handle_link_events(&mut self, events: Vec<LinkEvent>, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let settings = self.config.term_settings();
        let mut build = |screen: Screen| build_session(id, screen, &settings);
        let changes = self.screen.apply(events, &mut build, Instant::now());
        self.apply_changes(changes, window, cx);
    }

    fn apply_changes(&mut self, changes: Changes, window: &mut Window, cx: &mut Context<Self>) {
        if changes.replaced {
            tracing::debug!("session {}: new screen, channel {:?}", self.id, self.screen.channel());
            self.vt_replaced(cx);
        }
        if changes.fed {
            self.input_changed = true;
            self.completion_output(cx);
            // 有输出（包括键入的回显）时光标先亮起，免得打字时看不到它。
            self.reset_cursor_blink(window, cx);
        }
        if changes.theme_applied {
            // 高亮的颜色取自调色板。
            self.input_changed = true;
        }
        if changes.title_changed {
            cx.emit(TerminalEvent::TitleChanged);
        }
        if changes.agent_changed {
            self.agent_changed_at = Instant::now();
        }
        if changes.agent_finished {
            cx.emit(TerminalEvent::AgentFinished);
        }
        if changes.agent_blocked {
            cx.emit(TerminalEvent::AgentBlocked);
        }
        // 关掉建议时宿主不记，这边也不加。
        if self.config.command_suggestions {
            for command in changes.commands {
                history::observe(history::Entry {
                    cmd: command.cmd,
                    cwd: command.cwd,
                    exit: command.exit,
                    ts: command.ts,
                });
            }
        }
        if changes.bell {
            self.bell_pending = true;
        }
        if let Some(attach) = changes.attach {
            tracing::info!("session {} attaches again ({:?})", self.id, attach.mode);
            self.send_attach(attach);
        }
        if changes.lost {
            tracing::warn!("session {} lost its host; its last screen stays", self.id);
        }
        if changes.exited {
            // 退出前响过的铃先通知：外层收到 `Exited` 就关掉分屏，之后再通知就找不到这个终端了。
            self.ring_bell(cx);
            cx.emit(TerminalEvent::Exited);
        }
        if self.screen.live().is_some_and(Session::render_held) {
            self.schedule_hold_timeout(cx);
        }
        cx.notify();
    }

    /// 经连接重新连上会话（沿用原来的登记和收事件的一端）。
    fn send_attach(&self, attach: Attach) {
        if !session_host::link().reattach(self.id, attach.size, attach.mode) {
            // 连接已经断了：断开的消息已经排在收事件的一端里，处理到时按断开显示。
            tracing::debug!("session {} cannot attach again: not connected", self.id);
        }
    }

    /// 界面这份 VT 换了或者没了：跟着旧 VT 的选区、补全、建议都作废，搜索栏开着的话在新的上面
    /// 重新搜。
    fn vt_replaced(&mut self, cx: &mut Context<Self>) {
        self.completion = None;
        self.completion_pending = None;
        self._completion_wait = None;
        self.suggestion = None;
        self.highlight = Rc::default();
        self.selecting = false;
        self.reporting_press = false;
        self.click_cell = None;
        self._autoscroll = None;
        self.input_changed = true;
        if let Some((field, _)) = &self.search_field {
            let query = field.read(cx).query().to_owned();
            if let Some(session) = self.screen.shown_mut()
                && !query.is_empty()
            {
                session.search(&query);
            }
        }
    }

    /// 程序响过铃（宿主发了 `HostMsg::Bell`）的话通知外层。
    fn ring_bell(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.bell_pending) {
            cx.emit(TerminalEvent::Bell);
        }
    }

    /// 视图在不在窗口里显示（窗口当前 workspace 当前标签里的分屏，被放大的分屏挡住的也算）。
    /// 离开显示 `HIDE_GRACE` 后只看状态、丢掉界面这份 VT；回到显示时已经丢了的话按视图的尺寸
    /// 重新要一份屏幕，最多等 `SHOW_WAIT`，等不到先画背景，到了再补上。
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let attach = self.screen.set_visible(visible, now);
        if visible {
            self._hide_timer = None;
            if let Some(attach) = attach {
                self.send_attach(attach);
                self.wait_for_screen(window, cx);
                cx.notify();
            }
            return;
        }
        if self._hide_timer.is_none() && !self.screen.visible() {
            self._hide_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(HIDE_GRACE).await;
                this.update(cx, |view, cx| view.hide_if_due(cx)).ok();
            }));
        }
    }

    /// 视图显示着，见 `set_visible`。
    pub fn visible(&self) -> bool {
        self.screen.visible()
    }

    /// 不显示满了宽限：丢掉界面这份 VT，改成只看状态。
    fn hide_if_due(&mut self, cx: &mut Context<Self>) {
        self._hide_timer = None;
        if let Some(attach) = self.screen.tick(Instant::now()) {
            tracing::debug!("session {} hidden, dropping its screen", self.id);
            self.send_attach(attach);
            self.vt_replaced(cx);
            cx.notify();
        }
    }

    /// 在主线程上等宿主给屏幕，最多等到 `SHOW_WAIT`，期间到的事件照常处理。等的时候直接从收事件
    /// 的一端取，用的是自己的唤醒方，所以之后要重新起读事件的任务。
    fn wait_for_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let started = Instant::now();
        let deadline = started + SHOW_WAIT;
        let waker = futures::task::waker(Arc::new(Unpark(thread::current())));
        let mut task = std::task::Context::from_waker(&waker);
        while self.screen.is_attaching() {
            let mut events = Vec::new();
            let mut closed = false;
            {
                let mut rx = self.events.borrow_mut();
                loop {
                    match rx.poll_next_unpin(&mut task) {
                        Poll::Ready(Some(event)) => events.push(event),
                        Poll::Ready(None) => {
                            closed = true;
                            break;
                        }
                        Poll::Pending => break,
                    }
                }
            }
            let got = !events.is_empty();
            if got {
                self.handle_link_events(events, window, cx);
            }
            let now = Instant::now();
            if closed || now >= deadline {
                break;
            }
            if !got {
                thread::park_timeout(deadline - now);
            }
        }
        if self.screen.is_attaching() {
            tracing::debug!("session {}: no screen within {SHOW_WAIT:?}, drawing the background first", self.id);
        } else {
            tracing::debug!("session {}: screen shown after {:?}", self.id, started.elapsed());
        }
        self._reader = Self::read_events(self.events.clone(), window, cx);
    }

    /// 和宿主断开后点了「在原目录重开」：先重新连上宿主；这个终端的会话还在就接着用它，不在了就
    /// 在原来的目录开一个新的换上。别的断开着的终端里会话还在的随之重新连上，见 `HostReconnected`。
    pub(super) fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screen.is_lost() {
            return;
        }
        if let Err(err) = session_host::reconnect() {
            tracing::warn!("failed to reconnect to the host: {err:#}");
            return;
        }
        let alive: HashSet<SessionId> = match session_host::list_sessions() {
            Ok(sessions) => sessions.into_iter().filter(|session| !session.exited).map(|session| session.id).collect(),
            Err(err) => {
                tracing::warn!("failed to list the host's sessions after reconnecting: {err:#}");
                HashSet::new()
            }
        };
        if alive.contains(&self.id) {
            self.resume(window, cx);
        } else {
            let (size, cwd) = (self.screen.last_size(), self.cwd());
            if let Err(err) = self.replace_session(size, cwd, window, cx) {
                tracing::warn!("failed to reopen the terminal: {err:#}");
            }
        }
        let generation = cx.try_global::<HostReconnected>().map_or(0, |reconnected| reconnected.generation) + 1;
        cx.set_global(HostReconnected { generation, alive: Rc::new(alive) });
    }

    /// 别的终端重新连上了宿主：自己也断开着、会话还在的话重新连上。
    fn host_reconnected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screen.is_lost() {
            return;
        }
        let alive = cx.global::<HostReconnected>().alive.clone();
        if alive.contains(&self.id) {
            self.resume(window, cx);
        }
    }

    /// 断开后宿主又连上了、会话还在：重新登记、连上它。显示着的冻结着最后一屏等新的屏幕。
    fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(attach) = self.screen.reconnect(Instant::now()) else {
            return;
        };
        tracing::info!("session {} attaches again after reconnecting", self.id);
        let rx = session_host::link().attach(self.id, attach.size, attach.mode);
        self.events = Rc::new(RefCell::new(rx));
        if attach.mode == AttachMode::MetaOnly {
            self._reader = Self::read_events(self.events.clone(), window, cx);
        } else {
            self.wait_for_screen(window, cx);
        }
        cx.notify();
    }

    /// 在 `cwd`（为 `None` 时家目录）按 `size` 开一个已经启动的新会话、连上它，换掉现在的；换下来的
    /// 会话随之结束。
    fn replace_session(
        &mut self,
        size: GridSize,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let link = session_host::link();
        let id = link.spawn(SpawnOptions {
            size,
            cwd,
            integration: self.config.shell_integration,
            start: true,
            shell: None,
            settings: None,
            env: Vec::new(),
        })?;
        let (mut screen, rx) = connect(id, &self.config.term_settings())?;
        let old = std::mem::replace(&mut self.id, id);
        link.kill(old);
        screen.resize(size);
        self.screen = screen;
        self.started = true;
        self.events = Rc::new(RefCell::new(rx));
        self._reader = Self::read_events(self.events.clone(), window, cx);
        self.vt_replaced(cx);
        cx.emit(TerminalEvent::TitleChanged);
        cx.notify();
        Ok(())
    }

    /// 提前启动的 shell 按上次记下的尺寸启动，和这次量到的不一样（窗口大小或侧栏变了）时，
    /// 换一个按实际尺寸启动的 shell。不直接调整尺寸：那时提示符多半已经画好，shell 收到尺寸
    /// 变化会重画提示符，可能正赶上插件管理器延迟加载插件、临时切进了插件目录，画出错的路径。
    pub(super) fn respawn(&mut self, size: GridSize, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = self.replace_session(size, None, window, cx) {
            tracing::warn!("failed to restart the early shell at its real size: {err:#}");
            self.screen.resize(size);
        }
    }

    /// 接上启动时提前拉起的 shell（见 `prespawn`）并建好它的视图。
    pub fn adopt(shell: Prespawned, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let size = shell.size;
        let id = shell.into_id();
        let (screen, rx) = connect(id, &cx.global::<AppConfig>().0.term_settings())?;
        Ok(cx.new(|cx| {
            let mut view = Self::new(id, screen, true, rx, window, cx);
            view.adopted_size = Some(size);
            view
        }))
    }

    fn new(
        id: SessionId,
        screen: ScreenState<Session>,
        started: bool,
        rx: UnboundedReceiver<LinkEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = cx.global::<AppConfig>().0.clone();

        let config_watch = cx.observe_global_in::<AppConfig>(window, |view, window, cx| {
            view.config = cx.global::<AppConfig>().0.clone();
            // 主题等宿主标出位置后再换，见 `HostMsg::ThemeApplied`。
            let settings = view.config.term_settings();
            if let Some(session) = view.screen.shown_mut() {
                session.apply_config(&settings);
            }
            view.font = resolve_font(&view.config.font_family, window);
            view.font_size = px(view.config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
            // 字体、字号或行高调整都可能变了，单元格尺寸和字形缓存一律作废。
            view.metrics = None;
            view.glyphs.iter_mut().for_each(HashMap::clear);
            view.input_changed = true;
            cx.notify();
        });
        let appearance_watch = cx.observe_window_appearance(window, |_, _, cx| crate::config::follow_appearance(cx));
        let reconnect_watch =
            cx.observe_global_in::<HostReconnected>(window, |view, window, cx| view.host_reconnected(window, cx));

        // 第一次用到时开始在后台读命令历史，开着建议时现在就读起来。
        if config.command_suggestions {
            history::load_in_background();
        }

        let focus_handle = cx.focus_handle();
        let pane_focus = cx.focus_handle();
        let focus_watch = [
            cx.on_focus_in(&pane_focus, window, |_, _, cx| cx.emit(TerminalEvent::Focused)),
            cx.on_focus(&focus_handle, window, |view, window, cx| {
                view.reset_cursor_blink(window, cx);
            }),
            cx.on_blur(&focus_handle, window, |view, _, _| {
                view._cursor_blink = None;
                // 失焦时补全菜单也关掉。
                view.completion = None;
            }),
        ];

        let settings = config.term_settings();
        let mut view = Self {
            screen,
            events: Rc::new(RefCell::new(rx)),
            _hide_timer: None,
            colors: (settings.foreground, settings.background),
            start_dir: None,
            id,
            ended: false,
            bell_pending: false,
            started,
            font: resolve_font(&config.font_family, window),
            font_size: px(config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)),
            config,
            focus_handle,
            metrics: None,
            glyphs: Default::default(),
            marked_text: None,
            suggestion: None,
            suggester: Default::default(),
            highlight: Default::default(),
            input_changed: true,
            history_generation: 0,
            completion: None,
            completion_pending: None,
            _completion_wait: None,
            output_at: None,
            scroll_remainder: 0.,
            cursor_bounds: None,
            grid_origin: Point::default(),
            selecting: false,
            reporting_press: false,
            click_cell: None,
            cursor_blink_visible: true,
            cursor_blink_since: Instant::now(),
            adopted_size: None,
            start_pending: false,
            search_field: None,
            _reader: Task::ready(()),
            agent_changed_at: Instant::now(),
            _hold_timeout: None,
            _cursor_blink: None,
            _autoscroll: None,
            _config_watch: config_watch,
            _appearance_watch: appearance_watch,
            _reconnect_watch: reconnect_watch,
            pane_focus,
            _focus_watch: focus_watch,
        };

        // 提前启动的 shell 多半已经输出了提示符，宿主补发的事件现在就处理，第一帧就画得出来，
        // 不用等下面读事件的任务排上主线程。退出留给那个任务：现在还没有谁订阅这个视图的事件。
        let mut early = Vec::new();
        let mut ended_early = false;
        let mut bytes = 0;
        {
            let mut rx = view.events.borrow_mut();
            while bytes < EARLY_OUTPUT_LIMIT
                && let Ok(event) = rx.try_recv()
            {
                if ends(&event) {
                    ended_early = true;
                    early.push(event);
                    break;
                }
                bytes += output_len(&event);
                early.push(event);
            }
        }
        let last = if ended_early { early.pop() } else { None };
        if !early.is_empty() {
            view.handle_link_events(early, window, cx);
            // 补发的输出里响过铃的话，这时通知外层会丢：订阅要等这一轮的副作用处理到时才生效，
            // 排在它前面发出的事件没人收。推迟到外层订阅好以后再通知。
            cx.defer_in(window, |view, _, cx| view.ring_bell(cx));
        }
        view._reader = match last {
            Some(last) => Self::handle_later(last, window, cx),
            None => Self::read_events(view.events.clone(), window, cx),
        };
        view
    }

    /// 建视图时就已经到了的那件「之后不再有事件」的事（退出、断开）留到外层订阅好这个视图的事件
    /// 以后再处理。
    fn handle_later(last: LinkEvent, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, |view, window, cx| {
                view.handle_link_events(vec![last], window, cx);
                view.ring_bell(cx);
            })
            .ok();
        })
    }

    /// 宿主里这个终端的会话。
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// 宿主最近一次公布的这个会话的状态。
    // 后台标签和存档恢复用上它之前先放着。
    #[allow(dead_code)]
    pub fn meta(&self) -> &SessionMeta {
        self.screen.meta()
    }

    /// 最近一次别的终端里的程序操作这个会话的记录，见 `SessionMeta::driver`。
    // 驱动标记的显示用上它之前先放着。
    #[allow(dead_code)]
    pub fn driver(&self) -> Option<&Driver> {
        self.screen.meta().driver.as_ref()
    }

    /// 结束宿主里的会话（关标签、关分屏这类用户明确要关掉终端的时候）。之后丢掉视图时不再
    /// 另外发什么；没调过它就丢掉视图时见 `Drop`。
    pub fn end(&mut self) {
        if !std::mem::replace(&mut self.ended, true) {
            session_host::link().kill(self.id);
        }
    }

    /// 终端的尺寸：视图最近一次量出的，还没量过时是宿主给的会话尺寸。不显示的终端也有。
    pub fn size(&self) -> GridSize {
        self.screen.last_size()
    }

    /// 不等布局，按 `size` 现在就启动 shell。放在看不见的地方（后台标签、放大的分屏后面）的终端
    /// 等不来 `start` 要的那次布局；之后显示出来时照常按实际尺寸改。
    pub fn start_at(&mut self, size: GridSize, cx: &mut Context<Self>) {
        if self.started {
            return;
        }
        self.screen.resize(size);
        if self.screen.live().is_none() {
            // 没有界面这份 VT 替它请宿主改尺寸，直接请。
            session_host::link().send(ClientMsg::Resize { id: self.id, size });
        }
        self.start_pending = false;
        self.start_now(cx);
    }

    /// shell 当前所在的目录，新建标签或分屏时沿用；宿主还没报告时是建视图时给的目录。
    pub fn cwd(&self) -> Option<PathBuf> {
        self.screen.meta().cwd.clone().or_else(|| self.start_dir.clone())
    }

    /// 程序设置的标题；没设置时为前台进程的目录名或进程名，都没有时为 `DEFAULT_TITLE`。
    pub fn title(&self) -> &str {
        let meta = self.screen.meta();
        meta.title.as_deref().or(meta.fallback_title.as_deref()).unwrap_or(DEFAULT_TITLE)
    }

    /// 前台 agent 在标题里报告的状态；不是 agent 在前台时为 `None`。
    pub fn agent(&self) -> Option<Agent> {
        self.screen.meta().agent
    }

    /// 前台 agent 上次换了种类或状态的时刻。
    pub fn agent_changed_at(&self) -> Instant {
        self.agent_changed_at
    }

    /// 终端用的字体，Git 面板和预览栏里的代码也用它。
    pub fn font_family(&self) -> SharedString {
        self.font.family.clone()
    }

    /// 当前的默认前景色和背景色，标签栏跟着终端配色走。没有界面这份 VT 时是最后画出的那一份。
    pub fn colors(&mut self) -> (Rgb, Rgb) {
        if let Some(session) = self.screen.shown_mut() {
            let frame = session.frame();
            self.colors = (frame.foreground, frame.background);
        }
        self.colors
    }

    /// 让光标立即亮起，并从头开始计闪烁周期；没有焦点时不闪，也就不启动计时器。
    /// 每批输出都会调用，所以计时器只建一次，重新计周期只是改 `cursor_blink_since`。
    pub(super) fn reset_cursor_blink(&mut self, window: &Window, cx: &mut Context<Self>) {
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
                    if view.screen.shown_mut().is_some_and(|session| session.frame().cursor.is_some_and(|c| c.blinking))
                    {
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
}

/// 依次尝试配置的字体族，取第一个系统里有的。字体族整个找不到时 GPUI 会
/// 落到 Helvetica 这类非等宽字体，所以要先确认能解析成它自己。
fn resolve_font(families: &[String], window: &Window) -> Font {
    let text_system = window.text_system();
    let family = families
        .iter()
        .find(|family| {
            let id = text_system.resolve_font(&font(family.as_str()));
            text_system.get_font_for_id(id).is_some_and(|f| f.family.as_ref() == family.as_str())
        })
        .map_or(FALLBACK_FONT_FAMILY, String::as_str);
    tracing::debug!("terminal font: {family}");
    font(family.to_owned())
}

/// 事件里输出的字节数，合并批次时用。
fn output_len(event: &LinkEvent) -> usize {
    match event {
        LinkEvent::Output(data) => data.len(),
        _ => 0,
    }
}

/// 这件事之后不会再有这个会话的事件：shell 退出了，或者和宿主断开了。
fn ends(event: &LinkEvent) -> bool {
    matches!(event, LinkEvent::Lost | LinkEvent::Msg(HostMsg::Exited { .. }))
}
