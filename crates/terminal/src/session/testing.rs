//! 各子模块的测试共用的会话和辅助函数。

use std::time::Duration;

use runode_shared_types::{
    frame::Frame,
    grid::{GridPoint, GridSize},
    settings::TermSettings,
};

use super::Session;

pub(super) fn row_text(frame: &Frame, y: u16) -> String {
    frame
        .row(y)
        .iter()
        .filter(|c| !c.spacer)
        .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

pub(super) fn idle_session() -> Session {
    let size = GridSize {
        cols: 20,
        rows: 4,
        cell_width_px: 8,
        cell_height_px: 16,
    };
    // `cat` 自己不输出，VT 只会收到测试喂进去的内容。
    let mut session = Session::spawn_shell(size, Some("/bin/cat")).unwrap().0;
    session.apply_config(&TermSettings::default());
    session
}

pub(super) fn scrollback_rows(session: &Session) -> u64 {
    let scrollbar = session.terminal.scrollbar().unwrap();
    scrollbar.total - scrollbar.len
}

/// shell 集成标出的提示符：`$ ` 是提示符，后面是用户输入。
pub(super) const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

/// 测试里用的报告口令。
pub(super) const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// 一个持有 `TOKEN` 的会话，就像启动 shell 时注入了集成一样。
pub(super) fn reporting_session() -> Session {
    let session = idle_session();
    *session.effects.report_token.borrow_mut() = Some(TOKEN.into());
    session
}

pub(super) const REPEAT: Duration = Duration::from_millis(500);

pub(super) fn at(x: f32, y: f32) -> GridPoint {
    GridPoint { x, y }
}
