//! 把远端主机的端口转到本机：远端终端里点开 `localhost:<端口>` 这样的网址时、状态栏的端口浮层里点了远端的端口时
//! 转，agent 在远端起的开发服务器在本机的浏览器里就能看。SSH 主机经主连接转（`ssh::forward`），配对的电脑在本机
//! 开监听、每接一个连接另拨一条专用连接（`paired::forward`）；这里按 `HostId` 分流，挑本机的端口，认指向远端本机
//! 的网址。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use super::HostId;
use crate::host_client::{paired, ssh};

/// 查本机端口有没有在听时最多等这么久。
const CHECK_TIMEOUT: Duration = Duration::from_millis(300);

/// 远端网址指向远端本机时，换成经转发在本机打开的网址；转发不了时报错。不是指向远端本机的网址原样交回。
/// 会阻塞，在后台线程上调。
pub fn local_url(host: &HostId, url: &str) -> Result<String> {
    let Some(target) = Loopback::parse(url) else {
        return Ok(url.to_owned());
    };
    let local = forward(host, target.port)?;
    Ok(target.rewrite(local))
}

/// 把远端主机 `host` 的 `port` 转到本机，回本机的端口；已经转着、本机那头还在听时直接用它。本机的不用转。会阻塞，
/// 在后台线程上调。
pub fn forward(host: &HostId, port: u16) -> Result<u16> {
    match host {
        HostId::Local => Ok(port),
        HostId::Ssh(name) => ssh::forward::forward(&*ssh::find(name).context("not connected")?, port),
        HostId::Paired(id) => paired::forward::forward(&*paired::find(id).context("not connected")?, port),
    }
}

/// 停掉远端主机 `host` 的 `port` 到本机的转发；没转着时什么都不做。会阻塞，在后台线程上调。
pub fn stop(host: &HostId, port: u16) -> Result<()> {
    match host {
        HostId::Local => Ok(()),
        HostId::Ssh(name) => match ssh::find(name) {
            Some(remote) => ssh::forward::stop(&remote, port),
            None => Ok(()),
        },
        HostId::Paired(id) => {
            paired::forward::stop(id, port);
            Ok(())
        }
    }
}

/// 远端主机 `host` 上转着的端口：远端端口到本机端口，按远端端口排好。
pub fn forwarded(host: &HostId) -> Vec<(u16, u16)> {
    match host {
        HostId::Local => Vec::new(),
        HostId::Ssh(name) => ssh::forward::forwarded(name),
        HostId::Paired(id) => paired::forward::forwarded(id),
    }
}

/// 转发用的本机端口：尽量用上回用过的 `known`，再试和远端同一个端口号 `port`，网址里写死了端口的页面也能用；占着了、
/// 或者是要特权的端口时让系统挑一个。
pub(in crate::host_client) fn pick_local(known: Option<u16>, port: u16) -> Result<u16> {
    known.into_iter().chain([port]).find(|&local| local >= 1024 && free(local)).map_or_else(any_free, Ok)
}

pub(in crate::host_client) fn listening(addr: IpAddr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(addr, port), CHECK_TIMEOUT).is_ok()
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
