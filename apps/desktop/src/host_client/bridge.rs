//! 远端这一侧的桥接（`runode --ssh-bridge`）：桌面经 ssh 在远端跑它，它连上这台机器的宿主，在 stdout
//! 打一行开场白（`runode_ssh::bridge::Opening`），之后把 stdin 和 stdout 原样接到宿主的 socket 上，桌面
//! 照旧按 `runode_protocol` 和远端的宿主说话。
//!
//! 宿主没在跑就拉起来；在跑的是别的构建（桌面升级后装上了新版本）时让这个构建的新宿主接手它的会话，
//! 远端的终端和 agent 不断。接不了手时（远端还连着装旧版本的桌面）协议对得上就照样连旧的。
//! 宿主装成了登录服务时经服务管理器拉起（`service`）。Windows 上不交接：ConPTY 交不出去，换宿主就结束那台
//! 电脑上开着的终端，所以在跑的是别的构建时协议对得上就照样连它，只有它没有会话、也没有桌面连着时才换掉。

#[cfg(unix)]
mod logind;
#[cfg(unix)]
pub mod service;

use std::{
    io::{self, BufWriter, Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use runode_protocol::HandoffRefusal;
use runode_ssh::bridge::{Failure, Opening};

#[cfg(unix)]
use super::handoff::{self, Outcome};
use super::{
    UnixStream, build,
    launch::{self, Probe},
};

/// 拉起宿主后最多等这么久连上它：刚装上的构建第一次跑时，系统要先校验、扫描一遍可执行文件。
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
/// 另一个连接正在让新宿主接手时，等这么久再探。
const BUSY_RETRY: Duration = Duration::from_secs(1);
const BUSY_TRIES: u32 = 10;

/// 跑桥接，返回进程的退出码。
pub fn run() -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    crate::logging::init_file(dirs.ssh_bridge_log_file());
    // SAFETY: 还没起任何线程。
    unsafe { tidy_env(&dirs) };
    #[cfg(windows)]
    tidy_uploads(&dirs);
    let mut stdout = io::stdout().lock();
    match connect(&dirs) {
        Ok((stream, opening)) => {
            tracing::info!("bridging to the host: {opening:?}");
            if stdout.write_all(opening.line().as_bytes()).and_then(|()| stdout.flush()).is_err() {
                return 1;
            }
            drop(stdout);
            pump_stdio(stream, "bridge-in", 64 * 1024);
            0
        }
        Err(failure) => {
            tracing::warn!("cannot bridge to the host: {failure:?}");
            let _ = stdout.write_all(failure.line().as_bytes()).and_then(|()| stdout.flush());
            1
        }
    }
}

/// 拉起宿主之前整理环境，它和它起的 shell 都继承这里的环境：这次 ssh 连接的 `SSH_CONNECTION` 这类变量
/// 在连接断开后就是错的，不带下去；转发来的 ssh-agent 换成指向它的固定链接（`Dirs::ssh_agent_link`），
/// 每次连进来都改指最新的那个。
///
/// # Safety
///
/// 要改环境变量：调用时进程里不能有别的线程。
#[cfg_attr(windows, expect(unused_variables, reason = "Windows 上只去掉连接的变量"))]
unsafe fn tidy_env(dirs: &runode_paths::Dirs) {
    for key in ["SSH_CLIENT", "SSH_CONNECTION", "SSH_TTY", "SSH_ORIGINAL_COMMAND"] {
        // SAFETY: 调用方保证还没有别的线程。
        unsafe { std::env::remove_var(key) };
    }
    #[cfg(unix)]
    // SAFETY: 调用方保证还没有别的线程。
    unsafe {
        tidy_unix_env(dirs)
    };
}

/// 去掉本机带过来、这台机器上装不了的 locale，转发来的 ssh-agent 换成指向它的固定链接。
///
/// # Safety
///
/// 要改环境变量：调用时进程里不能有别的线程。
#[cfg(unix)]
unsafe fn tidy_unix_env(dirs: &runode_paths::Dirs) {
    // SAFETY: 调用方保证还没有别的线程。
    unsafe { drop_missing_locales() };
    let Some(agent) = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from) else {
        return;
    };
    let Some(link) = dirs.ssh_agent_link() else {
        return;
    };
    if agent == link || dirs.create_runtime_dir().is_err() {
        return;
    }
    if let Err(err) = relink(&agent, &link) {
        tracing::warn!("cannot point {} at the forwarded agent: {err}", link.display());
        return;
    }
    // SAFETY: 调用方保证还没有别的线程。
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &link) };
}

/// 本机的 ssh 常把自己的 `LANG`、`LC_*` 一起带过来（macOS 默认 `SendEnv LANG LC_*`），这台机器上没装那个
/// locale 时，宿主起的每个 shell 一开就报 setlocale 的警告。装不上的去掉；连 `LANG` 都去掉了、又没有别的
/// 设定时用 `C.UTF-8`，终端里照样是 UTF-8。
///
/// # Safety
///
/// 要改环境变量：调用时进程里不能有别的线程。
#[cfg(unix)]
unsafe fn drop_missing_locales() {
    const KEYS: [&str; 14] = [
        "LC_ALL",
        "LC_CTYPE",
        "LC_COLLATE",
        "LC_MESSAGES",
        "LC_MONETARY",
        "LC_NUMERIC",
        "LC_TIME",
        "LC_ADDRESS",
        "LC_IDENTIFICATION",
        "LC_MEASUREMENT",
        "LC_NAME",
        "LC_PAPER",
        "LC_TELEPHONE",
        "LANG",
    ];
    let mut dropped = false;
    for key in KEYS {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        if value.is_empty() || locale_exists(&value) {
            continue;
        }
        // SAFETY: 调用方保证还没有别的线程。
        unsafe { std::env::remove_var(key) };
        dropped = true;
    }
    if dropped && KEYS.iter().all(|key| std::env::var_os(key).is_none()) {
        // SAFETY: 调用方保证还没有别的线程。
        unsafe { std::env::set_var("LANG", "C.UTF-8") };
    }
}

/// 这台机器上能不能用 locale `name`。
#[cfg(unix)]
fn locale_exists(name: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: `name` 是以 NUL 结尾的字符串；建出来的 locale 马上释放。
    unsafe {
        let locale = libc::newlocale(libc::LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut());
        if locale.is_null() {
            return false;
        }
        libc::freelocale(locale);
    }
    true
}

/// 原子地把符号链接 `link` 改指 `target`。
#[cfg(unix)]
fn relink(target: &Path, link: &Path) -> io::Result<()> {
    let staged = link.with_extension(format!("{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)?;
    std::fs::rename(&staged, link)
}

/// 连上这台机器的宿主（需要时拉起、让新宿主接手），交回连接和开场白。
fn connect(dirs: &runode_paths::Dirs) -> Result<(UnixStream, Opening), Failure> {
    let fail = |code: &str, message: String| Failure { code: code.into(), message };
    let exe = std::env::current_exe().map_err(|err| fail("exe", format!("cannot tell where runode is: {err}")))?;
    dirs.create_runtime_dir().map_err(|err| fail("runtime", format!("no place for the host socket: {err}")))?;
    let socket = dirs.host_socket_file().ok_or_else(|| fail("runtime", "the host socket path is too long".into()))?;
    let ours = build();
    let mut busy = 0;
    loop {
        let probe = launch::probe(&socket, &ours);
        tracing::info!("host on {}: {probe:?}", socket.display());
        // 拉起宿主时交回的，拿到连上它或者放弃（`launch_host`）。
        let mut _launched = None;
        let via = match &probe {
            Probe::Absent if start_service(dirs, &exe) => Via::Service,
            Probe::Absent => {
                _launched = Some(
                    launch_host(&exe, &socket)
                        .map_err(|err| fail("launch", format!("cannot start the host: {err}")))?,
                );
                Via::Launched
            }
            Probe::OtherApp => Via::InApp,
            Probe::Running { build, .. } if *build == ours => Via::Kept,
            Probe::Running { .. } | Probe::Incompatible { .. } => match take_over(dirs, &exe, &socket, &probe) {
                Upgrade::Done(via) => via,
                Upgrade::Retired => {
                    _launched = Some(
                        launch_host(&exe, &socket)
                            .map_err(|err| fail("upgrade", format!("cannot replace the old host: {err}")))?,
                    );
                    Via::Launched
                }
                Upgrade::Busy if busy < BUSY_TRIES => {
                    busy += 1;
                    thread::sleep(BUSY_RETRY);
                    continue;
                }
                Upgrade::Busy => return Err(fail("busy", "another connection kept upgrading the host".into())),
                Upgrade::Failed(reason) => return Err(fail("upgrade", reason)),
            },
        };
        let stream = connect_raw(&socket).map_err(|err| fail("connect", format!("cannot reach the host: {err}")))?;
        let pid = launch::host_pid(&socket).unwrap_or(0);
        let host = match launch::probe(&socket, &ours) {
            Probe::Running { build, .. } => build.0,
            _ => String::new(),
        };
        let via = if matches!(via, Via::Kept | Via::InApp) || host == ours.0 { via } else { Via::Mismatch };
        // 桌面按 `/` 分隔认远端的路径，Windows 的也是；Unix 上 `\\` 是文件名里的普通字符，不换。
        let home = dirs
            .home
            .as_deref()
            .map(|home| {
                let home = home.to_string_lossy();
                if cfg!(windows) { home.replace('\\', "/") } else { home.into_owned() }
            })
            .unwrap_or_default();
        let opening = Opening {
            build: host,
            pid,
            via: via.name().into(),
            session_bound: session_bound(pid),
            os: std::env::consts::OS.into(),
            home,
        };
        return Ok((stream, opening));
    }
}

/// 拉起宿主。
#[cfg(unix)]
fn launch_host(exe: &Path, _: &Path) -> io::Result<()> {
    runode_host::launch(exe).map(drop)
}

/// 拉起宿主。屏幕上登录着同一个用户时起在他的桌面上（`desktop_session`），终端里开的窗口他看得见，他自己开的
/// runode 连上的也是这个宿主，交回的任务要拿到连上宿主以后再放；任务没建成、`LAUNCH_TIMEOUT` 内 `socket`
/// 上还没有宿主，或者没人登录时，起在 ssh 所在的会话里，脱开 ssh 照样跑着。
#[cfg(windows)]
fn launch_host(exe: &Path, socket: &Path) -> io::Result<Option<crate::desktop_session::Scheduled>> {
    if matches!(crate::desktop_session::desktop(), crate::desktop_session::Desktop::Mine) {
        match crate::desktop_session::schedule(exe, "--host") {
            Ok(task) => match connect_raw(socket) {
                Ok(_) => return Ok(Some(task)),
                Err(err) => tracing::warn!("the host started on the desktop never came up, starting it here: {err}"),
            },
            Err(err) => tracing::warn!("cannot start the host on the desktop, starting it here: {err}"),
        }
    }
    runode_host::launch(exe).map(|_| None)
}

/// 宿主装成了 Linux 的登录服务时经服务管理器起它；没装、没起成时返回假。
fn start_service(dirs: &runode_paths::Dirs, exe: &Path) -> bool {
    if !cfg!(target_os = "linux") || !runode_autostart::is_installed(runode_autostart::Kind::Host, dirs) {
        return false;
    }
    #[cfg(unix)]
    service::offer_agent(dirs);
    match runode_autostart::install(
        runode_autostart::Kind::Host,
        dirs,
        &stable_exe(dirs, exe),
        runode_autostart::Start::Now,
    ) {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!("cannot start the host service, starting the host directly: {err}");
            false
        }
    }
}

/// 桥接怎么连上远端宿主的，写进开场白。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Via {
    /// 新拉起的。
    Launched,
    /// 本来就在跑、同一个构建。
    Kept,
    /// 新宿主接手了旧宿主的会话。
    #[cfg_attr(windows, expect(dead_code, reason = "Windows 上不交接"))]
    Took,
    /// 登录服务换上了新版本。
    Service,
    /// 连上了别的构建、没能换。
    Mismatch,
    /// 远端 Mac 上开着 runode app、宿主跑在它里面。
    InApp,
}

impl Via {
    fn name(self) -> &'static str {
        match self {
            Self::Launched => "launched",
            Self::Kept => "kept",
            Self::Took => "took",
            Self::Service => "service",
            Self::Mismatch => "mismatch",
            Self::InApp => "in_app",
        }
    }
}

enum Upgrade {
    Done(Via),
    /// 旧宿主已经退出，由调用方拉起这个构建的宿主。
    Retired,
    #[cfg_attr(windows, expect(dead_code, reason = "Windows 上不交接"))]
    Busy,
    Failed(String),
}

/// 在跑的是别的构建：它没有会话、也没有那台电脑上的桌面连着时让它退出、换上这个构建的（`Retired`）；
/// 不然或者退不掉时协议对得上就照样连它（`Via::Mismatch`），对不上的连不了。
#[cfg(windows)]
fn take_over(_: &runode_paths::Dirs, _: &Path, socket: &Path, probe: &Probe) -> Upgrade {
    match probe {
        Probe::Running { sessions: 0, desktops: 0, .. } => match launch::retire(socket, &build()) {
            Ok(()) => Upgrade::Retired,
            // 退出时连接被重置而不是正常关上，也算退了。
            Err(_) if launch::probe(socket, &build()) == Probe::Absent => Upgrade::Retired,
            Err(err) => {
                tracing::warn!("cannot retire the old host without sessions, keeping it: {err}");
                Upgrade::Done(Via::Mismatch)
            }
        },
        Probe::Running { .. } => Upgrade::Done(Via::Mismatch),
        _ => Upgrade::Failed(
            "runode on this computer is a version that cannot talk to this one; quit it there first".into(),
        ),
    }
}

/// 在跑的是别的构建：让这个构建的新宿主接手。登录服务管着它时经服务换；接不了手、协议又对得上时照样
/// 用旧的（`Via::Mismatch`），旧宿主没有会话时换掉它。
#[cfg(unix)]
fn take_over(dirs: &runode_paths::Dirs, exe: &Path, socket: &Path, probe: &Probe) -> Upgrade {
    let compatible = matches!(probe, Probe::Running { .. });
    let sessions = match probe {
        Probe::Running { sessions, .. } => Some(*sessions),
        _ => None,
    };
    if let Some(service) = runode_autostart::host_service_pid()
        && launch::host_pid(socket).is_ok_and(|pid| pid == service)
    {
        return match runode_autostart::reload_host_service(dirs, &stable_exe(dirs, exe)) {
            Ok(()) => Upgrade::Done(Via::Service),
            Err(err) if compatible => {
                tracing::warn!("the host service did not take the sessions over, keeping the old host: {err}");
                Upgrade::Done(Via::Mismatch)
            }
            Err(err) => Upgrade::Failed(format!("the host service did not take the sessions over: {err}")),
        };
    }
    let failed = |reason: String| {
        // 交接要等上几十秒，其间旧宿主上可能开了新会话（手机、命令行），结束前重探一次。卡住、握手超时时
        // `probe` 也报 `Absent`，socket 还连得上就不当它没了。
        let still_empty = || match launch::probe(socket, &build()) {
            Probe::Running { sessions: 0, .. } => true,
            Probe::Absent => super::socket_gone(socket),
            _ => false,
        };
        if sessions == Some(0) && still_empty() {
            tracing::warn!("the handoff failed ({reason}); the old host has no sessions, so replacing it");
            return match launch::retire(socket, &build()) {
                Ok(()) => Upgrade::Retired,
                Err(err) => Upgrade::Failed(format!("cannot replace the old host: {err}")),
            };
        }
        if compatible {
            tracing::warn!("keeping the old host: {reason}");
            return Upgrade::Done(Via::Mismatch);
        }
        Upgrade::Failed(reason)
    };
    match handoff::hand_over(exe, handoff::Timing::default()) {
        Outcome::TookOver { sessions, replayed } => {
            tracing::info!("the new host took over {sessions} sessions, {replayed} without scrollback");
            Upgrade::Done(Via::Took)
        }
        Outcome::Refused(HandoffRefusal::Busy) => Upgrade::Busy,
        Outcome::Refused(HandoffRefusal::DesktopConnected) => {
            failed("another runode desktop of an older version is still connected to the host".into())
        }
        Outcome::Refused(reason) => failed(format!("the host refused to hand its sessions over: {reason:?}")),
        Outcome::Failed(reason) => failed(reason),
    }
}

/// 装在 `Dirs::deployed_bin_dir` 下的版本目录里时，指向当前版本的那个链接：登录服务和 shell 都用它，换了
/// 版本不用改；别处的可执行文件原样用。
pub(crate) fn stable_exe(dirs: &runode_paths::Dirs, exe: &Path) -> PathBuf {
    let Some(bin) = dirs.deployed_bin_dir() else {
        return exe.to_path_buf();
    };
    let link = bin.join("runode");
    let in_versions = exe.parent().and_then(Path::parent) == Some(bin.as_path());
    if in_versions && link.exists() { link } else { exe.to_path_buf() }
}

/// 连上 socket，不打招呼（招呼由远端的桌面打）；刚拉起的宿主还没开 socket 时接着试。
fn connect_raw(socket: &Path) -> io::Result<UnixStream> {
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if Instant::now() >= deadline => return Err(err),
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// 宿主 `pid` 会跟着 ssh 的登录被系统结束（`logind`）。Windows 上宿主脱开了 sshd 的作业，不会。
#[cfg(windows)]
const fn session_bound(_: u32) -> bool {
    false
}

/// 宿主 `pid` 会跟着 ssh 的登录被系统结束（`logind`）。
#[cfg(unix)]
fn session_bound(pid: u32) -> bool {
    if !cfg!(target_os = "linux") || pid == 0 {
        return false;
    }
    let Ok(cgroup) = std::fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
        return false;
    };
    match logind::placement(&cgroup) {
        logind::Placement::Session => {
            let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
            logind::KillRules::ask().kills(&user)
        }
        logind::Placement::UserManager => !runode_autostart::starts_at_boot(),
        logind::Placement::Other => false,
    }
}

/// 把 `runode_ssh::bootstrap::UPLOADS_DIR` 收成只让自己进，删掉放久了的上传（`project_host::upload::prune`）：
/// Windows 远端的上传脚本是 cmd，设不了访问控制，也不好按日期删。在后台做，不耽误连上。
#[cfg(windows)]
fn tidy_uploads(dirs: &runode_paths::Dirs) {
    let Some(uploads) = dirs.home.as_ref().map(|home| home.join(runode_ssh::bootstrap::UPLOADS_DIR)) else {
        return;
    };
    let _ = thread::Builder::new().name("tidy-uploads".into()).spawn(move || {
        if let Err(err) = runode_paths::create_private(&uploads) {
            tracing::warn!("cannot make {} private: {err}", uploads.display());
        }
        crate::project_host::upload::prune(&uploads);
    });
}

/// 这个进程的 stdin、stdout 和 `stream` 两头互搬，一次搬至多 `chunk` 字节。往 stdout 写的每块都马上 flush：
/// 协议一来一回、画面要尽快到，攒着不发对面就一直等。`stream` 读到结尾就关掉它返回；stdin 那头可能一直
/// 阻塞着（ssh 还开着），不等它。`name` 是搬 stdin 的线程名。
pub(crate) fn pump_stdio(stream: UnixStream, name: &str, chunk: usize) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let upstream = thread::Builder::new().name(name.into()).spawn({
        let writer = stream.try_clone();
        move || {
            if let Ok(writer) = writer {
                copy(io::stdin().lock(), &writer, chunk, false);
                let _ = writer.shutdown(Shutdown::Write);
            }
        }
    });
    let mut out = BufWriter::with_capacity(chunk, io::stdout().lock());
    copy(&reader, &mut out, chunk, true);
    let _ = stream.shutdown(Shutdown::Both);
    drop(upstream);
}

/// 一次至多搬 `chunk` 字节，`flush` 时每块写完就 flush；读到结尾或者读写出错就停。
fn copy(mut from: impl Read, mut to: impl Write, chunk: usize, flush: bool) {
    let mut buffer = vec![0; chunk];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let written = to.write_all(&buffer[..n]).and_then(|()| if flush { to.flush() } else { Ok(()) });
                if written.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::locale_exists;

    #[test]
    fn only_installed_locales_exist() {
        assert!(locale_exists("C"));
        assert!(!locale_exists("xx_NOWHERE.UTF-8"));
        assert!(!locale_exists("bad\0name"));
    }
}
