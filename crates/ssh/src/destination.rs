//! 用户填的连接目标：`~/.ssh/config` 里的别名，或者 `user@host:port`、`ssh://user@host:port`。

use std::fmt;

/// 一个连接目标，原样交给 ssh：别名的各项设置由 OpenSSH 按配置文件解析。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Destination {
    /// 交给 ssh 的主机部分：`host`、`user@host` 或别名。
    pub target: String,
    /// 用户写明的端口，没写时由配置和默认值定。
    pub port: Option<u16>,
}

impl Destination {
    /// 认出 `alias`、`user@host`、`user@host:port`、`ssh://user@host:port`、`[v6 地址]:port`。空的、带空白的、
    /// 以 `-` 开头（会被 ssh 当成选项）的、端口不是数字的认不出。
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix("ssh://").unwrap_or(text);
        let text = text.strip_suffix('/').unwrap_or(text);
        if text.is_empty() || text.starts_with('-') || text.contains(char::is_whitespace) {
            return None;
        }
        let (user, host) = match text.rsplit_once('@') {
            Some((user, host)) if !user.is_empty() => (Some(user), host),
            Some(_) => return None,
            None => (None, text),
        };
        let (host, port) = if let Some(rest) = host.strip_prefix('[') {
            let (addr, after) = rest.split_once(']')?;
            let port = match after.strip_prefix(':') {
                Some(port) => Some(port.parse().ok()?),
                None if after.is_empty() => None,
                None => return None,
            };
            (addr, port)
        } else {
            match host.split_once(':') {
                // 冒号不止一个的是没加方括号的 IPv6 地址，不拆端口。
                Some((name, port)) if !port.contains(':') => (name, Some(port.parse().ok()?)),
                _ => (host, None),
            }
        };
        if host.is_empty() || host.starts_with('-') {
            return None;
        }
        let target = match user {
            Some(user) => format!("{user}@{host}"),
            None => host.to_owned(),
        };
        Some(Self { target, port })
    }

    /// 交给 ssh 的参数：有端口时先是 `-p 端口`，最后是 `--` 和主机。
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(port) = self.port {
            args.extend(["-p".to_owned(), port.to_string()]);
        }
        args.extend(["--".to_owned(), self.target.clone()]);
        args
    }

    /// 这是 `~/.ssh/config` 里的一个别名那样的写法：没写用户和端口。VS Code 的 Remote-SSH 只认这样的。
    pub fn is_alias(&self) -> bool {
        self.port.is_none() && !self.target.contains('@')
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.port {
            Some(port) if self.target.contains(':') => match self.target.rsplit_once('@') {
                Some((user, addr)) => write!(f, "{user}@[{addr}]:{port}"),
                None => write!(f, "[{}]:{port}", self.target),
            },
            Some(port) => write!(f, "{}:{port}", self.target),
            None => f.write_str(&self.target),
        }
    }
}
