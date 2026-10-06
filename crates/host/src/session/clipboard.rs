//! 会话里的程序读写系统剪贴板（OSC 52）：宿主那份 VT 认出的请求（`ClipboardRequest`）按配置
//! （`ClipboardAccess`，见 `ClientMsg::SetOptions`）办。宿主不碰剪贴板，写和读都包成
//! `ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard` 请桌面办（`UiPort::ask`），交给最近和这个
//! 会话交互过的桌面：尺寸归属里最近交互的那个前端多半就是用户眼前的那台 Mac。连着的桌面都没交互
//! 过时交给最近连上的桌面；一个桌面都没连着（只有命令行、手机）时写的丢掉，读的回空。
//!
//! 读的结果回来以前程序在等，回话一律经 `HostSession::answer_clipboard`：不让读、没有桌面、桌面没
//! 回话就断开、等太久，都回一个空的剪贴板，不让等着的程序（比如粘贴时等剪贴板的编辑器）干等到它
//! 自己超时。日志里只记长度，不记剪贴板的内容。

use std::time::{Duration, Instant};

use runode_protocol::{ClientMsg, HostMsg};
use runode_shared_types::clipboard::{ClipboardAccess, ClipboardRead, ClipboardWrite};
use runode_terminal::host_session::{ClipboardQuery, ClipboardRequest};

use super::Runner;

/// 读剪贴板默认最多等桌面这么久（多半是在等用户点询问框），见 `Host::set_clipboard_read_patience`。
/// 过了就回程序一个空的剪贴板，用户之后再点允许也不再读：程序多半早就不等了。
pub(crate) const CLIPBOARD_READ_PATIENCE: Duration = Duration::from_secs(30);

/// 已经交给桌面、还在等它回话的读剪贴板请求。
pub(super) struct PendingRead {
    /// 请求的编号，见 `HostMsg::UiRequest::ui`。
    ui: u64,
    query: ClipboardQuery,
    deadline: Instant,
    /// 请求时的前台程序（进程号和名字），见 `HostSession::foreground_program`。
    program: Option<(u32, Option<String>)>,
}

impl Runner {
    /// 办宿主那份 VT 认出的一个剪贴板请求，见模块文档。
    pub(super) fn clipboard(&mut self, request: ClipboardRequest) {
        let access = self.clipboard;
        match request {
            ClipboardRequest::Write(text) => {
                let len = text.len();
                // VT 那边已经按同一份规矩拒绝了，这里兜底。
                if access.write == ClipboardWrite::Deny {
                    tracing::debug!(
                        "session {} dropped a clipboard write of {len} bytes: clipboard-write = deny",
                        self.id
                    );
                    return;
                }
                let request = ClientMsg::WriteClipboard { id: self.id, text: text.into() };
                if self.ui.ask(self.id, self.ui_connection(), request).is_none() {
                    tracing::info!(
                        "session {} dropped a clipboard write of {len} bytes: no runode window is connected",
                        self.id
                    );
                }
            }
            ClipboardRequest::Read(query) => self.read_clipboard(query, access.read),
        }
    }

    /// 程序要读剪贴板：不让读时当场回空的；要读时请桌面读（`ask` 时先问用户），等它回话。
    fn read_clipboard(&mut self, query: ClipboardQuery, policy: ClipboardRead) {
        if policy == ClipboardRead::Deny {
            tracing::debug!("session {} answered a clipboard read with nothing: clipboard-read = deny", self.id);
            self.session.answer_clipboard(query, None);
            return;
        }
        // 已经在等桌面回一个读请求（多半正问着用户）：这条当场回空的，不再弹一个询问框，程序连发
        // 也只有一个框。等的那个回话时只回它自己。
        if self.clipboard_read.is_some() {
            tracing::debug!("session {} answered a clipboard read with nothing: another one is pending", self.id);
            self.session.answer_clipboard(query, None);
            return;
        }
        let program = self.session.foreground_program();
        let name = program.as_ref().and_then(|(_, name)| name.clone());
        let request = ClientMsg::ReadClipboard { id: self.id, ask: policy == ClipboardRead::Ask, program: name };
        match self.ui.ask(self.id, self.ui_connection(), request) {
            Some(ui) => {
                let deadline = Instant::now() + self.read_patience;
                self.clipboard_read = Some(PendingRead { ui, query, deadline, program });
            }
            None => {
                tracing::info!(
                    "session {} answered a clipboard read with nothing: no runode window is connected",
                    self.id
                );
                self.session.answer_clipboard(query, None);
            }
        }
    }

    /// 改读写剪贴板的规矩：自己记一份，写的开关交给 VT（见 `HostSession::set_clipboard_writes`）。
    pub(super) fn set_clipboard(&mut self, clipboard: ClipboardAccess) {
        self.clipboard = clipboard;
        self.session.set_clipboard_writes(clipboard.write == ClipboardWrite::Allow);
    }

    /// 界面回了会话请它办的事（`Inbox::UiAnswer`）：是在等的读请求的话把结果回给程序；写剪贴板的
    /// 回话只在出错时记一笔。
    pub(super) fn ui_answered(&mut self, ui: u64, reply: HostMsg) {
        // 回话里说的是别的会话：多半是换过宿主以后旧询问框的回话凭同样的编号找了过来。不认，接着等
        // 真的回话（等太久照样回空）。
        if let HostMsg::ClipboardText { id, .. } = &reply
            && *id != self.id
            && self.clipboard_read.as_ref().is_some_and(|read| read.ui == ui)
        {
            tracing::warn!("session {} ignored a clipboard answer about session {id}", self.id);
            return;
        }
        if let Some(read) = self.clipboard_read.take_if(|read| read.ui == ui) {
            let text = match reply {
                // 等回话期间前台换了程序（比如要读的编辑器退出了、回到了 shell）：用户同意的是交给
                // 原来那个程序，不交给现在的。
                HostMsg::ClipboardText { text: Some(_), .. } if self.session.foreground_program() != read.program => {
                    tracing::info!(
                        "session {} answered a clipboard read with nothing: the program in front changed",
                        self.id
                    );
                    None
                }
                HostMsg::ClipboardText { text, .. } => text.map(|text| text.0),
                HostMsg::Error { message, .. } => {
                    tracing::info!("session {} answered a clipboard read with nothing: {message}", self.id);
                    None
                }
                other => {
                    tracing::warn!("session {} got an unexpected answer to a clipboard read: {other:?}", self.id);
                    None
                }
            };
            self.session.answer_clipboard(read.query, text.as_deref());
            return;
        }
        match reply {
            HostMsg::Done { .. } => {}
            HostMsg::Error { message, .. } => {
                tracing::info!("session {}: the runode window did not use the clipboard: {message}", self.id);
            }
            // 等太久已经回过空的读请求，桌面这时才回话。
            HostMsg::ClipboardText { .. } => tracing::debug!("session {} dropped a late clipboard read", self.id),
            other => tracing::debug!("session {} ignored an answer from the runode window: {other:?}", self.id),
        }
    }

    /// 等着的读请求什么时候算等太久，见 `Runner::deadline`。
    pub(super) fn clipboard_deadline(&self) -> Option<Instant> {
        self.clipboard_read.as_ref().map(|read| read.deadline)
    }

    /// 到了 `now`：等着的读请求等太久了的话回程序一个空的剪贴板。
    pub(super) fn clipboard_tick(&mut self, now: Instant) {
        if let Some(read) = self.clipboard_read.take_if(|read| now >= read.deadline) {
            tracing::info!(
                "session {} answered a clipboard read with nothing: the runode window did not answer",
                self.id
            );
            self.session.answer_clipboard(read.query, None);
        }
    }

    /// 剪贴板的请求交给哪条桌面的连接：连着这个会话的桌面里最近交互过的（尺寸归属里的交互，见
    /// `Runner::interact`）；都没交互过时取最近连上这个会话的；没有桌面连着这个会话时为 `None`，由
    /// `Shared::ask_ui` 交给最近连上的桌面。
    fn ui_connection(&self) -> Option<u64> {
        let last_active = |connection: u64| {
            self.viewers.iter().find(|viewer| viewer.connection == connection).map_or(0, |viewer| viewer.last_active)
        };
        self.subscribers
            .iter()
            .filter(|subscriber| subscriber.desktop)
            .max_by_key(|subscriber| last_active(subscriber.connection))
            .map(|subscriber| subscriber.connection)
    }
}
