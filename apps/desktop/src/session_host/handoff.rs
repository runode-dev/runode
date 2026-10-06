//! 升级时把旧宿主的会话交给新宿主：app 拉起这个构建的 `runode --host --take-over`（见
//! `runode_host::launch_successor`），它以 `ClientKind::Successor` 连上旧宿主谈交接、接过会话和
//! 监听的 socket，再往状态管道写一行 `HandoffStatus`；这边读到结果再决定怎么办（见
//! `session_host::establish`）。新宿主那一侧的入口是 `host_process::take_over`。

use std::{
    fs::File,
    io::{self, Read as _},
    os::fd::AsRawFd as _,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{HandoffRefusal, SessionId};
use serde::{Deserialize, Serialize};

/// 等新宿主报告结果最多这么久。比旧宿主等新宿主的期限长：到这时旧宿主已经提交或者回滚了。
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
/// 旧宿主说还有桌面连着（`HandoffRefusal::DesktopConnected`）时，最多重试这么久：旧版本的 app 刚
/// 退出时，它的连接可能还没断干净。
pub const DESKTOP_RETRY: Duration = Duration::from_secs(2);
/// 两次重试之间隔这么久。
const RETRY_PAUSE: Duration = Duration::from_millis(200);

/// 新宿主往状态管道写的一行 JSON。两端都是这个 app 自己（同一个构建），字段照样带默认值，
/// 读不懂时当失败。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffStatus {
    /// 接手成功，旧宿主已经交出会话和 socket。
    pub ok: bool,
    /// 接过来的会话个数。
    #[serde(default)]
    pub sessions: usize,
    /// 其中退成 VT 重放、回滚历史没带过来的会话。
    #[serde(default)]
    pub replayed: Vec<SessionId>,
    /// 没接手时的原因。
    #[serde(default)]
    pub error: Option<HandoffFailure>,
}

/// 新宿主没接手的原因，见 `HandoffStatus::error`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffFailure {
    /// 旧宿主不交。
    Refused { reason: HandoffRefusal },
    /// 旧宿主太老，不会交接（协议 3 及以前）。
    PreHandoff,
    /// 别的错。
    Failed { message: String },
    #[serde(other)]
    Unknown,
}

impl HandoffStatus {
    /// 接手成功。
    pub fn took_over(sessions: usize, replayed: Vec<SessionId>) -> Self {
        Self { ok: true, sessions, replayed, error: None }
    }

    /// 没接手。
    pub fn failed(error: HandoffFailure) -> Self {
        Self { ok: false, error: Some(error), ..Self::default() }
    }
}

/// 交接的结果，见 `hand_over`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 新宿主接手了 `sessions` 个会话，其中 `replayed` 个的回滚历史没带过来；它已经在 socket 上
    /// 接受连接。
    TookOver { sessions: usize, replayed: usize },
    /// 旧宿主不交，会话还在它手里。
    Refused(HandoffRefusal),
    /// 旧宿主太老，不会交接。
    PreHandoff,
    /// 没交成（拉不起新宿主、新宿主报错、没报告就退出、超时），会话还在旧宿主手里。
    Failed(String),
}

/// 两个期限，测试里改短。
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// 等一个新宿主报告结果最多多久。
    pub status: Duration,
    /// 旧宿主说还有桌面连着时最多重试多久。
    pub desktop_retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self { status: STATUS_TIMEOUT, desktop_retry: DESKTOP_RETRY }
    }
}

/// 拉起 `exe --host --take-over` 接手旧宿主的会话，等它报告结果。旧宿主说还有桌面连着时，每隔
/// 一会儿重新拉起一个再试，最多 `Timing::desktop_retry`。
pub fn hand_over(exe: &Path, timing: Timing) -> Outcome {
    let deadline = Instant::now() + timing.desktop_retry;
    loop {
        match attempt(exe, timing.status) {
            Outcome::Refused(HandoffRefusal::DesktopConnected) if Instant::now() < deadline => {
                tracing::info!("the old host still has a desktop connected, trying again");
                thread::sleep(RETRY_PAUSE);
            }
            outcome => return outcome,
        }
    }
}

/// 拉起一个新宿主，等它报告结果，最多 `timeout`。超时时不杀它：它要是在旧宿主提交之后才卡住，
/// 会话已经在它手里，杀掉会连会话一起结束；提交之前卡住的，旧宿主到期限会自己杀掉它、回滚。
fn attempt(exe: &Path, timeout: Duration) -> Outcome {
    let successor = match runode_host::launch_successor(exe) {
        Ok(successor) => successor,
        Err(err) => return Outcome::Failed(format!("cannot start the new host: {err}")),
    };
    let pid = successor.pid;
    tracing::info!("started the new host {pid} to take the sessions over");
    match read_status(&successor.status, timeout) {
        Ok(Some(status)) => outcome(status),
        Ok(None) => Outcome::Failed(format!("the new host (pid {pid}) exited without reporting")),
        Err(err) if err.kind() == io::ErrorKind::TimedOut => {
            Outcome::Failed(format!("the new host (pid {pid}) did not finish within {}s", timeout.as_secs()))
        }
        Err(err) => Outcome::Failed(format!("cannot read the new host's report: {err}")),
    }
}

fn outcome(status: HandoffStatus) -> Outcome {
    if status.ok {
        return Outcome::TookOver { sessions: status.sessions, replayed: status.replayed.len() };
    }
    match status.error {
        Some(HandoffFailure::Refused { reason }) => Outcome::Refused(reason),
        Some(HandoffFailure::PreHandoff) => Outcome::PreHandoff,
        Some(HandoffFailure::Failed { message }) => Outcome::Failed(message),
        Some(HandoffFailure::Unknown) | None => Outcome::Failed("the new host failed without saying why".into()),
    }
}

/// 从状态管道读一行、解出 `HandoffStatus`，最多等 `timeout`（超时时是 `TimedOut` 错误）。没写
/// 完一行就到了结尾时按读到的解，什么都没读到时为 `None`。
fn read_status(file: &File, timeout: Duration) -> io::Result<Option<HandoffStatus>> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = line.iter().position(|&byte| byte == b'\n') {
            line.truncate(end);
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no report in time"));
        }
        let mut poll = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let millis = libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
        // SAFETY: 只有一项，指向本地变量；描述符来自 `file`，调用期间开着。
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if ready == 0 {
            continue;
        }
        match (&mut &*file).read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => line.extend_from_slice(&chunk[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(&line).map(Some).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt as _, path::PathBuf};

    use super::*;

    const QUICK: Timing = Timing { status: Duration::from_secs(10), desktop_retry: Duration::from_secs(2) };

    /// 临时目录里的一个假的新宿主：shell 脚本 `body`，参数照收不用。
    fn fake_successor(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rnh-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("successor");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn json(status: &HandoffStatus) -> String {
        serde_json::to_string(status).unwrap()
    }

    #[test]
    fn the_status_line_round_trips() {
        let failed = HandoffStatus::failed(HandoffFailure::Refused { reason: HandoffRefusal::Busy });
        assert_eq!(serde_json::from_str::<HandoffStatus>(&json(&failed)).unwrap(), failed);
        // 读不懂的原因当失败。
        let odd: HandoffStatus = serde_json::from_str(r#"{"ok":false,"error":{"kind":"later"}}"#).unwrap();
        assert!(matches!(outcome(odd), Outcome::Failed(_)));
    }

    #[test]
    fn a_successor_that_took_over_reports_how_many_sessions() {
        let ids = vec![SessionId(1), SessionId(2)];
        let status = json(&HandoffStatus::took_over(3, ids));
        let exe = fake_successor("ok", &format!("[ \"$1 $2\" = '--host --take-over' ] || exit 9\necho '{status}' >&3"));
        assert_eq!(hand_over(&exe, QUICK), Outcome::TookOver { sessions: 3, replayed: 2 });
    }

    #[test]
    fn a_successor_that_failed_says_why() {
        let status = json(&HandoffStatus::failed(HandoffFailure::Failed { message: "boom".into() }));
        let exe = fake_successor("err", &format!("echo '{status}' >&3\nexit 1"));
        assert_eq!(hand_over(&exe, QUICK), Outcome::Failed("boom".into()));

        let status = json(&HandoffStatus::failed(HandoffFailure::PreHandoff));
        let exe = fake_successor("pre", &format!("echo '{status}' >&3"));
        assert_eq!(hand_over(&exe, QUICK), Outcome::PreHandoff);

        let refused = HandoffFailure::Refused { reason: HandoffRefusal::UnsupportedFormat { writes: 9 } };
        let exe = fake_successor("format", &format!("echo '{}' >&3", json(&HandoffStatus::failed(refused))));
        assert_eq!(hand_over(&exe, QUICK), Outcome::Refused(HandoffRefusal::UnsupportedFormat { writes: 9 }));
    }

    /// 没报告就退出（比如崩溃了）：读到结尾，不等到超时。
    #[test]
    fn a_successor_that_exits_without_reporting_fails_at_once() {
        let exe = fake_successor("eof", "exit 3");
        let started = Instant::now();
        let outcome = hand_over(&exe, QUICK);
        assert!(matches!(&outcome, Outcome::Failed(reason) if reason.contains("without reporting")), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    /// 写了半行就退出：照样按读到的解，解不开算失败。
    #[test]
    fn half_a_report_is_a_failure() {
        let exe = fake_successor("half", "printf '{\"ok\":tr' >&3");
        assert!(matches!(hand_over(&exe, QUICK), Outcome::Failed(_)));
    }

    #[test]
    fn a_successor_that_hangs_times_out() {
        // `exec` 让 sleep 接过 fd 3：脚本一直不写也不关。
        let exe = fake_successor("hang", "exec sleep 5");
        let timing = Timing { status: Duration::from_millis(300), ..QUICK };
        let started = Instant::now();
        let outcome = hand_over(&exe, timing);
        assert!(matches!(&outcome, Outcome::Failed(reason) if reason.contains("did not finish")), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    /// 旧版本的 app 刚退出、连接还没断干净：重新拉起新宿主再试，第二次成了。
    #[test]
    fn a_desktop_still_connected_is_retried_for_a_while() {
        let refused =
            json(&HandoffStatus::failed(HandoffFailure::Refused { reason: HandoffRefusal::DesktopConnected }));
        let ok = json(&HandoffStatus::took_over(1, Vec::new()));
        let marker = std::env::temp_dir().join(format!("rnh-{}-retry-marker", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let body = format!(
            "if [ -e '{m}' ]; then echo '{ok}' >&3; else touch '{m}'; echo '{refused}' >&3; fi",
            m = marker.display()
        );
        let exe = fake_successor("retry", &body);
        assert_eq!(hand_over(&exe, QUICK), Outcome::TookOver { sessions: 1, replayed: 0 });

        // 一直连着：到期限后报拒绝。
        let exe = fake_successor("stuck", &format!("echo '{refused}' >&3"));
        let timing = Timing { desktop_retry: Duration::from_millis(500), ..QUICK };
        assert_eq!(hand_over(&exe, timing), Outcome::Refused(HandoffRefusal::DesktopConnected));
    }

    #[test]
    fn a_successor_that_cannot_start_fails() {
        let outcome = hand_over(Path::new("/nonexistent/runode"), QUICK);
        assert!(matches!(&outcome, Outcome::Failed(reason) if reason.contains("cannot start")), "{outcome:?}");
    }
}
