//! 快照：把会话的 VT 编成快照、从快照建会话，以及快照格式对不上时用的 VT 重放。
//! 编码和解码本身见 `vt::encode_snapshot`、`vt::decode_snapshot`。

#[cfg(test)]
mod tests;

use anyhow::Result;
use runode_shared_types::grid::GridSize;

use super::Session;
use crate::{
    pty::Pty,
    vt::{self, SnapshotError},
};

impl Session {
    /// 用 `Session::snapshot` 编出的快照建一个会话，接到 `pty` 上。VT 回调和 `with_pty` 接上
    /// 的一样；尺寸取快照里的，`pty` 要已经是这个尺寸。
    ///
    /// 之后照常调 `apply_config`：它只改默认颜色，程序用 OSC 4、10、11 改过的颜色照旧优先。
    pub fn from_snapshot(snapshot: &[u8], pty: Pty) -> Result<Self> {
        let terminal = vt::decode_snapshot(snapshot)?;
        let (cols, rows) = (terminal.cols()?, terminal.rows()?);
        let cell = |total: u32, cells: u16| u16::try_from(total / u32::from(cells.max(1))).unwrap_or(u16::MAX);
        let size = GridSize {
            cols,
            rows,
            cell_width_px: cell(terminal.width_px()?, cols),
            cell_height_px: cell(terminal.height_px()?, rows),
        };
        let mut session = Self::with_terminal(size, pty, terminal)?;
        // 快照里带着标题，但不会触发标题变化的回调。按刚收到标题走一遍 `feed`，交给 agent
        // 识别、拆掉状态前缀；空的输出不算程序有动静。
        session.effects.title_changed.set(true);
        session.feed(&[]);
        Ok(session)
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
}
