//! 终端视图的生命周期：在宿主里开会话、连上它、启动 shell、处理宿主发来的输出和状态，以及
//! 光标闪烁和同步输出的计时器。前台进程的轮询和 agent 状态的判断在宿主里，结果随
//! `HostMsg::Meta` 到达。

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{FutureExt as _, StreamExt as _, channel::mpsc::UnboundedReceiver};
use gpui::{App, AppContext as _, Context, Entity, Font, Point, SharedString, Task, Window, font, px};
use runode_host::{ClientMsg, HostEvent, HostMsg, SessionId, SpawnOptions};
use runode_shared_types::{agent::Agent, color::Rgb, grid::GridSize};
use runode_terminal::{
    history,
    session::{Request, SYNC_OUTPUT_TIMEOUT, Session},
};

use super::{DEFAULT_TITLE, MAX_FONT_SIZE, MIN_FONT_SIZE, TerminalEvent, TerminalView};
use crate::{config::AppConfig, prespawn::Prespawned, session_host};

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

/// 连上宿主里的会话 `id`，建好界面这边的 `Session`，返回它、收事件的一端和 shell 是否已经
/// 启动。连不上时结束这个会话。
fn connect(id: SessionId) -> anyhow::Result<(Session, UnboundedReceiver<HostEvent>, bool)> {
    let client = session_host::client();
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let connected = client.attach(id, Box::new(move |event| tx.unbounded_send(event).is_ok())).and_then(|attached| {
        let sender = Box::new(move |request| match request {
            Request::Input(data) => client.input(id, data),
            Request::Resize(size) => client.send(ClientMsg::Resize { id, size }),
            Request::ClearScreen => client.send(ClientMsg::ClearScreen { id }),
        });
        let mut session = Session::new(attached.size, &attached.settings, sender)?;
        session.apply_meta(attached.meta);
        Ok((session, attached.started))
    });
    match connected {
        Ok((session, started)) => Ok((session, rx, started)),
        Err(err) => {
            client.send(ClientMsg::Kill { id });
            Err(err)
        }
    }
}

impl TerminalView {
    /// 建好视图，在 `cwd` 下启动 shell，`cwd` 为 `None` 时从家目录开始；shell 等第一次布局后
    /// 才启动，见 `start`。伪终端开不了时返回错误，不建视图。
    pub fn spawn(cwd: Option<&std::path::Path>, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let view = Self::unstarted(cwd, window, cx)?;
        view.update(cx, |view, cx| view.start(cx));
        Ok(view)
    }

    /// 建好视图但先不启动 shell，等 `start` 时再在 `cwd` 下启动。恢复布局时看不见的终端用它，
    /// 不切过去就不占进程。
    pub fn unstarted(cwd: Option<&std::path::Path>, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let integration = cx.global::<AppConfig>().0.shell_integration;
        let id = session_host::client().spawn(SpawnOptions {
            size: PROVISIONAL_SIZE,
            cwd: cwd.map(Into::into),
            integration,
            start: false,
            shell: None,
            settings: None,
        })?;
        let (session, rx, started) = connect(id)?;
        Ok(cx.new(|cx| Self::new(id, session, started, rx, window, cx)))
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
        session_host::client().start(self.id, self.config.shell_integration);
    }

    /// 收宿主发来的事件的任务：把排队的事件合并成一批处理，再重绘。
    fn read_events(
        mut rx: impl futures::Stream<Item = HostEvent> + Unpin + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = rx.next().await {
                // 把已排队的输出合并成一次 VT 写入和一次重绘。
                let mut bytes = output_len(&first);
                let mut events = vec![first];
                while bytes < MAX_OUTPUT_BATCH
                    && let Some(Some(event)) = rx.next().now_or_never()
                {
                    bytes += output_len(&event);
                    events.push(event);
                }
                let exited = events.iter().any(is_exited);
                let updated = this.update_in(cx, |view, window, cx| view.handle_host_events(events, window, cx));
                if updated.is_err() || exited {
                    break;
                }
            }
        })
    }

    /// 按先后处理宿主发来的一批事件：输出喂给 VT（连着的几块合成一次写入），在标出的位置改
    /// 尺寸、换主题，写入对外公布的状态，转发标题、响铃、退出等事件，再重绘。
    fn handle_host_events(&mut self, events: Vec<HostEvent>, window: &mut Window, cx: &mut Context<Self>) {
        let mut pending: Vec<Arc<[u8]>> = Vec::new();
        let mut fed = false;
        let mut exited = false;
        for event in events {
            let message = match event {
                HostEvent::Output(data) => {
                    pending.push(data);
                    continue;
                }
                HostEvent::Msg(message) => *message,
            };
            fed |= self.feed_output(&mut pending, cx);
            match message {
                HostMsg::Meta { meta, .. } => self.notify_agent_finished(cx, |view, cx| {
                    if view.session.apply_meta(meta) {
                        cx.emit(TerminalEvent::TitleChanged);
                    }
                }),
                HostMsg::Resized { size, .. } => self.session.apply_resized(size),
                // 用最新的配置：宿主按主线程最近一次发的主题换，这时全局配置已经是它了。
                HostMsg::ThemeApplied { .. } => {
                    let settings = cx.global::<AppConfig>().0.term_settings();
                    self.session.apply_theme(&settings);
                }
                HostMsg::CommandFinished { command, .. } => {
                    // 关掉建议时宿主不记，这边也不加。
                    if self.config.command_suggestions {
                        history::observe(history::Entry {
                            cmd: command.cmd,
                            cwd: command.cwd,
                            exit: command.exit,
                            ts: command.ts,
                        });
                    }
                }
                HostMsg::Exited { .. } => exited = true,
                _ => {}
            }
        }
        fed |= self.feed_output(&mut pending, cx);
        if fed {
            // 有输出（包括键入的回显）时光标先亮起，免得打字时看不到它。
            self.reset_cursor_blink(window, cx);
        }
        if self.session.take_bell() {
            cx.emit(TerminalEvent::Bell);
        }
        if exited {
            self.session.exited = true;
            cx.emit(TerminalEvent::Exited);
        }
        if self.session.render_held() {
            self.schedule_hold_timeout(cx);
        }
        cx.notify();
    }

    /// 把攒着的几块输出一次喂给 VT，返回是否喂了。
    fn feed_output(&mut self, pending: &mut Vec<Arc<[u8]>>, cx: &mut Context<Self>) -> bool {
        match pending.len() {
            0 => return false,
            1 => self.session.feed(&pending[0]),
            _ => self.session.feed(&pending.concat()),
        }
        pending.clear();
        self.input_changed = true;
        self.completion_output(cx);
        true
    }

    /// 提前启动的 shell 按上次记下的尺寸启动，和这次量到的不一样（窗口大小或侧栏变了）时，
    /// 换一个按实际尺寸启动的 shell。不直接调整尺寸：那时提示符多半已经画好，shell 收到尺寸
    /// 变化会重画提示符，可能正赶上插件管理器延迟加载插件、临时切进了插件目录，画出错的路径。
    pub(super) fn respawn(&mut self, size: GridSize, window: &mut Window, cx: &mut Context<Self>) {
        let spawned = session_host::client()
            .spawn(SpawnOptions {
                size,
                cwd: None,
                integration: self.config.shell_integration,
                start: true,
                shell: None,
                settings: None,
            })
            .and_then(|id| Ok((id, connect(id)?)));
        let (id, (mut session, rx, started)) = match spawned {
            Ok(spawned) => spawned,
            Err(err) => {
                tracing::warn!("failed to restart the early shell at its real size: {err:#}");
                self.session.resize(size);
                return;
            }
        };
        session.apply_config(&self.config.term_settings());
        // 换下来的会话随之结束。
        let old = std::mem::replace(&mut self.id, id);
        session_host::client().send(ClientMsg::Kill { id: old });
        self.session = session;
        self.started = started;
        self._reader = Self::read_events(rx, window, cx);
    }

    /// 接上启动时提前拉起的 shell（见 `prespawn`）并建好它的视图。
    pub fn adopt(shell: Prespawned, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let size = shell.size;
        let id = shell.into_id();
        let (session, rx, started) = connect(id)?;
        Ok(cx.new(|cx| {
            let mut view = Self::new(id, session, started, rx, window, cx);
            view.adopted_size = Some(size);
            view
        }))
    }

    fn new(
        id: SessionId,
        session: Session,
        started: bool,
        mut rx: UnboundedReceiver<HostEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = cx.global::<AppConfig>().0.clone();

        let config_watch = cx.observe_global_in::<AppConfig>(window, |view, window, cx| {
            view.config = cx.global::<AppConfig>().0.clone();
            // 主题等宿主标出位置后再换，见 `HostMsg::ThemeApplied`。
            view.session.apply_config(&view.config.term_settings());
            view.font = resolve_font(&view.config.font_family, window);
            view.font_size = px(view.config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
            // 字体、字号或行高调整都可能变了，单元格尺寸和字形缓存一律作废。
            view.metrics = None;
            view.glyphs.iter_mut().for_each(HashMap::clear);
            view.input_changed = true;
            cx.notify();
        });
        let appearance_watch = cx.observe_window_appearance(window, |_, _, cx| crate::config::follow_appearance(cx));

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

        let mut view = Self {
            session,
            id,
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
            pane_focus,
            _focus_watch: focus_watch,
        };
        view.session.apply_config(&view.config.term_settings());

        // 提前启动的 shell 多半已经输出了提示符，宿主补发的事件现在就处理，第一帧就画得出来，
        // 不用等下面读事件的任务排上主线程。退出留给那个任务：现在还没有谁订阅这个视图的事件。
        let mut early = Vec::new();
        let mut exited_early = None;
        let mut bytes = 0;
        while bytes < EARLY_OUTPUT_LIMIT
            && let Ok(event) = rx.try_recv()
        {
            if is_exited(&event) {
                exited_early = Some(event);
                break;
            }
            bytes += output_len(&event);
            early.push(event);
        }
        if !early.is_empty() {
            view.handle_host_events(early, window, cx);
        }
        view._reader = Self::read_events(futures::stream::iter(exited_early).chain(rx), window, cx);
        view
    }

    /// 宿主里这个终端的会话。
    pub fn session_id(&self) -> SessionId {
        self.id
    }

    /// 终端现在的尺寸。
    pub fn size(&self) -> GridSize {
        self.session.size()
    }

    /// 不等布局，按 `size` 现在就启动 shell。放在看不见的地方（后台标签、放大的分屏后面）的终端
    /// 等不来 `start` 要的那次布局；之后显示出来时照常按实际尺寸改。
    pub fn start_at(&mut self, size: GridSize, cx: &mut Context<Self>) {
        if self.started {
            return;
        }
        self.session.resize(size);
        self.start_pending = false;
        self.start_now(cx);
    }

    /// shell 当前所在的目录，新建标签或分屏时沿用。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.session.cwd()
    }

    /// 程序设置的标题；没设置时为前台进程的目录名或进程名，都没有时为 `DEFAULT_TITLE`。
    pub fn title(&self) -> &str {
        self.session.title.as_deref().or(self.session.fallback_title.as_deref()).unwrap_or(DEFAULT_TITLE)
    }

    /// 前台 agent 在标题里报告的状态；不是 agent 在前台时为 `None`。
    pub fn agent(&self) -> Option<Agent> {
        self.session.agent
    }

    /// 前台 agent 上次换了种类或状态的时刻。
    pub fn agent_changed_at(&self) -> Instant {
        self.agent_changed_at
    }

    /// 终端用的字体，Git 面板和预览栏里的代码也用它。
    pub fn font_family(&self) -> SharedString {
        self.font.family.clone()
    }

    /// 当前的默认前景色和背景色，标签栏跟着终端配色走。
    pub fn colors(&mut self) -> (Rgb, Rgb) {
        let frame = self.session.frame();
        (frame.foreground, frame.background)
    }

    /// 执行 `update`；前台 agent 本来在工作、执行完不在工作了（干完了、等着用户回答或者退出了）
    /// 时通知外层 `TerminalEvent::AgentFinished`，刚停下来等用户回答时通知
    /// `TerminalEvent::AgentBlocked`，从工作中直接变成等用户回答时两个都发。agent 的状态由宿主
    /// 判断，随 `HostMsg::Meta` 到达，`update` 里写进 `Session`。
    fn notify_agent_finished(&mut self, cx: &mut Context<Self>, update: impl FnOnce(&mut Self, &mut Context<Self>)) {
        let before = self.session.agent;
        update(self, cx);
        let after = self.session.agent;
        if before != after {
            self.agent_changed_at = Instant::now();
        }
        if before.is_some_and(Agent::is_working) && !after.is_some_and(Agent::is_working) {
            cx.emit(TerminalEvent::AgentFinished);
        }
        if !before.is_some_and(Agent::is_blocked) && after.is_some_and(Agent::is_blocked) {
            cx.emit(TerminalEvent::AgentBlocked);
        }
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
fn output_len(event: &HostEvent) -> usize {
    match event {
        HostEvent::Output(data) => data.len(),
        HostEvent::Msg(_) => 0,
    }
}

fn is_exited(event: &HostEvent) -> bool {
    matches!(event, HostEvent::Msg(message) if matches!(**message, HostMsg::Exited { .. }))
}
