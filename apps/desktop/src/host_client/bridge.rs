//! 远端这一侧的桥接（`runode --ssh-bridge`）：桌面经 ssh 在远端跑它，它连上这台机器的宿主，在 stdout
//! 打一行开场白（`runode_ssh::bridge::Opening`），之后把 stdin 和 stdout 原样接到宿主的 socket 上，桌面
//! 照旧按 `runode_protocol` 和远端的宿主说话。
//!
//! 宿主没在跑就拉起来；在跑的是别的构建（桌面升级后装上了新版本）时让这个构建的新宿主接手它的会话，
//! 远端的终端和 agent 不断。接不了手时（远端还连着装旧版本的桌面）协议对得上就照样连旧的。
//! 宿主装成了登录服务时经服务管理器拉起（`service`）。

mod logind;
pub mod service;

use std::{
    io::{self, BufWriter, Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use runode_protocol::HandoffRefusal;
use runode_ssh::bridge::{Failure, Opening};

use super::{
    UnixStream, build,
    handoff::{self, Outcome},
    launch::{self, Probe},
};

/// 拉起宿主后最多等这么久连上它。
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(5);
/// 另一个连接正在让新宿主接手时，等这么久再探。
const BUSY_RETRY: Duration = Duration::from_secs(1);
const BUSY_TRIES: u32 = 10;

/// 跑桥接，返回进程的退出码。
pub fn run() -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    crate::host_process::init_logging(dirs.ssh_bridge_log_file());
    // SAFETY: 还没起任何线程。
    unsafe { tidy_env(&dirs) };
    let mut stdout = io::stdout().lock();
    match connect(&dirs) {
        Ok((stream, opening)) => {
            tracing::info!("bridging to the host: {opening:?}");
            if stdout.write_all(opening.line().as_bytes()).and_then(|()| stdout.flush()).is_err() {
                return 1;
            }
            drop(stdout);
            pump(stream);
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
unsafe fn tidy_env(dirs: &runode_paths::Dirs) {
    for key in ["SSH_CLIENT", "SSH_CONNECTION", "SSH_TTY", "SSH_ORIGINAL_COMMAND"] {
        // SAFETY: 调用方保证还没有别的线程。
        unsafe { std::env::remove_var(key) };
    }
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
        let via = match &probe {
            Probe::Absent if start_service(dirs, &exe) => "service",
            Probe::Absent => {
                runode_host::launch(&exe).map_err(|err| fail("launch", format!("cannot start the host: {err}")))?;
                "launched"
            }
            Probe::OtherApp => "in_app",
            Probe::Running { build, .. } if *build == ours => "kept",
            Probe::Running { .. } | Probe::Incompatible { .. } => match take_over(dirs, &exe, &socket, &probe) {
                Upgrade::Done(via) => via,
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
        let via = if via == "kept" || host == ours.0 || via == "in_app" { via } else { "mismatch" };
        let home = dirs.home.as_deref().map(|home| home.to_string_lossy().into_owned()).unwrap_or_default();
        let opening = Opening { build: host, pid, via: via.into(), session_bound: session_bound(pid), home };
        return Ok((stream, opening));
    }
}

/// 宿主装成了 Linux 的登录服务时经服务管理器起它；没装、没起成时返回假。
fn start_service(dirs: &runode_paths::Dirs, exe: &Path) -> bool {
    if !cfg!(target_os = "linux") || !runode_autostart::is_installed(runode_autostart::Kind::Host, dirs) {
        return false;
    }
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

enum Upgrade {
    Done(&'static str),
    Busy,
    Failed(String),
}

/// 在跑的是别的构建：让这个构建的新宿主接手。登录服务管着它时经服务换；接不了手、协议又对得上时照样
/// 用旧的（`mismatch`），旧宿主没有会话时换掉它。
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
            Ok(()) => Upgrade::Done("service"),
            Err(err) if compatible => {
                tracing::warn!("the host service did not take the sessions over, keeping the old host: {err}");
                Upgrade::Done("mismatch")
            }
            Err(err) => Upgrade::Failed(format!("the host service did not take the sessions over: {err}")),
        };
    }
    let failed = |reason: String| {
        if sessions == Some(0) {
            tracing::warn!("the handoff failed ({reason}); the old host has no sessions, so replacing it");
            return match launch::retire(socket, &build()).and_then(|()| runode_host::launch(exe).map(drop)) {
                Ok(()) => Upgrade::Done("launched"),
                Err(err) => Upgrade::Failed(format!("cannot replace the old host: {err}")),
            };
        }
        if compatible {
            tracing::warn!("keeping the old host: {reason}");
            return Upgrade::Done("mismatch");
        }
        Upgrade::Failed(reason)
    };
    match handoff::hand_over(exe, handoff::Timing::default()) {
        Outcome::TookOver { sessions, replayed } => {
            tracing::info!("the new host took over {sessions} sessions, {replayed} without scrollback");
            Upgrade::Done("took")
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

/// 宿主 `pid` 会跟着 ssh 的登录被系统结束（`logind`）。
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

/// 两头互搬，任一头读到结尾就关掉另一头。
fn pump(stream: UnixStream) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let writer = stream;
    let upstream = thread::Builder::new().name("bridge-in".into()).spawn({
        let writer = writer.try_clone();
        move || {
            if let Ok(writer) = writer {
                copy(io::stdin().lock(), &writer);
                let _ = writer.shutdown(Shutdown::Write);
            }
        }
    });
    let mut out = BufWriter::with_capacity(64 * 1024, io::stdout().lock());
    copy_flushing(&reader, &mut out);
    let _ = writer.shutdown(Shutdown::Both);
    // stdin 那头可能一直阻塞着（ssh 还开着）：宿主断开就整个退出，不等它。
    drop(upstream);
}

fn copy(mut from: impl Read, mut to: impl Write) {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if to.write_all(&buffer[..n]).is_err() {
                    return;
                }
            }
        }
    }
}

/// 每读到一块就写出去并 flush：协议是一来一回的，攒着不发对面就一直等。
fn copy_flushing(mut from: impl Read, to: &mut impl Write) {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if to.write_all(&buffer[..n]).and_then(|()| to.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::locale_exists;

    #[test]
    fn only_installed_locales_exist() {
        assert!(locale_exists("C"));
        assert!(!locale_exists("xx_NOWHERE.UTF-8"));
        assert!(!locale_exists("bad\0name"));
    }
}
