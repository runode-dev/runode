//! `runode service …`：装上、去掉登录时自启（`runode_autostart`），和看装了哪些。不经宿主。

use std::io::Write;

use anyhow::{Context as _, bail};
use runode_autostart::{
    Kind, install as write_service, is_installed, service_file, starts_at_boot, uninstall as remove_service,
};

use crate::Env;

pub(crate) fn install(env: &Env, kind: Kind, out: &mut dyn Write) -> anyhow::Result<()> {
    if !kind.supported() {
        bail!("{} cannot be started at login on this system", kind.name());
    }
    // 服务文件里写的是可执行文件的真实位置：`rn`、`runode` 这些链接以后可能指到别处。
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).context("cannot find the runode executable")?;
    match write_service(kind, &env.dirs, &exe)? {
        Some(path) => writeln!(out, "Installed {}: runode {} starts at login.", path.display(), kind.name())?,
        None if cfg!(windows) => writeln!(out, "Registered the host under HKCU\\{}: it starts at login.", run_key())?,
        None => writeln!(out, "Added Runode to the login items: the app opens at login.")?,
    }
    if kind == Kind::Host {
        writeln!(out, "The host keeps running in the background, even with no sessions, until you end it.")?;
        // systemd 的用户服务只在有人登录时才起，`install` 试着开了 linger；没开成（多半没有权限）时告诉用户怎么办。
        if cfg!(target_os = "linux") {
            if starts_at_boot() {
                writeln!(out, "It also starts at boot, before anyone logs in (systemd linger is on).")?;
            } else {
                writeln!(out, "It starts when you log in. To start it at boot, run `loginctl enable-linger $USER`.")?;
            }
        }
    }
    Ok(())
}

pub(crate) fn uninstall(env: &Env, kind: Kind, out: &mut dyn Write) -> anyhow::Result<()> {
    if !kind.supported() {
        bail!("{} cannot be started at login on this system", kind.name());
    }
    if remove_service(kind, &env.dirs)? {
        writeln!(out, "Removed the login service for the {}.", kind.name())?;
    } else {
        writeln!(out, "The {} was not set to start at login.", kind.name())?;
    }
    Ok(())
}

pub(crate) fn status(env: &Env, out: &mut dyn Write) -> anyhow::Result<()> {
    for kind in Kind::ALL.into_iter().filter(|kind| kind.supported()) {
        if !is_installed(kind, &env.dirs) {
            writeln!(out, "{:<5} not installed", kind.name())?;
            continue;
        }
        match service_file(kind, &env.dirs).filter(|path| path.exists()) {
            Some(path) => writeln!(out, "{:<5} installed  {}", kind.name(), path.display())?,
            None if cfg!(windows) => writeln!(out, "{:<5} installed  HKCU\\{}", kind.name(), run_key())?,
            None => writeln!(out, "{:<5} installed  System Settings > General > Login Items", kind.name())?,
        }
    }
    Ok(())
}

/// 让这个构建的宿主接手正在跑的宿主的会话。Linux 上宿主是 systemd 用户服务的主进程时经服务的重载
/// （`runode_autostart::reload_host_service`），新宿主在服务里接着当主进程；别的情况拉起
/// `runode --host --upgrade` 交接。
pub(crate) fn upgrade(env: &Env, out: &mut dyn Write) -> anyhow::Result<()> {
    if cfg!(windows) {
        bail!("on Windows the host cannot hand its sessions over; quit runode and open it again");
    }
    let Some(running) = crate::client::probe(env)? else {
        writeln!(out, "No runode host is running; the next one to start runs this build.")?;
        return Ok(());
    };
    if running.build.0 == env.build {
        writeln!(out, "The running host is already this build.")?;
        return Ok(());
    }
    if !running.standalone {
        bail!("the host runs inside the runode app; quit the app and open the new one");
    }
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).context("cannot find the runode executable")?;
    let service = runode_autostart::host_service_pid();
    if service.is_some() && running.pid.is_none_or(|pid| Some(pid) == service) {
        writeln!(out, "Handing the sessions over through the runode-host systemd service.")?;
        out.flush()?;
        runode_autostart::reload_host_service(&env.dirs, &exe)
            .context("the new host did not take over; `journalctl --user -u runode-host` says why")?;
    } else {
        out.flush()?;
        // 结果和原因由它自己打出来。
        let status = std::process::Command::new(&exe)
            .args(["--host", "--upgrade"])
            .status()
            .with_context(|| format!("cannot run {}", exe.display()))?;
        if !status.success() {
            bail!("the host was not upgraded");
        }
    }
    match crate::client::probe(env)? {
        Some(now) if now.build.0 == env.build => {
            let pid = now.pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default();
            writeln!(out, "The host now runs this build{pid}; its sessions stayed open.")?;
            Ok(())
        }
        _ => bail!("the host did not come back as this build; its log says why"),
    }
}

/// Windows 上登录自启登记的注册表键（`HKEY_CURRENT_USER` 之下），别的系统上用不到。
fn run_key() -> &'static str {
    #[cfg(windows)]
    return runode_autostart::RUN_KEY;
    #[cfg(not(windows))]
    ""
}
