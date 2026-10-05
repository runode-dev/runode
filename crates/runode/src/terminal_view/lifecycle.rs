//! 终端视图的生命周期：建视图、启动 shell、读输出、跟踪前台进程，以及光标闪烁和同步输出的计时器。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, AppContext as _, Context, Entity, Font, Point, SharedString, Task, Window, font, px};
use runode_model::{agent::Agent, color::Rgb, grid::GridSize};
use runode_term::{
    history,
    pty::PtyEvent,
    session::{SYNC_OUTPUT_TIMEOUT, Session},
};

use super::{DEFAULT_TITLE, MAX_FONT_SIZE, MIN_FONT_SIZE, TerminalEvent, TerminalView};
use crate::{config::AppConfig, prespawn::Prespawned};

/// 配置的字体都不可用时使用的等宽字体，macOS 自带。
const FALLBACK_FONT_FAMILY: &str = "Menlo";
/// 重新读取前台进程的间隔：不输出的程序（比如 `sleep`）启动后，标签名也能跟上。
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 视图建好时伪终端的临时尺寸；第一次布局时会按实际大小重设，shell 等到那之后才启动。
const PROVISIONAL_SIZE: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width_px: 8,
    cell_height_px: 16,
};
/// 建视图时最多先喂进去这么多已经到达的输出，余下的照常交给读输出的任务：shell 一启动就
/// 大量输出时，第一帧不能等它们全部处理完。
const EARLY_OUTPUT_LIMIT: usize = 64 * 1024;
/// 一次最多合并这么多排队的输出再交给 VT：积压很多时不必为它们另拼一整块大缓冲。
const MAX_OUTPUT_BATCH: usize = 1024 * 1024;
/// 有输出时重读前台进程的最短间隔：大量输出时不必每批都做几次系统调用，推迟的那次到点补上。
const FOREGROUND_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
/// 光标闪烁时亮、灭各持续的时长。
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

impl TerminalView {
    /// 建好视图，在 `cwd` 下启动 shell，`cwd` 为 `None` 时从家目录开始；shell 等第一次布局后
    /// 才启动，见 `start`。伪终端开不了时返回错误，不建视图。
    pub fn spawn(
        cwd: Option<&std::path::Path>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let view = Self::unstarted(cwd, window, cx)?;
        view.update(cx, |view, cx| view.start(cx));
        Ok(view)
    }

    /// 建好视图但先不启动 shell，等 `start` 时再在 `cwd` 下启动。恢复布局时看不见的终端用它，
    /// 不切过去就不占进程。
    pub fn unstarted(
        cwd: Option<&std::path::Path>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let (session, rx) = Session::unstarted(PROVISIONAL_SIZE, cwd)?;
        Ok(cx.new(|cx| Self::new(session, rx, window, cx)))
    }

    pub fn started(&self) -> bool {
        self.session.started()
    }

    /// 启动 `unstarted` 建的视图的 shell：等下一次布局量出实际尺寸再启动，见 `start_pending`。
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if !self.session.started() {
            self.start_pending = true;
            cx.notify();
        }
    }

    /// 按已经设好的实际尺寸启动 shell；启动不了时按 shell 已退出处理，关掉这个终端。
    pub(super) fn start_now(&mut self, cx: &mut Context<Self>) {
        if self.session.started() {
            return;
        }
        if let Err(err) = self.session.start(self.config.shell_integration) {
            tracing::error!("failed to start terminal session: {err:#}");
            self.session.exited = true;
            cx.emit(TerminalEvent::Exited);
            return;
        }
        self._foreground_poll = Some(Self::poll_foreground(cx));
        if self.session.refresh_fallback_title() {
            cx.emit(TerminalEvent::TitleChanged);
        }
    }

    /// 读输出的任务：把排队的输出合并着喂给 VT，转发标题、响铃、退出等事件，再重绘。
    fn read_output(
        mut rx: impl futures::Stream<Item = PtyEvent> + Unpin + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
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
                    if output.len() < MAX_OUTPUT_BATCH {
                        next = rx.next().now_or_never().flatten();
                    }
                }
                let updated = this.update_in(cx, |view, window, cx| {
                    if !output.is_empty() {
                        let was_working = view.session.agent.is_some_and(Agent::is_working);
                        // 进出目录、启动或退出程序时通常都有输出，顺带重读前台进程。
                        let fallback_changed = view.refresh_foreground_soon(cx);
                        if view.session.feed(&output) || fallback_changed {
                            cx.emit(TerminalEvent::TitleChanged);
                        }
                        // 关掉建议时也取走，只是不记。
                        let commands = view.session.take_commands();
                        if view.config.command_suggestions {
                            commands.into_iter().for_each(history::record);
                        }
                        view.input_changed = true;
                        view.completion_output(cx);
                        if was_working && !view.session.agent.is_some_and(Agent::is_working) {
                            cx.emit(TerminalEvent::AgentFinished);
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
        })
    }

    /// 提前启动的 shell 按上次记下的尺寸启动，和这次量到的不一样（窗口大小或侧栏变了）时，
    /// 换一个按实际尺寸启动的 shell。不直接调整尺寸：那时提示符多半已经画好，shell 收到尺寸
    /// 变化会重画提示符，可能正赶上插件管理器延迟加载插件、临时切进了插件目录，画出错的路径。
    pub(super) fn respawn(&mut self, size: GridSize, window: &mut Window, cx: &mut Context<Self>) {
        let (mut session, rx) = match Session::spawn(size, None, self.config.shell_integration) {
            Ok(spawned) => spawned,
            Err(err) => {
                tracing::warn!("failed to restart the early shell at its real size: {err:#}");
                self.session.resize(size);
                return;
            }
        };
        session.apply_config(&self.config.term_settings());
        session.refresh_fallback_title();
        // 换下来的会话随之结束，它的 shell 由 `Pty` 收拾。
        self.session = session;
        self._reader = Self::read_output(rx, window, cx);
    }

    /// 定时重读前台进程，不输出的程序（比如 `sleep`）启动后标签名也能跟上。
    fn poll_foreground(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
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
        })
    }

    /// 接上启动时提前拉起的 shell（见 `prespawn`）并建好它的视图。
    pub fn adopt(shell: Prespawned, window: &mut Window, cx: &mut App) -> anyhow::Result<Entity<Self>> {
        let session = Session::with_pty(shell.size, shell.pty)?;
        Ok(cx.new(|cx| Self {
            adopted_size: Some(shell.size),
            ..Self::new(session, shell.rx, window, cx)
        }))
    }

    fn new(
        mut session: Session,
        mut rx: futures::channel::mpsc::UnboundedReceiver<PtyEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = cx.global::<AppConfig>().0.clone();
        session.apply_config(&config.term_settings());
        // 提前启动的 shell 多半已经输出了提示符：现在就喂进去，第一帧就画得出来，
        // 不用等下面读输出的任务排上主线程。已经读到的退出留给那个任务照常处理。
        let mut exited_early = None;
        let mut early = Vec::new();
        while early.len() < EARLY_OUTPUT_LIMIT
            && let Ok(event) = rx.try_recv()
        {
            match event {
                PtyEvent::Output(data) => early.extend_from_slice(&data),
                PtyEvent::Exited => exited_early = Some(PtyEvent::Exited),
            }
        }
        if !early.is_empty() {
            session.feed(&early);
        }

        let reader = Self::read_output(futures::stream::iter(exited_early).chain(rx), window, cx);

        // shell 已经在起始目录里跑起来了，不等第一次输出，新标签一出现就有名字。
        session.refresh_fallback_title();
        let config_watch = cx.observe_global_in::<AppConfig>(window, |view, window, cx| {
            view.config = cx.global::<AppConfig>().0.clone();
            view.session.apply_config(&view.config.term_settings());
            view.font = resolve_font(&view.config.font_family, window);
            view.font_size = px(view.config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
            // 字体、字号或行高调整都可能变了，单元格尺寸和字形缓存一律作废。
            view.metrics = None;
            view.glyphs.iter_mut().for_each(HashMap::clear);
            view.input_changed = true;
            cx.notify();
        });
        let appearance_watch =
            cx.observe_window_appearance(window, |_, _, cx| crate::config::follow_appearance(cx));

        let foreground_poll = session.started().then(|| Self::poll_foreground(cx));
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

        Self {
            session,
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
            _reader: reader,
            _foreground_poll: foreground_poll,
            foreground_read_at: Instant::now(),
            _foreground_refresh: None,
            _hold_timeout: None,
            _cursor_blink: None,
            _autoscroll: None,
            _config_watch: config_watch,
            _appearance_watch: appearance_watch,
            pane_focus,
            _focus_watch: focus_watch,
        }
    }

    /// shell 当前所在的目录，新建标签或分屏时沿用。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.session.cwd()
    }

    /// 程序设置的标题；没设置时为前台进程的目录名或进程名，都没有时为 `DEFAULT_TITLE`。
    pub fn title(&self) -> &str {
        self.session
            .title
            .as_deref()
            .or(self.session.fallback_title.as_deref())
            .unwrap_or(DEFAULT_TITLE)
    }

    /// 前台 agent 在标题里报告的状态；不是 agent 在前台时为 `None`。
    pub fn agent(&self) -> Option<Agent> {
        self.session.agent
    }

    /// 终端用的字体，改动栏里的代码也用它。
    pub fn font_family(&self) -> SharedString {
        self.font.family.clone()
    }

    /// 当前的默认前景色和背景色，标签栏跟着终端配色走。
    pub fn colors(&mut self) -> (Rgb, Rgb) {
        let frame = self.session.frame();
        (frame.foreground, frame.background)
    }

    /// 有输出时重读前台进程，返回程序没设置标题时用的名字或 agent 状态是否变了。离上次读
    /// 不到 `FOREGROUND_REFRESH_INTERVAL` 时推迟到间隔满了再读，那时有变化再通知外层。
    fn refresh_foreground_soon(&mut self, cx: &mut Context<Self>) -> bool {
        if self._foreground_refresh.is_some() {
            return false;
        }
        let elapsed = self.foreground_read_at.elapsed();
        if elapsed >= FOREGROUND_REFRESH_INTERVAL {
            self.foreground_read_at = Instant::now();
            return self.session.refresh_fallback_title();
        }
        let timer = cx.background_executor().timer(FOREGROUND_REFRESH_INTERVAL - elapsed);
        self._foreground_refresh = Some(cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |view, cx| {
                view._foreground_refresh = None;
                view.foreground_read_at = Instant::now();
                let was_working = view.session.agent.is_some_and(Agent::is_working);
                if view.session.refresh_fallback_title() {
                    cx.emit(TerminalEvent::TitleChanged);
                }
                if was_working && !view.session.agent.is_some_and(Agent::is_working) {
                    cx.emit(TerminalEvent::AgentFinished);
                }
            })
            .ok();
        }));
        false
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
            text_system
                .get_font_for_id(id)
                .is_some_and(|f| f.family.as_ref() == family.as_str())
        })
        .map_or(FALLBACK_FONT_FAMILY, String::as_str);
    tracing::debug!("terminal font: {family}");
    font(family.to_owned())
}
