//! 按 ssh 打在 stderr 上的话认出连接为什么失败，界面据此说清楚、决定要不要自动重试。

/// 连接失败的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// 认证没过：密码、密钥都不对，或者用户取消了。
    Auth,
    /// 远端的主机密钥和 `known_hosts` 里记着的不一样，或者用户没认新主机的指纹。
    HostKey,
    /// 解析不了主机名。
    Resolve,
    /// 连接被拒，或者网络不通。
    Unreachable,
    /// 连接超时。
    Timeout,
    /// 别的原因，附上 ssh 最后说的那句话。
    Other(String),
}

impl Failure {
    /// 按 ssh 的 stderr 认。
    pub fn of(stderr: &str) -> Self {
        let has = |needle: &str| stderr.contains(needle);
        if has("Host key verification failed") || has("REMOTE HOST IDENTIFICATION HAS CHANGED") {
            Self::HostKey
        } else if has("Permission denied") || has("Too many authentication failures") {
            Self::Auth
        } else if has("Could not resolve hostname") || has("Name or service not known") {
            Self::Resolve
        } else if has("Operation timed out") || has("Connection timed out") || has("timed out") {
            Self::Timeout
        } else if has("Connection refused")
            || has("No route to host")
            || has("Network is unreachable")
            || has("Connection closed")
        {
            Self::Unreachable
        } else {
            let last = stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("").trim();
            Self::Other(last.to_owned())
        }
    }

    /// 稍后自己再试有没有用：认证和主机密钥的问题要用户来办。
    pub fn retryable(&self) -> bool {
        !matches!(self, Self::Auth | Self::HostKey)
    }
}
