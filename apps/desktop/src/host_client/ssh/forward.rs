//! 把 SSH 主机的端口经主连接转到本机（`hosts::forward` 分流过来）。转发挂在主连接上，主连接退出时跟着没了。

use std::{
    collections::HashMap,
    net::Ipv4Addr,
    path::PathBuf,
    process::Stdio,
    sync::{LazyLock, Mutex, PoisonError},
};

use anyhow::{Result, bail};

use super::{RemoteHost, ssh};
use crate::host_client::hosts::forward::{listening, pick_local};

/// 主机名、远端端口到本机端口。
static FORWARDS: LazyLock<Mutex<HashMap<(String, u16), u16>>> = LazyLock::new(Default::default);

/// 主机 `name` 上转着的端口：远端端口到本机端口，按远端端口排好。
pub fn forwarded(name: &str) -> Vec<(u16, u16)> {
    let forwards = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut ports: Vec<_> =
        forwards.iter().filter(|((host, _), _)| host == name).map(|((_, remote), &local)| (*remote, local)).collect();
    ports.sort_unstable();
    ports
}

/// 主机 `name` 的主连接退出了，挂在上面的转发都没了：不再记着，不然本机同一个端口换成别的程序在听时，
/// 再点那个远端端口会打开本机的那个程序。
pub(super) fn forget(name: &str) {
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).retain(|(host, _), _| host != name);
}

/// 停掉远端 `port` 到本机的转发；没转着时什么都不做。会阻塞，在后台线程上调。
pub fn stop(host: &RemoteHost, port: u16) -> Result<()> {
    let key = (host.name.clone(), port);
    let Some(local) = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).get(&key).copied() else {
        return Ok(());
    };
    let ssh = ssh();
    let mut command = ssh.forward(&host.dest, control_socket(&ssh, host).as_deref(), local, port, true);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let output = command.output()?;
    // 主连接已经退出时转发本来就没了。
    if !output.status.success() && master_alive(&ssh, host) {
        bail!("{}", last_line(&output.stderr, "ssh refused to stop forwarding"));
    }
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
    tracing::info!("stopped forwarding localhost:{local} to {}:{port}", host.name);
    Ok(())
}

fn last_line(stderr: &[u8], otherwise: &str) -> String {
    let error = String::from_utf8_lossy(stderr);
    let line = runode_ssh::last_line(&error);
    if line.is_empty() { otherwise } else { line }.to_owned()
}

/// 把远端的 `port` 转到本机，回本机的端口；已经转着、本机那头还在听时直接用它。会阻塞，在后台线程上调。
pub fn forward(host: &RemoteHost, port: u16) -> Result<u16> {
    let key = (host.name.clone(), port);
    let known = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).get(&key).copied();
    let ssh = ssh();
    if ssh.control.is_none() {
        bail!("forwarding needs a shared ssh connection, which this system's ssh does not offer");
    }
    // 转发挂在主连接上：本机那头在听、主连接也还开着，才是这条转发还在；主连接重开过时那个端口可能已经是别的程序的。
    if let Some(local) = known.filter(|&local| listening(Ipv4Addr::LOCALHOST.into(), local) && master_alive(&ssh, host))
    {
        return Ok(local);
    }
    let local = pick_local(known, port)?;
    let mut command = ssh.forward(&host.dest, control_socket(&ssh, host).as_deref(), local, port, false);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let output = command.output()?;
    if !output.status.success() {
        bail!("{}", last_line(&output.stderr, "ssh refused to forward"));
    }
    tracing::info!("forwarding localhost:{local} to {}:{port}", host.name);
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).insert(key, local);
    Ok(local)
}

/// `host` 的主连接 socket 的实际路径，读不出时为空。
fn control_socket(ssh: &runode_ssh::Ssh, host: &RemoteHost) -> Option<PathBuf> {
    let mut command = ssh.resolve(&host.dest);
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let output = command.output().ok().filter(|output| output.status.success())?;
    runode_ssh::parse_control_path(&String::from_utf8_lossy(&output.stdout))
}

fn master_alive(ssh: &runode_ssh::Ssh, host: &RemoteHost) -> bool {
    let mut check = ssh.check(&host.dest);
    check.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    check.status().is_ok_and(|status| status.success())
}
