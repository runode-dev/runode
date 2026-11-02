//! 打到终端上的文字：表格、会话的标题和状态、家目录的写法，别的程序给的文字先去掉控制字符。

use std::{io::Write, path::Path};

use runode_types::{agent::AgentState, session::SessionMeta};
use unicode_width::UnicodeWidthStr as _;

/// `list` 显示的标识的长度，够区分几十个会话，命令里也能直接用。
pub(crate) const SHORT_ID: usize = 8;

pub(crate) fn title(meta: &SessionMeta) -> Option<String> {
    meta.title.clone().or_else(|| meta.fallback_title.clone())
}

pub(crate) fn state_name(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Blocked => "blocked",
    }
}

/// 要打到终端上的、别的程序给的文字（标题、路径、提示词、设备名）：控制字符换成 `?`、tab 换成空格，
/// 免得里面的转义序列清屏、改终端标题，或者把表格的一行拆成几行。
pub(crate) fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' {
                ' '
            } else if c.is_control() {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// 打印表格：各列按最宽的格子的显示宽度补齐，列间两个空格。
pub(crate) fn table(
    header: &[&str],
    rows: impl Iterator<Item = Vec<String>>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let header: Vec<String> = header.iter().map(|&cell| cell.to_owned()).collect();
    let rows = rows.map(|row| row.iter().map(|cell| plain(cell)).collect());
    let rows: Vec<Vec<String>> = std::iter::once(header).chain(rows).collect();
    let mut widths = vec![0; rows[0].len()];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.width());
        }
    }
    for row in &rows {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell}{}", " ".repeat(width - cell.width())))
            .collect();
        writeln!(out, "{}", cells.join("  ").trim_end())?;
    }
    Ok(())
}

/// 家目录下的路径写成 `~/…`。
pub(crate) fn tilde(path: &Path, home: Option<&Path>) -> String {
    // `/`：选择器和 `expand_home` 只认 `~/`。
    home.and_then(|home| runode_paths::home_relative(path, home, '/')).unwrap_or_else(|| path.display().to_string())
}
