//! 宿主这边的一个终端会话：权威的那份 VT 接在 shell 的 PTY 上。
//!
//! 终端查询（DA、DSR、XTVERSION、OSC 颜色、Kitty 键盘协议、mode 2048 的尺寸报告等）只由
//! 这里应答；标题、前台 agent、shell 集成的报告和命令历史也在这里认，变了就整理成
//! `SessionMeta` 交给界面。界面那份 VT（`Session`）只消费同样的字节流，自己不应答任何查询。
//!
//! 和 `Session` 一样不是 `Send`：libghostty 的 `Terminal` 只能单线程用，回调之间用 `Rc` 共享
//! 状态。宿主在会话自己的线程里建它，从不挪到别的线程。
//!
//! 这里是会话本身：创建、接上 PTY、注册 VT 回调、套用主题、改尺寸、写入和清屏。VT 回调累积的
//! 变化和 shell 集成的报告在 `effects`，前台 agent 的识别在 `detect`，转给别的进程前抹掉报告
//! 内容的 `ReportRedactor` 在 `redact`，别的进程发来的控制键和粘贴的编码在 `keys`。

mod detect;
mod effects;
mod keys;
mod redact;

use std::{
    cell::{Cell as StdCell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};

use anyhow::{Result, anyhow};
use libghostty_vt::{
    screen::Screen,
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, PrimaryDeviceAttributes, ProgressState,
        SecondaryDeviceAttributes, SemanticPrompt, SizeReportSize, Terminal, UnknownSequence,
    },
};
use runode_agent_detect::Tracker;
use runode_shared_types::{
    agent::Agent,
    grid::GridSize,
    session::{DriveAction, Driver, SessionMeta},
    settings::TermSettings,
    shell::IntegrationMode,
};

use crate::{
    history, prompt_input,
    pty::{Pty, PtyHandoff, PtyWriter},
    vt::{self, CommandOutput, SnapshotError},
};
use effects::{Effects, PromptEvent, SHELL_REPORT};
pub use redact::ReportRedactor;

pub struct HostSession {
    terminal: Terminal<'static, 'static>,
    pty: Pty,
    writer: PtyWriter,
    size: Rc<StdCell<GridSize>>,
    effects: Rc<Effects>,
    /// VT 应答查询、往 PTY 写回去的次数，排查时看。
    replies: Rc<StdCell<u64>>,
    /// 程序设置的标题；agent 的状态前缀已拆到 `agent` 里。
    title: Option<String>,
    /// 前台 agent 和它的状态，由 `agent_tracker` 合成；不是 agent 在前台时为 `None`。
    agent: Option<Agent>,
    /// 识别前台 agent 用的各路信号和去抖状态，见 `runode_agent_detect::Tracker`。
    agent_tracker: Tracker,
    /// 上次认前台进程组的结果，见 `detect::ForegroundProbe`。
    foreground_probe: detect::ForegroundProbe,
    /// 程序没设置标题时用的名字，见 `Pty::foreground_title`；由 `refresh_foreground` 更新。
    fallback_title: Option<String>,
    /// shell 当前所在的目录，由 `refresh_foreground` 更新；还没启动时是起始目录。
    cwd: Option<PathBuf>,
    /// 上次读到的前台是不是 shell 自己，由 `refresh_foreground` 更新。
    foreground_is_shell: bool,
    /// 上次读到的前台程序的名字，由 `refresh_foreground` 更新，对外见 `SessionMeta::foreground`。
    foreground: Option<String>,
    /// 最近一次别的终端里的程序操作这个会话的记录，见 `drive`。
    driver: Option<Driver>,
    /// 还没启动 shell 时它要从哪个目录开始，见 `new`。
    start_dir: Option<PathBuf>,
    /// shell 最近一次等着输入时所在的目录，记命令时当作命令运行的目录。
    prompt_cwd: Option<PathBuf>,
    /// 这一轮提示符的目录是 shell 报告的：同一轮里后面再来的输入开始（右提示符、续行提示符、
    /// 重画）不再现读去盖掉它，见 `take_commands`。命令开始运行时清掉。
    prompt_reported: bool,
    /// 正在运行、还没报告结束的那条命令。
    running: Option<history::Entry>,
    /// 最近一次收到界面发来的输入的时刻，agent 识别据此区分程序是自己在动还是在回显。
    input_at: Option<Instant>,
    /// 现在套用着的主题，见 `apply_theme`。
    settings: TermSettings,
    /// `SessionMeta` 里的东西可能变了，见 `take_meta`。
    meta_dirty: bool,
    /// 上次交出去的 `SessionMeta`，没变的不再交。
    last_meta: Option<SessionMeta>,
}

impl HostSession {
    /// 把 `pty` 接上一份按 `size` 新建、套好 `settings` 的 VT。`pty` 还没启动 shell 时，
    /// `start_dir` 是 `start` 时 shell 要从哪个目录开始，为 `None` 时从家目录；在那之前标题是
    /// 起始目录的名字，`cwd` 也报起始目录。`pty` 已经启动时不看 `start_dir`。
    pub fn new(size: GridSize, pty: Pty, start_dir: Option<&Path>, settings: &TermSettings) -> Result<Self> {
        let mut terminal = vt::new_terminal(size)?;
        vt::apply_theme(&mut terminal, settings);
        let mut session = Self::with_terminal(size, pty, terminal, settings.clone())?;
        if !session.pty.started() {
            session.start_dir = start_dir.map(Into::into).or_else(|| runode_paths::Dirs::from_env().home);
            session.fallback_title = session.start_dir.as_deref().map(crate::pty::dir_label);
            session.cwd.clone_from(&session.start_dir);
        }
        Ok(session)
    }

    /// 用 `snapshot` 编出的快照建会话，接到 `pty` 上；尺寸取快照里的，`pty` 要已经是这个尺寸。
    /// 快照里带着默认颜色和光标样式，这里不再套主题：套用会改 VT 的状态，见 `vt::apply_theme`。
    /// `settings` 只记下来，之后 `apply_theme` 拿它比较。
    pub fn from_snapshot(snapshot: &[u8], pty: Pty, settings: &TermSettings) -> Result<Self> {
        let terminal = vt::decode_snapshot(snapshot)?;
        let size = vt::terminal_size(&terminal)?;
        let mut session = Self::with_terminal(size, pty, terminal, settings.clone())?;
        // 快照里带着标题，但不会触发标题变化的回调。按刚收到标题走一遍 `feed`，交给 agent
        // 识别、拆掉状态前缀；空的输出不算程序有动静。
        session.effects.title_changed.set(true);
        session.feed(&[]);
        Ok(session)
    }

    /// 把 `terminal` 接到 `pty` 上：注册 VT 回调，建好会话的其余状态。`terminal` 要已经按
    /// `size` 改好尺寸、设好 `vt::configure_common` 的选项。
    fn with_terminal(
        size: GridSize,
        pty: Pty,
        mut terminal: Terminal<'static, 'static>,
        settings: TermSettings,
    ) -> Result<Self> {
        let writer = pty.writer.clone();
        let shared_size = Rc::new(StdCell::new(size));
        // 已经启动的 shell 的报告口令；还没启动的等 `start` 时再设。
        let effects = Rc::new(Effects {
            report_token: RefCell::new(pty.report_token().map(str::to_owned)),
            ..Effects::default()
        });
        let replies = Rc::new(StdCell::new(0));

        // 查询回复（DA、DECRQM、DSR 等）写回子进程，和用户的输入排在同一个写队列里。
        // 没有回复的话，vim、tmux 这类程序探测终端能力时会卡住。
        terminal
            .on_pty_write({
                let reply = writer.clone();
                let replies = replies.clone();
                move |_, data| {
                    replies.set(replies.get() + 1);
                    tracing::trace!(target: "runode::host::reply", bytes = data.len(), "answered a terminal query");
                    reply.write(data);
                }
            })?
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
                    let active = matches!(report.state(), Ok(ProgressState::Set | ProgressState::Indeterminate));
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
            // RIS 会清空标题，但不会触发标题变化回调。
            .on_reset({
                let effects = effects.clone();
                move |_| effects.title_changed.set(true)
            })?;

        Ok(Self {
            terminal,
            pty,
            writer,
            size: shared_size,
            effects,
            replies,
            title: None,
            agent: None,
            agent_tracker: Tracker::new(),
            foreground_probe: detect::ForegroundProbe::default(),
            fallback_title: None,
            cwd: None,
            foreground_is_shell: false,
            foreground: None,
            driver: None,
            start_dir: None,
            prompt_cwd: None,
            prompt_reported: false,
            running: None,
            input_at: None,
            settings,
            meta_dirty: true,
            last_meta: None,
        })
    }

    /// shell 启动了没有（`start` 过，或者接过来的 PTY 本来就启动了）。
    pub fn started(&self) -> bool {
        self.pty.started()
    }

    /// 还没启动 shell 时在起始目录下启动它，`shell` 为 `None` 时用用户的 `$SHELL`；已经启动过时
    /// 什么都不做。
    pub fn start(&mut self, shell: Option<&str>, integration: IntegrationMode) -> Result<()> {
        if self.pty.started() {
            return Ok(());
        }
        self.pty.start(shell, self.start_dir.as_deref(), integration)?;
        // 口令在启动 shell 时才生成，这时再交给校验报告的回调。
        *self.effects.report_token.borrow_mut() = self.pty.report_token().map(str::to_owned);
        Ok(())
    }

    /// 交接的第一步：让 PTY 的读线程停下，见 `Pty::stop_reading`。之后还要把 `PtySink` 那头
    /// 已经收到的输出喂完，等 `pty_reader_finished` 为 true，再编快照、`release_pty`。快照编不
    /// 出来（`SnapshotError::Unfinished`）或者不交了时 `resume_reading`。
    pub fn stop_reading(&mut self) {
        self.pty.stop_reading();
    }

    /// `stop_reading` 之后接着读，见 `Pty::resume_reading`。
    pub fn resume_reading(&mut self) -> Result<()> {
        self.pty.resume_reading()
    }

    /// PTY 的读线程已经结束，`PtySink` 不会再收到东西，见 `Pty::reader_finished`。
    pub fn pty_reader_finished(&self) -> bool {
        self.pty.reader_finished()
    }

    /// 把 PTY 交出去，不结束 shell，见 `Pty::release`。交出后这个会话不再管 shell：写进去的丢掉，
    /// 改尺寸只改这边的 VT、不碰已经交出去的 PTY，读不到前台进程，丢掉它也不结束 shell；VT 还在，
    /// 照样能编快照。出错时什么都没动。
    pub fn release_pty(&mut self) -> Result<PtyHandoff> {
        self.pty.release()
    }

    pub fn size(&self) -> GridSize {
        self.size.get()
    }

    /// 改 VT 和 PTY 的尺寸；和现在一样或者行列为 0 时什么都不做，返回 false。
    pub fn resize(&mut self, size: GridSize) -> bool {
        if self.size.get() == size || size.cols == 0 || size.rows == 0 {
            return false;
        }
        self.size.set(size);
        if let Err(err) =
            self.terminal.resize(size.cols, size.rows, u32::from(size.cell_width_px), u32::from(size.cell_height_px))
        {
            tracing::warn!("terminal resize failed: {err}");
        }
        self.pty.resize(size);
        true
    }

    /// 套用主题，见 `vt::apply_theme`；和现在套着的一样时什么都不做，返回 false。
    pub fn apply_theme(&mut self, settings: &TermSettings) -> bool {
        if self.settings == *settings {
            return false;
        }
        vt::apply_theme(&mut self.terminal, settings);
        self.settings = settings.clone();
        true
    }

    /// 把界面发来的输入写给程序。不等写完，见 `PtyWriter`。
    pub fn write(&mut self, data: Vec<u8>) {
        self.input_at = Some(Instant::now());
        self.writer.send(data);
    }

    /// VT 不停在一条序列中间，可以往里插自己的字节，见 `clear_screen`。
    pub fn at_ground(&self) -> bool {
        self.terminal.is_vt_ground().unwrap_or(false)
    }

    /// 清屏（⌘K）：清掉屏幕和回滚历史，返回为此往 VT 里写的字节，界面那份 VT 要在输出流的同一个
    /// 位置写同样的字节。备用屏幕归全屏程序（vim、less 等）自己管，不动，返回 `None`。要在
    /// `at_ground` 时调，否则这些字节会接在半条序列后面。
    ///
    /// 前台是 shell 时它多半停在提示符，整屏清掉后发一个 FF（Ctrl-L）让它在顶上重画提示符，
    /// 已经敲了一半的命令也会保留。前台在跑别的程序时不能给它塞 FF，只删掉光标以上的行，
    /// 光标所在行顶到第一行。
    pub fn clear_screen(&mut self) -> Option<Vec<u8>> {
        if self.terminal.active_screen().is_ok_and(|s| s == Screen::Alternate) {
            return None;
        }
        self.input_at = Some(Instant::now());
        let at_shell = self.pty.foreground_is_shell();
        // ED 3 放在最后：先 ED 2 时被推进回滚历史的内容也一起清掉。
        let bytes = if at_shell { b"\x1b[H\x1b[2J\x1b[3J".to_vec() } else { self.clear_above_cursor() };
        self.terminal.vt_write(&bytes);
        if at_shell {
            self.writer.write(b"\x0c");
        }
        Some(bytes)
    }

    /// 清掉回滚历史和光标以上的行、光标所在行及以下顶到最上面、光标留在原来的列的序列。
    fn clear_above_cursor(&self) -> Vec<u8> {
        let x = self.terminal.cursor_x().unwrap_or(0);
        let y = self.terminal.cursor_y().unwrap_or(0);
        // DL 从第一行起删掉 y 行，下面的内容跟着上移。
        if y > 0 { format!("\x1b[3J\x1b[H\x1b[{y}M\x1b[1;{}H", x + 1).into_bytes() } else { b"\x1b[3J".to_vec() }
    }

    /// shell 当前所在的目录：还没启动时是起始目录，否则现读。
    fn live_cwd(&self) -> Option<PathBuf> {
        if !self.pty.started() {
            return self.start_dir.clone();
        }
        self.pty.shell_cwd()
    }

    /// shell 最近一次等着输入时所在的目录；还没等过输入时现读一次。
    pub fn prompt_cwd(&self) -> Option<PathBuf> {
        self.prompt_cwd.clone().or_else(|| self.live_cwd())
    }

    /// 前台是不是 shell 自己，现读。
    pub fn foreground_is_shell(&self) -> bool {
        self.pty.foreground_is_shell()
    }

    /// 对外公布的状态。
    pub fn meta(&self) -> SessionMeta {
        SessionMeta {
            title: self.title.clone(),
            fallback_title: self.fallback_title.clone(),
            agent: self.agent,
            cwd: self.cwd.clone(),
            foreground_is_shell: self.foreground_is_shell,
            shell_path: self.effects.shell_path.borrow().clone(),
            shell_names: self.effects.shell_names.borrow().clone(),
            prompt_cwd: self.prompt_cwd.clone(),
            foreground: self.foreground.clone(),
            driver: self.driver.clone(),
        }
    }

    /// 记下别的终端里的程序（`by` 是它所在会话的标识）在 `at_ms`（Unix 毫秒）这一刻对这个会话做了
    /// `action`，对外见 `SessionMeta::driver`。同一方接着做同样的事时最多每秒更新一次时刻，免得
    /// 对外公布的状态跟着每个按键变。
    pub fn drive(&mut self, by: Option<String>, action: DriveAction, at_ms: u64) {
        if let Some(driver) = &self.driver
            && driver.by == by
            && driver.action == action
            && at_ms.saturating_sub(driver.at_ms) < DRIVER_RESOLUTION_MS
        {
            return;
        }
        self.driver = Some(Driver { by, action, at_ms });
        self.meta_dirty = true;
    }

    /// 用户自己在界面里操作了：清掉 `drive` 的记录。
    pub fn clear_driver(&mut self) {
        if self.driver.take().is_some() {
            self.meta_dirty = true;
        }
    }

    /// 自上次以来对外公布的状态变了的话，返回新的状态。
    pub fn take_meta(&mut self) -> Option<SessionMeta> {
        if !std::mem::take(&mut self.meta_dirty) && !self.effects.reported.take() {
            return None;
        }
        let meta = self.meta();
        if self.last_meta.as_ref() == Some(&meta) {
            return None;
        }
        self.last_meta = Some(meta.clone());
        Some(meta)
    }

    /// 自上次以来程序响过铃（喂给 VT 的输出里有不属于任何序列的 BEL）的话返回 true。
    pub fn take_bell(&self) -> bool {
        self.effects.bell.take()
    }

    /// VT 应答查询写回程序的次数。
    pub fn replies(&self) -> u64 {
        self.replies.get()
    }

    /// 把 VT 的全部状态编成快照，见 `vt::encode_snapshot`。VT 正停在一条很长的序列中间时
    /// 返回 `SnapshotError::Unfinished`，等下一批输出后再试。
    pub fn snapshot(&self) -> Result<Vec<u8>, SnapshotError> {
        vt::encode_snapshot(&self.terminal)
    }

    /// 用 VT 序列重画当前状态，快照格式对不上时兜底，见 `vt::format_replay`。
    pub fn vt_replay(&self) -> Result<Vec<u8>> {
        Ok(vt::format_replay(&self.terminal)?)
    }

    /// 给别的进程的快照，之后给它的输出是经 `redactor` 抹过的。和 `snapshot` 不同的只在输出流
    /// 正停在一条 shell 集成报告里（`ReportRedactor::in_report`）时：VT 这时停在报告中间，
    /// `snapshot` 会把没写完的报告连着口令原样编进续接。这里在快照解出来的一份 VT 上先结束这条
    /// 报告（ST，报告本来就不改 VT 的状态，见 `ReportRedactor`），再开一条只有开头 `ESC ] 6973;`
    /// 的，编出的快照停在同样的状态，只是没有报告的内容；之后抹过的输出接着喂，报告照样在
    /// 原来的结束序列处结束。
    pub fn redacted_snapshot(&self, redactor: &ReportRedactor) -> Result<Vec<u8>, SnapshotError> {
        let snapshot = self.snapshot()?;
        if !redactor.in_report() {
            return Ok(snapshot);
        }
        let mut copy = vt::decode_snapshot(&snapshot)?;
        copy.vt_write(b"\x1b\\");
        copy.vt_write(&redact::REPORT_START);
        vt::encode_snapshot(&copy)
    }

    /// 给别的进程的 VT 重放，之后给它的输出是经 `redactor` 抹过的。重放不带没写完的序列；输出流
    /// 正停在一条 shell 集成报告里时，末尾补上报告的开头 `ESC ] 6973;`，前端的 VT 也停进一条
    /// 报告里，之后抹过的输出里报告的结束序列结束它，不会落到别的状态里（比如 BEL 成了响铃）。
    pub fn redacted_vt_replay(&self, redactor: &ReportRedactor) -> Result<Vec<u8>> {
        let mut replay = self.vt_replay()?;
        if redactor.in_report() {
            replay.extend_from_slice(&redact::REPORT_START);
        }
        Ok(replay)
    }

    /// 屏幕底部的文字：`lines` 为 `None` 时是当前一屏，否则是含回滚历史的最底下这么多行，
    /// 见 `vt::screen_tail`。
    pub fn screen_text(&self, lines: Option<u32>) -> Result<String> {
        Ok(vt::screen_tail(&self.terminal, lines)?)
    }

    /// 倒数第 `n` 条命令（1 是最近一条）的输出和它的开头是否已经被挤出回滚历史，见
    /// `vt::command_output`。没有 shell 集成标出的提示符、全屏程序占着屏幕、或者没有这么多条命令
    /// 时返回错误，说明里告诉读的一方怎么办。
    pub fn command_output(&self, n: u32) -> Result<(String, bool)> {
        if self.terminal.active_screen()? == Screen::Alternate {
            return Err(anyhow!("a full-screen program is using the terminal; read the screen with --lines instead"));
        }
        match vt::command_output(&self.terminal, n)? {
            CommandOutput::Found { text, truncated } => Ok((text, truncated)),
            CommandOutput::NoMarks => Err(anyhow!("needs shell integration; use --lines")),
            CommandOutput::Fewer(count) => Err(anyhow!(
                "there {} only {count} command{} on the screen",
                if count == 1 { "is" } else { "are" },
                if count == 1 { "" } else { "s" }
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.terminal
    }

    /// 装作启动 shell 时注入了集成、口令是 `token`。
    #[cfg(test)]
    pub(crate) fn set_report_token(&self, token: &str) {
        *self.effects.report_token.borrow_mut() = Some(token.into());
    }
}

/// `SessionMeta::driver` 的时刻最多这么久更新一次，毫秒。
const DRIVER_RESOLUTION_MS: u64 = 1000;

/// 这个构建编的快照的格式版本，前端据此判断解不解得了宿主的快照，见 `vt::snapshot_format`。
pub fn snapshot_format() -> Result<u16, SnapshotError> {
    vt::snapshot_format()
}

/// 记日志并吞掉错误：读屏幕这类失败只影响这一下。
fn log_err<T>(what: &str, result: libghostty_vt::error::Result<T>) -> Option<T> {
    result.inspect_err(|err| tracing::warn!("{what} failed: {err}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;

    /// 回滚历史的字节上限随主题一起变，开着的终端也立刻按新上限来；0 表示不留回滚历史。
    #[test]
    fn apply_theme_changes_the_scrollback_limit() {
        use runode_shared_types::settings;

        let mut session = idle_host();
        assert_eq!(session.terminal.scrollback_max_bytes().unwrap(), Some(settings::DEFAULT_SCROLLBACK_LIMIT));
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7");
        assert!(scrollback_rows(&session.terminal) > 0);
        assert!(session.apply_theme(&TermSettings { scrollback_limit: 0, ..TermSettings::default() }));
        assert_eq!(session.terminal.scrollback_max_bytes().unwrap(), Some(0));
        assert_eq!(scrollback_rows(&session.terminal), 0);
        assert_eq!(session.terminal.scrollback_max_lines().unwrap(), Some(vt::SCROLLBACK_LINES));
        // 同样的主题不再套一遍。
        assert!(!session.apply_theme(&TermSettings { scrollback_limit: 0, ..TermSettings::default() }));
    }

    #[test]
    fn clear_screen_at_the_shell_clears_everything() {
        // `idle_host` 的「shell」就是前台进程，走 shell 那一支。
        let mut session = idle_host();
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n$ ls");
        assert!(scrollback_rows(&session.terminal) > 0);
        assert_eq!(session.clear_screen().as_deref(), Some(&b"\x1b[H\x1b[2J\x1b[3J"[..]));
        assert_eq!(scrollback_rows(&session.terminal), 0);
        assert!((0..4).all(|y| screen_row(&session.terminal, y).is_empty()));
    }

    #[test]
    fn clear_above_cursor_keeps_the_cursor_row_at_the_top() {
        let mut session = idle_host();
        session.feed(b"1\r\n2\r\n3\r\n4\r\n5\r\nrunning");
        let bytes = session.clear_above_cursor();
        session.terminal.vt_write(&bytes);
        assert_eq!(scrollback_rows(&session.terminal), 0);
        assert_eq!(session.terminal.cursor_y().unwrap(), 0);
        assert_eq!(session.terminal.cursor_x().unwrap(), 7);
        assert_eq!(screen_row(&session.terminal, 0), "running");
        assert!((1..4).all(|y| screen_row(&session.terminal, y).is_empty()));
    }

    #[test]
    fn clear_screen_leaves_the_alternate_screen_alone() {
        let mut session = idle_host();
        session.feed(b"\x1b[?1049hvim");
        assert_eq!(session.clear_screen(), None);
        assert_eq!(screen_row(&session.terminal, 0), "vim");
    }
}
