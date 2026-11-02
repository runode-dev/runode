//! 把远端的端口经主连接转到本机：远端终端里点开 `localhost:<端口>` 这样的网址时、状态栏的端口浮层里点了远端
//! 的端口时转，agent 在远端起的开发服务器在本机的浏览器里就能看。转发挂在主连接上，主连接退出时跟着没了。

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    process::Stdio,
    sync::{LazyLock, Mutex, PoisonError},
    time::Duration,
};

use anyhow::{Result, bail};

use super::{RemoteHost, ssh};

/// 主机名、远端端口到本机端口。
static FORWARDS: LazyLock<Mutex<HashMap<(String, u16), u16>>> = LazyLock::new(Default::default);

/// 查本机端口有没有在听时最多等这么久。
const CHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// 远端网址指向远端本机时，换成经转发在本机打开的网址；转发不了时报错。不是指向远端本机的网址原样交回。
/// 会阻塞，在后台线程上调。
pub fn local_url(host: &RemoteHost, url: &str) -> Result<String> {
    let Some(target) = Loopback::parse(url) else {
        return Ok(url.to_owned());
    };
    let local = forward(host, target.port)?;
    Ok(target.rewrite(local))
}

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
    let mut command = ssh.forward(&host.dest, local, port, true);
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
    // 尽量用同一个端口号，网址里写死了端口的页面也能用；占着了、或者是要特权的端口时让系统挑一个。
    let local =
        known.into_iter().chain([port]).find(|&local| local >= 1024 && free(local)).map_or_else(any_free, Ok)?;
    let mut command = ssh.forward(&host.dest, local, port, false);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let output = command.output()?;
    if !output.status.success() {
        bail!("{}", last_line(&output.stderr, "ssh refused to forward"));
    }
    tracing::info!("forwarding localhost:{local} to {}:{port}", host.name);
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).insert(key, local);
    Ok(local)
}

fn listening(addr: IpAddr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(addr, port), CHECK_TIMEOUT).is_ok()
}

fn master_alive(ssh: &runode_ssh::Ssh, host: &RemoteHost) -> bool {
    let mut check = ssh.check(&host.dest);
    check.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    check.status().is_ok_and(|status| status.success())
}

/// 本机的 `port` 没被占：转发只绑 127.0.0.1，打开的网址写的是 `localhost`，浏览器可能先连 `::1`，所以那边有
/// 程序在听也不行。只试着绑一下不够：别的程序绑的是 `0.0.0.0` 时，有的系统照样让这里绑上 127.0.0.1。
fn free(port: u16) -> bool {
    !listening(Ipv4Addr::LOCALHOST.into(), port)
        && !listening(Ipv6Addr::LOCALHOST.into(), port)
        && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}

/// 让系统挑一个空着的端口，挑中的在 `::1` 上被占着时再挑。
fn any_free() -> Result<u16> {
    for _ in 0..8 {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port();
        if !listening(Ipv6Addr::LOCALHOST.into(), port) {
            return Ok(port);
        }
    }
    bail!("no free local port")
}

/// 指向本机（远端那台）的网址：`scheme://` 之后、端口之后的部分原样留着。
#[derive(Debug, PartialEq, Eq)]
struct Loopback<'a> {
    scheme: &'a str,
    port: u16,
    rest: &'a str,
}

impl<'a> Loopback<'a> {
    fn parse(url: &'a str) -> Option<Self> {
        let (scheme, after) = url.split_once("://")?;
        let default = match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => 80,
            "https" | "wss" => 443,
            _ => return None,
        };
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let (authority, rest) = after.split_at(end);
        if authority.contains('@') {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.contains(']') => (host, Some(port)),
            _ => (authority, None),
        };
        let loopback = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"];
        if !loopback.iter().any(|name| host.eq_ignore_ascii_case(name)) {
            return None;
        }
        let port = match port {
            Some(port) => port.parse().ok().filter(|&port| port > 0)?,
            None => default,
        };
        Some(Self { scheme, port, rest })
    }

    /// 换成本机的 `local` 端口。主机写成 `localhost`：浏览器打不开 `0.0.0.0`。
    fn rewrite(&self, local: u16) -> String {
        format!("{}://localhost:{local}{}", self.scheme, self.rest)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

    use super::{Loopback, free};

    /// 本机别的程序在 `0.0.0.0` 或 `::1` 上听着的端口不算空着。
    #[test]
    fn ports_taken_on_any_loopback_are_not_free() {
        let wildcard = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        assert!(!free(wildcard.local_addr().unwrap().port()));
        if let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) {
            assert!(!free(v6.local_addr().unwrap().port()));
        }
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        assert!(free(port));
    }

    #[test]
    fn finds_urls_on_the_remote_machine_itself() {
        let parse = |url| Loopback::parse(url).map(|target| (target.port, target.rewrite(4000)));
        assert_eq!(parse("http://localhost:3000/a?b#c"), Some((3000, "http://localhost:4000/a?b#c".into())));
        assert_eq!(parse("http://0.0.0.0:5173"), Some((5173, "http://localhost:4000".into())));
        assert_eq!(parse("https://127.0.0.1/x"), Some((443, "https://localhost:4000/x".into())));
        assert_eq!(parse("http://[::1]:8080/"), Some((8080, "http://localhost:4000/".into())));
        assert_eq!(parse("HTTP://LOCALHOST:1"), Some((1, "HTTP://localhost:4000".into())));
        assert_eq!(parse("http://example.com:3000/"), None);
        assert_eq!(parse("http://user@localhost:3000/"), None);
        assert_eq!(parse("http://localhost:0/"), None);
        assert_eq!(parse("ftp://localhost:21/"), None);
        assert_eq!(parse("file:///tmp/x"), None);
    }
}
