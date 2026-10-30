//! 远端的桥接（`runode --ssh-bridge`）连好远端的宿主后先在 stdout 上打一行开场白，之后才是协议的字节。
//! 登录 shell 的启动脚本可能先往 stdout 打别的东西（`.bashrc` 里的 echo），读的一边跳过开场白之前的行。

use std::io::{self, BufRead, Read as _};

use crate::bootstrap::Platform;

const PREFIX: &str = "RUNODE-BRIDGE 1 ";
/// 开场白之前最多跳过这么多字节的杂行。
const MAX_NOISE: u64 = 64 * 1024;

/// 桥接怎么连上远端宿主的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    /// 远端宿主的构建号。
    pub build: String,
    /// 远端宿主的进程号。
    pub pid: u32,
    /// `launched`（新拉起的）、`kept`（本来就在跑、同一个构建）、`took`（新宿主接手了旧宿主的会话）、
    /// `service`（登录服务换上了新版本）、`mismatch`（连上了别的构建、没能换）、`in_app`（远端 Mac 上开着
    /// runode app、宿主跑在它里面）。
    pub via: String,
    /// 远端宿主跟着 ssh 登录会话走（Linux 上在 logind 的 session scope 里、又没开 linger）：最后一个 ssh
    /// 连接断开时系统可能把它连同会话一起结束。
    pub session_bound: bool,
}

impl Opening {
    pub fn line(&self) -> String {
        format!(
            "{PREFIX}ok build={} pid={} via={} session={}\n",
            self.build,
            self.pid,
            self.via,
            u8::from(self.session_bound)
        )
    }
}

/// 桥接没能连上远端宿主：`code` 是给程序认的，`message` 给人看。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

impl Failure {
    pub fn line(&self) -> String {
        format!("{PREFIX}error {} {}\n", self.code, self.message.replace('\n', " "))
    }
}

/// 连接脚本的开头：桥接连上了，或者远端还没装这个构建、打出了平台信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    Bridge(Opening),
    Failed(Failure),
    /// 读到结尾都没有开场白，`Platform` 是读到的平台信息（没有时为空）。
    Probe(Option<Platform>),
}

/// 读到开场白为止（之后 `input` 里剩下的就是协议的字节）；读到结尾也没有时按平台信息读。
pub fn read_start(input: &mut impl BufRead) -> io::Result<Start> {
    let mut seen = String::new();
    let mut limited = input.take(MAX_NOISE);
    loop {
        let mut line = String::new();
        let mut bytes = Vec::new();
        if limited.read_until(b'\n', &mut bytes)? == 0 {
            return Ok(Start::Probe(Platform::parse(&seen)));
        }
        line.push_str(&String::from_utf8_lossy(&bytes));
        if let Some(rest) = line.strip_prefix(PREFIX) {
            return Ok(parse(rest.trim_end()));
        }
        seen.push_str(&line);
        if limited.limit() == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no bridge opening line"));
        }
    }
}

fn parse(rest: &str) -> Start {
    if let Some(error) = rest.strip_prefix("error ") {
        let (code, message) = error.split_once(' ').unwrap_or((error, ""));
        return Start::Failed(Failure { code: code.to_owned(), message: message.to_owned() });
    }
    let mut opening = Opening { build: String::new(), pid: 0, via: String::new(), session_bound: false };
    for field in rest.strip_prefix("ok").unwrap_or(rest).split_whitespace() {
        match field.split_once('=') {
            Some(("build", value)) => opening.build = value.to_owned(),
            Some(("pid", value)) => opening.pid = value.parse().unwrap_or(0),
            Some(("via", value)) => opening.via = value.to_owned(),
            Some(("session", value)) => opening.session_bound = value == "1",
            _ => {}
        }
    }
    Start::Bridge(opening)
}
