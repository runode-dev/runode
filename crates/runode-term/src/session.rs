//! 一个终端：接到子 shell 上的 libghostty-vt 状态机。
//!
//! `Session` 持有 VT 状态并放在 UI 线程上，因为 `libghostty_vt::Terminal` 只能单线程
//! 使用。PTY 输出经 channel 到达（见 `Pty::spawn`），由 `feed` 喂进去；渲染器读取
//! `Frame`，`refresh` 只复制 libghostty 报告为脏的行来保持它最新。
//!
//! 这里是 `Session` 本身：创建、接上 PTY、注册 VT 回调、应用设置和改尺寸。其余按职责分在
//! 子模块里：VT 回调累积的变化（`effects`）、帧（`render`）、输入（`input`）、光标所在的
//! 输入行（`input_line`）、鼠标（`pointer`）、视口滚动（`scroll`）、选区（`selection`）、
//! 搜索（`search`），以及和 libghostty 类型之间的转换（`convert`）。

mod convert;
mod effects;
mod input;
mod input_line;
mod pointer;
mod render;
mod scroll;
mod search;
mod selection;
#[cfg(test)]
mod testing;

use std::{
    cell::{Cell as StdCell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use anyhow::Result;
use futures::channel::mpsc::UnboundedReceiver;
use libghostty_vt::{
    key::{self, OptionAsAlt},
    mouse,
    search::Search,
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, PointCoordinate,
        PrimaryDeviceAttributes, ProgressState, SecondaryDeviceAttributes, SemanticPrompt, SizeReportSize, Terminal,
        UnknownSequence,
    },
};

use runode_model::{
    agent::Agent,
    color::TerminalColor,
    frame::Frame,
    grid::{GridPoint, GridSize},
    settings::{self, TermSettings},
    shell::IntegrationMode,
};

use crate::{
    history, prompt_input,
    pty::{Pty, PtyEvent, PtyWriter},
};
use convert::{ghostty_cursor_style, ghostty_rgb};
use effects::{Effects, PromptEvent, SHELL_REPORT, UNKNOWN_SEQUENCE_MAX_BYTES};
pub use input::Paste;
use render::Renderer;
use selection::Selecting;

const SCROLLBACK_LINES: usize = 10_000;
/// 程序用同步输出（mode 2026）冻结屏幕的最长时间，超时后不再遵守，以免程序异常时画面卡死。
pub const SYNC_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

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
        let renderer = Rc::new(RefCell::new(Renderer::new()?));
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
            .on_xtversion(|_| Some(crate::XTVERSION))?
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
            selecting: Selecting::new()?,
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

    /// shell 当前所在的目录。
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        if !self.pty.started() {
            return self.start_dir.clone();
        }
        self.pty.shell_cwd()
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
    use crate::session::testing::*;
    use runode_model::input::{Key, KeyInput, Mods};
    use futures::{StreamExt as _, executor::block_on};

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
}
