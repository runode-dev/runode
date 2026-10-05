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
    screen::Screen,
    selection::Selection,
    snapshot::Decoder,
    terminal::{Point, PointCoordinate},
};
use runode_shared_types::grid::GridSize;

/// 回滚历史最多留多少行。
pub(crate) const SCROLLBACK_LINES: usize = 10_000;

/// 留给未知 OSC 的最多字节数：函数很多的 shell 报告的函数名能有几十 KB。更长的被截断，
/// 不采用。
pub(crate) const UNKNOWN_SEQUENCE_MAX_BYTES: usize = 256 * 1024;

/// 编码快照时最多带多少字节没写完的序列（续接）。VT 停在一条更长的序列中间时编不出快照，
/// 要等它结束，见 `SnapshotError::Unfinished`。解码时也按它限制续接的长度。
pub(crate) const CONTINUATION_MAX_BYTES: usize = 1024 * 1024;

/// 按 `size` 新建一个 VT，设好 `configure_common` 的选项，并从一开始就记录没写完的序列
/// （`track_continuation`），随时能编快照。记录的开销可以忽略（release 下喂 100 MiB 混合
/// 输出实测，和不记录的差别在测量误差以内）。
pub(crate) fn new_terminal(size: GridSize) -> Result<Terminal<'static, 'static>> {
    let mut terminal = Terminal::new(size.cols, size.rows)?;
    configure_common(&mut terminal)?;
    track_continuation(&mut terminal)?;
    terminal.resize(size.cols, size.rows, u32::from(size.cell_width_px), u32::from(size.cell_height_px))?;
    Ok(terminal)
}

/// 设好两份 VT 必须一致、快照里又不带的选项。新建的和从快照解出来的 VT 都要调；快照里带着
/// 的选项（比如回滚上限）照样设一遍，免得以后快照格式不再带它时两边悄悄分叉。
///
/// 默认颜色和光标样式随配置变，由 `Session::apply_config` 设，不在这里。
pub(crate) fn configure_common(terminal: &mut Terminal<'_, '_>) -> Result<()> {
    terminal
        .set_scrollback_max_lines(Some(SCROLLBACK_LINES))?
        .set_unknown_sequence_max_bytes(UNKNOWN_SEQUENCE_MAX_BYTES)?;
    Ok(())
}

/// 开始记录没写完的序列，之后停在序列中间的 VT 也能编码快照。要在喂输入之前开：开的时候
/// 已经停在序列中间的话，这条序列编不出来，要等它结束。
fn track_continuation(terminal: &mut Terminal<'_, '_>) -> Result<()> {
    terminal.set_continuation_max_bytes(CONTINUATION_MAX_BYTES)?;
    Ok(())
}

/// 编码或解码快照失败的原因。
#[derive(Debug)]
pub enum SnapshotError {
    /// VT 停在一条还没写完的序列中间，现在编不出快照：这条序列长过 `CONTINUATION_MAX_BYTES`，
    /// 或者是在开始记录之前开始的，或者是 libghostty 不肯编的乱码（ESC 后面跟着 UTF-8 和 C1
    /// 字节之类）。等下一批输出让 VT 回到 ground 后再试；程序停在这里不再输出的话会一直这样。
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

/// 解出一份 `encode_snapshot` 编的快照，设好 `configure_common` 的选项，并接着记录没写完的
/// 序列（`track_continuation`），所以解出来的 VT 也能再编快照。一次解完，不按页增量解码。
pub(crate) fn decode_snapshot(bytes: &[u8]) -> std::result::Result<Terminal<'static, 'static>, SnapshotError> {
    let mut decoder = Decoder::new_buf(bytes)?;
    decoder.set_max_continuation_bytes(CONTINUATION_MAX_BYTES)?.set_retain_continuation(true)?;
    let mut terminal = decoder.decode()?;
    configure_common(&mut terminal)?;
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

/// 整个屏幕（含回滚历史）第 `first` 到 `last` 行的纯文字，一行一项，行尾空白去掉。软换行
/// 不接起来，和屏幕上的行一一对应；末尾的空行可能没有。
pub(crate) fn screen_lines(terminal: &Terminal<'_, '_>, first: usize, last: usize) -> Result<Vec<String>> {
    let cols = terminal.cols()?;
    let point = |x: u16, y: usize| Point::Screen(PointCoordinate { x, y: u32::try_from(y).unwrap_or(u32::MAX) });
    let selection = Selection::new(terminal.grid_ref(point(0, first))?, terminal.grid_ref(point(cols.saturating_sub(1), last))?, false);
    let options = FormatterOptions::new()
        .with_format(Format::Plain)
        .with_unwrap(false)
        .with_trim(true)
        .with_selection(&selection);
    let bytes = Formatter::new(terminal, options)?.format_alloc(None)?.to_vec();
    Ok(String::from_utf8_lossy(&bytes).split('\n').map(|line| line.trim_end().to_owned()).collect())
}
