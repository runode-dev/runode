//! 终端视图的生命周期：在宿主里开会话、连上它、启动 shell、处理宿主发来的输出和状态，在显示
//! 和不显示之间切换，和宿主断开后重开，以及光标闪烁和同步输出的计时器。前台进程的轮询和 agent
//! 状态的判断在宿主里，结果随 `HostMsg::Meta` 到达。
//!
//! 和视图所在主机（`HostId`）的宿主之间经那台主机的 `Link` 说话：连上会话时宿主给一份当时的屏幕（同一个构建时是
//! 快照，否则是 VT 重放），之后的输出和标记按先后到达；事件按界面这份 VT 现在的样子怎么处理见
//! `ScreenState`。响铃只认宿主的 `HostMsg::Bell`，不看界面这份 VT 的 `on_bell`：两边认的是同一个
//! BEL，都认会响两次；宿主那份在视图只看状态时也认得到，VT 重放出来的屏幕也不会再响一遍。

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, PoisonError},
    task::Poll,
    thread,
    time::{Duration, Instant},
};

use futures::{
    StreamExt as _,
    channel::{mpsc::UnboundedReceiver, oneshot},
    task::ArcWake,
};
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Font, Global, Point, SharedString, Subscription, Task, Window,
    font, px,
};
use runode_protocol::{AttachMode, ClientMsg, Conversation, ERROR_BACKED_UP, HostMsg, SessionId};
use runode_terminal::{
    pty::{dir_label, program_label},
    session::{self, Request, SYNC_OUTPUT_TIMEOUT, Session},
};
use runode_types::{
    agent::Agent,
    color::Rgb,
    grid::GridSize,
    session::{Driver, SessionMeta},
    settings::TermSettings,
};

use super::{
    DEFAULT_TITLE, Events, MAX_FONT_SIZE, MIN_FONT_SIZE, TerminalEvent, TerminalView,
    cursor_blink::{CURSOR_BLINK_INTERVAL, CursorBlink},
    screen::{Attach, Changes, HIDE_GRACE, Reopen, SHOW_WAIT, ScreenState, reopen_plan},
};
use crate::{
    config::AppConfig,
    host_client::{self, Link as HostLink, LinkEvent, Screen, SpawnOptions, hosts::HostId},
    prespawn::Prespawned,
};

/// 配置的字体都不可用时使用的等宽字体：macOS 自带的 Menlo，别的系统上用随 app 带的那个（`assets::install_fonts`）。
const FALLBACK_FONT_FAMILY: &str = if cfg!(target_os = "macos") { "Menlo" } else { "Hack Nerd Font Mono" };
/// 视图建好时伪终端的临时尺寸；第一次布局时会按实际大小重设，shell 等到那之后才启动。窗口里没有
/// 终端可参照尺寸时，开在看不见的地方的新终端也先按它启动。
pub(crate) const PROVISIONAL_SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
/// 建视图时最多先喂进去这么多已经到达的输出，余下的照常交给读输出的任务：shell 一启动就
/// 大量输出时，第一帧不能等它们全部处理完。
const EARLY_OUTPUT_LIMIT: usize = 64 * 1024;
/// 一次最多合并这么多排队的输出再交给 VT：积压很多时不必为它们另拼一整块大缓冲。
const MAX_OUTPUT_BATCH: usize = 1024 * 1024;
/// 一次最多合并这么多件排队的事件：标记、响铃这些不带输出的事件不算进 `MAX_OUTPUT_BATCH`，积压很多
/// 时也不能一直取下去、占着主线程。
const MAX_EVENT_BATCH: usize = 4096;
/// 连上会话时最多等这么久宿主给的第一份屏幕。
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// 宿主以积压拒了 `Attach` 后隔这么久再连。
const ATTACH_RETRY: Duration = Duration::from_millis(100);
/// `run_command` 最多等 shell 出提示符这么久。
const PROMPT_TIMEOUT: Duration = Duration::from_secs(3);

/// 和主机 `host` 的宿主断开后又连上了（本机的是有个终端点了「在原目录重开」，远端的是自己重连上）：
/// 第几次重连，以及那时宿主里还活着的会话。同一台主机上断开着的视图见了，会话还在的就重新连上。
struct HostReconnected {
    generation: u64,
    host: HostId,
    alive: Rc<HashSet<SessionId>>,
}

impl Global for HostReconnected {}

/// 远端主机 `host` 断开后又连上了，那时它的宿主里还活着的会话是 `alive`。
pub fn remote_reconnected(host: HostId, alive: HashSet<SessionId>, cx: &mut App) {
    let generation = cx.try_global::<HostReconnected>().map_or(0, |reconnected| reconnected.generation) + 1;
    cx.set_global(HostReconnected { generation, host, alive: Rc::new(alive) });
}

/// 界面这边的 `Session` 要宿主做的事，经连接发给会话 `id`。
fn sender(link: HostLink, id: SessionId) -> session::Sender {
    Box::new(move |request| match request {
        Request::Input(data) => link.input(id, &data),
        Request::Resize(size) => link.send(ClientMsg::Resize { id, size }),
        Request::ClearScreen => link.send(ClientMsg::ClearScreen { id }),
    })
}

/// 按宿主给的屏幕建界面这边的 `Session`：快照直接解出来（已经套着宿主的主题）；VT 重放按宿主
/// 给的尺寸和主题（没给时用 `settings`）新建一份 VT 再喂。界面自己的那部分配置按 `settings` 套上。
fn build_session(link: &HostLink, id: SessionId, screen: Screen, settings: &TermSettings) -> anyhow::Result<Session> {
    let Screen { attached, data } = screen;
    let mut session = match attached.mode {
        AttachMode::Snapshot => Session::from_snapshot(&data, sender(link.clone(), id))?,
        AttachMode::VtReplay | AttachMode::MetaOnly => {
            let theme = attached.settings.as_ref().unwrap_or(settings);
            let mut session = Session::new(attached.size, theme, sender(link.clone(), id))?;
            session.feed(&data);
            session
        }
    };
    session.apply_config(settings);
    Ok(session)
}

/// 已经连上、拿到第一份屏幕：建好界面这份 VT，正看着。
fn live(
    link: &HostLink,
    id: SessionId,
    screen: Screen,
    settings: &TermSettings,
) -> anyhow::Result<ScreenState<Session>> {
    let (channel, meta, size) = (screen.attached.channel, screen.attached.meta.clone(), screen.attached.size);
    let session = build_session(link, id, screen, settings)?;
    Ok(ScreenState::new_live(session, channel, meta, size))
}

/// 宿主以积压拒了等第一份屏幕的 `Attach`（`ERROR_BACKED_UP`）：会话还在，不算没连成。
fn backed_up(err: &anyhow::Error) -> bool {
    err.to_string().starts_with(ERROR_BACKED_UP)
}

/// 第一次连会话 `id` 被宿主以积压拒了：重新登记、再要一次屏幕，视图先显示着等它（画背景）；又被拒
/// 的话照 `Changes::retry_attach` 过一会儿再连。
fn attach_later(link: &HostLink, id: SessionId) -> (ScreenState<Session>, UnboundedReceiver<LinkEvent>) {
    tracing::info!("the host is backed up, attaching session {id} again");
    let rx = link.attach(id, None, AttachMode::Snapshot);
    (ScreenState::new_attaching(AttachMode::Snapshot, PROVISIONAL_SIZE, Instant::now()), rx)
}

/// 连上宿主里的会话 `id`，等它给第一份屏幕，建好界面这边的 VT，返回它和收之后的事件的一端。
/// 连不上时结束这个会话（用于刚开的会话）；宿主积压着时不结束，见 `attach_later`。
fn connect(
    link: &HostLink,
    id: SessionId,
    settings: &TermSettings,
) -> anyhow::Result<(ScreenState<Session>, UnboundedReceiver<LinkEvent>)> {
    let connected = match link.attach_now(id, None, AttachMode::Snapshot, ATTACH_TIMEOUT) {
        Err(err) if backed_up(&err) => return Ok(attach_later(link, id)),
        attached => attached.and_then(|(screen, rx)| Ok((live(link, id, screen, settings)?, rx))),
    };
    if connected.is_err() {
        link.kill(id);
    }
    connected
}

/// 远端主机上在后台开着的会话：开好以后要不要启动 shell、按哪个 `Attach` 连上（等的时候视图的
/// 显示变了就换成新的）。
/// 挂在终端所在窗口上的订阅；终端搬进别的窗口时换成那个窗口的（`TerminalView::attach_window`）。
pub(super) struct WindowWatches {
    /// 窗口重新激活时恢复空闲停下的光标闪烁。
    _activation: Subscription,
    _appearance: Subscription,
    /// 盯着 `HostReconnected`：自己所在主机的宿主断开后又连上了。
    _reconnect: Subscription,
    _focus: [Subscription; 4],
}

impl WindowWatches {
    fn new(
        focus_handle: &FocusHandle,
        pane_focus: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<TerminalView>,
    ) -> Self {
        // 切回这个窗口也算一次活动：焦点一直在这个终端上时不会触发 `on_focus`。
        let activation = cx.observe_window_activation(window, |view, window, cx| {
            if window.is_window_active() {
                view.reset_cursor_blink(cx);
            }
        });
        let appearance = cx.observe_window_appearance(window, |_, _, cx| crate::config::follow_appearance(cx));
        let reconnect =
            cx.observe_global_in::<HostReconnected>(window, |view, window, cx| view.host_reconnected(window, cx));
        let focus = [
            cx.on_focus_in(pane_focus, window, |view, _, cx| {
                view.report_focus(true);
                cx.emit(TerminalEvent::Focused);
            }),
            cx.on_focus_out(pane_focus, window, |view, _, _, _| view.report_focus(false)),
            cx.on_focus(focus_handle, window, |view, _, cx| {
                view.reset_cursor_blink(cx);
            }),
            cx.on_blur(focus_handle, window, |view, _, _| {
                view.cursor_blink.pause();
            }),
        ];
        Self { _activation: activation, _appearance: appearance, _reconnect: reconnect, _focus: focus }
    }
}

pub(super) struct Opening {
    start: bool,
    attach: Attach,
    /// 等着会话开好的，见 `TerminalView::opened`。
    waiting: Vec<oneshot::Sender<Option<SessionId>>>,
    /// 开会话的后台线程和 `TerminalView::end` 在这里碰头，谁后到谁结束会话：视图结束后可能马上被丢掉，
    /// 等不到回话，开出来的会话没人结束。
    claim: Arc<Mutex<Claim>>,
    _task: Task<()>,
}

#[derive(Default)]
struct Claim {
    ended: bool,
    opened: Option<SessionId>,
}

/// 唤醒时叫醒等着的线程，主线程上等宿主给屏幕时用，见 `TerminalView::wait_for_screen`。
struct Unpark(thread::Thread);

impl ArcWake for Unpark {
    fn wake_by_ref(arc: &Arc<Self>) {
        arc.0.unpark();
    }
}

impl TerminalView {
    /// 终端搬进了窗口 `window`（单独的终端窗口，或者从那里放回主窗口）：焦点、窗口激活这些订阅换成这个窗口的。
    pub(crate) fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._window_watches = WindowWatches::new(&self.focus_handle, &self.pane_focus, window, cx);
    }

    /// 建好视图，在主机 `host` 的 `cwd` 下启动 `shell`（为空时宿主的默认 shell），`cwd` 为 `None` 时从家目录
    /// 开始；shell 等第一次布局后才启动，见 `start`。伪终端开不了时返回错误，不建视图。
    pub fn spawn(
        host: &HostId,
        cwd: Option<&Path>,
        fallback: Option<&Path>,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let view = Self::unstarted(host, cwd, fallback, None, shell, window, cx)?;
        view.update(cx, |view, cx| view.start(cx));
        Ok(view)
    }

    /// 建好视图但先不启动 shell，等 `start` 时再在 `cwd` 下启动。恢复布局时显示出来的终端用它，
    /// 等第一次布局量出尺寸再启动；看不见的连会话都不开，见 `deferred`。`cwd` 不是目录时从 `fallback` 开。
    /// `resume` 是出了提示符后接回的 agent 对话。远端主机上的会话不在主线程上等宿主回话，先建成
    /// `deferred` 的视图，显示出来时在后台开，见 `open_session`。
    pub fn unstarted(
        host: &HostId,
        cwd: Option<&Path>,
        fallback: Option<&Path>,
        resume: Option<Conversation>,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        if !host.is_local() {
            return Ok(Self::deferred(host, cwd, fallback, resume, shell, window, cx));
        }
        let config = cx.global::<AppConfig>().0.clone();
        let link = host_client::hosts::link_of(host);
        let id = link.spawn(SpawnOptions {
            size: PROVISIONAL_SIZE,
            cwd: cwd.map(Into::into),
            fallback_cwd: fallback.map(Into::into),
            integration: config.shell_integration,
            start: false,
            shell: shell.clone(),
            settings: None,
            resume: resume.clone(),
        })?;
        let (screen, rx) = connect(&link, id, &config.term_settings())?;
        Ok(cx.new(|cx| {
            let mut view = Self::new(host, Some(id), screen, false, rx, window, cx);
            view.start_resume = resume;
            view.start_shell = shell;
            view
        }))
    }

    /// 建好视图但还不在宿主里开会话：不开伪终端、不起会话线程、不建 VT，回到显示
    /// （`request_visible`）或者要启动（`start_at`）时再在 `cwd` 下开，为 `None` 时从家目录开始。
    /// 恢复布局时看不见、又从没启动过的终端用它，恢复很多标签时只有显示出来的那几个马上开。
    /// 开之前标题和目录按起始目录给，和宿主给还没启动的会话的一样。
    pub fn deferred(
        host: &HostId,
        cwd: Option<&Path>,
        fallback: Option<&Path>,
        resume: Option<Conversation>,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let home = || if host.is_local() { runode_paths::Dirs::from_env().home } else { None };
        let on_host =
            |path: &Path| if host.is_local() { path.to_path_buf() } else { crate::project_host::remote_path(path) };
        let dir = cwd.map(on_host).or_else(home);
        let meta = SessionMeta { fallback_title: dir.as_deref().map(dir_label), cwd: dir, ..SessionMeta::default() };
        let screen = ScreenState::new_unopened(meta, PROVISIONAL_SIZE, Instant::now());
        // 还没有会话也就没有事件：发的一端直接丢掉，读事件的任务一开始就结束，开了会话再起。
        let (_, rx) = futures::channel::mpsc::unbounded();
        let start_dir = cwd.map(on_host);
        let start_fallback = fallback.map(on_host);
        cx.new(|cx| {
            let mut view = Self::new(host, None, screen, false, rx, window, cx);
            view.start_dir = start_dir;
            view.start_fallback = start_fallback;
            view.start_resume = resume;
            view.start_shell = shell;
            view
        })
    }

    /// 给 `deferred` 建的视图在宿主里开会话（`start` 时连 shell 一起启动），按 `attach` 连上它，之后
    /// 的事件由调用方起读事件的任务（或者等屏幕）来收。开不了时当作和宿主断开了：断开交给那个任务
    /// 处理，视图显示断开的提示，点「在原目录重开」再开。远端主机上在后台等宿主回话（`opening`），开好
    /// 以后按那时最近一次要的 `Attach` 连上、自己起读事件的任务。
    fn open_session(&mut self, start: bool, attach: Attach, window: &mut Window, cx: &mut Context<Self>) {
        // 已经结束了的（所在的 workspace 关掉了）不再开：开出来的会话关标签时没人结束它。
        if self.ended {
            return;
        }
        if let Some(opening) = &mut self.opening {
            opening.start |= start;
            opening.attach = attach;
            return;
        }
        let options = SpawnOptions {
            size: self.screen.last_size(),
            cwd: self.start_dir.clone(),
            fallback_cwd: self.start_fallback.clone(),
            integration: self.config.shell_integration,
            start,
            shell: self.start_shell.clone(),
            settings: None,
            resume: self.start_resume.clone(),
        };
        let link = self.host_link.clone();
        if self.host.is_local() {
            let spawned = link.spawn(options);
            self.session_opened(spawned, start, attach);
            return;
        }
        let claim = Arc::new(Mutex::new(Claim::default()));
        let opened = claim.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let spawned = cx
                .background_executor()
                .spawn(async move {
                    let spawned = link.spawn(options);
                    if let Ok(id) = &spawned {
                        let mut opened = opened.lock().unwrap_or_else(PoisonError::into_inner);
                        if opened.ended {
                            link.kill(*id);
                        } else {
                            opened.opened = Some(*id);
                        }
                    }
                    spawned
                })
                .await;
            this.update_in(cx, |view, window, cx| {
                let Some(opening) = view.opening.take() else {
                    return;
                };
                view.session_opened(spawned, start, opening.attach);
                for waiting in opening.waiting {
                    let _ = waiting.send(view.id);
                }
                if opening.start
                    && let Some(id) = view.id.filter(|_| !start)
                {
                    view.started = true;
                    view.host_link.send(ClientMsg::Start { id, integration: view.config.shell_integration });
                }
                view._reader = Self::read_events(view.events.clone(), window, cx);
                cx.notify();
            })
            .ok();
        });
        self.opening = Some(Opening { start, attach, waiting: Vec::new(), claim, _task: task });
    }

    /// `open_session` 等到了宿主的回话。
    fn session_opened(&mut self, spawned: anyhow::Result<SessionId>, start: bool, attach: Attach) {
        match spawned {
            // `end` 经 `Opening::claim` 结束了它。
            Ok(_) if self.ended => {}
            Ok(id) => {
                tracing::debug!("session {id} opened for a terminal that was put off");
                self.id = Some(id);
                self.started |= start;
                self.events = Rc::new(RefCell::new(self.host_link.attach(id, attach.size, attach.mode)));
            }
            Err(err) => {
                tracing::warn!("failed to open the session of a terminal that was put off: {err:#}");
                self.pending_end = Some(LinkEvent::Lost);
            }
        }
    }

    /// 远端主机还没连上时恢复存档里的终端：先按断开显示，主机连上后会话 `id` 还在就接回，不在了就在
    /// `cwd`（为空时 `fallback`）新开、接回 `resume` 这个 agent 对话，见 `host_reconnected`。
    #[allow(clippy::too_many_arguments)]
    pub fn awaiting(
        host: &HostId,
        id: Option<SessionId>,
        cwd: Option<&Path>,
        fallback: Option<&Path>,
        resume: Option<Conversation>,
        shell: Option<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let dir = cwd.or(fallback).map(Path::to_path_buf);
        let meta =
            SessionMeta { fallback_title: dir.as_deref().map(dir_label), cwd: dir.clone(), ..SessionMeta::default() };
        let screen = ScreenState::new_lost(meta, PROVISIONAL_SIZE, Instant::now());
        let (_, rx) = futures::channel::mpsc::unbounded();
        let view = cx.new(|cx| {
            let mut view = Self::new(host, id, screen, true, rx, window, cx);
            view.start_dir = dir;
            view.start_fallback = fallback.map(Path::to_path_buf);
            view.start_resume = resume;
            view.start_shell = shell;
            view
        });
        // 主机已经连上了的话等不来 `host_reconnected`，现在就接。
        if view.read(cx).host_link.connected() {
            view.update(cx, |view, cx| view.reopen(window, cx));
        }
        view
    }

    /// 用宿主里已有的会话 `id` 建视图（存档恢复、接上后台会话）。`cwd` 是调用方记着的目录，宿主
    /// 还没报告目录时当作它的目录。`visible` 为假时视图只看状态（`AttachMode::MetaOnly`），等
    /// `request_visible` 再看屏幕。会话已经启动过，不再 `start`。连不上时返回错误，不结束会话；宿主
    /// 积压着时不算连不上，见 `attach_later`。`shell` 是建这个会话时挑的，会话没了在原目录重开时用。
    pub fn reattach(
        host: &HostId,
        id: SessionId,
        cwd: Option<&Path>,
        shell: Option<String>,
        visible: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> anyhow::Result<Entity<Self>> {
        let config = cx.global::<AppConfig>().0.clone();
        let link = host_client::hosts::link_of(host);
        if !link.connected() {
            anyhow::bail!(rust_i18n::t!("host_error.not_connected").into_owned());
        }
        let (screen, rx) = if visible && !host.is_local() {
            // 远端的不在主线程上等屏幕，先画背景，屏幕到了由读事件的任务补上。
            let rx = link.attach(id, None, AttachMode::Snapshot);
            (ScreenState::new_attaching(AttachMode::Snapshot, PROVISIONAL_SIZE, Instant::now()), rx)
        } else if visible {
            match link.attach_now(id, None, AttachMode::Snapshot, ATTACH_TIMEOUT) {
                Err(err) if backed_up(&err) => attach_later(&link, id),
                attached => {
                    let (screen, rx) = attached?;
                    match live(&link, id, screen, &config.term_settings()) {
                        Ok(screen) => (screen, rx),
                        Err(err) => {
                            // 不留着订阅：不然宿主一直当它有窗口在看，进不了后台会话。
                            link.detach(id);
                            return Err(err);
                        }
                    }
                }
            }
        } else {
            let rx = link.attach(id, None, AttachMode::MetaOnly);
            (ScreenState::new_attaching(AttachMode::MetaOnly, PROVISIONAL_SIZE, Instant::now()), rx)
        };
        let start_dir = cwd.map(Path::to_path_buf);
        Ok(cx.new(|cx| {
            let mut view = Self::new(host, Some(id), screen, true, rx, window, cx);
            view.start_dir = start_dir;
            view.start_shell = shell;
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

    /// 告诉宿主这个会话在这边是不是当前看着的（焦点进出这个终端，包括它的搜索栏）。获得焦点也算一次
    /// 交互：几个前端看同一个会话时，宿主让最近交互的那个决定尺寸，见 `ScreenState::size_owner`。
    /// 宿主里还没开会话时没什么可报的。
    pub(super) fn report_focus(&self, focused: bool) {
        let Some(id) = self.id else {
            return;
        };
        self.host_link.send(ClientMsg::Focus { id, focused });
    }

    /// 按已经设好的实际尺寸启动 shell。启动不了时宿主发 `HostMsg::Exited`，按 shell 已退出
    /// 处理，关掉这个终端。还没开会话（`deferred`）时什么都不做，开了以后照常启动。
    pub(super) fn start_now(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        self.host_link.send(ClientMsg::Start { id, integration: self.config.shell_integration });
    }

    /// 收宿主发来的事件的任务：把排队的事件合并成一批处理，再重绘。收事件的一端在视图里，
    /// 只在取事件时借用：回到显示时主线程要直接从它取（见 `wait_for_screen`）。取满一批（见
    /// `MAX_OUTPUT_BATCH`、`MAX_EVENT_BATCH`）时处理完先让出主线程（`yield_now`），积压很多时也能
    /// 在批与批之间画帧、处理输入。
    fn read_events(events: Events, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            // 建视图时已经到了的结束（见 `pending_end`）先处理：任务排上主线程时外层已经订阅好了。
            let ended = this.update_in(cx, |view, window, cx| view.handle_pending_end(window, cx));
            if !matches!(ended, Ok(false)) {
                return;
            }
            loop {
                let next = futures::future::poll_fn(|task| events.borrow_mut().poll_next_unpin(task)).await;
                let Some(first) = next else { break };
                // 把已排队的输出合并成一次 VT 写入和一次重绘。
                let mut bytes = output_len(&first);
                let mut batch = vec![first];
                while bytes < MAX_OUTPUT_BATCH
                    && batch.len() < MAX_EVENT_BATCH
                    && let Ok(event) = events.borrow_mut().try_recv()
                {
                    bytes += output_len(&event);
                    batch.push(event);
                }
                let full = bytes >= MAX_OUTPUT_BATCH || batch.len() >= MAX_EVENT_BATCH;
                let ended = batch.iter().any(ends);
                let updated = this.update_in(cx, |view, window, cx| {
                    view.handle_link_events(batch, window, cx);
                    view.ring_bell(cx);
                });
                if updated.is_err() || ended {
                    break;
                }
                // 取满了一批，后面多半还排着：不让的话下一轮取事件马上就绪，整片积压仍在这一次里处理完。
                if full {
                    yield_now().await;
                }
            }
        })
    }

    /// 按先后处理宿主发来的一批事件（见 `ScreenState::apply`），再把变化转给外层：标题、agent、
    /// 退出等事件，要重新连上的就连，然后重绘。响铃留给调用方用 `ring_bell` 转发，
    /// 见 `new`；这一批里有退出时在退出之前转发。
    fn handle_link_events(&mut self, events: Vec<LinkEvent>, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.config.term_settings();
        let link = self.host_link.clone();
        let mut build = |screen: Screen| build_session(&link, screen.attached.id, screen, &settings);
        let changes = self.screen.apply(events, &mut build, Instant::now());
        self.apply_changes(changes, window, cx);
        self.flush_pending_command(false);
    }

    /// 等 shell 出了提示符（有 shell 集成时 `SessionMeta::prompt_cwd` 有了）把 `command` 打进去回车；
    /// 等了 `PROMPT_TIMEOUT` 还没等到时照样打，和 `runode open -- 命令` 一样。
    pub fn run_command(&mut self, command: String, cx: &mut Context<Self>) {
        self.pending_command = Some(command);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROMPT_TIMEOUT).await;
            this.update(cx, |view, _| view.flush_pending_command(true)).ok();
        })
        .detach();
    }

    fn flush_pending_command(&mut self, force: bool) {
        if self.pending_command.is_none() || !(force || self.screen.meta().prompt_cwd.is_some()) {
            return;
        }
        let (Some(id), Some(command)) = (self.id, self.pending_command.take()) else {
            return;
        };
        self.host_link.input(id, format!("{command}\r").as_bytes());
    }

    fn apply_changes(&mut self, changes: Changes, window: &mut Window, cx: &mut Context<Self>) {
        if changes.replaced {
            tracing::debug!("session {}: new screen, channel {:?}", self.session_text(), self.screen.channel());
            self.vt_replaced(cx);
        }
        if changes.fed {
            // 有输出（包括键入的回显）时光标先亮起，免得打字时看不到它。
            self.reset_cursor_blink(cx);
        }
        if changes.title_changed {
            cx.emit(TerminalEvent::TitleChanged);
        }
        if changes.meta_changed {
            // 会话报了对话就用它的；换了别家 agent、或者 agent 退出了（续接取消、失败）时要接回的那场作废。
            let meta = self.screen.meta();
            let other_agent =
                meta.agent.zip(self.start_resume.as_ref()).is_some_and(|(agent, resume)| agent.kind != resume.kind);
            if meta.conversation.is_some() || other_agent || (changes.agent_changed && meta.agent.is_none()) {
                self.start_resume = None;
            }
            cx.emit(TerminalEvent::MetaChanged);
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
        if changes.bell {
            self.bell_pending = true;
        }
        for (title, body) in changes.notifications {
            cx.emit(TerminalEvent::Notify { title, body });
        }
        if let Some((rows, columns)) = changes.resize_window {
            cx.emit(TerminalEvent::ResizeWindow { rows, columns });
        }
        if let Some(attach) = changes.attach {
            tracing::info!("session {} attaches again ({:?})", self.session_text(), attach.mode);
            self.send_attach(attach);
        }
        if changes.retry_attach {
            // 等宿主把这条连接上积压的写出去。不留着 `Task`：到点时已经连上、
            // 断开、退出或者另发了 `Attach` 的话 `retry_attach` 为 `None`，什么也不发。
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ATTACH_RETRY).await;
                this.update(cx, |view, _| {
                    if let Some(attach) = view.screen.retry_attach() {
                        view.send_attach(attach);
                    }
                })
                .ok();
            })
            .detach();
        }
        if changes.lost {
            tracing::warn!("session {} lost its host; its last screen stays", self.session_text());
        }
        if changes.exited {
            // 退出前响过的铃先通知：外层收到 `Exited` 就关掉分屏，之后再通知就找不到这个终端了。
            // 推迟到这一轮的副作用处理时再发：刚建好的视图（比如建好就切到显示、在主线程上等屏幕时
            // 收到了退出）外层的订阅要到那时才生效。
            cx.defer_in(window, |view, _, cx| {
                view.ring_bell(cx);
                cx.emit(TerminalEvent::Exited);
            });
        }
        if self.screen.live().is_some_and(Session::render_held) {
            self.schedule_hold_timeout(cx);
        }
        cx.notify();
    }

    /// 经连接重新连上会话（沿用原来的登记和收事件的一端）。
    fn send_attach(&mut self, attach: Attach) {
        let Some(id) = self.id else {
            if let Some(opening) = &mut self.opening {
                opening.attach = attach;
            }
            return;
        };
        if !self.host_link.reattach(id, attach.size, attach.mode) {
            // 连接已经断了：断开的消息已经排在收事件的一端里，处理到时按断开显示。
            tracing::debug!("session {id} cannot attach again: not connected");
        }
    }

    /// 日志里写的会话：还没开（`deferred`）时写明。
    fn session_text(&self) -> String {
        self.id.map_or_else(|| "(not opened yet)".to_owned(), |id| id.to_string())
    }

    /// 界面这份 VT 换了或者没了：跟着旧 VT 的选区作废，搜索栏开着的话在新的上面
    /// 重新搜。
    fn vt_replaced(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        self.reporting_press = false;
        self.click_cell = None;
        self._autoscroll = None;
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
    /// 离开显示 `HIDE_GRACE` 后（刷屏的更早）只看状态、丢掉界面这份 VT；回到显示时要是已经丢了，
    /// 按视图的尺寸发出要屏幕的 `Attach` 并返回 true，接着用 `wait_for_screen` 等（最多 `SHOW_WAIT`，等不到先画
    /// 背景，到了再补上）。一次显示好几个终端时先都发出去再一起等（见 `WindowView::sync_visibility`），
    /// 不必一个等完再要下一个。
    pub fn request_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let attach = self.screen.set_visible(visible, Instant::now());
        if visible {
            self._hide_timer = None;
            if let Some(attach) = attach {
                match self.id {
                    Some(_) => self.send_attach(attach),
                    // `deferred` 建的视图第一次显示：这时才开会话。
                    None => self.open_session(false, attach, window, cx),
                }
                cx.notify();
                return true;
            }
            return false;
        }
        if self._hide_timer.is_none() && !self.screen.visible() {
            self._hide_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(HIDE_GRACE).await;
                this.update(cx, |view, cx| view.hide_if_due(cx)).ok();
            }));
        }
        false
    }

    /// 视图显示着，见 `request_visible`。
    pub fn visible(&self) -> bool {
        self.screen.visible()
    }

    /// 界面这份 VT 上有丢了要不回来的状态：往回翻着、选着字、开着搜索。宿主给的屏幕停在最底下、
    /// 没有选区，丢掉 VT 再要回来时这些都没了。
    pub fn holds_view_state(&self) -> bool {
        self.search_field.is_some()
            || self.screen.shown().is_some_and(|session| !session.viewport_at_bottom() || session.has_selection())
    }

    /// 不显示满了宽限：丢掉界面这份 VT，改成只看状态。
    fn hide_if_due(&mut self, cx: &mut Context<Self>) {
        self._hide_timer = None;
        if let Some(attach) = self.screen.tick(Instant::now()) {
            tracing::debug!("session {} hidden, dropping its screen", self.session_text());
            self.send_attach(attach);
            self.vt_replaced(cx);
            cx.notify();
        }
    }

    /// 在主线程上等宿主给屏幕，最多等到 `deadline`，期间到的事件照常处理。等的时候直接从收事件
    /// 的一端取，用的是自己的唤醒方，所以之后要重新起读事件的任务。
    pub fn wait_for_screen(&mut self, deadline: Instant, window: &mut Window, cx: &mut Context<Self>) {
        // 远端主机上还在后台开会话：开好了自己起读事件的任务。
        if self.opening.is_some() {
            return;
        }
        let started = Instant::now();
        // 建视图时已经到了的结束先处理：屏幕不会再来了，不用等。
        let ended = self.handle_pending_end(window, cx);
        let waker = futures::task::waker(Arc::new(Unpark(thread::current())));
        let mut task = std::task::Context::from_waker(&waker);
        while !ended && self.screen.is_attaching() {
            let (events, closed) = take_batch(&mut self.events.borrow_mut(), &mut task);
            let got = !events.is_empty();
            if got {
                self.handle_link_events(events, window, cx);
                self.ring_bell(cx);
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
            tracing::debug!(
                "session {}: no screen after {:?}, drawing the background first",
                self.session_text(),
                started.elapsed()
            );
        } else {
            tracing::debug!("session {}: screen shown after {:?}", self.session_text(), started.elapsed());
        }
        self._reader = Self::read_events(self.events.clone(), window, cx);
        cx.notify();
    }

    /// 处理建视图时就已经到了的那件「之后不再有事件」的事（退出、断开），见 `pending_end`；返回
    /// 有没有。
    fn handle_pending_end(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(end) = self.pending_end.take() else {
            return false;
        };
        self.handle_link_events(vec![end], window, cx);
        self.ring_bell(cx);
        true
    }

    /// 和宿主断开后点了「在原目录重开」：先重新连上宿主；这个终端的会话还在就接着用它，不在了就
    /// 在原来的目录开一个新的换上。别的断开着的终端里会话还在的随之重新连上，见 `HostReconnected`。
    pub(super) fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screen.is_lost() {
            return;
        }
        if !self.host.is_local() {
            self.reopen_remote(window, cx);
            return;
        }
        if let Err(err) = host_client::reconnect() {
            tracing::warn!("failed to reconnect to the host: {err:#}");
            return;
        }
        // 重连可能把宿主从单独的进程换到了 app 里，远程访问的监听要跟着在 app 里开起来。
        let config = cx.global::<AppConfig>().0.clone();
        host_client::configure(&config);
        let alive: Option<HashSet<SessionId>> = match host_client::list_sessions() {
            Ok(sessions) => {
                Some(sessions.into_iter().filter(|session| !session.exited).map(|session| session.id).collect())
            }
            Err(err) => {
                tracing::warn!("failed to list the host's sessions after reconnecting: {err:#}");
                None
            }
        };
        // 还没开过会话（`deferred` 的视图开的时候就断开了）时没有可接着用的，开一个新的。
        match self.id.map_or(Reopen::Replace, |id| reopen_plan(id, alive.as_ref())) {
            Reopen::Resume => self.resume(window, cx),
            Reopen::Replace => {
                let (size, cwd) = (self.screen.last_size(), self.cwd());
                if let Err(err) = self.replace_session(size, cwd, window, cx) {
                    tracing::warn!("failed to reopen the terminal: {err:#}");
                }
            }
        }
        // 列不出会话时不知道别的终端的会话还在不在，不替它们决定，各自点重开时再看。
        if let Some(alive) = alive {
            let generation = cx.try_global::<HostReconnected>().map_or(0, |reconnected| reconnected.generation) + 1;
            cx.set_global(HostReconnected { generation, host: HostId::Local, alive: Rc::new(alive) });
        }
    }

    /// 远端主机上的「在原目录重开」：没连着时马上重连，连上后会话还在的经 `remote_reconnected` 接回；
    /// 连着、会话却不在了时在原来的目录开一个新的。
    fn reopen_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.host_link.connected() {
            host_client::hosts::retry_now(&self.host);
            return;
        }
        let host = self.host.clone();
        let link = self.host_link.clone();
        let id = self.id;
        cx.spawn_in(window, async move |this, cx| {
            let sessions =
                cx.background_executor().spawn(async move { link.list_sessions(Duration::from_secs(10)) }).await;
            let alive: Option<HashSet<SessionId>> = sessions
                .inspect_err(|err| tracing::warn!("failed to list the sessions on {host:?}: {err:#}"))
                .ok()
                .map(|sessions| {
                    sessions.into_iter().filter(|session| !session.exited).map(|session| session.id).collect()
                });
            this.update_in(cx, |view, window, cx| {
                match id.map_or(Reopen::Replace, |id| reopen_plan(id, alive.as_ref())) {
                    Reopen::Resume => view.resume(window, cx),
                    Reopen::Replace => view.replace_remote(window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// 远端主机上换一个新会话：结束现在的，按 `deferred` 那样在原来的目录后台开一个已经启动的。
    fn replace_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.screen.is_lost() {
            return;
        }
        let (size, cwd, visible) = (self.screen.last_size(), self.cwd(), self.screen.visible());
        if let Some(old) = self.id.take() {
            self.host_link.kill(old);
        }
        self.start_resume = self.conversation();
        let meta =
            SessionMeta { fallback_title: cwd.as_deref().map(dir_label), cwd: cwd.clone(), ..SessionMeta::default() };
        self.screen = ScreenState::new_unopened(meta, size, Instant::now());
        self.start_dir = cwd;
        self.pending_end = None;
        self.vt_replaced(cx);
        let attach = if visible { self.screen.set_visible(true, Instant::now()) } else { None };
        self.open_session(true, attach.unwrap_or(Attach { size: None, mode: AttachMode::MetaOnly }), window, cx);
        cx.emit(TerminalEvent::TitleChanged);
        cx.notify();
    }

    /// 自己所在的主机重新连上了：自己也断开着、会话还在的话重新连上。
    fn host_reconnected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reconnected = cx.global::<HostReconnected>();
        if !self.screen.is_lost() || reconnected.host != self.host {
            return;
        }
        let alive = reconnected.alive.clone();
        if self.id.is_some_and(|id| alive.contains(&id)) {
            self.resume(window, cx);
        } else if !self.host.is_local() {
            // 远端主机重启过之类，会话没了：不等用户点，在原来的目录新开。
            self.replace_remote(window, cx);
        }
    }

    /// 断开后宿主又连上了、会话还在：重新登记、连上它。显示着的冻结着最后一屏等新的屏幕。
    fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.id else {
            return;
        };
        let Some(attach) = self.screen.reconnect(Instant::now()) else {
            return;
        };
        tracing::info!("session {id} attaches again after reconnecting");
        let rx = self.host_link.attach(id, attach.size, attach.mode);
        self.events = Rc::new(RefCell::new(rx));
        if attach.mode == AttachMode::MetaOnly || !self.host.is_local() {
            self._reader = Self::read_events(self.events.clone(), window, cx);
        } else {
            self.wait_for_screen(Instant::now() + SHOW_WAIT, window, cx);
        }
        cx.notify();
    }

    /// 在 `cwd`（为 `None` 时家目录，不是目录时 `start_fallback`）按 `size` 开一个已经启动的新会话、连上它，
    /// 换掉现在的；换下来的会话随之结束。
    fn replace_session(
        &mut self,
        size: GridSize,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let link = self.host_link.clone();
        let cwd = if self.host.is_local() { cwd } else { cwd.as_deref().map(crate::project_host::remote_path) };
        let id = link.spawn(SpawnOptions {
            size,
            cwd,
            fallback_cwd: self.start_fallback.clone(),
            integration: self.config.shell_integration,
            start: true,
            shell: self.start_shell.clone(),
            settings: None,
            resume: None,
        })?;
        let (mut screen, rx) = connect(&link, id, &self.config.term_settings())?;
        if let Some(old) = self.id.replace(id) {
            link.kill(old);
        }
        screen.resize(size);
        self.screen = screen;
        self.started = true;
        self.events = Rc::new(RefCell::new(rx));
        // 换下来的会话的结束不再要。
        self.pending_end = None;
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
        let link = host_client::link().clone();
        let (screen, rx) = connect(&link, id, &cx.global::<AppConfig>().0.term_settings())?;
        Ok(cx.new(|cx| {
            let mut view = Self::new(&HostId::Local, Some(id), screen, true, rx, window, cx);
            view.adopted_size = Some(size);
            view
        }))
    }

    fn new(
        host: &HostId,
        id: Option<SessionId>,
        screen: ScreenState<Session>,
        started: bool,
        rx: UnboundedReceiver<LinkEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = cx.global::<AppConfig>().0.clone();

        let config_watch = cx.observe_global_in::<AppConfig>(window, |view, window, cx| {
            let old = std::mem::replace(&mut view.config, cx.global::<AppConfig>().0.clone());
            // 主题等宿主标出位置后再换，见 `HostMsg::ThemeApplied`。
            let settings = view.config.term_settings();
            match view.screen.shown_mut() {
                Some(session) => session.apply_config(&settings),
                // 没有界面这份 VT 的（后台标签）收不到宿主换主题的标记，标签栏的颜色按新配置的走，
                // 回到显示时再从屏幕读。
                None => view.colors = (settings.foreground, settings.background),
            }
            view.font = resolve_font(&view.config.font_family, window);
            // 配置的字号没变就留着用户按 ⌘+/⌘- 调出来的字号：明暗切换、改别的项都会走到这里。
            if old.font_size != view.config.font_size {
                view.font_size = px(view.config.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
            }
            // 字体、字号或行高调整都可能变了，单元格尺寸和字形缓存一律作废。
            view.metrics = None;
            view.glyphs.iter_mut().for_each(HashMap::clear);
            // 闪不闪、空闲期限都可能改了，空闲停下的闪烁按新配置重新算。
            view.reset_cursor_blink(cx);
            cx.notify();
        });
        let focus_handle = cx.focus_handle();
        let pane_focus = cx.focus_handle();
        let window_watches = WindowWatches::new(&focus_handle, &pane_focus, window, cx);

        // 关掉的终端不会再画下一帧，画过的图片在这里交还贴图。
        cx.on_release(|view: &mut Self, cx| {
            for (_, image) in view.images.drain() {
                crate::ui::drop_render_image(image, cx);
            }
        })
        .detach();

        let settings = config.term_settings();
        let mut view = Self {
            screen,
            events: Rc::new(RefCell::new(rx)),
            _hide_timer: None,
            pending_end: None,
            colors: (settings.foreground, settings.background),
            start_dir: None,
            start_fallback: None,
            start_resume: None,
            start_shell: None,
            host: host.clone(),
            host_link: host_client::hosts::link_of(host),
            opening: None,
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
            images: HashMap::new(),
            marked_text: None,
            scroll_remainder: 0.,
            cursor_bounds: None,
            grid_origin: Point::default(),
            crop: None,
            selecting: false,
            reporting_press: false,
            hovered_link: None,
            link_lookup: None,
            click_cell: None,
            cursor_blink: CursorBlink::new(),
            adopted_size: None,
            start_pending: false,
            pending_command: None,
            task: None,
            search_field: None,
            menu: None,
            _reader: Task::ready(()),
            agent_changed_at: Instant::now(),
            _hold_timeout: None,
            _image_frame: None,
            _autoscroll: None,
            _config_watch: config_watch,
            dictation: None,
            pane_focus,
            _window_watches: window_watches,
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
        view.pending_end = if ended_early { early.pop() } else { None };
        if !early.is_empty() {
            view.handle_link_events(early, window, cx);
            // 补发的输出里响过铃的话，这时通知外层会丢：订阅要等这一轮的副作用处理到时才生效，
            // 排在它前面发出的事件没人收。推迟到外层订阅好以后再通知。
            cx.defer_in(window, |view, _, cx| view.ring_bell(cx));
        }
        view._reader = Self::read_events(view.events.clone(), window, cx);
        view
    }

    /// 会话开好以后交回它，开不成时交回空；已经开好或者没在后台开时当场交回。
    pub fn opened(&mut self) -> oneshot::Receiver<Option<SessionId>> {
        let (tx, rx) = oneshot::channel();
        match &mut self.opening {
            Some(opening) => opening.waiting.push(tx),
            None => {
                let _ = tx.send(self.id);
            }
        }
        rx
    }

    /// 本机上的会话的 shell 的进程号；远端主机上的进程号在那台机器上，这里为空。
    pub fn local_pid(&self) -> Option<u32> {
        self.screen.meta().pid.filter(|_| self.host.is_local())
    }

    /// 会话所在的主机。
    pub fn host(&self) -> &HostId {
        &self.host
    }

    /// 和宿主断开了，画面停在最后一屏。
    pub fn lost(&self) -> bool {
        self.screen.is_lost()
    }

    /// 宿主里这个终端的会话；`deferred` 建的视图还没开会话时为 `None`。
    pub fn session_id(&self) -> Option<SessionId> {
        self.id
    }

    /// 宿主最近一次公布的这个会话的状态。
    pub fn meta(&self) -> &SessionMeta {
        self.screen.meta()
    }

    /// 最近一次别的终端里的程序操作这个会话的记录，见 `SessionMeta::driver`。
    pub fn driver(&self) -> Option<&Driver> {
        self.screen.meta().driver.as_ref()
    }

    /// 结束宿主里的会话（关标签、关分屏这类用户明确要关掉终端的时候）。之后丢掉视图时不再
    /// 另外发什么；没调过它就丢掉视图时见 `Drop`。
    pub fn end(&mut self) {
        if std::mem::replace(&mut self.ended, true) {
            return;
        }
        if let Some(id) = self.id {
            self.host_link.kill(id);
        } else if let Some(opening) = &self.opening {
            let mut claim = opening.claim.lock().unwrap_or_else(PoisonError::into_inner);
            claim.ended = true;
            if let Some(id) = claim.opened.take() {
                self.host_link.kill(id);
            }
        }
    }

    /// 终端的尺寸：视图最近一次量出的，还没量过时是宿主给的会话尺寸。不显示的终端也有。
    pub fn size(&self) -> GridSize {
        self.screen.last_size()
    }

    /// 不等布局，按 `size` 现在就启动 shell。放在看不见的地方（后台标签、放大的分屏后面）的终端
    /// 等不来 `start` 要的那次布局；之后显示出来时照常按实际尺寸改。还没开会话的（`deferred`）
    /// 连 shell 一起开，只看状态。
    pub fn start_at(&mut self, size: GridSize, window: &mut Window, cx: &mut Context<Self>) {
        if self.started {
            return;
        }
        self.screen.resize(size);
        self.start_pending = false;
        let Some(id) = self.id else {
            // `deferred` 建的视图走到这里时，界面这份状态没标成正在附着（`request_visible` 经 `set_visible`
            // 才标上），附着失败时宿主回的错误不会被认成会话没了。
            self.open_session(true, Attach { size: None, mode: AttachMode::MetaOnly }, window, cx);
            self._reader = Self::read_events(self.events.clone(), window, cx);
            return;
        };
        if self.screen.live().is_none() {
            // 没有界面这份 VT 替它请宿主改尺寸，直接请。
            self.host_link.send(ClientMsg::Resize { id, size });
        }
        self.start_now();
    }

    /// shell 当前所在的目录，新建标签或分屏、右侧面板读项目时沿用；宿主还没报告时是建视图时给的
    /// 目录。shell 在前台时用它显示提示符时的目录（`SessionMeta::prompt_cwd`）：插件管理器在提示符
    /// 出来后延迟加载插件，会临时切进插件目录，这时现读的 `SessionMeta::cwd` 是插件的仓库。
    pub fn cwd(&self) -> Option<PathBuf> {
        let meta = self.screen.meta();
        let prompt_cwd = meta.prompt_cwd.as_ref().filter(|_| meta.foreground_is_shell);
        prompt_cwd.or(meta.cwd.as_ref()).cloned().or_else(|| self.start_dir.clone())
    }

    /// `cwd` 给人看的写法，家目录按终端所在的机器写成 `~`。
    pub fn display_cwd(&self) -> Option<String> {
        self.cwd().map(|dir| self.host.display_dir(&dir))
    }

    /// 记下这个终端是开来跑命令菜单里名为 `name`、命令行是 `command` 的那一条的。
    pub fn set_task(&mut self, name: SharedString, command: String) {
        self.task = Some((name, command));
    }

    /// 开来跑的命令菜单里那一条的名字；不是从命令菜单开的为空。
    pub fn task(&self) -> Option<&SharedString> {
        self.task.as_ref().map(|(name, _)| name)
    }

    /// 这个终端是开来跑命令菜单里名为 `name`、命令行是 `command` 的那一条的，而且会话还在：再跑这一条
    /// 就在它里面跑，不另开终端。
    pub fn runs_task(&self, name: &str, command: &str) -> bool {
        !self.ended
            && !self.screen.is_lost()
            && self.task.as_ref().is_some_and(|(task, line)| task.as_ref() == name && line == command)
    }

    /// 程序设置的标题；没设置时为前台进程的目录名或进程名，都没有时为 `DEFAULT_TITLE`。
    pub fn title(&self) -> &str {
        let meta = self.screen.meta();
        meta.title.as_deref().or(meta.fallback_title.as_deref()).unwrap_or(DEFAULT_TITLE)
    }

    /// 前台 agent 在进行的对话，写进窗口存档；恢复出来、agent 还没起来的终端是要接回的那场。
    pub fn conversation(&self) -> Option<Conversation> {
        self.screen.meta().conversation.clone().or_else(|| self.start_resume.clone())
    }

    /// 新建时挑的 shell，写进窗口存档；用宿主默认 shell 的为 `None`。
    pub fn shell(&self) -> Option<&str> {
        self.start_shell.as_deref()
    }

    /// 前台 agent 在标题里报告的状态；不是 agent 在前台时为 `None`。
    pub fn agent(&self) -> Option<Agent> {
        self.screen.meta().agent
    }

    /// 前台还在跑的程序，关掉终端前据此提醒：agent 时为它的名字，别的命令时为进程名。shell 在等
    /// 输入、还没启动，会话已经结束或者和宿主断开了时为 `None`。`AgentKind::Other` 是用 OSC 9;4
    /// 报进度的普通程序，按进程名算。
    pub fn running(&self) -> Option<String> {
        if self.ended || self.screen.is_lost() {
            return None;
        }
        let meta = self.screen.meta();
        if let Some(agent) = meta.agent.filter(|agent| agent.kind.is_known()) {
            return Some(agent.kind.display_name().to_owned());
        }
        meta.foreground.as_deref().filter(|_| !meta.foreground_is_shell).map(|name| program_label(name).to_owned())
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

    /// 有了活动（键盘输入、终端输出、重新获得焦点、窗口重新激活、改了配置）：让光标立即亮起，
    /// 从头开始计闪烁周期和空闲期限，空闲停了的闪烁也恢复。每批输出都会调用，所以这里不碰
    /// 计时器；计时器由绘制时的 `sync_cursor_blink` 按要不要闪来起停。
    pub(super) fn reset_cursor_blink(&mut self, cx: &mut Context<Self>) {
        if self.cursor_blink.reset() {
            cx.notify();
        }
    }

    /// 绘制时调用，`blinking` 是这一帧光标要不要闪（有焦点、有光标、光标在闪）。要闪而计时器
    /// 没在跑时起一个，不闪时停掉。光标从不闪变成闪只会因为有输出、换了主题或者重新获得焦点，
    /// 这几样都会引起重画，所以只在绘制时检查就够了。
    pub(super) fn sync_cursor_blink(&mut self, blinking: bool, cx: &mut Context<Self>) {
        if !blinking {
            self.cursor_blink.halt();
            return;
        }
        if !self.cursor_blink.wants_timer() {
            return;
        }
        let timer = cx.spawn(async move |this, cx| {
            let mut wait = CURSOR_BLINK_INTERVAL;
            loop {
                cx.background_executor().timer(wait).await;
                match this.update(cx, |view, cx| view.cursor_blink_tick(cx)) {
                    Ok(Some(next)) => wait = next,
                    Ok(None) | Err(_) => break,
                }
            }
        });
        self.cursor_blink.start(timer);
    }

    /// 图片动画在播时，到下一帧该画的时候（`Frame::next_image_frame`）重画一次；没在播时停掉计时器。
    pub(super) fn schedule_image_frame(&mut self, due: Option<Duration>, cx: &mut Context<Self>) {
        // 不比 120 Hz 屏幕的一帧更勤。
        const MIN_INTERVAL: Duration = Duration::from_millis(8);
        self._image_frame = due.map(|due| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(due.max(MIN_INTERVAL)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
        });
    }

    /// 闪烁计时器到点，返回下次再等多久，停了时为空。光标这期间不闪了的话，这次重画时
    /// `sync_cursor_blink` 会停掉计时器。
    fn cursor_blink_tick(&mut self, cx: &mut Context<Self>) -> Option<Duration> {
        let (next, repaint) = self.cursor_blink.tick(self.config.cursor_style_blink_timeout);
        if repaint {
            cx.notify();
        }
        next
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

/// 从收事件的一端取出已经排着的一批，返回这批和收事件的一端是否已经关了。宿主那边一直在往里放时
/// 一次取不完：和读事件的任务一样取够一批（`MAX_OUTPUT_BATCH` 字节的输出或 `MAX_EVENT_BATCH` 件）
/// 就先交出去，`wait_for_screen` 处理完看看时限，剩下的下一轮再取。
fn take_batch(rx: &mut UnboundedReceiver<LinkEvent>, task: &mut std::task::Context<'_>) -> (Vec<LinkEvent>, bool) {
    let mut events = Vec::new();
    let mut bytes = 0;
    while bytes < MAX_OUTPUT_BATCH && events.len() < MAX_EVENT_BATCH {
        match rx.poll_next_unpin(task) {
            Poll::Ready(Some(event)) => {
                bytes += output_len(&event);
                events.push(event);
            }
            Poll::Ready(None) => return (events, true),
            Poll::Pending => break,
        }
    }
    (events, false)
}

/// 让出一次主线程：先返回 `Pending` 并马上叫醒自己，排到主线程上已经排着的事（输入、画下一帧）后面
/// 再接着跑。
async fn yield_now() {
    let mut yielded = false;
    futures::future::poll_fn(|task| {
        if std::mem::replace(&mut yielded, true) {
            Poll::Ready(())
        } else {
            task.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
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

#[cfg(test)]
mod tests {
    use futures::channel::mpsc;

    use super::*;

    /// Windows 的 Ctrl+C 有选区时复制并清掉选区，再按一次就发 ETX，不改剪贴板。
    #[cfg(windows)]
    #[test]
    fn ctrl_c_copies_selection_or_interrupts() {
        use gpui::{ClipboardItem, KeyDownEvent, Keystroke};

        host_client::skip_start();
        let mut app = gpui::TestAppContext::single();
        app.update(|cx| {
            let mut config = runode_config::Config::default();
            config.font_family.clear();
            cx.set_global(AppConfig(Arc::new(config)));
        });
        let requests = Rc::new(RefCell::new(Vec::new()));
        let sent = requests.clone();
        let mut session = Session::new(
            PROVISIONAL_SIZE,
            &TermSettings::default(),
            Box::new(move |request| sent.borrow_mut().push(request)),
        )
        .unwrap();
        let text = "Windows 复制测试";
        session.feed(text.as_bytes());
        session.select_all();
        let screen = ScreenState::new_live(session, 1, SessionMeta::default(), PROVISIONAL_SIZE);
        let (_tx, rx) = mpsc::unbounded();
        let window = app.add_window(|_, _| gpui::Empty);
        window
            .update(&mut app, |_, window, cx| {
                let view = cx.new(|cx| TerminalView::new(&HostId::Local, None, screen, true, rx, window, cx));
                view.update(cx, |view, cx| {
                    let event = KeyDownEvent {
                        keystroke: Keystroke::parse("ctrl-c").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    };
                    cx.write_to_clipboard(ClipboardItem::new_string("before".into()));
                    view.key_down(&event, window, cx);
                    assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some(text));
                    assert!(requests.borrow().is_empty(), "copy must not send Ctrl+C to the program");
                    assert!(!view.screen.shown().unwrap().has_selection());

                    view.key_down(&event, window, cx);
                    assert!(matches!(&requests.borrow()[..], [session::Request::Input(bytes)] if bytes == b"\x03"));
                    assert_eq!(cx.read_from_clipboard().and_then(|item| item.text()).as_deref(), Some(text));
                });
            })
            .unwrap();
    }

    /// 只排着不带输出的事件（响铃）时，一批也只取 `MAX_EVENT_BATCH` 件，剩下的留到下一轮。
    #[test]
    fn take_batch_bounds_events_without_output() {
        let (tx, mut rx) = mpsc::unbounded();
        let id = SessionId(1);
        for _ in 0..MAX_EVENT_BATCH * 2 + 3 {
            tx.unbounded_send(LinkEvent::Msg(HostMsg::Bell { id })).unwrap();
        }
        let mut task = std::task::Context::from_waker(futures::task::noop_waker_ref());
        let (first, closed) = take_batch(&mut rx, &mut task);
        assert_eq!((first.len(), closed), (MAX_EVENT_BATCH, false));
        let (second, _) = take_batch(&mut rx, &mut task);
        assert_eq!(second.len(), MAX_EVENT_BATCH);
        drop(tx);
        let (rest, closed) = take_batch(&mut rx, &mut task);
        assert_eq!((rest.len(), closed), (3, true));
    }

    /// `yield_now` 头一次让出、叫醒自己，再被拉起时就往下走。
    #[test]
    fn yield_now_wakes_itself_once() {
        struct Count(std::sync::atomic::AtomicUsize);
        impl ArcWake for Count {
            fn wake_by_ref(this: &Arc<Self>) {
                this.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let count = Arc::new(Count(Default::default()));
        let waker = futures::task::waker(count.clone());
        let mut task = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(yield_now());
        assert!(future.as_mut().poll(&mut task).is_pending());
        assert_eq!(count.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(future.as_mut().poll(&mut task).is_ready());
    }

    /// 输出够了 `MAX_OUTPUT_BATCH` 字节就先交出去，后面排着的事件留到下一轮。
    #[test]
    fn take_batch_bounds_output_bytes() {
        let (tx, mut rx) = mpsc::unbounded();
        tx.unbounded_send(LinkEvent::Output(vec![b'x'; MAX_OUTPUT_BATCH].into())).unwrap();
        tx.unbounded_send(LinkEvent::Output(b"y".to_vec().into())).unwrap();
        let mut task = std::task::Context::from_waker(futures::task::noop_waker_ref());
        let (first, closed) = take_batch(&mut rx, &mut task);
        assert_eq!((first.len(), closed), (1, false));
        let (second, closed) = take_batch(&mut rx, &mut task);
        assert_eq!((second.len(), closed), (1, false));
    }
}
