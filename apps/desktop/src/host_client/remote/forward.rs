//! 远端终端里点开 `localhost:<端口>` 这样的网址时，把远端的端口经主连接转到本机，再在本机打开：agent 在远端
//! 起的开发服务器在本机的浏览器里就能看。转发挂在主连接上，主连接退出时跟着没了。
//!
//! shortcut: 只在点链接时转发，没有列出远端在监听的端口、也没有界面管理已有的转发，要时再加。

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
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

/// 把远端的 `port` 转到本机，回本机的端口；已经转着、本机那头还在听时直接用它。
fn forward(host: &RemoteHost, port: u16) -> Result<u16> {
    let key = (host.name.clone(), port);
    let known = FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).get(&key).copied();
    if let Some(local) = known.filter(|&local| listening(local)) {
        return Ok(local);
    }
    let ssh = ssh();
    if ssh.control.is_none() {
        bail!("forwarding needs a shared ssh connection, which this system's ssh does not offer");
    }
    // 尽量用同一个端口号，网址里写死了端口的页面也能用；占着了、或者是要特权的端口时让系统挑一个。
    let local =
        known.into_iter().chain([port]).find(|&local| local >= 1024 && free(local)).map_or_else(any_free, Ok)?;
    let mut command = ssh.forward(&host.dest, local, port, false);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    crate::subprocess::hide_window(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!("{}", error.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("ssh refused to forward"));
    }
    tracing::info!("forwarding localhost:{local} to {}:{port}", host.name);
    FORWARDS.lock().unwrap_or_else(PoisonError::into_inner).insert(key, local);
    Ok(local)
}

fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), CHECK_TIMEOUT).is_ok()
}

fn free(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}

fn any_free() -> Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port())
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
    use super::Loopback;

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
