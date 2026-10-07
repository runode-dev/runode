//! 只和 VT 本身有关、宿主和界面两份 VT 共用的东西：两边必须一致的选项、快照的编码和解码、
//! 版本对不上时的 VT 重放，以及从 VT 里读屏幕上的文字。这里只碰 `Terminal`，不管 PTY 和回调。
//!
//! 界面连上宿主时先拿一份快照，解出来接着喂之后的 PTY 输出，两份 VT 从此按同样的字节走，
//! 所以凡是影响 VT 状态、又不在快照里的选项，都要两边一样，统一由 `configure_common` 设。

#[cfg(test)]
pub(crate) mod tests;

use libghostty_vt::{
    Terminal,
    error::{Error, Result},
    fmt::{Format, Formatter, FormatterOptions},
    screen::{CellSemanticContent, RowSemanticPrompt, Screen},
    selection::Selection,
    snapshot::Decoder,
    terminal::{Mode, Point, PointCoordinate},
};
use runode_shared_types::{
    color::TerminalColor,
    grid::GridSize,
    settings::{DEFAULT_SCROLLBACK_LIMIT, TermSettings},
};

use crate::session::convert::{ghostty_cursor_style, ghostty_rgb};

/// 回滚历史最多留多少行。另有字节上限（`configure_common` 的 `scrollback_bytes`，默认 10 MiB），先到
/// 哪个按哪个算。两个上限都按 page 整块丢弃最老的历史，实际留下的比上限少一些，最多少一个
/// page。一行占的字节随列数变：默认上限下 80 列先到行数上限（留 9800 多行），200 列先到字节
/// 上限（留五千多行）。
pub(crate) const SCROLLBACK_LINES: usize = 10_000;

/// 留给未知 OSC 的最多字节数：函数很多的 shell 报告的函数名能有几十 KB。更长的被截断，
/// 不采用。
pub(crate) const UNKNOWN_SEQUENCE_MAX_BYTES: usize = 256 * 1024;

/// 编码快照时最多带多少字节没写完的序列（续接）。VT 停在一条更长的序列中间时编不出快照，
/// 要等它结束，见 `SnapshotError::Unfinished`。解码时也按它限制续接的长度。
pub(crate) const CONTINUATION_MAX_BYTES: usize = 1024 * 1024;

/// 按 `size` 新建一个 VT，按默认配置设好 `configure_common` 的选项（之后 `apply_theme` 再按
/// 实际配置设一遍），并从一开始就记录没写完的序列，随时能编快照。要在喂输入之前开始记录：
/// 开的时候已经停在序列中间的话，这条序列编不出来，要等它结束。记录的开销可以忽略（release
/// 下喂 100 MiB 混合输出实测，和不记录的差别在测量误差以内）。
pub(crate) fn new_terminal(size: GridSize) -> Result<Terminal<'static, 'static>> {
    let mut terminal = Terminal::new(size.cols, size.rows)?;
    configure_common(&mut terminal, Some(DEFAULT_SCROLLBACK_LIMIT))?;
    terminal.set_continuation_max_bytes(CONTINUATION_MAX_BYTES)?;
    terminal.resize(size.cols, size.rows, u32::from(size.cell_width_px), u32::from(size.cell_height_px))?;
    Ok(terminal)
}

/// VT 现在的尺寸，单元格的像素按总像素除以行列数算。
pub(crate) fn terminal_size(terminal: &Terminal<'_, '_>) -> Result<GridSize> {
    let (cols, rows) = (terminal.cols()?, terminal.rows()?);
    let cell = |total: u32, cells: u16| u16::try_from(total / u32::from(cells.max(1))).unwrap_or(u16::MAX);
    Ok(GridSize {
        cols,
        rows,
        cell_width_px: cell(terminal.width_px()?, cols),
        cell_height_px: cell(terminal.height_px()?, rows),
    })
}

/// 设好两份 VT 必须一致的选项：回滚历史的行数和字节上限，未知序列的长度上限。新建的、从
/// 快照解出来的 VT 都要调，配置变了也要调；快照里带着的选项（比如回滚上限）照样设一遍，
/// 免得哪天快照格式不再带它时两边悄悄分叉，或者退回 libghostty 自己的默认值。
///
/// `scrollback_bytes` 是回滚历史最多占多少字节，即配置项 `scrollback-limit`；`None` 表示不限，
/// 只按行数算。调低回滚上限会立刻丢掉超出的历史。默认颜色和光标样式由 `apply_theme` 设，不在这里。
pub(crate) fn configure_common(terminal: &mut Terminal<'_, '_>, scrollback_bytes: Option<usize>) -> Result<()> {
    terminal
        .set_scrollback_max_lines(Some(SCROLLBACK_LINES))?
        .set_scrollback_max_bytes(scrollback_bytes)?
        .set_unknown_sequence_max_bytes(UNKNOWN_SEQUENCE_MAX_BYTES)?;
    Ok(())
}

/// 把配置里和 VT 状态有关的部分（主题）套到 `terminal` 上：默认颜色、调色板、光标样式和闪烁，
/// 以及 `configure_common` 的选项。改的是默认值：程序自己用转义序列设置的颜色、光标形状和
/// 闪烁照旧优先，所以配置可以随时重载。
///
/// 它不只改默认值：光标还跟着默认样式时，当前的闪烁和形状随新的默认值变；调低回滚上限会
/// 丢掉历史。所以宿主和界面两份 VT 要在输出流的同一个位置套用同样的主题，见
/// `HostMsg::ThemeApplied`。结果只取决于 VT 自己的状态（快照里都带着），所以从快照解出来的
/// VT 和原来那份套同样的主题，结果一样。
pub(crate) fn apply_theme(terminal: &mut Terminal<'_, '_>, settings: &TermSettings) {
    // 默认调色板读出来的是上次设置的值，先重置回内置调色板再叠加配置，
    // 否则旧主题设过、新主题没设的条目会残留。
    let mut palette = match terminal.set_default_color_palette(None).and_then(|t| t.default_color_palette()) {
        Ok(palette) => palette,
        Err(err) => {
            tracing::warn!("failed to read the default palette: {err}");
            return;
        }
    };
    for &(index, color) in &settings.palette {
        palette.0[usize::from(index)] = ghostty_rgb(color);
    }
    let applied = terminal
        .set_default_bg_color(Some(ghostty_rgb(settings.background)))
        .and_then(|t| t.set_default_fg_color(Some(ghostty_rgb(settings.foreground))))
        // 跟随单元格的光标色由渲染时按光标所在单元格解析，VT 里不设默认值。
        .and_then(|t| {
            t.set_default_cursor_color(match settings.cursor_color {
                Some(TerminalColor::Rgb(color)) => Some(ghostty_rgb(color)),
                _ => None,
            })
        })
        .and_then(|t| set_default_cursor(t, settings))
        .and_then(|t| t.set_default_color_palette(Some(palette)));
    if let Err(err) = applied {
        tracing::warn!("failed to apply config to the terminal: {err}");
    }
    if let Err(err) = configure_common(terminal, Some(settings.scrollback_limit)) {
        tracing::warn!("failed to apply the scrollback limit: {err}");
    }
}

/// 设默认的光标形状和闪烁，但不覆盖程序用 `CSI ? 12 h/l`（DEC 模式 12）自己设的闪烁。
///
/// 程序没用 DECSCUSR 设过形状（或用 `CSI 0 SP q` 回到了默认）时，光标跟着默认值走，libghostty
/// 改任一个默认值都会把当前的形状和闪烁一起换成默认的，模式 12 设的闪烁也就被冲掉了。这里
/// 先只换默认形状：光标跟着默认值走的话，闪烁这时变回旧的默认值，和原来不一样就说明程序用
/// 模式 12 改过，换完默认闪烁后把它还原；一样就跟着新的默认闪烁走（配置里改了闪烁要立刻
/// 生效）。程序用 DECSCUSR 设过形状时 libghostty 两样都不动，这里也不用管。
///
/// 程序用模式 12 设的值恰好等于旧的默认闪烁时分不出来，当作跟着默认值，随新配置变。
fn set_default_cursor<'a, 't, 's>(
    terminal: &'a mut Terminal<'t, 's>,
    settings: &TermSettings,
) -> Result<&'a mut Terminal<'t, 's>> {
    let blinking = terminal.mode(Mode::CURSOR_BLINKING)?;
    terminal.set_default_cursor_style(Some(ghostty_cursor_style(settings.cursor_style)))?;
    let old_default = terminal.mode(Mode::CURSOR_BLINKING)?;
    // 没配置时默认不闪烁：闪烁要一直重画，进程就一直多占着画帧的那些内存。
    terminal.set_default_cursor_blink(Some(settings.cursor_blink.unwrap_or(false)))?;
    if blinking != old_default {
        terminal.set_mode(Mode::CURSOR_BLINKING, blinking)?;
    }
    Ok(terminal)
}

/// 编码或解码快照失败的原因。
#[derive(Debug)]
pub enum SnapshotError {
    /// VT 停在一条还没写完的序列中间，现在编不出快照：这条序列长过 `CONTINUATION_MAX_BYTES`，
    /// 或者是在开始记录之前开始的。别的停法都编得出，包括停在 UTF-8 字符中间、由 8 位 C1 字节
    /// 开头的序列（含结束了 SOS、PM、APC 的那种），见差分测试 `a_snapshot_cut_inside_any_sequence_resumes`。
    /// 等下一批输出让 VT 回到 ground 后再试；程序停在这里不再输出的话会一直这样。
    Unfinished,
    /// libghostty 报的其他错误，只留说明文字，libghostty 的类型不出这个 crate。解码时多半是
    /// 数据坏了或者不是这个版本编出来的。
    Vt(String),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unfinished => write!(f, "the terminal is in the middle of a sequence it cannot replay yet"),
            Self::Vt(err) => write!(f, "terminal snapshot failed: {err}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<Error> for SnapshotError {
    fn from(err: Error) -> Self {
        Self::Vt(err.to_string())
    }
}

/// 把 VT 的全部状态编成一份快照：屏幕、回滚历史、光标、各种模式、调色板、标题、没写完的
/// 序列等，见 `libghostty_vt::snapshot`。快照格式还没有兼容保证，只能交给同一个构建解码。
///
/// 不带的东西：Kitty 图片、选区、视口滚到了哪里、搜索，以及 `configure_common` 设的选项和
/// 各种回调。
pub(crate) fn encode_snapshot(terminal: &Terminal<'_, '_>) -> std::result::Result<Vec<u8>, SnapshotError> {
    match terminal.encode_snapshot_alloc(None) {
        Ok(bytes) => Ok(bytes.map(|bytes| bytes.to_vec()).unwrap_or_default()),
        // 续接取不到、或者续接里的字节不肯编时 libghostty 都报 InvalidValue；回到 ground 的
        // VT 不会是这两个原因。
        Err(Error::InvalidValue) if !terminal.is_vt_ground()? => Err(SnapshotError::Unfinished),
        Err(err) => Err(err.into()),
    }
}

/// 解出一份 `encode_snapshot` 编的快照，按编快照那份的回滚上限设好 `configure_common` 的选项，
/// 并接着记录没写完的序列，所以解出来的 VT 也能再编快照。一次解完，不按页增量解码。
pub(crate) fn decode_snapshot(bytes: &[u8]) -> std::result::Result<Terminal<'static, 'static>, SnapshotError> {
    let mut decoder = Decoder::new_buf(bytes)?;
    decoder.set_max_continuation_bytes(CONTINUATION_MAX_BYTES)?.set_retain_continuation(true)?;
    let mut terminal = decoder.decode()?;
    // 快照里带着编快照那份的字节上限；照它设，不退回默认值。
    let scrollback_bytes = terminal.scrollback_max_bytes()?;
    configure_common(&mut terminal, scrollback_bytes)?;
    Ok(terminal)
}

/// 用 VT 序列重画当前状态，交给新建的 VT（先 `configure_common`、改好尺寸）喂进去，就能
/// 大致复原。快照的格式对不上（两边不是同一个构建）时用它兜底。
///
/// 输出的是活动屏幕（备用屏幕上就只有备用屏幕，没有主屏幕和回滚历史）的内容，加上调色板、
/// 模式、滚动区域、制表位、目录（OSC 7）、键盘模式、光标位置和样式、超链接、保护模式、
/// Kitty 键盘协议和字符集；格式化本身不带标题，这里在末尾补一条 OSC 2。重放丢掉的东西
/// 见测试 `replay_loses_what_the_formatter_cannot_express`。
pub(crate) fn format_replay(terminal: &Terminal<'_, '_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    Formatter::new(terminal, replay_options())?.format_into(&mut out)?;
    let title = terminal.title()?;
    if !title.is_empty() {
        // 标题本来就是从 OSC 里解出来的，不该有控制字符；万一有，去掉，免得提前结束这条 OSC。
        out.extend_from_slice(b"\x1b]2;");
        out.extend(title.chars().filter(|c| !c.is_control()).collect::<String>().bytes());
        out.push(0x07);
    }
    Ok(out)
}

/// `format_replay` 的格式化选项。软折行的行接起来输出，靠自动折行在原处折回去；分开输出
/// 的话每行末尾是 `\r\n`，软折行就成了硬换行。
pub(crate) fn replay_options<'t, 's>() -> FormatterOptions<'t, 's> {
    FormatterOptions::new()
        .with_format(Format::Vt)
        .with_unwrap(true)
        .with_trim(false)
        .with_palette(true)
        .with_modes(true)
        .with_scrolling_region(true)
        .with_tabstops(true)
        .with_pwd(true)
        .with_keyboard(true)
        .with_cursor(true)
        .with_style(true)
        .with_hyperlink(true)
        .with_protection(true)
        .with_kitty_keyboard(true)
        .with_charsets(true)
}

/// 屏幕底部一屏高的文字，给识别规则用：一行一个 `\n`，行尾空白去掉，末尾的空行去掉。
///
/// 读的是活动区，不管用户把视口翻到了哪里。主屏幕上以最后一个有字的行和光标所在行中靠下
/// 的那行为底，往上取一屏高，内容没占满一屏时会带上回滚历史里的几行；备用屏幕上就是整屏。
pub(crate) fn detection_text(terminal: &Terminal<'_, '_>) -> Result<Option<String>> {
    let rows = usize::from(terminal.rows()?);
    let total = terminal.total_rows()?;
    if rows == 0 || total == 0 {
        return Ok(None);
    }
    let active_top = total.saturating_sub(rows);
    let alternate = terminal.active_screen()? == Screen::Alternate;
    // 主屏幕上底可能往上移，最多再多读一屏回滚历史。
    let first = if alternate { active_top } else { active_top.saturating_sub(rows) };
    let lines = screen_lines(terminal, first, total - 1)?;
    let line = |row: usize| lines.get(row - first).map_or("", String::as_str);
    let bottom = if alternate {
        total - 1
    } else {
        let cursor = active_top + usize::from(terminal.cursor_y()?);
        (active_top..total).rev().find(|&row| !line(row).trim().is_empty()).map_or(total - 1, |row| row.max(cursor))
    };
    let top = (bottom + 1).saturating_sub(rows).max(first);
    let mut picked: Vec<&str> = (top..=bottom).map(line).collect();
    while picked.last().is_some_and(|line| line.trim().is_empty()) {
        picked.pop();
    }
    if picked.is_empty() {
        return Ok(Some(String::new()));
    }
    let mut text = picked.join("\n");
    text.push('\n');
    Ok(Some(text))
}

/// 屏幕底部的文字，给前端读屏幕用：`lines` 为 `None` 时是活动区的一屏，否则是整个屏幕（含
/// 回滚历史）里最后一个有字的行往上这么多行，活动区里还没写到的空行不算。一行一个 `\n`，
/// 行尾空白去掉，末尾的空行去掉。
pub(crate) fn screen_tail(terminal: &Terminal<'_, '_>, lines: Option<u32>) -> Result<String> {
    let total = terminal.total_rows()?;
    let rows = usize::from(terminal.rows()?);
    if total == 0 || lines == Some(0) {
        return Ok(String::new());
    }
    // 末尾的空行都在活动区里，最多一屏；按行数取时多读一屏，去掉它们后还够数。
    let count = lines.map_or(rows, |lines| lines as usize + rows);
    let mut picked = screen_lines(terminal, total.saturating_sub(count), total - 1)?;
    while picked.last().is_some_and(String::is_empty) {
        picked.pop();
    }
    if let Some(lines) = lines {
        picked.drain(..picked.len().saturating_sub(lines as usize));
    }
    Ok(picked.into_iter().map(|line| line + "\n").collect())
}

/// `command_output` 找到的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandOutput {
    /// 那条命令的输出，格式同 `screen_tail`。`truncated` 为真时它的提示符和输出的开头已经被挤出
    /// 回滚历史，给的是还留着的部分。
    Found { text: String, truncated: bool },
    /// 屏幕上（含回滚历史）没有 shell 集成标出的提示符。
    NoMarks,
    /// 屏幕上只找得到这么多条命令。
    Fewer(usize),
}

/// 倒数第 `n` 条命令（1 是最近一条）的输出，靠 shell 集成在提示符行上的标记
/// （`RowSemanticPrompt::Prompt`）分块：从底往上数提示符，一块从提示符那行起，到下一个提示符
/// 之前；输出是输入那几行（提示符行和接在后面的软折行、续行提示符的行）之后的部分。
///
/// 一个提示符的几行里还没有用户输入时，紧接着又标成主提示符的行算同一个提示符的下一行：
/// 空着回车之后 shell 在下一行另画一个提示符，没输入过东西的那个提示符这样就不算一条命令，
/// `n` 数的都是真敲过的命令。
///
/// 光标还在最后一个提示符的输入行里时，那是正等着输入的提示符，不算一条命令；光标已经到了
/// 输入行下面时那条命令还在跑，输出取到底。最早那个提示符上面还有内容、又正好要它前面那条
/// 时，那条命令的提示符已经被挤出回滚历史，给剩下的部分并标上截断；shell 启动时打印的内容
/// 也会这样被当成一条截断的输出。读的是活动的屏幕，全屏程序在备用屏幕上时没有标记。
pub(crate) fn command_output(terminal: &Terminal<'_, '_>, n: u32) -> Result<CommandOutput> {
    let total = terminal.total_rows()?;
    let cols = terminal.cols()?;
    let point = |x: u16, y: usize| Point::Screen(PointCoordinate { x, y: u32::try_from(y).unwrap_or(u32::MAX) });
    let row = |y: usize| terminal.grid_ref(point(0, y))?.row();
    let typed_in = |y: usize| -> Result<bool> {
        for x in 0..cols {
            if terminal.grid_ref(point(x, y))?.cell()?.semantic_content()? == CellSemanticContent::Input {
                return Ok(true);
            }
        }
        Ok(false)
    };
    // 提示符行之后、仍属于这个提示符和它的输入的行。
    let input_end = |prompt: usize| -> Result<usize> {
        let mut typed = typed_in(prompt)?;
        let mut y = prompt + 1;
        while y < total {
            let row = row(y)?;
            let same = row.is_wrap_continuation()?
                || match row.semantic_prompt()? {
                    RowSemanticPrompt::Continuation => true,
                    RowSemanticPrompt::Prompt => !typed,
                    RowSemanticPrompt::None => false,
                };
            if !same {
                break;
            }
            typed = typed || typed_in(y)?;
            y += 1;
        }
        Ok(y)
    };
    let mut prompts = Vec::new();
    let mut y = 0;
    while y < total {
        if row(y)?.semantic_prompt()? == RowSemanticPrompt::Prompt {
            prompts.push(y);
            y = input_end(y)?;
        } else {
            y += 1;
        }
    }
    let Some(&last) = prompts.last() else {
        return Ok(CommandOutput::NoMarks);
    };
    let cursor = total.saturating_sub(usize::from(terminal.rows()?)) + usize::from(terminal.cursor_y()?);
    let editing = cursor >= last && cursor < input_end(last)?;
    let commands = prompts.len() - usize::from(editing);
    let n = n.max(1) as usize;
    let (first, end, truncated) = if n <= commands {
        let index = commands - n;
        let end = prompts.get(index + 1).copied().unwrap_or(total);
        (input_end(prompts[index])?, end, false)
    } else if n == commands + 1 && prompts[0] > 0 {
        (0, prompts[0], true)
    } else {
        return Ok(CommandOutput::Fewer(commands));
    };
    let mut lines = if first < end { screen_lines(terminal, first, end - 1)? } else { Vec::new() };
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    Ok(CommandOutput::Found { text: lines.into_iter().map(|line| line + "\n").collect(), truncated })
}

/// 这个构建编的快照的格式版本：快照开头 `GHOSTSNP` 后面的 u16。libghostty 没有单独给出这个
/// 数，这里编一份最小的快照读出来。
pub(crate) fn snapshot_format() -> std::result::Result<u16, SnapshotError> {
    let bytes = encode_snapshot(&new_terminal(GridSize { cols: 1, rows: 1, cell_width_px: 1, cell_height_px: 1 })?)?;
    match bytes.get(..10) {
        Some([b'G', b'H', b'O', b'S', b'T', b'S', b'N', b'P', lo, hi]) => Ok(u16::from_le_bytes([*lo, *hi])),
        _ => Err(SnapshotError::Vt("snapshot has no GHOSTSNP header".into())),
    }
}

/// 整个屏幕（含回滚历史）第 `first` 到 `last` 行的纯文字，一行一项，行尾空白去掉。软换行
/// 不接起来，和屏幕上的行一一对应；末尾的空行可能没有。
pub(crate) fn screen_lines(terminal: &Terminal<'_, '_>, first: usize, last: usize) -> Result<Vec<String>> {
    let cols = terminal.cols()?;
    let point = |x: u16, y: usize| Point::Screen(PointCoordinate { x, y: u32::try_from(y).unwrap_or(u32::MAX) });
    let selection = Selection::new(
        terminal.grid_ref(point(0, first))?,
        terminal.grid_ref(point(cols.saturating_sub(1), last))?,
        false,
    );
    let options = FormatterOptions::new()
        .with_format(Format::Plain)
        .with_unwrap(false)
        .with_trim(true)
        .with_selection(&selection);
    let bytes = Formatter::new(terminal, options)?.format_alloc(None)?.to_vec();
    Ok(String::from_utf8_lossy(&bytes).split('\n').map(|line| line.trim_end().to_owned()).collect())
}
