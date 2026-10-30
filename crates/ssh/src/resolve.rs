//! 按 OpenSSH 的规矩解析一个连接目标最后连到哪：读 `ssh -G` 打出的设置。

/// `ssh -G` 解析出的、runode 用得上的几项设置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub user: String,
    pub hostname: String,
    pub port: u16,
    /// 经过的跳板，没有时为空。
    pub proxy_jump: Option<String>,
}

impl Resolved {
    /// `user@hostname:port`：几个别名指着同一台机器时据此认出是同一台。
    pub fn key(&self) -> String {
        format!("{}@{}:{}", self.user, self.hostname, self.port)
    }
}

/// 读 `ssh -G` 的输出：每行一个小写的关键字和它的值。读不到主机名时为空。
pub fn parse_settings(output: &str) -> Option<Resolved> {
    let mut resolved = Resolved { port: 22, ..Resolved::default() };
    for line in output.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match key {
            "user" => resolved.user = value.to_owned(),
            "hostname" => resolved.hostname = value.to_owned(),
            "port" => resolved.port = value.parse().unwrap_or(22),
            "proxyjump" if value != "none" => resolved.proxy_jump = Some(value.to_owned()),
            _ => {}
        }
    }
    (!resolved.hostname.is_empty()).then_some(resolved)
}
