//! `runode --ssh-service`：桌面经 ssh 在远端 Linux 上跑它，把这台机器的宿主装成 systemd 用户服务、开 linger，
//! 宿主和它的 shell 才不跟着 ssh 的登录被结束。结果打成一行（`runode_ssh::bridge::Service`）。
//!
//! 已经在跑的宿主还有会话时不换：它的 shell 在原来的 cgroup 里，交接也挪不走，等它退出后桥接才经服务起宿主。

use std::{
    io::{self, Write as _},
    path::Path,
    process::Command,
};

use runode_autostart::{Kind, Start};
use runode_ssh::bridge::{Failure, Service};

use super::{
    super::{
        build,
        launch::{self, Probe},
    },
    stable_exe,
};

/// 跑一次，返回进程的退出码。
pub fn run() -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    crate::host_process::init_logging(dirs.ssh_bridge_log_file());
    // SAFETY: 还没起任何线程。
    unsafe { super::tidy_env(&dirs) };
    let (line, code) = match install(&dirs) {
        Ok(service) => {
            tracing::info!("the host runs as a login service: {service:?}");
            (service.line(), 0)
        }
        Err(failure) => {
            tracing::warn!("cannot make the host a login service: {failure:?}");
            (failure.service_line(), 1)
        }
    };
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(line.as_bytes()).and_then(|()| stdout.flush());
    code
}

fn install(dirs: &runode_paths::Dirs) -> Result<Service, Failure> {
    let fail = |code: &str, message: String| Failure { code: code.into(), message };
    if !cfg!(target_os = "linux") {
        return Err(fail("unsupported", "only Linux hosts run as a systemd user service".into()));
    }
    // 没有 linger 时用户服务管理器跟着最后一次登录退出，装成服务反倒连会话 scope 里留得住的宿主也留不住。
    if !runode_autostart::starts_at_boot() {
        let enabled = runode_autostart::enable_linger();
        if !runode_autostart::starts_at_boot() {
            let reason = enabled.err().map_or_else(|| "linger is still off".to_owned(), |err| err.to_string());
            return Err(fail("linger", reason));
        }
    }
    let exe = std::env::current_exe().map_err(|err| fail("exe", format!("cannot tell where runode is: {err}")))?;
    let socket = dirs.host_socket_file().ok_or_else(|| fail("runtime", "the host socket path is too long".into()))?;
    let service = runode_autostart::host_service_pid();
    let running = launch::host_pid(&socket).ok();
    let moved = match launch::probe(&socket, &build()) {
        Probe::Absent => true,
        _ if service.is_some() && running == service => true,
        Probe::Running { sessions: 0, .. } => {
            launch::retire(&socket, &build())
                .map_err(|err| fail("retire", format!("cannot stop the old host: {err}")))?;
            true
        }
        _ => false,
    };
    offer_agent(dirs);
    let start = if moved { Start::Now } else { Start::Later };
    runode_autostart::install(Kind::Host, dirs, &stable_exe(dirs, &exe), start)
        .map_err(|err| fail("install", err.to_string()))?;
    Ok(Service { moved })
}

/// 登录服务起的宿主从用户服务管理器继承环境，不经这次 ssh；管理器里还没有 `SSH_AUTH_SOCK` 时设成转发来的
/// ssh-agent 的固定链接（`Dirs::ssh_agent_link`），已有别的（桌面登录的钥匙串）时不动它。
pub(super) fn offer_agent(dirs: &runode_paths::Dirs) {
    let Some(link) = dirs.ssh_agent_link() else {
        return;
    };
    if std::env::var_os("SSH_AUTH_SOCK").as_deref() != Some(link.as_os_str()) {
        return;
    }
    let Ok(output) = Command::new("systemctl").args(["--user", "show-environment"]).output() else {
        return;
    };
    let shown = String::from_utf8_lossy(&output.stdout);
    let current = shown.lines().find_map(|line| line.strip_prefix("SSH_AUTH_SOCK="));
    if current.is_some_and(|current| Path::new(current) != link) {
        return;
    }
    let assignment = format!("SSH_AUTH_SOCK={}", link.display());
    if let Err(err) = Command::new("systemctl").args(["--user", "set-environment", &assignment]).status() {
        tracing::warn!("cannot hand the forwarded agent to the user manager: {err}");
    }
}
