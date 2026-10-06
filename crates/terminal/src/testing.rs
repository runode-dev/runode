//! 各模块的测试共用的会话和辅助函数。

use std::{cell::RefCell, rc::Rc, time::Duration};

use libghostty_vt::Terminal;
use runode_shared_types::{
    frame::Frame,
    grid::{GridPoint, GridSize},
    session::SessionMeta,
    settings::TermSettings,
    shell::IntegrationMode,
};

use crate::{
    host_session::HostSession,
    pty::Pty,
    session::{Request, Session},
    vt,
};

pub(crate) fn row_text(frame: &Frame, y: u16) -> String {
    frame
        .row(y)
        .iter()
        .filter(|c| !c.spacer)
        .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// 活动区第 `y` 行的文字，行尾空白去掉。
pub(crate) fn screen_row(terminal: &Terminal<'static, 'static>, y: u32) -> String {
    let top = terminal.total_rows().unwrap() - usize::from(terminal.rows().unwrap());
    vt::screen_lines(terminal, top + y as usize, top + y as usize).unwrap().remove(0)
}

const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };

/// 宿主这边的会话：`cat` 当 shell，自己不输出，VT 只会收到测试喂进去的内容。它也是前台
/// 进程，所以前台总是「shell」。
pub(crate) fn idle_host() -> HostSession {
    let pty = Pty::spawn(SIZE, Some("/bin/cat"), None, IntegrationMode::Off, Box::new(|_| true)).unwrap();
    HostSession::new(SIZE, pty, None, &TermSettings::default()).unwrap()
}

/// 宿主这边没有 shell 的会话：只喂字节、不看前台进程的测试用它，不用每次起一个进程。
pub(crate) fn bare_host() -> HostSession {
    HostSession::new(SIZE, unstarted_pty(SIZE.cols, SIZE.rows), None, &TermSettings::default()).unwrap()
}

/// 一个持有 `TOKEN` 的宿主会话，就像启动 shell 时注入了集成一样。
pub(crate) fn reporting_host() -> HostSession {
    let session = idle_host();
    session.set_report_token(TOKEN);
    session
}

/// 一个没有 shell 的伪终端，测试里只喂字节。
pub(crate) fn unstarted_pty(cols: u16, rows: u16) -> Pty {
    Pty::open(GridSize { cols, rows, ..SIZE }, Box::new(|_| true)).unwrap()
}

/// 界面这边的会话，交给宿主的请求都记在返回的列表里。
pub(crate) fn capturing_session() -> (Session, Rc<RefCell<Vec<Request>>>) {
    capturing_session_sized(SIZE)
}

pub(crate) fn capturing_session_sized(size: GridSize) -> (Session, Rc<RefCell<Vec<Request>>>) {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let sender = {
        let requests = requests.clone();
        Box::new(move |request| requests.borrow_mut().push(request))
    };
    let mut session = Session::new(size, &TermSettings::default(), sender).unwrap();
    session.apply_config(&TermSettings::default());
    (session, requests)
}

/// 界面这边 20 列 4 行的会话，宿主报告前台是 shell。
pub(crate) fn idle_session() -> Session {
    let (mut session, _) = capturing_session();
    session.apply_meta(SessionMeta { foreground_is_shell: true, ..SessionMeta::default() });
    session
}

pub(crate) fn scrollback_rows(terminal: &Terminal<'static, 'static>) -> u64 {
    let scrollbar = terminal.scrollbar().unwrap();
    scrollbar.total - scrollbar.len
}

/// shell 集成标出的提示符：`$ ` 是提示符，后面是用户输入。
pub(crate) const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

/// 测试里用的报告口令。
pub(crate) const TOKEN: &str = "0123456789abcdef0123456789abcdef";

pub(crate) const REPEAT: Duration = Duration::from_millis(500);

pub(crate) fn at(x: f32, y: f32) -> GridPoint {
    GridPoint { x, y }
}
