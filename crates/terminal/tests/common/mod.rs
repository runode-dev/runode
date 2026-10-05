//! 各个测试文件共用的会话和辅助函数。
#![allow(dead_code, reason = "每个测试文件各自编译成一个 crate，只用到这里的一部分")]

use std::{cell::RefCell, rc::Rc, time::Duration};

use runode_shared_types::{
    frame::Frame,
    grid::{GridPoint, GridSize},
    session::SessionMeta,
    settings::TermSettings,
    shell::IntegrationMode,
};
use runode_terminal::{
    host_session::HostSession,
    pty::Pty,
    session::{Request, Session},
};

pub fn row_text(frame: &Frame, y: u16) -> String {
    frame
        .row(y)
        .iter()
        .filter(|c| !c.spacer)
        .map(|c| if c.text.is_empty() { " " } else { c.text.as_str() })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };

/// 宿主这边的会话：`cat` 当 shell，自己不输出，VT 只会收到测试喂进去的内容。它也是前台
/// 进程，所以前台总是「shell」。
pub fn idle_host() -> HostSession {
    let pty = Pty::spawn(SIZE, Some("/bin/cat"), None, IntegrationMode::Off, Box::new(|_| true)).unwrap();
    HostSession::new(SIZE, pty, None, &TermSettings::default()).unwrap()
}

/// 一个没有 shell 的伪终端，测试里只喂字节。
pub fn unstarted_pty(cols: u16, rows: u16) -> Pty {
    Pty::open(GridSize { cols, rows, ..SIZE }, Box::new(|_| true)).unwrap()
}

/// 界面这边的会话，交给宿主的请求都记在返回的列表里。
pub fn capturing_session() -> (Session, Rc<RefCell<Vec<Request>>>) {
    capturing_session_sized(SIZE)
}

pub fn capturing_session_sized(size: GridSize) -> (Session, Rc<RefCell<Vec<Request>>>) {
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
pub fn idle_session() -> Session {
    let (mut session, _) = capturing_session();
    session.apply_meta(SessionMeta { foreground_is_shell: true, ..SessionMeta::default() });
    session
}

/// shell 集成标出的提示符：`$ ` 是提示符，后面是用户输入。
pub const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

pub const REPEAT: Duration = Duration::from_millis(500);

pub fn at(x: f32, y: f32) -> GridPoint {
    GridPoint { x, y }
}
