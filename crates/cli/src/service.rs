//! `runode service …`：装上、去掉登录时自启（`runode_autostart`），和看装了哪些。不经宿主。

use std::io::Write;

use anyhow::{Context as _, bail};
use runode_autostart::{Kind, install as write_service, is_installed, service_file, uninstall as remove_service};

use crate::Env;

pub(crate) fn install(env: &Env, kind: Kind, out: &mut dyn Write) -> anyhow::Result<()> {
    if !kind.supported() {
        bail!("{} cannot be started at login on this system", kind.name());
    }
    // 服务文件里写的是可执行文件的真实位置：`rn`、`runode` 这些链接以后可能指到别处。
    let exe = std::env::current_exe().and_then(std::fs::canonicalize).context("cannot find the runode executable")?;
    let path = write_service(kind, &env.dirs, &exe)?;
    writeln!(out, "Installed {}: runode {} starts at login.", path.display(), kind.name())?;
    if kind == Kind::Host {
        writeln!(
            out,
            "The host stays in the background only while remote-access = true and a phone is paired; otherwise it \
             exits when idle."
        )?;
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
        match service_file(kind, &env.dirs).filter(|_| is_installed(kind, &env.dirs)) {
            Some(path) => writeln!(out, "{:<5} installed  {}", kind.name(), path.display())?,
            None => writeln!(out, "{:<5} not installed", kind.name())?,
        }
    }
    Ok(())
}
