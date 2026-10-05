//! 一个终端：接到子 shell 上的 libghostty-vt 状态机。
//!
//! `Session` 持有 VT 状态并放在 UI 线程上，因为 `libghostty_vt::Terminal` 只能单线程
//! 使用。PTY 输出经 channel 到达（见 `Pty::spawn`），由 `feed` 喂进去；渲染器读取
//! `Frame`，`refresh` 只复制 libghostty 报告为脏的行来保持它最新。

use std::{
    cell::{Cell as StdCell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::channel::mpsc::UnboundedReceiver;
use libghostty_vt::{
    Error,
    fmt::{Format, Formatter, FormatterOptions},
    key::{self, OptionAsAlt},
    mouse,
    paste::PasteSource,
    render::{CellIterator, CursorVisualStyle, Dirty, Overscan, RenderState, RowIterator, Snapshot},
    screen::{CellSemanticContent, CellWide, GridRef, RowSemanticPrompt, Screen},
    search::Search,
    selection::{
        Adjustment, FormatOptions, Order,
        gesture::{self, AutoscrollTickEvent, DragEvent, Gesture, Geometry, PressEvent, ReleaseEvent},
    },
    style::{RgbColor, Underline},
    terminal::{
        ConformanceLevel, CursorStyle, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
        PointCoordinate, PointSpace, PrimaryDeviceAttributes, ProgressState, ScrollViewport,
        SecondaryDeviceAttributes, SemanticPrompt, SizeReportSize, Terminal, UnknownSequence,
    },
};

use runode_model::{
    agent::{Agent, AgentKind, AgentState},
    color::{Rgb, TerminalColor},
    frame::{Attrs, Cell, Cursor, CursorShape, Frame},
    grid::{GridPoint, GridSize, ViewportScroll},
    input::{Key, KeyInput, Mods, MouseAction, MouseButton, SelectionAdjust},
    settings::{self, TermSettings},
    shell::{IntegrationMode, ShellNames},
};

use crate::{
    agent, history,
    prompt_input::{self, PromptInput},
    pty::{Pty, PtyEvent, PtyWriter},
};

const SCROLLBACK_LINES: usize = 10_000;
/// 程序用同步输出（mode 2026）冻结屏幕的最长时间，超时后不再遵守，以免程序异常时画面卡死。
pub const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// VT 回调累积下来、UI 关心的变化。
#[derive(Default)]
struct Effects {
    title_changed: StdCell<bool>,
    bell: StdCell<bool>,
    /// 最近一次 OSC 9;4 进度报告是不是在进行中。
    progress: StdCell<Option<bool>>,
    /// shell 集成报告的命令步骤，按到达的先后，由 `take_commands` 取走。
    prompts: RefCell<Vec<PromptEvent>>,
    /// shell 集成用 `SHELL_REPORT` 报告的 shell 自己的 PATH，见 `Session::shell_path`。
    shell_path: RefCell<Option<std::ffi::OsString>>,
    /// shell 集成用 `SHELL_REPORT` 报告的别名、函数、内建命令和关键字，见 `Session::shell_names`。
    shell_names: RefCell<ShellNames>,
    /// 启动 shell 时交给集成脚本的报告口令（见 `shell_integration::prepare`），`SHELL_REPORT`
    /// 带的口令和它一致才采用；没有口令时一条报告都不采用。
    report_token: RefCell<Option<String>>,
    /// 带口令的 `command` 报告：shell 说紧接着的 OSC 133;C 是它真的开始运行命令。外层为 `None`
    /// 表示没收到；里面是命令原文，shell 拿不到原文时为 `None`，到时从屏幕上读。由下一个
    /// 133;C 取走，先来了 133;A、133;B 或 133;D 就作废，免得被之后伪造的 133;C 借用。
    command_report: RefCell<Option<Option<String>>>,
}

/// shell 集成在显示提示符时、内容和上次报告的不一样时用的私有 OSC：
/// `ESC ] 6973;<口令>;<字段>=<百分号编码的值> BEL`。字段是 `path`（PATH）或 `aliases`、
/// `functions`、`builtins`、`keywords`（名字之间用空格分开）、`alias_values`（一行一个
/// 「名字<Tab>值」）。另有 `command`：shell 开始运行命令时紧接在 OSC 133;C 前面发，值是命令
/// 原文（拿不到时为空），见 `Effects::command_report`。
///
/// 记进历史的命令同样只认这条报告，133;C 自带的命令原文一律不用：伪造的命令进了历史，就会被
/// 当成灰字建议推给用户。
///
/// 任何打印到终端的内容（`cat` 一个文件、ssh 远端的输出）都能写出这样的序列，而报告的 PATH
/// 会被补全拿去跑命令，所以只认带着本 shell 口令的报告。口令只留在 shell 自己的变量里，子进程的
/// 环境里没有，在 shell 里运行的程序打印不出它。只看是否处在提示符状态挡不住伪造：输出里可以先
/// 伪造一个 OSC 133;D。
const SHELL_REPORT: &[u8] = b"6973;";
/// 留给未知 OSC 的最多字节数：函数很多的 shell 报告的函数名能有几十 KB。更长的被截断，
/// 不采用。
const UNKNOWN_SEQUENCE_MAX_BYTES: usize = 256 * 1024;

impl Effects {
    /// 记下 shell 集成的一条报告 `<口令>;<字段>=<百分号编码的值>`。口令不对、缺口令或者
    /// 这个 shell 没有口令时整条丢掉；不认识的字段不管。
    fn shell_report(&self, report: &[u8]) {
        let Some(semicolon) = report.iter().position(|&b| b == b';') else {
            return;
        };
        let (token, report) = (&report[..semicolon], &report[semicolon + 1..]);
        let trusted = self
            .report_token
            .try_borrow()
            .is_ok_and(|expected| expected.as_deref().is_some_and(|expected| same_token(expected.as_bytes(), token)));
        if !trusted {
            return;
        }
        let Some(eq) = report.iter().position(|&b| b == b'=') else {
            return;
        };
        let (field, value) = (&report[..eq], percent_decode(&report[eq + 1..]));
        if field == b"command" {
            if let Ok(mut pending) = self.command_report.try_borrow_mut() {
                *pending = Some((!value.is_empty()).then(|| String::from_utf8_lossy(&value).into_owned()));
            }
            return;
        }
        if field == b"path" {
            if let Ok(mut path) = self.shell_path.try_borrow_mut() {
                use std::os::unix::ffi::OsStringExt as _;
                *path = Some(std::ffi::OsString::from_vec(value));
            }
            return;
        }
        let Ok(mut names) = self.shell_names.try_borrow_mut() else {
            return;
        };
        if field == b"alias_values" {
            let text = String::from_utf8_lossy(&value);
            names.alias_values =
                text.lines().filter_map(|line| line.split_once('\t')).map(|(n, v)| (n.to_owned(), v.to_owned())).collect();
            return;
        }
        let list = match field {
            b"aliases" => &mut names.aliases,
            b"functions" => &mut names.functions,
            b"builtins" => &mut names.builtins,
            b"keywords" => &mut names.keywords,
            _ => return,
        };
        *list = String::from_utf8_lossy(&value).split_whitespace().map(str::to_owned).collect();
    }
}

/// `Effects::prompts` 里的一步。
enum PromptEvent {
    /// 提示符画完，shell 等着输入（OSC 133;B）。
    InputStart,
    /// 命令开始运行（OSC 133;C），带着要记进历史的命令。前面没有带口令的 `command` 报告
    /// （可能是伪造的）、或者报告没带原文又没能从屏幕上读到时为 `None`，不记。
    OutputStart(Option<String>),
    /// 命令运行结束（OSC 133;D），带着退出码。
    CommandEnd(Option<i32>),
}

/// 把 libghostty 的 render state 复制进 `Frame`。和 render-hold 回调共用，
/// 因为同步更新一开始，回调就要立即截下当前帧。
struct Renderer {
    render_state: RenderState<'static>,
    row_it: RowIterator<'static>,
    cell_it: CellIterator<'static>,
    frame: Frame,
    /// 配置的选区颜色；没配时选区反色显示。
    selection_bg: Option<TerminalColor>,
    selection_fg: Option<TerminalColor>,
    /// 配置的光标颜色。固定色已交给 VT 作默认光标色，这里只用来解析跟随单元格的两种。
    cursor_color: Option<TerminalColor>,
    /// 配置的光标下文字颜色；没配时用背景色。
    cursor_text: Option<TerminalColor>,
    /// 视口里的搜索匹配，按行切成段。
    highlights: Vec<Highlight>,
    /// 搜索匹配的颜色：普通匹配和选中匹配各一对（背景、文字）。
    search_colors: [(TerminalColor, TerminalColor); 2],
    /// 高亮或颜色变了，下一次刷新不能只画脏行。
    force_full: bool,
}

/// 视口第 `y` 行从 `x0` 到 `x1`（含）的一段搜索匹配。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Highlight {
    y: u16,
    x0: u16,
    x1: u16,
    selected: bool,
}

/// 鼠标选区：libghostty 的手势状态机，加上各类事件复用的对象。
struct Selecting {
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    autoscroll: AutoscrollTickEvent<'static>,
    /// 按下事件的时间基准，手势靠两次按下的间隔判断双击、三击。
    epoch: Instant,
    /// 拖动时指针最后的位置和是否选矩形块，自动滚动时沿用。
    pointer: (GridPoint, bool),
}

/// 光标所在的输入行，各单元格按行连成一串，下标从这一行第一格算起。
struct InputLine {
    top: u32,
    bottom: u32,
    cols: usize,
    /// 每一格是不是宽字符的占位格；方向键和退格按字符走，占位格不算一步。
    spacer: Vec<bool>,
    /// 最后一个有字的格子之后。
    end: usize,
    /// 用户输入从哪一格开始：shell 集成标出的提示符之后；没有标记时为 0。
    start: usize,
    cursor: usize,
}

impl InputLine {
    /// 活动区第 `y` 行第 `x` 列在这一串里的下标；不在这一行上时为 `None`。
    fn index(&self, x: u16, y: u32) -> Option<usize> {
        (self.top..=self.bottom)
            .contains(&y)
            .then(|| (y - self.top) as usize * self.cols + usize::from(x))
    }

    fn chars(&self, range: std::ops::Range<usize>) -> isize {
        let range = range.start.min(self.spacer.len())..range.end.min(self.spacer.len());
        self.spacer[range].iter().filter(|spacer| !**spacer).count() as isize
    }

    /// 光标从 `from` 走到 `to` 要按几下方向键，负数往左。
    fn steps(&self, from: usize, to: usize) -> isize {
        if to >= from { self.chars(from..to) } else { -self.chars(to..from) }
    }
}

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
    pty: Pty,
    writer: PtyWriter,
    size: Rc<StdCell<GridSize>>,
    effects: Rc<Effects>,
    /// 待写出的已编码输入，各次按键复用这块缓冲。
    scratch: Vec<u8>,
    /// 程序设置的标题；agent 的状态前缀已拆到 `agent` 里。
    pub title: Option<String>,
    /// 前台 agent 的状态，由 `title_agent` 和 `progress` 合成；不是 agent 在前台时为 `None`。
    pub agent: Option<Agent>,
    /// claude、codex 等在标题里报告的状态，见 `agent::split_status`。
    title_agent: Option<Agent>,
    /// pi 等用 OSC 9;4 报告的进度：`Some(true)` 进行中，`Some(false)` 已停下但程序还在前台。
    progress: Option<bool>,
    /// 打开搜索栏期间的搜索；关掉就丢弃。
    search: Option<Search<'static>>,
    /// 平滑滚动不足一行的部分，0 到 1 之间：画面整体往下错开这么多行，见 `scroll_smoothly`。
    scroll_offset: f32,
    /// 程序没设置标题时用的名字，见 `Pty::foreground_title`；由 `refresh_fallback_title` 更新。
    pub fallback_title: Option<String>,
    pub exited: bool,
    option_as_alt: OptionAsAlt,
    /// 还没启动 shell 时它要从哪个目录开始，见 `unstarted`。
    start_dir: Option<std::path::PathBuf>,
    /// shell 最近一次等着输入时所在的目录，记命令时当作命令运行的目录。
    prompt_cwd: Option<std::path::PathBuf>,
    /// 正在运行、还没报告结束的那条命令。
    running: Option<history::Entry>,
    /// 最近一次向程序发输入的时刻，见 `last_input`。
    input_at: Option<Instant>,
}

impl Session {
    /// 在 `cwd` 下启动用户的 shell；为 `None` 时在家目录。
    pub fn spawn(
        size: GridSize,
        cwd: Option<&std::path::Path>,
        integration: IntegrationMode,
    ) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        Self::spawn_in(size, None, cwd, integration)
    }

    #[cfg(test)]
    pub fn spawn_shell(
        size: GridSize,
        shell: Option<&str>,
    ) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        Self::spawn_in(size, shell, None, IntegrationMode::Off)
    }

    fn spawn_in(
        size: GridSize,
        shell: Option<&str>,
        cwd: Option<&std::path::Path>,
        integration: IntegrationMode,
    ) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        let (pty, rx) = Pty::spawn(size, shell, cwd, integration)?;
        Ok((Self::with_pty(size, pty)?, rx))
    }

    /// 先不启动 shell，等 `start` 时再在 `cwd` 下启动；为 `None` 时在家目录。在那之前标题是
    /// 起始目录的名字，`cwd` 也报起始目录。
    pub fn unstarted(
        size: GridSize,
        cwd: Option<&std::path::Path>,
    ) -> Result<(Self, UnboundedReceiver<PtyEvent>)> {
        let (pty, rx) = Pty::open(size)?;
        let mut session = Self::with_pty(size, pty)?;
        session.start_dir = cwd.map(Into::into).or_else(|| runode_dirs::Dirs::from_env().home);
        session.fallback_title = session.start_dir.as_deref().map(crate::pty::dir_label);
        Ok((session, rx))
    }

    /// 还没启动 shell 时按 `integration` 启动它；已经启动过时什么都不做。
    pub fn start(&mut self, integration: IntegrationMode) -> Result<()> {
        self.pty.start(None, self.start_dir.as_deref(), integration)?;
        // 口令在启动 shell 时才生成，这时再交给校验报告的回调。
        *self.effects.report_token.borrow_mut() = self.pty.report_token().map(str::to_owned);
        Ok(())
    }

    pub fn started(&self) -> bool {
        self.pty.started()
    }

    /// 接上已经按 `size` 启动好的 shell。
    pub fn with_pty(size: GridSize, pty: Pty) -> Result<Self> {
        let writer = pty.writer.clone();

        let mut terminal = Terminal::new(size.cols, size.rows)?;
        terminal.set_scrollback_max_lines(Some(SCROLLBACK_LINES))?;
        terminal.resize(
            size.cols,
            size.rows,
            u32::from(size.cell_width_px),
            u32::from(size.cell_height_px),
        )?;

        let shared_size = Rc::new(StdCell::new(size));
        // 已经启动的 shell 的报告口令；还没启动的等 `start` 时再设。
        let effects = Rc::new(Effects {
            report_token: RefCell::new(pty.report_token().map(str::to_owned)),
            ..Effects::default()
        });
        let mut render_state = RenderState::new()?;
        // 多取视口上面一行：平滑滚动时画面往下错开，顶上要露出它的一部分。
        render_state.set_overscan(Overscan { above: 1, below: 0 })?;
        let renderer = Rc::new(RefCell::new(Renderer {
            render_state,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
            frame: Frame::default(),
            selection_bg: None,
            selection_fg: None,
            cursor_color: None,
            cursor_text: None,
            highlights: Vec::new(),
            // 应用配置之前用默认配色里的搜索高亮。
            search_colors: {
                use crate::theme::{SEARCH_BACKGROUND, SEARCH_FOREGROUND, SEARCH_SELECTED_BACKGROUND};
                [
                    (TerminalColor::Rgb(SEARCH_BACKGROUND), TerminalColor::Rgb(SEARCH_FOREGROUND)),
                    (TerminalColor::Rgb(SEARCH_SELECTED_BACKGROUND), TerminalColor::Rgb(SEARCH_FOREGROUND)),
                ]
            },
            force_full: false,
        }));
        let held_since = Rc::new(StdCell::new(None));

        // 查询回复（DA、DECRQM、DSR 等）直接写回子进程。
        // 没有回复的话，vim、tmux 这类程序探测终端能力时会卡住。
        let reply = writer.clone();
        terminal
            .on_pty_write(move |_, data| reply.write(data))?
            .on_size({
                let size = shared_size.clone();
                move |_| {
                    let size = size.get();
                    Some(SizeReportSize {
                        rows: size.rows,
                        columns: size.cols,
                        cell_width: u32::from(size.cell_width_px),
                        cell_height: u32::from(size.cell_height_px),
                    })
                }
            })?
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: Default::default(),
                })
            })?
            .on_xtversion(|_| Some(concat!("runode ", env!("CARGO_PKG_VERSION"))))?
            .on_title_changed({
                let effects = effects.clone();
                move |_| effects.title_changed.set(true)
            })?
            .on_bell({
                let effects = effects.clone();
                move |_| effects.bell.set(true)
            })?
            .on_progress_report({
                let effects = effects.clone();
                move |_, report| {
                    let active =
                        matches!(report.state(), Ok(ProgressState::Set | ProgressState::Indeterminate));
                    effects.progress.set(Some(active));
                }
            })?
            // 命令开始运行的那一刻它还原样留在屏幕上，在这里就读出来，之后的输出可能把它冲掉。
            // 只有 shell 集成用带口令的 `command` 报告认过的命令开始才记，133;C 自带的原文不用，
            // 见 `Effects::command_report`。回调运行在 extern "C" 函数里，不能 panic，所以借用
            // 失败时丢掉这一步。
            .on_semantic_prompt({
                let effects = effects.clone();
                move |term, event| {
                    let pending = effects.command_report.try_borrow_mut().ok().and_then(|mut pending| pending.take());
                    let event = match event {
                        SemanticPrompt::InputStart => PromptEvent::InputStart,
                        SemanticPrompt::OutputStart { .. } => PromptEvent::OutputStart(match pending {
                            Some(Some(command)) => Some(command),
                            Some(None) => {
                                log_err("read submitted command", prompt_input::submitted_command(term)).flatten()
                            }
                            None => None,
                        }),
                        SemanticPrompt::CommandEnd { exit_code, .. } => PromptEvent::CommandEnd(exit_code),
                        _ => return,
                    };
                    if let Ok(mut prompts) = effects.prompts.try_borrow_mut() {
                        prompts.push(event);
                    }
                }
            })?
            // shell 集成报告的 PATH 和各种名字；别的未知序列不管。回调同样不能 panic。
            .on_unknown_sequence({
                let effects = effects.clone();
                move |_, sequence| {
                    if let UnknownSequence::Osc { content, truncated: false, .. } = sequence
                        && let Some(report) = content.strip_prefix(SHELL_REPORT)
                    {
                        effects.shell_report(report);
                    }
                }
            })?
            .set_unknown_sequence_max_bytes(UNKNOWN_SEQUENCE_MAX_BYTES)?
            // RIS 会清空标题，但不会触发标题变化回调。
            .on_reset({
                let effects = effects.clone();
                move |_| effects.title_changed.set(true)
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
            selecting: Selecting {
                gesture: Gesture::new()?,
                press: PressEvent::new()?,
                drag: DragEvent::new()?,
                release: ReleaseEvent::new()?,
                autoscroll: AutoscrollTickEvent::new()?,
                epoch: Instant::now(),
                pointer: Default::default(),
            },
            pty,
            writer,
            size: shared_size,
            effects,
            scratch: Vec::with_capacity(64),
            title: None,
            agent: None,
            title_agent: None,
            progress: None,
            search: None,
            scroll_offset: 0.,
            fallback_title: None,
            exited: false,
            option_as_alt: OptionAsAlt::False,
            start_dir: None,
            prompt_cwd: None,
            running: None,
            input_at: None,
        })
    }

    /// 应用配置中与终端状态相关的部分。改的是默认值：程序自己用转义序列设置的
    /// 颜色和光标形状照旧优先，所以配置可以随时重载。
    pub fn apply_config(&mut self, settings: &TermSettings) {
        // 默认调色板读出来的是上次设置的值，先重置回内置调色板再叠加配置，
        // 否则旧主题设过、新主题没设的条目会残留。
        let mut palette = match self
            .terminal
            .set_default_color_palette(None)
            .and_then(|t| t.default_color_palette())
        {
            Ok(palette) => palette,
            Err(err) => {
                tracing::warn!("failed to read the default palette: {err}");
                return;
            }
        };
        for &(index, color) in &settings.palette {
            palette.0[usize::from(index)] = ghostty_rgb(color);
        }
        let applied = self
            .terminal
            .set_default_bg_color(Some(ghostty_rgb(settings.background)))
            .and_then(|t| t.set_default_fg_color(Some(ghostty_rgb(settings.foreground))))
            // 跟随单元格的光标色由渲染时按光标所在单元格解析，VT 里不设默认值。
            .and_then(|t| {
                t.set_default_cursor_color(match settings.cursor_color {
                    Some(TerminalColor::Rgb(color)) => Some(ghostty_rgb(color)),
                    _ => None,
                })
            })
            .and_then(|t| t.set_default_cursor_style(Some(ghostty_cursor_style(settings.cursor_style))))
            // 没配置时默认闪烁；libghostty 的 `None` 是不闪烁，所以这里显式给 true。
            .and_then(|t| t.set_default_cursor_blink(Some(settings.cursor_blink.unwrap_or(true))))
            .and_then(|t| t.set_default_color_palette(Some(palette)));
        if let Err(err) = applied {
            tracing::warn!("failed to apply config to the terminal: {err}");
        }
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

    /// 把 PTY 输出喂给 VT，返回标题或 agent 状态是否变化。
    pub fn feed(&mut self, data: &[u8]) -> bool {
        self.terminal.vt_write(data);
        let mut changed = false;
        // 不少 shell 每次出提示符都重发一遍同样的标题，agent 工作时每一帧转圈都改一次标题，
        // 只有去掉状态前缀后的标题或状态真变了才算。
        if self.effects.title_changed.take() {
            let raw = self.terminal.title().ok().unwrap_or_default();
            let (title, title_agent) = match agent::split_status(raw) {
                Some((state, rest)) => (rest, Some(state)),
                // codex 空闲时不带前缀：刚才还在报告状态的 agent 只要仍在前台，就是停下来了。
                None => (
                    raw,
                    self.title_agent
                        .filter(|_| !self.pty.foreground_is_shell())
                        .map(|agent| Agent { state: AgentState::Idle, ..agent }),
                ),
            };
            let title = (!title.is_empty()).then(|| title.to_owned());
            changed |= title != self.title;
            self.title = title;
            self.title_agent = title_agent;
        }
        // pi 工作中每秒重发一次进度；停下时清掉进度，回到 shell 的不再算 agent。
        if let Some(active) = self.effects.progress.take() {
            self.progress = if active {
                Some(true)
            } else {
                (!self.pty.foreground_is_shell()).then_some(false)
            };
        }
        let progress_agent = |state| {
            let kind = match self.title_agent {
                Some(agent) => agent.kind,
                None if self.title.as_deref().is_some_and(agent::is_pi_title) => AgentKind::Pi,
                None => AgentKind::Other,
            };
            Agent { kind, state }
        };
        let agent = match self.progress {
            Some(true) => Some(progress_agent(AgentState::Working)),
            progress => self.title_agent.or(progress.map(|_| progress_agent(AgentState::Idle))),
        };
        changed |= agent != self.agent;
        self.agent = agent;
        changed
    }

    /// shell 当前所在的目录。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        if !self.pty.started() {
            return self.start_dir.clone();
        }
        self.pty.shell_cwd()
    }

    /// 取走 shell 集成报告运行完了的命令，带着运行的目录和退出码，按结束的先后。每次 `feed`
    /// 之后调用：shell 回到提示符时在这里记下它的目录，之后开始运行的命令就算在那个目录里。
    pub fn take_commands(&mut self) -> Vec<history::Entry> {
        let events = self.effects.prompts.take();
        let mut finished = Vec::new();
        for event in events {
            match event {
                PromptEvent::InputStart => self.prompt_cwd = self.cwd(),
                PromptEvent::OutputStart(command) => {
                    self.running = command.map(|command| {
                        history::Entry::now(command, self.prompt_cwd.clone().or_else(|| self.cwd()))
                    });
                }
                // 有的 shell 每次出提示符都报告一次结束，没在运行的命令时不算。
                PromptEvent::CommandEnd(exit) => {
                    if let Some(mut entry) = self.running.take() {
                        entry.exit = exit;
                        finished.push(entry);
                    }
                }
            }
        }
        finished
    }

    /// 光标停在 shell 提示符上时正在编辑的那条输入，见 `prompt_input::read`。
    pub fn prompt_input(&self) -> Option<PromptInput> {
        log_err("read prompt input", prompt_input::read(&self.terminal)).flatten()
    }

    /// shell 集成报告的 shell 自己的 PATH（每次显示提示符时 PATH 变了才报告）；还没报告过时
    /// 为 `None`。补全跑生成器命令时用它，和用户在 shell 里能找到的命令一致。
    pub fn shell_path(&self) -> Option<std::ffi::OsString> {
        self.effects.shell_path.borrow().clone()
    }

    /// shell 集成报告的别名、函数、内建命令和关键字（内容变了才报告）；没报告过的为空。
    pub fn shell_names(&self) -> ShellNames {
        self.effects.shell_names.borrow().clone()
    }

    /// 终端当前的 16 个 ANSI 颜色，跟着主题、配置和程序用 OSC 4 改的颜色走。读不到时用
    /// 默认前景色代替。
    pub fn ansi_colors(&self) -> [Rgb; 16] {
        match self.terminal.color_palette() {
            Ok(palette) => std::array::from_fn(|i| rgb(palette.0[i])),
            Err(err) => {
                tracing::warn!("failed to read the palette: {err}");
                [self.peek_colors().0; 16]
            }
        }
    }

    /// shell 最近一次等着输入时所在的目录；还没等过输入时现读一次。
    pub fn prompt_cwd(&self) -> Option<std::path::PathBuf> {
        self.prompt_cwd.clone().or_else(|| self.cwd())
    }

    pub fn has_selection(&self) -> bool {
        self.terminal.selection().is_ok_and(|s| s.is_some())
    }

    /// 视口停在最底部，没有翻回滚历史，也没有平滑滚动错开的半行。
    pub fn viewport_at_bottom(&self) -> bool {
        self.terminal.viewport_active().unwrap_or(false) && self.scroll_offset == 0.
    }

    /// 最近一次向程序发输入（按键、文本、粘贴等）的时刻；从没发过时为 `None`。
    pub fn last_input(&self) -> Option<Instant> {
        self.input_at
    }

    /// 重新读取终端的前台进程，返回 `fallback_title` 或 `agent` 是否变化。
    pub fn refresh_fallback_title(&mut self) -> bool {
        // 还没启动时没有前台进程，标题保持起始目录的名字。
        if !self.pty.started() {
            return false;
        }
        // agent 退出、回到 shell 后，它留下的标题不再代表任何状态。
        let agent_gone = self.agent.is_some() && self.pty.foreground_is_shell();
        if agent_gone {
            self.agent = None;
            self.title_agent = None;
            self.progress = None;
        }
        let title = self.pty.foreground_title();
        if title == self.fallback_title {
            return agent_gone;
        }
        self.fallback_title = title;
        true
    }

    pub fn take_bell(&self) -> bool {
        self.effects.bell.take()
    }

    pub fn resize(&mut self, size: GridSize) {
        if self.size.get() == size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.size.set(size);
        self.scroll_offset = 0.;
        if let Err(err) = self.terminal.resize(
            size.cols,
            size.rows,
            u32::from(size.cell_width_px),
            u32::from(size.cell_height_px),
        ) {
            tracing::warn!("terminal resize failed: {err}");
        }
        self.pty.resize(size);
    }

    /// 经 libghostty 编码一次按键，它会遵守运行中程序要求的模式（应用光标键、
    /// Kitty 键盘协议、modifyOtherKeys 等）。按键没有产生字节时返回 false，
    /// 调用方可以交给平台处理。
    pub fn key(&mut self, input: &KeyInput) -> bool {
        // Option 当作 Alt 时不能算「已消耗」，编码器才会改用未加修饰的字符并加 ESC 前缀。
        let right = input.mods.right_alt;
        let option_is_alt = match self.option_as_alt {
            OptionAsAlt::True => true,
            OptionAsAlt::Left => !right,
            OptionAsAlt::Right => right,
            _ => false,
        };
        let mut consumed = input.consumed_mods;
        if option_is_alt {
            consumed.alt = false;
        }
        self.key_event
            .set_action(key::Action::Press)
            .set_key(ghostty_key(input.key))
            .set_mods(ghostty_mods(input.mods))
            .set_consumed_mods(ghostty_mods(consumed))
            .set_unshifted_codepoint(input.unshifted)
            .set_utf8(input.text.as_deref());
        self.scratch.clear();
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.terminal)
            .set_macos_option_as_alt(self.option_as_alt)
            .encode_to_vec(&self.key_event, &mut self.scratch);
        if let Err(err) = encoded {
            tracing::warn!("key encode failed: {err}");
            return false;
        }
        if self.scratch.is_empty() {
            return false;
        }
        self.before_input();
        self.writer.write(&self.scratch);
        true
    }

    /// 单击把光标挪到点击处：按两处之间隔着的字符数发左右方向键，由 shell 自己移动光标。
    /// 点在这一行文字末尾之后时停在末尾。只在 `input_line` 能取到输入行、并且点在这一行上时
    /// 生效，返回是否发了按键。
    pub fn click_to_move(&mut self, at: GridPoint) -> bool {
        let Some(steps) = self.click_to_move_steps(at) else {
            return false;
        };
        let Some(bytes) = self.arrow_keys(steps) else {
            return false;
        };
        self.before_input();
        self.writer.write(&bytes);
        true
    }

    /// 从光标走到点击处要按几下方向键，负数往左；不该移动时为 `None`。
    fn click_to_move_steps(&mut self, at: GridPoint) -> Option<isize> {
        let target = self.viewport_cell(at);
        let line = log_err("input line", self.input_line()).flatten()?;
        let to = line
            .index(target.x, target.y)?
            .max(line.start)
            .min(line.end.max(line.cursor));
        let steps = line.steps(line.cursor, to);
        (steps != 0).then_some(steps)
    }

    /// 删掉选中的文字（退格、Delete 时）：先把光标挪到选区末尾，再按选中的字符数发退格。
    /// 选区要整个落在光标所在的输入行里，`input_line` 的其他条件也一样；超出文字末尾的部分
    /// 不算。返回是否处理了；没处理时按键照常交给程序。
    pub fn delete_selection(&mut self) -> bool {
        let Some((steps, count)) = log_err("delete selection", self.delete_selection_keys()).flatten() else {
            return false;
        };
        let (Some(arrows), Some(backspace)) = (self.arrow_keys(steps), self.encode_key(key::Key::Backspace))
        else {
            return false;
        };
        let mut bytes = arrows;
        bytes.extend(backspace.repeat(count));
        self.before_input();
        self.writer.write(&bytes);
        true
    }

    /// 改写 shell 正在编辑的输入：先按 `backspace` 下退格，然后把 `text` 当作普通文字写进去
    /// （不走粘贴），一次发给程序。编码不出退格键时什么也不发，返回 false。
    pub fn edit_input(&mut self, backspace: usize, text: &str) -> bool {
        let mut bytes = Vec::new();
        if backspace > 0 {
            let Some(key) = self.encode_key(key::Key::Backspace) else {
                return false;
            };
            bytes.extend(key.repeat(backspace));
        }
        bytes.extend_from_slice(text.as_bytes());
        if bytes.is_empty() {
            return true;
        }
        self.before_input();
        self.writer.write(&bytes);
        true
    }

    /// 删除选区要先按几下方向键（负数往左）、再按几下退格；不该处理时为 `None`。
    fn delete_selection_keys(&mut self) -> libghostty_vt::error::Result<Option<(isize, usize)>> {
        let Some(line) = self.input_line()? else {
            return Ok(None);
        };
        let range = {
            let Some(selection) = self.terminal.selection()? else {
                return Ok(None);
            };
            if selection.is_rectangle() {
                return Ok(None);
            }
            let selection = selection.to_ordered(&self.terminal, Order::Forward)?;
            let start = self.terminal.point_from_grid_ref(&selection.start(), PointSpace::Active)?;
            let end = self.terminal.point_from_grid_ref(&selection.end(), PointSpace::Active)?;
            let (Some(start), Some(end)) = (start, end) else {
                return Ok(None);
            };
            let (Some(start), Some(end)) = (line.index(start.x, start.y), line.index(end.x, end.y)) else {
                return Ok(None);
            };
            start.max(line.start).min(line.end)..(end + 1).min(line.end)
        };
        let count = line.chars(range.clone()) as usize;
        if count == 0 {
            return Ok(None);
        }
        Ok(Some((line.steps(line.cursor, range.end), count)))
    }

    /// 光标所在的输入行：光标那一行，连同软换行接在一起的上下几行。只在主屏、视口在底部、
    /// 光标停在 shell 提示符上时有，别的时候为 `None`。
    ///
    /// shell 集成用 OSC 133 标出了提示符时，以它为准：输入从提示符之后开始。没有这些标记时
    /// 只能看前台是不是 shell 自己，也不知道提示符在哪里结束。
    fn input_line(&mut self) -> libghostty_vt::error::Result<Option<InputLine>> {
        if self.terminal.active_screen()? == Screen::Alternate
            || !self.terminal.viewport_active().unwrap_or(false)
        {
            return Ok(None);
        }
        let rows = u32::from(self.size.get().rows);
        let cursor = (self.terminal.cursor_x()?, u32::from(self.terminal.cursor_y()?));
        let row = |y: u32| {
            self.terminal
                .grid_ref(Point::Active(PointCoordinate { x: 0, y }))
                .and_then(|r| r.row())
        };
        let mut top = cursor.1;
        while top > 0 && row(top)?.is_wrap_continuation()? {
            top -= 1;
        }
        let mut bottom = cursor.1;
        while bottom + 1 < rows && row(bottom)?.is_wrapped()? {
            bottom += 1;
        }
        let cols = self.size.get().cols;
        let mut prompt_end = None;
        for y in top..=bottom {
            for x in 0..cols {
                let cell = self.terminal.grid_ref(Point::Active(PointCoordinate { x, y }))?.cell()?;
                if cell.semantic_content()? == CellSemanticContent::Prompt {
                    prompt_end = Some((y - top) as usize * usize::from(cols) + usize::from(x) + 1);
                }
            }
        }
        let at_prompt = match prompt_end {
            Some(_) => self.terminal.is_cursor_at_prompt()?,
            None => self.pty.foreground_is_shell(),
        };
        if !at_prompt {
            return Ok(None);
        }
        let frame = self.frame();
        let cells: Vec<&Cell> = (top..=bottom).flat_map(|y| frame.row(y as u16)).collect();
        let mut line = InputLine {
            top,
            bottom,
            cols: usize::from(frame.cols),
            spacer: cells.iter().map(|c| c.spacer).collect(),
            end: cells.iter().rposition(|c| !c.text.is_empty()).map_or(0, |i| i + 1),
            start: prompt_end.unwrap_or(0),
            cursor: 0,
        };
        line.cursor = line.index(cursor.0, cursor.1).unwrap_or(0).min(line.spacer.len());
        Ok(Some(line))
    }

    /// 往左（负数）或往右按 `steps` 下方向键的编码。
    fn arrow_keys(&mut self, steps: isize) -> Option<Vec<u8>> {
        let key = if steps < 0 { key::Key::ArrowLeft } else { key::Key::ArrowRight };
        Some(self.encode_key(key)?.repeat(steps.unsigned_abs()))
    }

    /// 不带修饰键按一下 `key` 的编码，跟随终端当前的键盘模式。
    fn encode_key(&mut self, key: key::Key) -> Option<Vec<u8>> {
        self.key_event
            .set_action(key::Action::Press)
            .set_key(key)
            .set_mods(key::Mods::empty())
            .set_consumed_mods(key::Mods::empty())
            .set_unshifted_codepoint('\0')
            .set_utf8(None::<String>);
        self.scratch.clear();
        let encoded = self
            .key_encoder
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&self.key_event, &mut self.scratch);
        log_err("key encode", encoded)?;
        (!self.scratch.is_empty()).then(|| self.scratch.clone())
    }

    /// 输入法上屏的文本，按原样发送。
    pub fn commit_text(&mut self, text: &str) {
        self.before_input();
        self.writer.write(text.as_bytes());
    }

    /// 按终端当前模式粘贴剪贴板文本（bracketed paste、粘贴事件等由 libghostty 处理）。
    ///
    /// 文本可能注入命令时（未开 bracketed paste 却含换行，或含 bracketed paste
    /// 结束序列），除非 `allow_unsafe`，否则什么也不写并返回 `Paste::NeedsConfirmation`，
    /// 由界面向用户确认后再带 `allow_unsafe` 重试。
    pub fn paste(&mut self, text: &str, allow_unsafe: bool) -> Paste {
        self.before_input();
        match self
            .terminal
            .paste_text(text, PasteSource::Clipboard, allow_unsafe)
        {
            Ok(_) => Paste::Done,
            Err(Error::Rejected) => Paste::NeedsConfirmation,
            Err(err) => {
                tracing::warn!("paste failed: {err}");
                Paste::Done
            }
        }
    }

    /// 滚轮输入：程序开启鼠标上报时发给程序，否则在回滚缓冲里滚动视口。
    pub fn scroll(&mut self, lines: isize, at: GridPoint, mods: Mods) {
        if lines == 0 {
            return;
        }
        if self.mouse_tracking() {
            let button = if lines < 0 {
                mouse::Button::Four
            } else {
                mouse::Button::Five
            };
            self.sync_mouse_encoder();
            self.scratch.clear();
            for _ in 0..lines.unsigned_abs() {
                if !self.encode_mouse(mouse::Action::Press, Some(button), at, ghostty_mods(mods)) {
                    return;
                }
            }
            self.writer.write(&self.scratch);
        } else {
            self.terminal.scroll_viewport(ScrollViewport::Delta(lines));
        }
    }

    /// 不开鼠标上报时按像素滚动回滚历史：`lines` 为正往回看，可以是零点几行。凑够整行的
    /// 部分挪视口，剩下的记在 `scroll_offset`，绘制时整屏往下错开这么多，露出视口上面那一行
    /// 的一部分。返回画面是否变了。
    pub fn scroll_smoothly(&mut self, lines: f32) -> bool {
        let top = |terminal: &Terminal<'static, 'static>| terminal.scrollbar().map_or(0, |s| s.offset);
        let before = (top(&self.terminal), self.scroll_offset);
        let mut offset = self.scroll_offset + lines;
        let whole = offset.floor();
        if whole != 0. {
            self.terminal.scroll_viewport(ScrollViewport::Delta(-(whole as isize)));
            // 到了历史顶上或者已经在底部时视口挪不动，按实际挪了多少扣。
            offset -= before.0 as f32 - top(&self.terminal) as f32;
        }
        // 视口上面没有行时不能往下错开；在底部继续往下滚时也不会错开成负的。
        let top = top(&self.terminal);
        self.scroll_offset = if top == 0 { 0. } else { offset.clamp(0., 0.999) };
        (top, self.scroll_offset) != before
    }

    /// 运行中的程序是否开启了鼠标上报。
    pub fn mouse_tracking(&self) -> bool {
        self.terminal.is_mouse_tracking().unwrap_or(false)
    }

    /// 把鼠标按键或移动按程序要求的上报格式发给它；程序没开上报时编码结果为空，什么都不发。
    /// 移动时 `button` 是按着的键，按键拖动模式只上报这时的移动。
    pub fn mouse_report(&mut self, action: MouseAction, button: Option<MouseButton>, at: GridPoint, mods: Mods) {
        let action = match action {
            MouseAction::Press => mouse::Action::Press,
            MouseAction::Release => mouse::Action::Release,
            MouseAction::Motion => mouse::Action::Motion,
        };
        let button = button.map(|button| match button {
            MouseButton::Left => mouse::Button::Left,
            MouseButton::Right => mouse::Button::Right,
            MouseButton::Middle => mouse::Button::Middle,
        });
        let mods = ghostty_mods(mods);
        self.sync_mouse_encoder();
        self.mouse_encoder
            .set_any_button_pressed(action != mouse::Action::Release && button.is_some());
        self.scratch.clear();
        if self.encode_mouse(action, button, at, mods) && !self.scratch.is_empty() {
            self.writer.write(&self.scratch);
        }
    }

    /// 编码一个鼠标事件，追加到 `scratch`；失败时记日志并返回 false。
    fn encode_mouse(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        at: GridPoint,
        mods: key::Mods,
    ) -> bool {
        let (x, y) = self.surface_position(at);
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mods)
            .set_position(mouse::Position { x: x as f32, y: y as f32 });
        match self.mouse_encoder.encode_to_vec(&self.mouse_event, &mut self.scratch) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("mouse encode failed: {err}");
                false
            }
        }
    }

    /// 让鼠标编码器跟上终端的上报模式和当前网格尺寸。
    fn sync_mouse_encoder(&mut self) {
        let size = self.size.get();
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            // 同一单元格里的移动不重复上报。
            .set_track_last_cell(true)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(size.cols) * u32::from(size.cell_width_px),
                screen_height: u32::from(size.rows) * u32::from(size.cell_height_px),
                cell_width: u32::from(size.cell_width_px),
                cell_height: u32::from(size.cell_height_px),
                padding_top: 0,
                padding_bottom: 0,
                padding_left: 0,
                padding_right: 0,
            });
    }

    /// 左键按下。手势按两次按下的间隔和距离数次数：单击清掉选区、从这里开始拖，
    /// 双击选词，三击选行。`repeat_interval` 是算作连击的最长间隔。
    pub fn select_press(&mut self, at: GridPoint, repeat_interval: Duration) {
        let (x, y) = self.surface_position(at);
        let size = self.size.get();
        let cell = self.viewport_cell(at);
        let s = &mut self.selecting;
        let result = self.terminal.grid_ref(Point::Viewport(cell)).and_then(|grid_ref| {
            let selection = s
                .press
                .set_repeat_distance(f64::from(size.cell_width_px))?
                .set_repeat_interval(repeat_interval)?
                .set_time(s.epoch.elapsed())?
                .set_position(x, y)?
                .apply(&mut s.gesture, &self.terminal, grid_ref)?;
            self.terminal.set_selection(selection.as_ref())?;
            Ok(())
        });
        log_err("selection press", result);
    }

    /// 按住左键拖动，扩展选区；`rectangle` 时选矩形块。返回是否拖到了网格上下边以外，
    /// 这时要定时调用 `select_autoscroll` 滚动视口。
    pub fn select_drag(&mut self, at: GridPoint, rectangle: bool) -> bool {
        let (x, y) = self.surface_position(at);
        let geometry = self.gesture_geometry();
        let cell = self.viewport_cell(at);
        let s = &mut self.selecting;
        s.pointer = (at, rectangle);
        let result = self.terminal.grid_ref(Point::Viewport(cell)).and_then(|grid_ref| {
            let selection = s
                .drag
                .set_rectangle(rectangle)?
                .set_position(x, y)?
                .apply(&mut s.gesture, &self.terminal, grid_ref, geometry)?;
            self.terminal.set_selection(selection.as_ref())?;
            Ok(s.gesture.autoscroll(&self.terminal)? != gesture::Autoscroll::None)
        });
        log_err("selection drag", result).unwrap_or(false)
    }

    /// 拖到网格外时的定时调用：视口滚一行，再按最后的指针位置扩展选区。返回这次是否滚动了。
    pub fn select_autoscroll(&mut self) -> bool {
        let (at, rectangle) = self.selecting.pointer;
        let (x, y) = self.surface_position(at);
        let geometry = self.gesture_geometry();
        let cell = self.viewport_cell(at);
        let s = &mut self.selecting;
        let result = s.gesture.autoscroll(&self.terminal).and_then(|autoscroll| {
            if autoscroll == gesture::Autoscroll::None {
                return Ok(false);
            }
            let selection = s
                .autoscroll
                .set_rectangle(rectangle)?
                .set_position(x, y)?
                .apply(&mut s.gesture, &self.terminal, cell, geometry)?;
            // 没有结果说明起点已不在当前屏幕上（比如切到了备用屏幕），原有选区保持不动。
            if let Some(selection) = &selection {
                self.terminal.set_selection(Some(selection))?;
            }
            Ok(selection.is_some())
        });
        log_err("selection autoscroll", result).unwrap_or(false)
    }

    /// 松开左键，结束这次拖动；选区留着。
    pub fn select_release(&mut self, at: GridPoint) {
        let size = self.size.get();
        let inside = (0. ..f32::from(size.cols)).contains(&at.x)
            && (0. ..f32::from(size.rows)).contains(&at.y);
        let cell = self.viewport_cell(at);
        let grid_ref = inside
            .then(|| self.terminal.grid_ref(Point::Viewport(cell)).ok())
            .flatten();
        let s = &mut self.selecting;
        log_err("selection release", s.release.apply(&mut s.gesture, &self.terminal, grid_ref));
    }

    /// 选区的纯文本：软换行处接起来，行尾空白去掉。没有选区时为 `None`。
    pub fn selection_text(&self) -> Option<String> {
        let options = FormatOptions::new()
            .with_emit_format(Format::Plain)
            .with_unwrap(true)
            .with_trim(true);
        log_err("selection format", self.terminal.format_selection_alloc(None, options))
            .flatten()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 选中屏幕和回滚历史里的全部内容。
    pub fn select_all(&mut self) {
        let result = self
            .terminal
            .select_all()
            .and_then(|selection| self.terminal.set_selection(selection.as_ref()).map(drop));
        log_err("select all", result);
    }

    /// 用键盘移动选区的终点，并让终点留在视野里。没有选区时返回 `false`，按键照常交给程序。
    pub fn adjust_selection(&mut self, adjustment: SelectionAdjust) -> bool {
        let adjustment = match adjustment {
            SelectionAdjust::Left => Adjustment::Left,
            SelectionAdjust::Right => Adjustment::Right,
            SelectionAdjust::Up => Adjustment::Up,
            SelectionAdjust::Down => Adjustment::Down,
            SelectionAdjust::PageUp => Adjustment::PageUp,
            SelectionAdjust::PageDown => Adjustment::PageDown,
            SelectionAdjust::Home => Adjustment::Home,
            SelectionAdjust::End => Adjustment::End,
        };
        let result = (|| {
            let Some(mut selection) = self.terminal.selection()? else {
                return Ok(None);
            };
            selection.adjust(&self.terminal, adjustment)?;
            self.terminal.set_selection(Some(&selection))?;
            self.reveal_row(&selection.end()).map(Some)
        })();
        match log_err("selection adjust", result).flatten() {
            Some(row) => {
                if let Some(row) = row {
                    self.scroll_to_row(row);
                }
                true
            }
            None => false,
        }
    }

    /// 把视口滚到选区开头；没有选区时什么都不做。
    pub fn scroll_to_selection(&mut self) {
        let result = (|| {
            let Some(selection) = self.terminal.selection()? else {
                return Ok(None);
            };
            self.reveal_row(&selection.start())
        })();
        if let Some(row) = log_err("scroll to selection", result).flatten() {
            self.scroll_to_row(row);
        }
    }

    /// `grid_ref` 不在视口里时它在整个屏幕（含回滚历史）中的行号；已经看得到时为 `None`。
    fn reveal_row(&self, grid_ref: &GridRef<'_>) -> libghostty_vt::error::Result<Option<u32>> {
        if self.terminal.point_from_grid_ref(grid_ref, PointSpace::Viewport)?.is_some() {
            return Ok(None);
        }
        Ok(self.terminal.point_from_grid_ref(grid_ref, PointSpace::Screen)?.map(|p| p.y))
    }

    /// 滚动视口，让屏幕第 `row` 行落在视口中间。
    fn scroll_to_row(&mut self, row: u32) {
        self.scroll_offset = 0.;
        let half = usize::from(self.size.get().rows / 2);
        self.terminal
            .scroll_viewport(ScrollViewport::Row((row as usize).saturating_sub(half)));
    }

    /// 滚动视口；`Page(n)` 按视口高度翻 n 页，负数往回翻。
    pub fn scroll_viewport(&mut self, scroll: ViewportScroll) {
        self.scroll_offset = 0.;
        let scroll = match scroll {
            ViewportScroll::Top => ScrollViewport::Top,
            ViewportScroll::Bottom => ScrollViewport::Bottom,
            ViewportScroll::Page(pages) => {
                ScrollViewport::Delta(pages * self.size.get().rows as isize)
            }
        };
        self.terminal.scroll_viewport(scroll);
    }

    /// 视口跳到上一个（`backward`）或下一个提示符所在的行，靠 shell 集成标在提示符上的记号。
    /// 往后没有提示符时回到底部。
    pub fn jump_to_prompt(&mut self, backward: bool) {
        self.scroll_offset = 0.;
        let result = self.terminal.scrollbar().map(|scrollbar| {
            let is_prompt = |y: u64| {
                self.terminal
                    .grid_ref(Point::Screen(PointCoordinate { x: 0, y: y as u32 }))
                    .and_then(|r| r.row())
                    .and_then(|r| r.semantic_prompt())
                    .is_ok_and(|p| p == RowSemanticPrompt::Prompt)
            };
            if backward {
                (0..scrollbar.offset).rev().find(|y| is_prompt(*y))
            } else {
                (scrollbar.offset + 1..scrollbar.total).find(|y| is_prompt(*y))
            }
        });
        match log_err("jump to prompt", result) {
            Some(Some(row)) => self.terminal.scroll_viewport(ScrollViewport::Row(row as usize)),
            Some(None) if !backward => self.terminal.scroll_viewport(ScrollViewport::Bottom),
            _ => {}
        }
    }

    /// 把字节直接发给程序，用于映射成控制字符的快捷键（比如 ⌘← 发 Ctrl-A）。
    pub fn send_text(&mut self, bytes: &[u8]) {
        self.before_input();
        self.writer.write(bytes);
    }

    /// 当前屏幕连同回滚历史的纯文本：软换行处接起来，行尾空白去掉。
    pub fn screen_text(&self) -> Option<String> {
        let options = FormatterOptions::new()
            .with_format(Format::Plain)
            .with_unwrap(true)
            .with_trim(true);
        let result = Formatter::new(&self.terminal, options)
            .and_then(|mut formatter| formatter.format_alloc(None).map(|bytes| bytes.to_vec()));
        log_err("format screen", result).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 搜索 `needle`，并选中最新的一个匹配（必要时滚过去）；`needle` 为空时清掉匹配。
    /// 还没在搜索时开始搜索。
    pub fn search(&mut self, needle: &str) {
        let result = self.try_search(needle);
        log_err("search", result);
    }

    fn try_search(&mut self, needle: &str) -> libghostty_vt::error::Result<()> {
        let search = match &mut self.search {
            Some(search) => search,
            None => self.search.insert(Search::new(&mut self.terminal)?),
        };
        if needle.is_empty() {
            search.clear_needle(&mut self.terminal)?;
        } else {
            search.set_needle(&mut self.terminal, needle)?;
            search.run(&mut self.terminal)?;
            search.select_next(&mut self.terminal)?;
        }
        Ok(())
    }

    /// 选中下一个（更旧的）匹配，`backward` 时选上一个（更新的）；必要时滚动视口。
    pub fn search_step(&mut self, backward: bool) {
        let Some(search) = &mut self.search else {
            return;
        };
        let result = search.run(&mut self.terminal).and_then(|()| {
            if backward {
                search.select_prev(&mut self.terminal)
            } else {
                search.select_next(&mut self.terminal)
            }
        });
        log_err("search step", result);
    }

    /// 上一帧的默认前景色和背景色，不触发刷新；画搜索栏这类界面元素时用。
    pub fn peek_colors(&self) -> (Rgb, Rgb) {
        let renderer = self.renderer.borrow();
        (renderer.frame.foreground, renderer.frame.background)
    }

    /// 搜索栏上显示的进度：选中的是第几个（从 0 数，没选中为 `None`）和匹配总数。
    pub fn search_status(&self) -> Option<(Option<usize>, usize)> {
        let search = self.search.as_ref()?;
        let selected = search.selected_index().ok().flatten();
        Some((selected, search.total_matches().unwrap_or(0)))
    }

    /// 关掉搜索，清掉高亮。
    pub fn end_search(&mut self) {
        self.search = None;
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

    fn gesture_geometry(&self) -> Geometry {
        let size = self.size.get();
        Geometry {
            columns: u32::from(size.cols.max(1)),
            cell_width: u32::from(size.cell_width_px.max(1)),
            padding_left: 0,
            screen_height: (u32::from(size.rows) * u32::from(size.cell_height_px)).max(1),
        }
    }

    /// 向程序发输入之前：记下时刻，回到最底部，并清掉选区。
    fn before_input(&mut self) {
        self.input_at = Some(Instant::now());
        if self.terminal.selection().is_ok_and(|s| s.is_some()) {
            log_err("selection clear", self.terminal.set_selection(None));
        }
        if !self.terminal.viewport_active().unwrap_or(true) {
            self.terminal.scroll_viewport(ScrollViewport::Bottom);
        }
        // 打字时回到底部对齐整行，不留半行错开。
        self.scroll_offset = 0.;
    }

    /// 清屏（⌘K）：清掉屏幕和回滚历史。备用屏幕归全屏程序（vim、less 等）自己管，不动。
    ///
    /// 前台是 shell 时它多半停在提示符，整屏清掉后发一个 FF（Ctrl-L）让它在顶上重画提示符，
    /// 已经敲了一半的命令也会保留。前台在跑别的程序时不能给它塞 FF，只删掉光标以上的行，
    /// 光标所在行顶到第一行。
    pub fn clear_screen(&mut self) {
        if self.terminal.active_screen().is_ok_and(|s| s == Screen::Alternate) {
            return;
        }
        self.before_input();
        if self.pty.foreground_is_shell() {
            // ED 3 放在最后：先 ED 2 时被推进回滚历史的内容也一起清掉。
            self.terminal.vt_write(b"\x1b[H\x1b[2J\x1b[3J");
            self.writer.write(b"\x0c");
        } else {
            self.clear_above_cursor();
        }
    }

    /// 清掉回滚历史和光标以上的行，光标所在行及以下顶到最上面，光标留在原来的列。
    fn clear_above_cursor(&mut self) {
        let x = self.terminal.cursor_x().unwrap_or(0);
        let y = self.terminal.cursor_y().unwrap_or(0);
        // DL 从第一行起删掉 y 行，下面的内容跟着上移。
        let seq = if y > 0 {
            format!("\x1b[3J\x1b[H\x1b[{y}M\x1b[1;{}H", x + 1)
        } else {
            "\x1b[3J".to_owned()
        };
        self.terminal.vt_write(seq.as_bytes());
    }

    /// 运行中的程序是否正为同步更新冻结屏幕；即使没有新输出，
    /// 过了 `SYNC_OUTPUT_TIMEOUT` 视图也必须重绘。
    pub fn render_held(&self) -> bool {
        self.held_since.get().is_some()
    }

    /// 取出最新的帧，让绘制方能同时持有其他可变状态。用完用 `restore_frame` 放回。
    pub fn take_frame(&mut self) -> Frame {
        self.sync_frame();
        std::mem::take(&mut self.renderer.borrow_mut().frame)
    }

    pub fn restore_frame(&mut self, frame: Frame) {
        self.renderer.borrow_mut().frame = frame;
    }

    pub fn frame(&mut self) -> std::cell::Ref<'_, Frame> {
        self.sync_frame();
        std::cell::Ref::map(self.renderer.borrow(), |r| &r.frame)
    }

    fn sync_frame(&mut self) {
        if let Some(since) = self.held_since.get() {
            if since.elapsed() < SYNC_OUTPUT_TIMEOUT {
                return;
            }
            // 程序崩溃或忘了释放时，不能让屏幕永远冻结。
            if let Err(err) = self.terminal.set_mode(Mode::SYNC_OUTPUT, false) {
                tracing::warn!("failed to end synchronized output: {err}");
            }
            self.held_since.set(None);
        }
        let highlights = match &mut self.search {
            Some(search) => log_err("search highlights", search_highlights(search, &mut self.terminal))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let mut renderer = self.renderer.borrow_mut();
        if renderer.highlights != highlights {
            renderer.highlights = highlights;
            renderer.force_full = true;
        }
        if let Err(err) = renderer.refresh(&self.terminal) {
            tracing::warn!("render state update failed: {err}");
        }
        let frame = &mut renderer.frame;
        frame.scroll_offset = if frame.above.is_empty() { 0. } else { self.scroll_offset };
    }
}

/// 让搜索追上终端的最新内容，再把视口里的匹配换算成逐行的高亮段。
fn search_highlights(
    search: &mut Search<'static>,
    terminal: &mut Terminal<'static, 'static>,
) -> libghostty_vt::error::Result<Vec<Highlight>> {
    search.feed(terminal)?;
    search.run(terminal)?;
    let terminal = &*terminal;
    let cols = terminal.cols()?;
    let rows = terminal.rows()?;
    let selected = search.selected_match(terminal)?;
    let mut highlights = Vec::new();
    for found in search.viewport_matches(terminal)? {
        let start = terminal.point_from_grid_ref(&found.start(), PointSpace::Viewport)?;
        let end = terminal.point_from_grid_ref(&found.end(), PointSpace::Viewport)?;
        // 一端在视口外的匹配按视口边缘截断；两端都在外面的不画。
        let ((x0, y0), (x1, y1)) = match (start, end) {
            (None, None) => continue,
            (start, end) => (
                start.map_or((0, 0), |p| (p.x, p.y)),
                end.map_or((cols.saturating_sub(1), u32::from(rows.saturating_sub(1))), |p| (p.x, p.y)),
            ),
        };
        let is_selected = match &selected {
            Some(selected) => selected.equals(terminal, &found)?,
            None => false,
        };
        for y in y0..=y1.min(u32::from(rows.saturating_sub(1))) {
            highlights.push(Highlight {
                y: y as u16,
                x0: if y == y0 { x0 } else { 0 },
                x1: if y == y1 { x1 } else { cols.saturating_sub(1) },
                selected: is_selected,
            });
        }
    }
    Ok(highlights)
}

impl Renderer {
    fn refresh(&mut self, terminal: &Terminal<'static, '_>) -> libghostty_vt::error::Result<()> {
        let snapshot = self.render_state.update(terminal)?;
        let dirty = snapshot.dirty()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let colors = snapshot.colors()?;
        let background = rgb(colors.background);
        let foreground = rgb(colors.foreground);

        // VT 里的光标色（配置的固定色，或程序用 OSC 12 设的）优先。
        let cursor_colors = (
            colors.cursor.map(|color| TerminalColor::Rgb(rgb(color))).or(self.cursor_color),
            self.cursor_text,
        );

        let frame = &mut self.frame;
        let reshaped = frame.cols != cols || frame.rows != rows;
        let recolored = frame.background != background || frame.foreground != foreground;
        if dirty == Dirty::Clean && !reshaped && !recolored && !self.force_full {
            // 只改光标形状或闪烁（DECSCUSR、DEC 模式 12）不会让 render state 变脏，光标要每次都重读。
            frame.cursor = read_cursor(&snapshot, frame, cursor_colors)?;
            return Ok(());
        }
        if reshaped {
            frame.cols = cols;
            frame.rows = rows;
            frame.cells = vec![Cell::default(); usize::from(cols) * usize::from(rows)];
        }
        // 视口上面那一行这次有没有取到；刚出现时它的脏标记不一定反映我们这边是空的，要整行读。
        let above_len = if snapshot.overscan()?.above > 0 { usize::from(cols) } else { 0 };
        let above_fresh = frame.above.len() != above_len;
        if above_fresh {
            frame.above = vec![Cell::default(); above_len];
        }
        frame.background = background;
        frame.foreground = foreground;
        let full = dirty == Dirty::Full || reshaped || recolored || std::mem::take(&mut self.force_full);

        let mut row_it = self.row_it.update(&snapshot)?;
        while let Some(row) = row_it.next() {
            // 视口里的行从 0 数，视口上面那一行是 -1。
            let y = row.viewport_y()?;
            if y >= i32::from(rows) {
                break;
            }
            let cells = match usize::try_from(y) {
                Ok(y) => &mut frame.cells[y * usize::from(cols)..(y + 1) * usize::from(cols)],
                Err(_) => &mut frame.above[..],
            };
            if cells.is_empty() {
                continue;
            }
            if full || (y < 0 && above_fresh) || row.dirty()? {
                let selection = row.selection()?;
                let mut cell_it = self.cell_it.update(row)?;
                let mut x = 0usize;
                while let Some(cell) = cell_it.next() {
                    if x >= usize::from(cols) {
                        break;
                    }
                    let out = &mut cells[x];
                    // 一次批量读取拿到渲染所需的全部字段，比逐个 getter 少几次 FFI。
                    let read = cell.read(&colors.palette, &mut out.text)?;
                    out.wide = read.wide == CellWide::Wide;
                    out.spacer = matches!(read.wide, CellWide::SpacerTail | CellWide::SpacerHead);
                    let mut fg = read.fg_color.map_or(foreground, rgb);
                    let mut bg = read.bg_color.map(rgb);
                    out.attrs = Attrs::default();
                    if read.has_styling {
                        let style = read.style;
                        out.attrs = Attrs {
                            bold: style.bold,
                            italic: style.italic,
                            faint: style.faint,
                            underline: style.underline != Underline::None,
                            strikethrough: style.strikethrough,
                        };
                        if style.inverse {
                            let swapped = bg.unwrap_or(background);
                            bg = Some(fg);
                            fg = swapped;
                        }
                        if style.invisible {
                            out.text.clear();
                        }
                    }
                    let x16 = x as u16;
                    // 宽字符的右半格跟着左半格：匹配的终点只落在宽字符的头格上。
                    let tail = read.wide == CellWide::SpacerTail;
                    let highlight = self.highlights.iter().find(|h| {
                        i32::from(h.y) == y
                            && h.x0 <= x16
                            && (x16 <= h.x1 || (tail && x16 == h.x1 + 1))
                    });
                    if let Some(highlight) = highlight {
                        let cell_bg = bg.unwrap_or(background);
                        let (hl_bg, hl_fg) = self.search_colors[usize::from(highlight.selected)];
                        bg = Some(resolve(hl_bg, fg, cell_bg));
                        fg = resolve(hl_fg, fg, cell_bg);
                    }
                    // 选中的单元格用配置的选区颜色。没配底色时用统一的蓝色、文字不变：
                    // 逐格反色会让彩色文字变成一块块彩色底，看起来像高亮而不像选区。
                    out.selected = selection.is_some_and(|s| s.start_x <= x16 && x16 <= s.end_x);
                    if out.selected {
                        let cell_bg = bg.unwrap_or(background);
                        let text = fg;
                        bg = Some(match self.selection_bg {
                            Some(c) => resolve(c, text, cell_bg),
                            None => default_selection_bg(background),
                        });
                        fg = match (self.selection_fg, self.selection_bg) {
                            (Some(c), _) => resolve(c, text, cell_bg),
                            (None, Some(_)) => cell_bg,
                            (None, None) => text,
                        };
                    }
                    out.fg = fg;
                    out.bg = bg;
                    x += 1;
                }
                row.set_dirty(false)?;
            }
        }

        frame.cursor = read_cursor(&snapshot, frame, cursor_colors)?;
        snapshot.set_dirty(Dirty::Clean)?;
        Ok(())
    }
}

/// 两个口令是否相同。逐字节比完全部内容再下结论，耗时不随第一个不同字节的位置变化，
/// 不会因此泄露口令的内容。
fn same_token(expected: &[u8], given: &[u8]) -> bool {
    expected.len() == given.len() && expected.iter().zip(given).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// 解开百分号编码；`%` 后面不是两位十六进制数时原样保留。
fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (bytes.get(i + 1).and_then(|&b| hex(b)), bytes.get(i + 2).and_then(|&b| hex(b)))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// 记日志并吞掉错误：鼠标和选区操作失败时只影响这一下，不该打断输入。
fn log_err<T>(what: &str, result: libghostty_vt::error::Result<T>) -> Option<T> {
    result.inspect_err(|err| tracing::warn!("{what} failed: {err}")).ok()
}

/// 把配置的颜色按单元格的前景、背景色解析成具体值。
fn default_selection_bg(background: Rgb) -> Rgb {
    let Rgb(r, g, b) = background;
    let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luma < 128. { crate::theme::SELECTION_ON_DARK } else { crate::theme::SELECTION_ON_LIGHT }
}

fn resolve(color: TerminalColor, fg: Rgb, bg: Rgb) -> Rgb {
    match color {
        TerminalColor::Rgb(color) => color,
        TerminalColor::CellForeground => fg,
        TerminalColor::CellBackground => bg,
    }
}

// 下面几个函数在共用的数据类型和 libghostty 的类型之间转换：对外只用前者，调 libghostty 时才换。

fn rgb(color: RgbColor) -> Rgb {
    Rgb(color.r, color.g, color.b)
}

fn ghostty_rgb(Rgb(r, g, b): Rgb) -> RgbColor {
    RgbColor { r, g, b }
}

fn ghostty_cursor_style(style: settings::CursorStyle) -> CursorStyle {
    match style {
        settings::CursorStyle::Block => CursorStyle::Block,
        settings::CursorStyle::BlockHollow => CursorStyle::BlockHollow,
        settings::CursorStyle::Bar => CursorStyle::Bar,
        settings::CursorStyle::Underline => CursorStyle::Underline,
    }
}

fn ghostty_mods(mods: Mods) -> key::Mods {
    let mut out = key::Mods::empty();
    for (on, flag) in [
        (mods.shift, key::Mods::SHIFT),
        (mods.ctrl, key::Mods::CTRL),
        (mods.alt, key::Mods::ALT),
        (mods.right_alt, key::Mods::ALT_SIDE),
    ] {
        if on {
            out |= flag;
        }
    }
    out
}

fn ghostty_key(key: Key) -> key::Key {
    match key {
        Key::Unidentified => key::Key::Unidentified,
        Key::A => key::Key::A,
        Key::B => key::Key::B,
        Key::C => key::Key::C,
        Key::D => key::Key::D,
        Key::E => key::Key::E,
        Key::F => key::Key::F,
        Key::G => key::Key::G,
        Key::H => key::Key::H,
        Key::I => key::Key::I,
        Key::J => key::Key::J,
        Key::K => key::Key::K,
        Key::L => key::Key::L,
        Key::M => key::Key::M,
        Key::N => key::Key::N,
        Key::O => key::Key::O,
        Key::P => key::Key::P,
        Key::Q => key::Key::Q,
        Key::R => key::Key::R,
        Key::S => key::Key::S,
        Key::T => key::Key::T,
        Key::U => key::Key::U,
        Key::V => key::Key::V,
        Key::W => key::Key::W,
        Key::X => key::Key::X,
        Key::Y => key::Key::Y,
        Key::Z => key::Key::Z,
        Key::Digit0 => key::Key::Digit0,
        Key::Digit1 => key::Key::Digit1,
        Key::Digit2 => key::Key::Digit2,
        Key::Digit3 => key::Key::Digit3,
        Key::Digit4 => key::Key::Digit4,
        Key::Digit5 => key::Key::Digit5,
        Key::Digit6 => key::Key::Digit6,
        Key::Digit7 => key::Key::Digit7,
        Key::Digit8 => key::Key::Digit8,
        Key::Digit9 => key::Key::Digit9,
        Key::Minus => key::Key::Minus,
        Key::Equal => key::Key::Equal,
        Key::BracketLeft => key::Key::BracketLeft,
        Key::BracketRight => key::Key::BracketRight,
        Key::Backslash => key::Key::Backslash,
        Key::Semicolon => key::Key::Semicolon,
        Key::Quote => key::Key::Quote,
        Key::Comma => key::Key::Comma,
        Key::Period => key::Key::Period,
        Key::Slash => key::Key::Slash,
        Key::Backquote => key::Key::Backquote,
        Key::Space => key::Key::Space,
        Key::Enter => key::Key::Enter,
        Key::Tab => key::Key::Tab,
        Key::Backspace => key::Key::Backspace,
        Key::Escape => key::Key::Escape,
        Key::Delete => key::Key::Delete,
        Key::Insert => key::Key::Insert,
        Key::Home => key::Key::Home,
        Key::End => key::Key::End,
        Key::PageUp => key::Key::PageUp,
        Key::PageDown => key::Key::PageDown,
        Key::ArrowUp => key::Key::ArrowUp,
        Key::ArrowDown => key::Key::ArrowDown,
        Key::ArrowLeft => key::Key::ArrowLeft,
        Key::ArrowRight => key::Key::ArrowRight,
        Key::F1 => key::Key::F1,
        Key::F2 => key::Key::F2,
        Key::F3 => key::Key::F3,
        Key::F4 => key::Key::F4,
        Key::F5 => key::Key::F5,
        Key::F6 => key::Key::F6,
        Key::F7 => key::Key::F7,
        Key::F8 => key::Key::F8,
        Key::F9 => key::Key::F9,
        Key::F10 => key::Key::F10,
        Key::F11 => key::Key::F11,
        Key::F12 => key::Key::F12,
    }
}

/// 视口里可见的光标；`frame` 的单元格和默认颜色须已是最新，用来判断光标是否落在
/// 宽字符上，以及解析跟随单元格的颜色。第三个参数是配置的光标色和光标下文字色，
/// 没配时分别用前景色和背景色。
fn read_cursor(
    snapshot: &Snapshot<'_, '_>,
    frame: &Frame,
    (color, text): (Option<TerminalColor>, Option<TerminalColor>),
) -> libghostty_vt::error::Result<Option<Cursor>> {
    if !snapshot.cursor_visible()? {
        return Ok(None);
    }
    let Some(vp) = snapshot.cursor_viewport()? else {
        return Ok(None);
    };
    let shape = match snapshot.cursor_visual_style()? {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
        _ => CursorShape::Block,
    };
    let index = usize::from(vp.y) * usize::from(frame.cols) + usize::from(vp.x);
    let cell = frame.cells.get(index);
    let cell_fg = cell.map_or(frame.foreground, |c| c.fg);
    let cell_bg = cell.and_then(|c| c.bg).unwrap_or(frame.background);
    Ok(Some(Cursor {
        x: vp.x,
        y: vp.y,
        shape,
        color: color.map_or(frame.foreground, |c| resolve(c, cell_fg, cell_bg)),
        text: text.map_or(frame.background, |c| resolve(c, cell_fg, cell_bg)),
        wide: cell.is_some_and(|c| c.wide),
        blinking: snapshot.cursor_blinking()?,
    }))
}

/// `Session::paste` 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paste {
    Done,
    /// 内容可能直接执行命令，需要用户确认。
    NeedsConfirmation,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, theme};
    use futures::{StreamExt as _, executor::block_on};

    fn row_text(frame: &Frame, y: u16) -> String {
        frame
            .row(y)
            .iter()
            .filter(|c| !c.spacer)
            .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// 端到端驱动真实 shell 经过 PTY 和 libghostty-vt：按键送达子进程，
    /// 彩色和宽字符输出落进帧，子进程退出时读到 EOF。
    #[test]
    fn shell_round_trip() {
        let size = GridSize {
            cols: 40,
            rows: 10,
            cell_width_px: 8,
            cell_height_px: 16,
        };
        let (mut session, mut rx) = Session::spawn_shell(size, Some("/bin/sh")).unwrap();
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
        block_on(async {
            while let Some(event) = rx.next().await {
                match event {
                    PtyEvent::Output(data) => {
                        session.feed(&data);
                    }
                    PtyEvent::Exited => break,
                }
            }
        });

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
    }

    fn idle_session() -> Session {
        let size = GridSize {
            cols: 20,
            rows: 4,
            cell_width_px: 8,
            cell_height_px: 16,
        };
        // `cat` 自己不输出，VT 只会收到测试喂进去的内容。
        let mut session = Session::spawn_shell(size, Some("/bin/cat")).unwrap().0;
        session.apply_config(&Config::default().term_settings());
        session
    }

    #[test]
    fn synchronized_output_freezes_the_frame_until_released() {
        let mut session = idle_session();
        session.feed(b"before");
        assert_eq!(row_text(&session.frame(), 0), "before");

        session.feed(b"\x1b[?2026h\r\x1b[2Kafter");
        assert!(session.render_held());
        assert_eq!(row_text(&session.frame(), 0), "before", "held frame must not change");

        session.feed(b"\x1b[?2026l");
        assert!(!session.render_held());
        assert_eq!(row_text(&session.frame(), 0), "after");
    }

    fn scrollback_rows(session: &Session) -> u64 {
        let scrollbar = session.terminal.scrollbar().unwrap();
        scrollbar.total - scrollbar.len
    }

    #[test]
    fn clear_screen_at_the_shell_clears_everything() {
        // `idle_session` 的「shell」就是前台进程，走 shell 那一支。
        let mut session = idle_session();
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n$ ls");
        assert!(scrollback_rows(&session) > 0);
        session.clear_screen();
        assert_eq!(scrollback_rows(&session), 0);
        let frame = session.frame();
        assert!((0..4).all(|y| row_text(&frame, y).is_empty()));
    }

    #[test]
    fn clear_above_cursor_keeps_the_cursor_row_at_the_top() {
        let mut session = idle_session();
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\nrunning");
        session.clear_above_cursor();
        assert_eq!(scrollback_rows(&session), 0);
        assert_eq!(session.terminal.cursor_y().unwrap(), 0);
        assert_eq!(session.terminal.cursor_x().unwrap(), 7);
        let frame = session.frame();
        assert_eq!(row_text(&frame, 0), "running");
        assert!((1..4).all(|y| row_text(&frame, y).is_empty()));
    }

    #[test]
    fn clear_screen_leaves_the_alternate_screen_alone() {
        let mut session = idle_session();
        session.feed(b"\x1b[?1049hvim");
        session.clear_screen();
        assert_eq!(row_text(&session.frame(), 0), "vim");
    }

    #[test]
    fn select_all_covers_the_scrollback() {
        let mut session = idle_session();
        session.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        session.select_all();
        assert_eq!(session.selection_text().as_deref(), Some("one\ntwo\nthree\nfour\nfive\nsix"));
    }

    #[test]
    fn shift_arrows_extend_an_existing_selection_only() {
        let mut session = idle_session();
        session.feed(b"helloworld");
        assert!(!session.adjust_selection(SelectionAdjust::Right));
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(4.8, 0.5), false);
        session.select_release(at(4.8, 0.5));
        assert_eq!(session.selection_text().as_deref(), Some("hello"));
        assert!(session.adjust_selection(SelectionAdjust::Right));
        assert_eq!(session.selection_text().as_deref(), Some("hellow"));
    }

    #[test]
    fn scrolling_the_viewport_by_page_and_to_the_ends() {
        let mut session = idle_session();
        for n in 0..20 {
            session.feed(format!("{n}\r\n").as_bytes());
        }
        session.scroll_viewport(ViewportScroll::Top);
        assert_eq!(row_text(&session.frame(), 0), "0");
        session.scroll_viewport(ViewportScroll::Page(1));
        assert_eq!(row_text(&session.frame(), 0), "4");
        session.scroll_viewport(ViewportScroll::Bottom);
        assert_eq!(row_text(&session.frame(), 0), "17");
    }

    #[test]
    fn smooth_scrolling_shifts_by_fractions_and_stops_at_the_ends() {
        let mut session = idle_session();
        for n in 0..20 {
            session.feed(format!("{n}\r\n").as_bytes());
        }
        // 在底部往回滚半行：视口不动，整屏错开半行，露出上面那一行。
        assert!(session.scroll_smoothly(0.5));
        let frame = session.frame();
        assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("17", 0.5));
        assert_eq!(frame.above.iter().map(|c| c.text.as_str()).collect::<String>().trim_end(), "16");
        drop(frame);
        // 再滚 0.75 行：凑够一行挪视口，剩下 0.25 行。
        session.scroll_smoothly(0.75);
        let frame = session.frame();
        assert_eq!(row_text(&frame, 0), "16");
        assert!((frame.scroll_offset - 0.25).abs() < 1e-6);
        drop(frame);
        // 往底部滚过头：回到底部，不留错开。
        session.scroll_smoothly(-5.);
        let frame = session.frame();
        assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("17", 0.));
        drop(frame);
        // 滚到历史顶上以后不能再错开。
        session.scroll_smoothly(100.5);
        let frame = session.frame();
        assert_eq!((row_text(&frame, 0).as_str(), frame.scroll_offset), ("0", 0.));
        assert!(frame.above.is_empty());
    }

    #[test]
    fn screen_text_includes_the_scrollback() {
        let mut session = idle_session();
        session.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        assert_eq!(session.screen_text().as_deref(), Some("one\ntwo\nthree\nfour\nfive"));
    }

    #[test]
    fn search_highlights_matches_and_steps_through_them() {
        let mut session = idle_session();
        session.feed(b"error one\r\nok\r\nerror two");
        session.search("error");
        // 先选中最新的那个。
        assert_eq!(session.search_status(), Some((Some(0), 2)));
        let frame = session.frame();
        let selected = theme::SEARCH_SELECTED_BACKGROUND;
        let other = theme::SEARCH_BACKGROUND;
        assert_eq!(frame.row(2)[0].bg, Some(selected));
        assert_eq!(frame.row(2)[4].bg, Some(selected));
        assert_eq!(frame.row(2)[5].bg, None);
        assert_eq!(frame.row(0)[0].bg, Some(other));
        assert_eq!(frame.row(1)[0].bg, None);
        drop(frame);

        session.search_step(false);
        assert_eq!(session.search_status(), Some((Some(1), 2)));
        assert_eq!(session.frame().row(0)[0].bg, Some(selected));

        // 新输出里的匹配也会算进来。
        session.feed(b"\r\nerror three");
        session.frame();
        assert_eq!(session.search_status().map(|(_, total)| total), Some(3));

        session.end_search();
        assert_eq!(session.search_status(), None);
        assert_eq!(session.frame().row(0)[0].bg, None);
    }

    #[test]
    fn search_highlight_covers_both_halves_of_a_trailing_wide_char() {
        let mut session = idle_session();
        session.feed("a你好b".as_bytes());
        session.search("你好");
        let frame = session.frame();
        let selected = Some(theme::SEARCH_SELECTED_BACKGROUND);
        let colored: Vec<bool> = (0..6).map(|x| frame.row(0)[x].bg == selected).collect();
        assert_eq!(colored, [false, true, true, true, true, false]);
    }

    #[test]
    fn click_to_move_counts_characters_on_the_cursor_line() {
        let mut session = idle_session();
        // 光标停在 "$ 你好 world" 末尾（第 12 列，两个宽字符各占两列）。
        session.feed("$ 你好 world".as_bytes());
        // 点在第 7 列的 w 上：往左跨过 "world" 五个字符。
        assert_eq!(session.click_to_move_steps(at(7.5, 0.5)), Some(-5));
        // 点在「你」上：宽字符各算一步，"你好 world" 共八步。
        assert_eq!(session.click_to_move_steps(at(2.5, 0.5)), Some(-8));
        // 文字末尾之后的空白、别的行都不动。
        assert_eq!(session.click_to_move_steps(at(17.5, 0.5)), None);
        assert_eq!(session.click_to_move_steps(at(3.5, 2.5)), None);
    }

    #[test]
    fn click_to_move_follows_soft_wraps() {
        let mut session = idle_session();
        // 20 列宽：24 个字符折成两行，光标在第二行第 4 列。
        session.feed(b"abcdefghijklmnopqrstuvwx");
        assert_eq!(session.click_to_move_steps(at(2.5, 0.5)), Some(-22));
        session.feed(b"\x1b[?1049h");
        assert_eq!(session.click_to_move_steps(at(2.5, 0.5)), None);
    }

    #[test]
    fn deleting_a_selection_moves_to_its_end_and_backspaces_over_it() {
        let mut session = idle_session();
        // 光标在末尾第 12 列；选中「好 w」（第 4 到 7 列，宽字符「好」占 4、5 两列）。
        session.feed("$ 你好 world".as_bytes());
        session.select_press(at(4.2, 0.5), REPEAT);
        session.select_drag(at(7.8, 0.5), false);
        session.select_release(at(7.8, 0.5));
        assert_eq!(session.selection_text().as_deref(), Some("好 w"));
        // 从第 12 列往左走到 w 之后（"orld" 四步），再退格三个字符。
        assert_eq!(session.delete_selection_keys().unwrap(), Some((-4, 3)));
    }

    #[test]
    fn deleting_a_selection_ignores_the_blank_after_the_text_and_other_lines() {
        let mut session = idle_session();
        session.feed(b"out\r\n$ ab");
        // 只选了文字后面的空白：没有可删的字符。
        session.select_press(at(8.2, 1.5), REPEAT);
        session.select_drag(at(12.8, 1.5), false);
        session.select_release(at(12.8, 1.5));
        assert_eq!(session.delete_selection_keys().unwrap(), None);
        // 选在上面的输出行里：不在输入行上。
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(2.8, 0.5), false);
        session.select_release(at(2.8, 0.5));
        assert_eq!(session.delete_selection_keys().unwrap(), None);
    }

    #[test]
    fn agent_status_prefix_is_split_from_the_title() {
        let mut session = idle_session();
        assert!(session.feed("\x1b]0;⠋ 美化图标 | runode\x07".as_bytes()));
        assert_eq!(session.title.as_deref(), Some("美化图标 | runode"));
        assert_eq!(session.agent, Some(Agent { kind: AgentKind::Codex, state: AgentState::Working }));
        // 转圈换一帧不算标题变化。
        assert!(!session.feed("\x1b]0;⠙ 美化图标 | runode\x07".as_bytes()));
        assert!(session.feed("\x1b]0;✳ 美化图标\x07".as_bytes()));
        assert_eq!(session.title.as_deref(), Some("美化图标"));
        assert_eq!(session.agent, Some(Agent { kind: AgentKind::Claude, state: AgentState::Idle }));
        // 前台已经回到 shell（这里的 `cat`），不带前缀的标题不再算 agent 的。
        assert!(session.feed(b"\x1b]0;~\x07"));
        assert_eq!(session.agent, None);
    }

    #[test]
    fn progress_report_marks_the_agent_working() {
        let mut session = idle_session();
        let pi_working = Some(Agent { kind: AgentKind::Pi, state: AgentState::Working });
        assert!(session.feed("\x1b]0;π - runode\x07\x1b]9;4;3\x07".as_bytes()));
        assert_eq!(session.agent, pi_working);
        // 工作中的保活重发和不带前缀的新标题都不改变状态。
        assert!(!session.feed(b"\x1b]9;4;3\x07"));
        assert!(session.feed("\x1b]0;π - 问候 - runode\x07".as_bytes()));
        assert_eq!(session.agent, pi_working);
        // 前台是 shell（这里的 `cat`）时清掉进度，就不再算 agent。
        assert!(session.feed(b"\x1b]9;4;0\x07"));
        assert_eq!(session.agent, None);
    }

    /// shell 集成标出的提示符：`$ ` 是提示符，后面是用户输入。
    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

    #[test]
    fn marked_prompts_bound_clicks_and_deletes_to_the_input() {
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(b"hello");
        // 点在提示符上：最多退到输入开头（第 2 列），"hello" 五步。
        assert_eq!(session.click_to_move_steps(at(0.5, 0.5)), Some(-5));
        // 选区把提示符也选进去了：只删输入里的 "hel"。
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(4.8, 0.5), false);
        session.select_release(at(4.8, 0.5));
        assert_eq!(session.delete_selection_keys().unwrap(), Some((-2, 3)));
    }

    #[test]
    fn marked_prompts_tell_when_a_command_is_running() {
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(b"sleep 9\r\n\x1b]133;C\x07");
        // 命令在跑：光标在输出区，不算停在提示符上。
        assert_eq!(session.click_to_move_steps(at(0.5, 1.5)), None);
    }

    #[test]
    fn finished_commands_carry_the_prompt_directory_and_exit_code() {
        let mut session = reporting_session();
        session.feed(PROMPT);
        assert!(session.take_commands().is_empty());
        let cwd = session.prompt_cwd();
        assert_eq!(session.prompt_input().map(|input| input.text), Some(String::new()));
        session.feed(b"git st");
        let input = session.prompt_input().unwrap();
        assert_eq!((input.before_cursor(), input.at_end), ("git st", true));
        // 报告没带原文，从屏幕上读。
        session.feed(format!("atus\r\n\x1b]6973;{TOKEN};command=\x07\x1b]133;C\x07clean\r\n").as_bytes());
        // 命令还在跑，没有结束报告。
        assert!(session.take_commands().is_empty());
        assert_eq!(session.prompt_input(), None);
        session.feed(b"\x1b]133;D;1\x07");
        let commands = session.take_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!((commands[0].cmd.as_str(), commands[0].exit, &commands[0].cwd), ("git status", Some(1), &cwd));
        // 没在运行命令时的结束报告（有的 shell 每次出提示符都发）不算。
        session.feed(b"\x1b]133;D;0\x07");
        assert!(session.take_commands().is_empty());
    }

    /// 测试里用的报告口令。
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// 一个持有 `TOKEN` 的会话，就像启动 shell 时注入了集成一样。
    fn reporting_session() -> Session {
        let session = idle_session();
        *session.effects.report_token.borrow_mut() = Some(TOKEN.into());
        session
    }

    #[test]
    fn the_shell_reports_its_path() {
        let mut session = reporting_session();
        assert_eq!(session.shell_path(), None);
        session.feed(format!("\x1b]6973;{TOKEN};path=/usr/bin%3A/opt/my%20bin\x07").as_bytes());
        assert_eq!(session.shell_path(), Some("/usr/bin:/opt/my bin".into()));
        // 别的私有 OSC 不算。
        session.feed(format!("\x1b]69730;{TOKEN};path=/x\x07\x1b]6973;{TOKEN};other=1\x1b\\").as_bytes());
        assert_eq!(session.shell_path(), Some("/usr/bin:/opt/my bin".into()));
        // 各种名字，空格编码成 %20；很长的列表也收得下。
        let functions: Vec<String> = (0..3000).map(|i| format!("function_number_{i}")).collect();
        let mut report = format!("\x1b]6973;{TOKEN};functions=").into_bytes();
        report.extend(functions.join("%20").bytes());
        report.extend(
            format!(
                "\x07\x1b]6973;{TOKEN};aliases=ll%20gs\x07\x1b]6973;{TOKEN};alias_values=ll%09ls%20-l%0Ags%09git%20status%0A\x07"
            )
            .bytes(),
        );
        session.feed(&report);
        let names = session.shell_names();
        assert_eq!(names.aliases, ["ll", "gs"]);
        assert_eq!(names.alias_values, [("ll".into(), "ls -l".into()), ("gs".into(), "git status".into())]);
        assert_eq!(names.functions, functions);
        assert!(names.builtins.is_empty());
        assert_eq!(percent_decode(b"a%2fb%zz%4"), b"a/b%zz%4");
    }

    #[test]
    fn shell_reports_without_the_right_token_are_ignored() {
        let mut session = reporting_session();
        let wrong = "0123456789abcdef0123456789abcdee";
        // 口令不对、少一位、缺口令（旧格式）、空口令，都不采用。
        session.feed(format!("\x1b]6973;{wrong};path=/evil\x07").as_bytes());
        session.feed(format!("\x1b]6973;{};path=/evil\x07", &TOKEN[1..]).as_bytes());
        session.feed(b"\x1b]6973;path=/evil\x07\x1b]6973;;path=/evil\x07\x1b]6973;aliases=git\x07");
        assert_eq!(session.shell_path(), None);
        assert!(session.shell_names().aliases.is_empty());
        // 先伪造命令结束、回到提示符也没用。
        session.feed(format!("\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b]6973;{wrong};path=/evil\x07").as_bytes());
        assert_eq!(session.shell_path(), None);
        session.feed(format!("\x1b]6973;{TOKEN};path=/usr/bin\x07").as_bytes());
        assert_eq!(session.shell_path(), Some("/usr/bin".into()));

        // 没注入集成、没有口令的 shell 报告什么都不采用。
        let mut session = idle_session();
        session.feed(format!("\x1b]6973;{TOKEN};path=/evil\x07\x1b]6973;;path=/evil\x07").as_bytes());
        session.feed(b"\x1b]6973;path=/evil\x07");
        assert_eq!(session.shell_path(), None);

        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abc", b"abd") && !same_token(b"abc", b"ab") && !same_token(b"", b"a"));
    }

    #[test]
    fn the_command_line_sent_by_the_shell_wins_over_the_screen() {
        let mut session = reporting_session();
        session.feed(PROMPT);
        // 屏幕上只看得到一部分（比如被插件改写过），shell 报告的原文用百分号编码，分号、
        // 换行和 ESC 都原样还原；133;C 自带的原文不用。
        session.feed(
            format!("echo\r\n\x1b]6973;{TOKEN};command=echo%20a%3Bb%0Ac%1B\x07\x1b]133;C;cmdline_url=other\x07\x1b]133;D;0\x07")
                .as_bytes(),
        );
        let commands = session.take_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].cmd, "echo a;b\nc\x1b");
    }

    #[test]
    fn forged_command_starts_stay_out_of_the_history() {
        let mut session = reporting_session();
        let wrong = "0123456789abcdef0123456789abcdee";
        let forged = [
            // 程序输出里伪造整套提示符和带原文的命令开始。
            "\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07evil\r\n\x1b]133;C;cmdline_url=evil\x07\x1b]133;D;0\x07".to_owned(),
            // 口令不对的 command 报告不算。
            format!("\x1b]133;A\x07$ \x1b]133;B\x07evil\r\n\x1b]6973;{wrong};command=evil\x07\x1b]133;C\x07\x1b]133;D;0\x07"),
            // 真的报告之后先来了提示符或命令结束，就不能再被伪造的 133;C 借用。
            format!("\x1b]6973;{TOKEN};command=ls\x07\x1b]133;D;0\x07\x1b]133;C;cmdline_url=evil\x07\x1b]133;D;0\x07"),
            format!("\x1b]6973;{TOKEN};command=ls\x07\x1b]133;A\x07$ \x1b]133;B\x07evil\r\n\x1b]133;C\x07\x1b]133;D;0\x07"),
            format!("\x1b]6973;{TOKEN};command=ls\x07\x1b]133;B\x07evil\r\n\x1b]133;C\x07\x1b]133;D;0\x07"),
        ];
        for bytes in forged {
            session.feed(bytes.as_bytes());
            assert!(session.take_commands().is_empty(), "{bytes:?}");
        }
        // 没有口令的 shell 不记任何命令。
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(format!("ls\r\n\x1b]6973;{TOKEN};command=ls\x07\x1b]133;C\x07\x1b]133;D;0\x07").as_bytes());
        assert!(session.take_commands().is_empty());
    }

    #[test]
    fn jumping_between_marked_prompts() {
        let mut session = idle_session();
        for n in 0..3 {
            session.feed(PROMPT);
            session.feed(format!("cmd{n}\r\n\x1b]133;C\x07out\r\nout\r\n\x1b]133;D;0\x07").as_bytes());
        }
        session.feed(PROMPT);
        // 4 行高的视口最上面一行是 "$ cmd2"，往上跳到它之前的那个提示符。
        assert_eq!(row_text(&session.frame(), 0), "$ cmd2");
        session.jump_to_prompt(true);
        assert_eq!(row_text(&session.frame(), 0), "$ cmd1");
        session.jump_to_prompt(true);
        assert_eq!(row_text(&session.frame(), 0), "$ cmd0");
        session.jump_to_prompt(false);
        assert_eq!(row_text(&session.frame(), 0), "$ cmd1");
    }

    #[test]
    fn full_reset_clears_the_title() {
        let mut session = idle_session();
        assert!(session.feed(b"\x1b]2;hello\x07"));
        assert_eq!(session.title.as_deref(), Some("hello"));
        assert!(session.feed(b"\x1bc"));
        assert_eq!(session.title, None);
    }

    #[test]
    fn multiline_paste_needs_confirmation_unless_bracketed() {
        let mut session = idle_session();
        assert_eq!(session.paste("ls", false), Paste::Done);
        assert_eq!(session.paste("rm -rf x\nls", false), Paste::NeedsConfirmation);
        assert_eq!(session.paste("rm -rf x\nls", true), Paste::Done);

        // 程序开启 bracketed paste 后，换行不会被直接执行，无需确认。
        session.feed(b"\x1b[?2004h");
        assert_eq!(session.paste("a\nb", false), Paste::Done);
    }

    #[test]
    fn option_as_alt_follows_the_configured_side() {
        /// 按一次 Option+s（美式布局下打出 ß），返回编码结果。
        fn option_s(session: &mut Session, right: bool) -> Vec<u8> {
            let alt = Mods { alt: true, ..Mods::default() };
            session.key(&KeyInput {
                key: Key::S,
                mods: Mods { right_alt: right, ..alt },
                consumed_mods: alt,
                unshifted: 's',
                text: Some("ß".into()),
            });
            session.scratch.clone()
        }

        let mut session = idle_session();
        assert_eq!(option_s(&mut session, false), "ß".as_bytes());

        session.apply_config(&Config {
            macos_option_as_alt: settings::OptionAsAlt::Left,
            ..Config::default()
        }
        .term_settings());
        assert_eq!(option_s(&mut session, false), b"\x1bs");
        assert_eq!(option_s(&mut session, true), "ß".as_bytes());
    }

    #[test]
    fn default_colors_come_from_the_theme() {
        let mut session = idle_session();
        session.feed(b"\x1b[32mok\x1b[0m");
        let frame = session.frame();
        assert_eq!(frame.background, theme::BACKGROUND);
        assert_eq!(frame.foreground, theme::FOREGROUND);
        assert_eq!(frame.row(0)[0].fg, theme::ANSI[2]);
        assert_eq!(frame.cursor.map(|c| c.color), Some(theme::FOREGROUND));
    }

    const REPEAT: Duration = Duration::from_millis(500);

    fn at(x: f32, y: f32) -> GridPoint {
        GridPoint { x, y }
    }

    #[test]
    fn dragging_selects_text_and_highlights_it() {
        let mut session = idle_session();
        session.feed(b"hello world");
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(4.8, 0.5), false);
        session.select_release(at(4.8, 0.5));
        assert_eq!(session.selection_text().as_deref(), Some("hello"));
        // 没配选区颜色时用统一的蓝色底，文字保持原来的颜色。
        let frame = session.frame();
        assert_eq!(frame.row(0)[0].bg, Some(theme::SELECTION_ON_DARK));
        assert_eq!(frame.row(0)[0].fg, theme::FOREGROUND);
        assert!(frame.row(0)[0].selected && !frame.row(0)[6].selected);
        assert_eq!(frame.row(0)[6].bg, None);
        drop(frame);

        // 单击清掉选区。
        session.select_press(at(2.5, 1.5), REPEAT);
        session.select_release(at(2.5, 1.5));
        assert_eq!(session.selection_text(), None);
        assert_eq!(session.frame().row(0)[0].bg, None);
    }

    #[test]
    fn double_click_selects_a_word_and_typing_clears_it() {
        let mut session = idle_session();
        session.feed(b"hello world");
        for _ in 0..2 {
            session.select_press(at(7.5, 0.5), REPEAT);
            session.select_release(at(7.5, 0.5));
        }
        assert_eq!(session.selection_text().as_deref(), Some("world"));
        session.commit_text("x");
        assert_eq!(session.selection_text(), None);
    }

    #[test]
    fn selection_colors_can_follow_the_cell() {
        let mut session = idle_session();
        session.apply_config(&Config {
            selection_background: Some(TerminalColor::CellForeground),
            selection_foreground: Some(TerminalColor::Rgb(Rgb(1, 2, 3))),
            ..Config::default()
        }
        .term_settings());
        session.feed(b"\x1b[31mred\x1b[0m");
        session.select_press(at(0.2, 0.5), REPEAT);
        session.select_drag(at(2.8, 0.5), false);
        let cell = session.frame().row(0)[0].clone();
        assert_eq!(cell.bg, Some(theme::ANSI[1]));
        assert_eq!(cell.fg, Rgb(1, 2, 3));
    }

    #[test]
    fn cursor_colors_can_follow_the_cell() {
        let mut session = idle_session();
        session.apply_config(&Config {
            cursor_color: Some(TerminalColor::CellForeground),
            cursor_text: Some(TerminalColor::CellBackground),
            ..Config::default()
        }
        .term_settings());
        // 光标退回到红色的 X 上。
        session.feed(b"\x1b[31mX\x1b[0m\x1b[D");
        let cursor = session.frame().cursor.unwrap();
        assert_eq!(cursor.color, theme::ANSI[1]);
        assert_eq!(cursor.text, theme::BACKGROUND);

        // 程序用 OSC 12 设的光标色优先于跟随单元格。
        session.feed(b"\x1b]12;#010203\x07");
        let color = session.frame().cursor.unwrap().color;
        assert_eq!(color, Rgb(1, 2, 3));
    }

    #[test]
    fn cursor_blinks_unless_configured_or_steadied() {
        let blinking = |session: &mut Session| session.frame().cursor.map(|c| c.blinking);
        let mut session = idle_session();
        session.feed(b"ok");
        assert_eq!(blinking(&mut session), Some(true));
        // DECSCUSR 2：稳定的块状光标。
        session.feed(b"\x1b[2 q");
        assert_eq!(blinking(&mut session), Some(false));

        session.apply_config(&Config {
            cursor_style_blink: Some(false),
            ..Config::default()
        }
        .term_settings());
        session.feed(b"\x1b[0 q");
        assert_eq!(blinking(&mut session), Some(false));
    }
}
