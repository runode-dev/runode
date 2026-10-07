//! 宿主单独一个进程跑时（`runode --host`）什么时候退出：没有会话也没有连接、持续一段时间后
//! 自己退出（`Host::set_stay_up` 时不因空闲退出），或者前端发来 `Shutdown`。

use std::time::{Duration, Instant};

use crate::Host;

/// 有连接断开时之外，隔多久再看一次是不是空闲了；短的空闲时限按它的几分之一看。
const MAX_TICK: Duration = Duration::from_millis(500);
/// 收到 `Shutdown` 后，最多等这么久让各条连接写完 `Goodbye` 断开。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// `Host::run_until_idle` 为什么返回。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// 没有会话也没有连接，持续了给定的时长。
    Idle,
    /// 前端发来 `ClientMsg::Shutdown`：会话都结束了，各条连接收到了 `Goodbye`。
    Shutdown,
    /// 会话和监听的 socket 都交给了接手的新版本宿主（见 `Host::take_over`），各条连接收到了
    /// `Goodbye`。socket 文件留着，新宿主接着在上面监听；进程照常退出。
    Handoff,
}

impl Host {
    /// 说明宿主单独一个进程在跑，在 `Host::listen` 之前调：之后连上来的前端在 `Welcome` 里就
    /// 知道它不是哪个 app 里的宿主，不会把它当成别的 app 的。`run_until_idle` 也会标上。
    pub fn mark_standalone(&self) {
        self.shared.peers().standalone = true;
    }

    /// 宿主单独一个进程跑时的主循环：等到该退出时返回，调用方接着退出进程。会话数和连接数都是 0、
    /// 持续 `idle` 后退出；收到 `Shutdown` 时结束所有会话、给所有连接发 `Goodbye` 后退出。
    ///
    /// 退出前不再接新连接，删掉 `Host::listen` 开的 socket、放开锁：判断空闲和停止接新连接在同一把
    /// 锁里，不会有连接在两者之间连上来又被丢下；之后来的前端连不上，自己拉起新的宿主，新宿主
    /// 拿得到锁。交接给了新宿主（`Stopped::Handoff`）时 socket 文件不删，锁也还在新宿主手里。
    ///
    /// 调用之后的 `Shutdown` 才会让宿主退出，在这之前（以及从不调用、宿主跑在 app 进程里时）
    /// `Shutdown` 只结束所有会话。
    pub fn run_until_idle(&self, idle: Duration) -> Stopped {
        let shared = &self.shared;
        let tick = (idle / 4).clamp(Duration::from_millis(5), MAX_TICK);
        let mut peers = shared.peers();
        peers.standalone = true;
        let mut quiet_since: Option<Instant> = None;
        let reason = loop {
            if let Some(reason) = peers.stop {
                break reason;
            }
            // 锁的先后：拿着 `peers` 再拿 `registry`。
            let quiet = !peers.stay_up && peers.connection_count() == 0 && shared.registry().sessions.is_empty();
            if quiet {
                let since = *quiet_since.get_or_insert_with(Instant::now);
                let since = since.max(peers.activity_at);
                if since.elapsed() >= idle {
                    peers.stop = Some(Stopped::Idle);
                    break Stopped::Idle;
                }
            } else {
                quiet_since = None;
            }
            peers = shared.peers_changed.wait_timeout(peers, tick).map_or_else(|err| err.into_inner().0, |(p, _)| p);
        };
        peers.accepting = false;
        if let Some(listening) = peers.listening.take() {
            listening.control.stop();
            if reason != Stopped::Handoff
                && let Err(err) = std::fs::remove_file(&listening.socket)
            {
                tracing::debug!("failed to remove {}: {err}", listening.socket.display());
            }
            // 丢掉时放开锁。
            drop(listening);
        }
        // 等 `Goodbye` 写出去、各条连接断开；对面不读的不等太久。
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        while peers.connection_count() > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                tracing::debug!("{} connections still open on exit", peers.connection_count());
                break;
            }
            peers = shared.peers_changed.wait_timeout(peers, left).map_or_else(|err| err.into_inner().0, |(p, _)| p);
        }
        tracing::info!("the host stops: {reason:?}");
        reason
    }

    /// `stay_up` 时没有会话也没有连接也不算空闲，`run_until_idle` 不因为空闲返回（`Shutdown`、交接
    /// 照旧）：比如开着远程访问、有配对过的手机，手机随时可能连上来开新会话。改回 false 时空闲从
    /// 这一刻重新算起。
    pub fn set_stay_up(&self, stay_up: bool) {
        let mut peers = self.shared.peers();
        if peers.stay_up == stay_up {
            return;
        }
        peers.stay_up = stay_up;
        peers.activity_at = Instant::now();
        drop(peers);
        self.shared.peers_changed.notify_all();
    }
}
