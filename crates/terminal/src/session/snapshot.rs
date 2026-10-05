//! 快照：从宿主编出的快照建界面这边的会话。编码和解码本身见 `vt::encode_snapshot`、
//! `vt::decode_snapshot`，宿主那边编快照见 `HostSession::snapshot`。

#[cfg(test)]
mod tests;

use anyhow::Result;

use super::{Sender, Session};
use crate::vt;

impl Session {
    /// 用 `HostSession::snapshot` 编出的快照建一个会话，尺寸取快照里的。之后接着喂宿主在快照
    /// 之后转来的输出，就和宿主那份一样。
    ///
    /// 快照里带着默认颜色和光标样式，这里不套主题，之后也不该再 `apply_theme`：套用会改 VT 的
    /// 状态（见 `vt::apply_theme`），只在宿主标出的位置套。界面自己的那部分配置照常
    /// `apply_config`。标题、agent 这些状态不在 VT 里，由宿主连上时给的 `SessionMeta` 带来。
    pub fn from_snapshot(snapshot: &[u8], sender: Sender) -> Result<Self> {
        let terminal = vt::decode_snapshot(snapshot)?;
        let size = vt::terminal_size(&terminal)?;
        Self::with_terminal(size, terminal, sender)
    }
}
