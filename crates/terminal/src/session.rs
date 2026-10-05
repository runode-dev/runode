//! 界面这边的一个终端：一份 libghostty-vt 状态机，只消费宿主转来的 PTY 输出。
//!
//! 权威的那份 VT 在宿主里（`HostSession`），终端查询只由它应答；这里的 VT 只用来画屏幕、
//! 选区、搜索、滚动，以及读提示符上正在编辑的输入，不注册 `on_pty_write`，查询序列在这边被
//! 静默忽略。改 VT 状态的操作（改尺寸、⌘K 清屏、换主题）一律经宿主，在输出流里标出位置后
//! 两边在同一处做，两份 VT 才不会分叉。按键、粘贴等输入编码好后交给 `Sender`，由调用方
//! 转给宿主。标题、agent 这些对外公布的状态由宿主整理成 `SessionMeta`，经 `apply_meta` 写进来。
//!
//! `Session` 放在 UI 线程上，因为 `libghostty_vt::Terminal` 只能单线程使用；渲染器读取
//! `Frame`，`refresh` 只复制 libghostty 报告为脏的行来保持它最新。
//!
//! 这里是 `Session` 本身：创建、注册 VT 回调、套用配置、改尺寸和对外公布的状态。其余按职责
//! 分在子模块里：帧（`render`）、输入（`input`）、光标所在的输入行（`input_line`）、
//! 鼠标（`pointer`）、视口滚动（`scroll`）、选区（`selection`）、搜索（`search`）、
//! 快照（`snapshot`），以及和 libghostty 类型之间的转换（`convert`）。

pub(crate) mod convert;
mod input;
mod input_line;
mod pointer;
mod render;
mod scroll;
mod search;
mod selection;
mod snapshot;

use std::{
    cell::{Cell as StdCell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::Result;
use libghostty_vt::{
    key::{self, OptionAsAlt},
    mouse,
    search::Search,
    terminal::{PointCoordinate, Terminal},
};

use runode_shared_types::{
    agent::Agent,
    frame::Frame,
    grid::{GridPoint, GridSize},
    session::SessionMeta,
    settings::{self, TermSettings},
    shell::ShellNames,
};

use crate::{
    prompt_input::{self, PromptInput},
    vt,
};
pub use input::Paste;
use render::Renderer;
use selection::Selecting;

/// 程序用同步输出（mode 2026）冻结屏幕的最长时间，超时后不再遵守，以免程序异常时画面卡死。
pub const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// 界面这边的会话要宿主做的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// 写给程序的输入：按键、粘贴、鼠标上报等，已经按终端当前的模式编码好。
    Input(Vec<u8>),
    /// 视图的尺寸变了。宿主改好 PTY 和自己的 VT 后在输出流里标出位置，界面到那里再调
    /// `Session::apply_resized`。
    Resize(GridSize),
    /// ⌘K 清屏。宿主把清屏要写进 VT 的字节当成一段输出发回来，两份 VT 一起清。
    ClearScreen,
}

/// 把 `Request` 交给宿主的一方。
pub type Sender = Box<dyn Fn(Request)>;

pub struct Session {
    terminal: Terminal<'static, 'static>,
    renderer: Rc<RefCell<Renderer>>,
    /// 运行中的程序开始冻结屏幕（mode 2026）的时刻。
    held_since: Rc<StdCell<Option<Instant>>>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    selecting: Selecting,
    sender: Sender,
    /// VT 现在的尺寸，见 `apply_resized`。
    size: StdCell<GridSize>,
    /// 最近一次要宿主改成的尺寸，见 `resize`。
    requested_size: GridSize,
    /// 程序响过铃，由 `take_bell` 取走。
    bell: Rc<StdCell<bool>>,
    /// 待写出的已编码输入，各次按键复用这块缓冲。
    scratch: Vec<u8>,
    /// 程序设置的标题；agent 的状态前缀已拆到 `agent` 里。来自宿主，见 `apply_meta`。
    pub title: Option<String>,
    /// 前台 agent 和它的状态；不是 agent 在前台时为 `None`。来自宿主，见 `apply_meta`。
    pub agent: Option<Agent>,
    /// 程序没设置标题时用的名字：前台程序名，或者 shell 所在目录的名字。来自宿主，见 `apply_meta`。
    pub fallback_title: Option<String>,
    /// 宿主最近一次公布的状态。
    meta: SessionMeta,
    /// 打开搜索栏期间的搜索；关掉就丢弃。
    search: Option<Search<'static>>,
    /// 平滑滚动不足一行的部分，0 到 1 之间：画面整体往下错开这么多行，见 `scroll_smoothly`。
    scroll_offset: f32,
    pub exited: bool,
    option_as_alt: OptionAsAlt,
    /// 最近一次向程序发输入的时刻，见 `last_input`。
    input_at: Option<Instant>,
    /// 发出输入后还没等到输出的那次输入的时刻，量按键到回显的延迟用，见 `feed`。
    echo_pending: Option<Instant>,
}

impl Session {
    /// 按 `size` 新建一份 VT，套好 `settings` 的主题。宿主那份 VT 要用同样的尺寸和主题建好、
    /// 还没喂过任何输出，或者之后的输出原样喂给这边，两份才一样。
    pub fn new(size: GridSize, settings: &TermSettings, sender: Sender) -> Result<Self> {
        let mut terminal = vt::new_terminal(size)?;
        vt::apply_theme(&mut terminal, settings);
        Self::with_terminal(size, terminal, sender)
    }

    /// 接上 `terminal`：注册界面要的 VT 回调，建好会话的其余状态。`terminal` 要已经按 `size`
    /// 改好尺寸、设好 `vt::configure_common` 的选项。
    fn with_terminal(size: GridSize, mut terminal: Terminal<'static, 'static>, sender: Sender) -> Result<Self> {
        let renderer = Rc::new(RefCell::new(Renderer::new()?));
        let held_since = Rc::new(StdCell::new(None));
        let bell = Rc::new(StdCell::new(false));

        // 不注册 `on_pty_write`：查询由宿主那份 VT 应答，这边再答一遍程序就会收到两份回复。
        terminal
            .on_bell({
                let bell = bell.clone();
                move |_| bell.set(true)
            })?
            .on_render_hold({
                let renderer = renderer.clone();
                let held_since = held_since.clone();
                move |term, held| {
                    if held {
                        // 冻结在更新开始前的那一帧。renderer 从不跨 VT 写入被借用，
                        // 但这里运行在 extern "C" 回调里，绝不能冒会 panic 的借用风险。
                        if let Ok(mut renderer) = renderer.try_borrow_mut()
                            && let Err(err) = renderer.refresh(term)
                        {
                            tracing::warn!("render hold capture failed: {err}");
                        }
                        held_since.set(Some(Instant::now()));
                    } else {
                        held_since.set(None);
                    }
                }
            })?;

        Ok(Self {
            terminal,
            renderer,
            held_since,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            selecting: Selecting::new()?,
            sender,
            size: StdCell::new(size),
            requested_size: size,
            bell,
            scratch: Vec::with_capacity(64),
            title: None,
            agent: None,
            fallback_title: None,
            meta: SessionMeta::default(),
            search: None,
            scroll_offset: 0.,
            exited: false,
            option_as_alt: OptionAsAlt::False,
            input_at: None,
            echo_pending: None,
        })
    }

    /// 应用配置中只归界面管的部分：选区、搜索和光标的颜色，以及 Option 键当不当 Alt。主题
    /// （默认颜色、光标样式、回滚上限）改的是 VT 的状态，要等宿主在输出流里标出位置后用
    /// `apply_theme` 套用。
    pub fn apply_config(&mut self, settings: &TermSettings) {
        let mut renderer = self.renderer.borrow_mut();
        renderer.selection_bg = settings.selection_background;
        renderer.selection_fg = settings.selection_foreground;
        renderer.search_colors = [
            (settings.search_background, settings.search_foreground),
            (settings.search_selected_background, settings.search_selected_foreground),
        ];
        renderer.cursor_color = settings.cursor_color;
        renderer.cursor_text = settings.cursor_text;
        // 选区颜色不经过 VT 的脏标记，强制下一帧整屏重画。
        renderer.frame = Frame::default();
        self.option_as_alt = match settings.option_as_alt {
            settings::OptionAsAlt::False => OptionAsAlt::False,
            settings::OptionAsAlt::True => OptionAsAlt::True,
            settings::OptionAsAlt::Left => OptionAsAlt::Left,
            settings::OptionAsAlt::Right => OptionAsAlt::Right,
        };
    }

    /// 在宿主套用主题的位置（`HostMsg::ThemeApplied`）套用同样的主题，见 `vt::apply_theme`。
    pub fn apply_theme(&mut self, settings: &TermSettings) {
        vt::apply_theme(&mut self.terminal, settings);
        // 默认颜色变了，整屏重画。
        self.renderer.borrow_mut().frame = Frame::default();
    }

    /// 把宿主转来的 PTY 输出喂给 VT。
    pub fn feed(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if let Some(sent) = self.echo_pending.take() {
            tracing::trace!(target: "runode::latency", micros = sent.elapsed().as_micros() as u64, "input to echo");
        }
        self.terminal.vt_write(data);
    }

    /// 写入宿主公布的状态，返回标题、`fallback_title` 或 agent 是否变了。
    pub fn apply_meta(&mut self, meta: SessionMeta) -> bool {
        let changed =
            meta.title != self.title || meta.fallback_title != self.fallback_title || meta.agent != self.agent;
        self.title.clone_from(&meta.title);
        self.fallback_title.clone_from(&meta.fallback_title);
        self.agent = meta.agent;
        self.meta = meta;
        changed
    }

    /// 光标停在 shell 提示符上时正在编辑的那条输入，见 `prompt_input::read`。
    pub fn prompt_input(&self) -> Option<PromptInput> {
        log_err("read prompt input", prompt_input::read(&self.terminal)).flatten()
    }

    /// shell 当前所在的目录，宿主最近一次读到的。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.meta.cwd.clone()
    }

    /// shell 最近一次等着输入时所在的目录；还没等过输入时为 shell 当前所在的目录。
    pub fn prompt_cwd(&self) -> Option<std::path::PathBuf> {
        self.meta.prompt_cwd.clone().or_else(|| self.cwd())
    }

    /// shell 集成报告的 shell 自己的 PATH（每次显示提示符时 PATH 变了才报告）；还没报告过时
    /// 为 `None`。补全跑生成器命令时用它，和用户在 shell 里能找到的命令一致。
    pub fn shell_path(&self) -> Option<std::ffi::OsString> {
        self.meta.shell_path.clone()
    }

    /// shell 集成报告的别名、函数、内建命令和关键字（内容变了才报告）；没报告过的为空。
    pub fn shell_names(&self) -> ShellNames {
        self.meta.shell_names.clone()
    }

    /// 宿主最近一次读到的前台是不是 shell 自己。
    pub fn foreground_is_shell(&self) -> bool {
        self.meta.foreground_is_shell
    }

    pub fn take_bell(&self) -> bool {
        self.bell.take()
    }

    /// 视图的尺寸变了：请宿主改 PTY 和它的 VT。这边的 VT 等宿主在输出流里标出位置后，由
    /// `apply_resized` 再改，两份 VT 在同一个字节位置折行。和上次请求的一样或者行列为 0 时
    /// 什么都不做。
    pub fn resize(&mut self, size: GridSize) {
        if self.requested_size == size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.requested_size = size;
        (self.sender)(Request::Resize(size));
    }

    /// 在宿主改尺寸的位置（`HostMsg::Resized`）改这边 VT 的尺寸。
    pub fn apply_resized(&mut self, size: GridSize) {
        if self.size.get() == size {
            return;
        }
        self.size.set(size);
        self.scroll_offset = 0.;
        if let Err(err) =
            self.terminal.resize(size.cols, size.rows, u32::from(size.cell_width_px), u32::from(size.cell_height_px))
        {
            tracing::warn!("terminal resize failed: {err}");
        }
    }

    /// VT 现在的尺寸。
    pub fn size(&self) -> GridSize {
        self.size.get()
    }

    /// 把编码好的输入交给宿主写给程序。
    fn send_input(&mut self, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        self.echo_pending.get_or_insert_with(Instant::now);
        (self.sender)(Request::Input(data));
    }

    /// 网格位置换算成以设备像素计的坐标，和 `GridSize` 的单元格尺寸一致。
    fn surface_position(&self, at: GridPoint) -> (f64, f64) {
        let size = self.size.get();
        (
            f64::from(at.x) * f64::from(size.cell_width_px),
            f64::from(at.y) * f64::from(size.cell_height_px),
        )
    }

    /// 指针所在的视口单元格；落在网格外时取最近的边上的单元格。
    fn viewport_cell(&self, at: GridPoint) -> PointCoordinate {
        let size = self.size.get();
        PointCoordinate {
            x: (at.x.max(0.) as u16).min(size.cols.saturating_sub(1)),
            y: u32::from((at.y.max(0.) as u16).min(size.rows.saturating_sub(1))),
        }
    }
}

/// 记日志并吞掉错误：鼠标和选区操作失败时只影响这一下，不该打断输入。
fn log_err<T>(what: &str, result: libghostty_vt::error::Result<T>) -> Option<T> {
    result.inspect_err(|err| tracing::warn!("{what} failed: {err}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{host_session::HostSession, pty::{Pty, PtyEvent}, testing::*};
    use runode_shared_types::input::{Key, KeyInput, Mods};

    /// 改尺寸只是请求：VT 等宿主标出位置后才改，重复的请求不再发。
    #[test]
    fn resize_is_a_request_until_the_host_applies_it() {
        let (mut session, requests) = capturing_session();
        let before = session.size();
        let size = GridSize { cols: 30, rows: 5, cell_width_px: 8, cell_height_px: 16 };
        session.resize(size);
        session.resize(size);
        assert_eq!(*requests.borrow(), [Request::Resize(size)]);
        assert_eq!(session.size(), before);
        session.apply_resized(size);
        assert_eq!(session.size(), size);
        assert_eq!(session.terminal.cols().unwrap(), 30);
    }

    /// 端到端：真实 shell 经过 PTY，宿主那份 VT 和界面这份同时喂同样的输出，按键从界面经宿主
    /// 送达子进程，彩色和宽字符输出落进帧，子进程退出时读到 EOF，两份 VT 的屏幕一样。
    #[test]
    fn shell_round_trip() {
        let size = GridSize {
            cols: 40,
            rows: 10,
            cell_width_px: 8,
            cell_height_px: 16,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let pty = Pty::spawn(
            size,
            Some("/bin/sh"),
            None,
            runode_shared_types::shell::IntegrationMode::Off,
            Box::new(move |event| tx.send(event).is_ok()),
        )
        .unwrap();
        let mut host = HostSession::new(size, pty, None, &TermSettings::default()).unwrap();
        let (mut session, requests) = capturing_session_sized(size);
        let script = "printf '\\033[31mred\\033[0m \\344\\270\\255\\346\\226\\207 ok\\n'; exit\r";
        for c in script.chars() {
            let (key, unshifted) = if c == '\r' {
                (Key::Enter, '\r')
            } else {
                (Key::Unidentified, c)
            };
            let input = KeyInput {
                key,
                mods: Mods::default(),
                consumed_mods: Mods::default(),
                unshifted,
                text: (c != '\r').then(|| c.to_string()),
            };
            assert!(session.key(&input), "key {c:?} produced no bytes");
        }
        for request in requests.borrow_mut().drain(..) {
            match request {
                Request::Input(data) => host.write(data),
                other => panic!("unexpected {other:?}"),
            }
        }
        for event in rx {
            match event {
                PtyEvent::Output(data) => {
                    session.feed(&data);
                    host.feed(&data);
                }
                PtyEvent::Exited => break,
            }
        }

        let frame = session.frame().clone();
        let line = (0..frame.rows)
            .find(|&y| row_text(&frame, y).starts_with("red "))
            .unwrap_or_else(|| {
                let screen: Vec<_> = (0..frame.rows).map(|y| row_text(&frame, y)).collect();
                panic!("output line not found in {screen:#?}")
            });
        assert_eq!(row_text(&frame, line), "red 中文 ok");
        let row = frame.row(line);
        assert_ne!(row[0].fg, frame.foreground, "SGR 31 should color the text");
        assert_eq!(row[4].text, "中");
        assert!(row[4].wide && row[5].spacer);
        assert_eq!(screen_row(host.terminal(), u32::from(line)), "red 中文 ok");
    }
}
