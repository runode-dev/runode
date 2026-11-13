//! 连接上的前端：别的进程经 `Host::listen` 开的 Unix socket 连上来，同一个进程里的桌面经
//! `Host::connect_pair` 拿一对 socket 的一端，两种连接走同一个 `serve`，按 `runode_protocol` 的帧
//! 收发，读和写各一个线程。
//!
//! 读的线程先等 `Hello`，回 `Welcome`（协议版本对不上时回 `Incompatible` 后断开），之后把控制
//! 消息和输入帧转给宿主。写的线程按到达的先后把帧写出去：连上的会话在自己的线程里把事件交给
//! 这条连接的 `Outbox`，一个会话的帧在连接上的先后就是它发生的先后。前端读得太慢、积压超过
//! `OUTBOX_LIMIT` 时，那个会话不再往这条连接发输出，改发 `Resync`，前端重新 `Attach`；连上会话时
//! 排进去的屏幕不算会话输出的积压。转给连接的输出里，shell 集成的报告抹掉了内容（见 `ReportRedactor`），报告带的口令不出宿主。
//!
//! 读的线程不等会话线程回话：列会话、读屏幕先在读的线程里把请求按先后送到会话线程，再起一个
//! 短命的线程等回话、交给 `Outbox`，同一条连接上之后的输入照常转发。这类等回话的请求按 `Waiting`
//! 限量，超出的当场回 `Error`；git 请求、列目录（`ClientMsg::ListDirs`）、列命令（`ClientMsg::ListTasks`）
//! 和开会话（`ClientMsg::Spawn`）各另起线程办、另算名额，一类卡住（联网的 git、断掉的网络挂载上的
//! stat）不挤占别的；回话按 `req` 认，先后不定。积压过了 `OUTBOX_LIMIT` 的连接，列会话、读屏幕、
//! 连上会话和 git 请求都当场回 `Error`，不再往积压上加回话；别的回话照办（输入的 `Done` 回错的话
//! 粘贴就丢了），积压远超上限（`OUTBOX_HARD_LIMIT`）时读的线程直接断开连接。
//!
//! 能连上 socket 就能读写所有终端：socket 放在只有自己能进的目录里（见调用方建目录的方式），
//! 连上来的进程也要是同一个用户的。`Hello` 里的 `ClientKind` 是前端自报的，宿主不核实：终端里的
//! 程序都拿得到 `RUNODE_SOCKET`，自称 `Desktop` 就能登记为界面、改宿主的选项。按种类拦的那些请求
//! 只防误用（比如命令行误发），不防同一个用户的恶意程序，那本来也挡不住。

use std::{
    collections::HashMap,
    sync::{Arc, Weak, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[cfg(unix)]
pub(crate) use std::os::unix::net::{UnixListener, UnixStream};

use runode_protocol::{ClientMsg, GoodbyeReason, HostMsg, SessionId, WindowLayout};
#[cfg(windows)]
pub(crate) use uds_windows::{UnixListener, UnixStream};

use crate::{Shared, Stopped, session::Inbox};

mod connection;
mod listen;
mod outbox;
#[cfg(windows)]
mod windows;

pub(crate) use connection::WaitSlot;
#[cfg(unix)]
pub(crate) use listen::ListenControl;
pub(crate) use listen::Listening;
pub use listen::{bind_fresh, set_buffers};
#[cfg(unix)]
pub use listen::{same_user, socket_pair};
pub(crate) use outbox::Outbox;
#[cfg(windows)]
pub use windows::{peer_pid, same_user, socket_pair};

/// 等会话线程回话（列会话、读屏幕）的最长时间。
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// 连着的前端、转给界面还没回话的请求，以及监听和退出的状态。
pub(crate) struct Peers {
    /// 连着的连接，按编号；`out` 在连接的写线程起好之前为 `None`。
    connections: HashMap<u64, Peer>,
    /// 说自己是 `ClientKind::Desktop` 的连接，按 `Hello` 的先后；请求没指明交给哪个界面、或者指明的那个
    /// 已经断开时交给最后一个。种类是自报的，见模块文档。
    desktops: Vec<u64>,
    /// 转给界面、还没回话的请求，按 `HostMsg::UiRequest::ui`。
    pending: HashMap<u64, Pending>,
    next_ui: u64,
    /// 宿主手里的布局：界面上次报来的，没有界面连着时宿主自己办的请求也改它，见 `workspaces`。
    pub(crate) layout: Vec<WindowLayout>,
    /// 还接新连接；退出时在判断要退出的同一把锁里置为 false。
    pub(crate) accepting: bool,
    /// `Host::listen` 开的 socket 和拿着的锁。
    pub(crate) listening: Option<Listening>,
    /// 宿主单独一个进程在跑，见 `Host::run_until_idle`：收到 `Shutdown` 时连宿主一起退出。
    /// `HostMsg::Welcome::standalone` 报的就是它。
    pub(crate) standalone: bool,
    /// 照常办事、正在把会话交给新宿主，还是要退出了。
    pub(crate) phase: Phase,
    /// 最近一次有连接连上或者断开的时刻，空闲从这时起算。
    pub(crate) activity_at: Instant,
    /// 没有会话也没有连接时也不算空闲，见 `Host::set_stay_up`。
    pub(crate) stay_up: bool,
    /// 看板上有定时：到点要开任务，没有会话也没有连接时也不算空闲，见 `todos`。
    pub(crate) scheduled: bool,
    /// `run_until_idle` 不因空闲返回（给的时限是 `Duration::MAX`），交接时告诉新宿主。
    #[cfg(unix)]
    pub(crate) stay: bool,
}

/// 宿主走到哪一步了。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// 照常办事。
    Serving,
    /// 正在把会话交给新宿主（`handoff::give`）：新来的连接收到 `Goodbye { Handoff }`，不开新会话，不改看板；
    /// 交接没成时回到 `Serving`。
    #[cfg_attr(windows, expect(dead_code, reason = "Windows 上不交接"))]
    HandingOver,
    /// 要退出了，为什么；不再开新会话。
    Stopping(Stopped),
}

impl Default for Peers {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
            desktops: Vec::new(),
            pending: HashMap::new(),
            next_ui: 1,
            layout: Vec::new(),
            accepting: true,
            listening: None,
            standalone: false,
            phase: Phase::Serving,
            activity_at: Instant::now(),
            stay_up: false,
            scheduled: false,
            #[cfg(unix)]
            stay: false,
        }
    }
}

impl Peers {
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.len()
    }

    #[cfg(unix)]
    pub(crate) fn has_desktop(&self) -> bool {
        !self.desktops.is_empty()
    }

    pub(crate) fn desktop_count(&self) -> u32 {
        u32::try_from(self.desktops.len()).unwrap_or(u32::MAX)
    }

    /// 布局变了：告诉问过 `Layout` 的连接。
    pub(crate) fn layout_changed(&self) {
        for peer in self.connections.values().filter(|peer| peer.watches_layout) {
            if let Some(out) = &peer.out {
                out.control(&HostMsg::LayoutChanged);
            }
        }
    }

    /// 看板变了：告诉问过 `ListTodos` 的连接。
    pub(crate) fn todos_changed(&self) {
        for peer in self.connections.values().filter(|peer| peer.watches_todos) {
            if let Some(out) = &peer.out {
                out.control(&HostMsg::TodosChanged);
            }
        }
    }

    /// 给 `except` 以外的连接都发 `Goodbye`，写完后断开。
    /// 写线程还没起好的连接起好时自己发，见 `serve`。
    pub(crate) fn say_goodbye(&self, except: Option<u64>, reason: &GoodbyeReason) {
        for (&id, peer) in &self.connections {
            if Some(id) != except
                && let Some(out) = &peer.out
            {
                out.control(&HostMsg::Goodbye { reason: reason.clone() });
                out.close();
            }
        }
    }
}

struct Peer {
    out: Option<Outbox>,
    /// 问过 `Layout`：界面说布局变了时告诉它，见 `HostMsg::LayoutChanged`。
    watches_layout: bool,
    /// 问过 `ListTodos`：看板变了时告诉它，见 `HostMsg::TodosChanged`。
    watches_todos: bool,
}

/// 转给界面、还没回话的一条请求。
struct Pending {
    /// 转给了哪条界面的连接；只认它回的话。
    desktop: u64,
    /// 回话交给谁。
    asker: Asker,
}

/// 请界面办事的一方。
enum Asker {
    /// 别的连接发来的请求（`Open`、`Reveal`、`Layout`）。
    Client {
        /// 发请求的那条连接的编号；它断开时这条请求跟着撤掉。
        from: u64,
        /// 发请求的那条连接，回话交给它。
        origin: Outbox,
        req: u32,
    },
    /// 会话自己的请求（读写剪贴板），回话交给会话线程（`Inbox::UiAnswer`）。
    Session(SessionId),
}

/// `Shared::send_to_ui` 办得怎样。
enum ToUi {
    /// 交出去了，登记的编号。
    Sent(u64),
    /// 没有界面连着，请求交回来。
    NoDesktop(Box<ClientMsg>),
    /// 界面的连接刚断开，撤掉的登记；读线程已经撤掉、替它回过话时为空。
    Gone(Option<Asker>),
}

/// 会话线程请界面办事、报状态变了的一头，见 `Shared::ask_ui`、`Shared::todo_session_changed`。拿着宿主的弱引用：
/// 会话线程不该让宿主留着不放，宿主没了时什么都办不成。
#[derive(Clone, Default)]
pub(crate) struct UiPort(Weak<Shared>);

impl UiPort {
    pub(crate) fn new(shared: Weak<Shared>) -> Self {
        Self(shared)
    }

    /// 见 `Shared::todo_session_changed`。
    pub(crate) fn todo_session_changed(
        &self,
        session: SessionId,
        ended: bool,
        agent: Option<runode_types::agent::AgentState>,
    ) {
        if let Some(shared) = self.0.upgrade() {
            shared.todo_session_changed(session, ended, agent);
        }
    }

    /// 见 `Shared::ask_ui`。
    pub(crate) fn ask(&self, session: SessionId, preferred: Option<u64>, request: ClientMsg) -> Option<u64> {
        self.0.upgrade()?.ask_ui(session, preferred, request)
    }

    /// 会话线程 `id` 出了事退出时把它从登记表里摘掉；这个 id 已经换成别的线程（接手、收回）时不动。
    pub(crate) fn forget(&self, id: SessionId, killed: &Arc<AtomicBool>) {
        let Some(shared) = self.0.upgrade() else { return };
        let mut registry = shared.registry();
        if registry.sessions.get(&id).is_some_and(|handle| handle.owns(killed)) {
            registry.sessions.remove(&id);
        }
    }
}

impl Shared {
    /// 要退出了（收到 `Shutdown`，调用方已经结束了所有会话）。宿主单独一个进程在跑时，给所有连接发
    /// `Goodbye`、写完后断开，由 `Host::run_until_idle` 收尾，返回 true；跑在 app 进程里时什么都不做，
    /// 返回 false。
    ///
    /// 置上 `Phase::Stopping` 和再清一遍登记表在同一把锁里：调用方结束所有会话之后、这里拿到锁之前开好的会话（别的
    /// 连接、没有界面时另起的线程开的）在这里结束；之后 `Shared::spawn_with` 看得到它，不再登记。
    pub(crate) fn shut_down(&self) -> bool {
        let mut peers = self.peers();
        if !peers.standalone {
            return false;
        }
        // 已经在停（交接已经提交、闲着退出）时不改原因。
        if !matches!(peers.phase, Phase::Stopping(_)) {
            peers.phase = Phase::Stopping(Stopped::Shutdown);
        }
        peers.accepting = false;
        // 锁的先后：拿着 `peers` 再拿 `registry`。
        for (_, handle) in self.registry().sessions.drain() {
            handle.send(Inbox::Kill);
        }
        peers.say_goodbye(None, &GoodbyeReason::Shutdown);
        drop(peers);
        self.peers_changed.notify_all();
        true
    }

    /// 会话 `session` 请界面办一件事（`request`），返回这条请求的编号，界面的回话经
    /// `Inbox::UiAnswer` 带着它交回会话线程。交给 `preferred` 这条界面的连接，它不是（或者不再是）
    /// 界面时交给最近连上的界面；没有界面连着、或者刚断开时返回 `None`。
    fn ask_ui(&self, session: SessionId, preferred: Option<u64>, request: ClientMsg) -> Option<u64> {
        match self.send_to_ui(preferred, Asker::Session(session), request) {
            ToUi::Sent(ui) => Some(ui),
            ToUi::NoDesktop(_) | ToUi::Gone(_) => None,
        }
    }

    /// 把 `request` 登记成 `asker` 请界面办的事，交给 `preferred` 这条界面的连接，没给、或者它不是（或者不再是）
    /// 界面时交给最近连上的界面。界面的连接刚断开时撤掉登记：它的读线程撤掉登记时已经替还在等的请求回过话的话，
    /// `ToUi::Gone` 里是空的。
    fn send_to_ui(&self, preferred: Option<u64>, asker: Asker, request: ClientMsg) -> ToUi {
        let mut peers = self.peers();
        let desktop =
            preferred.filter(|connection| peers.desktops.contains(connection)).or(peers.desktops.last().copied());
        let Some((desktop, out)) =
            desktop.and_then(|desktop| Some((desktop, peers.connections.get(&desktop)?.out.clone()?)))
        else {
            return ToUi::NoDesktop(Box::new(request));
        };
        let ui = peers.next_ui;
        peers.next_ui += 1;
        peers.pending.insert(ui, Pending { desktop, asker });
        drop(peers);
        if out.control(&HostMsg::UiRequest { ui, request: Box::new(request) }) {
            return ToUi::Sent(ui);
        }
        ToUi::Gone(self.peers().pending.remove(&ui).map(|pending| pending.asker))
    }
}

#[cfg(test)]
mod tests;
