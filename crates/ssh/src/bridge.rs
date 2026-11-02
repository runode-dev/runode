//! 远端的桥接（`runode --ssh-bridge`）连好远端的宿主后先在 stdout 上打一行开场白，之后才是协议的字节。
//! 登录 shell 的启动脚本可能先往 stdout 打别的东西（`.bashrc` 里的 echo），读的一边跳过开场白之前的行。
//! 把远端的宿主装成登录服务（`runode --ssh-service`）也照这样回一行结果（`Service`）。

use std::io::{self, BufRead, Read as _};

use crate::bootstrap::Platform;

const PREFIX: &str = "RUNODE-BRIDGE 1 ";
const SERVICE_PREFIX: &str = "RUNODE-SERVICE 1 ";
/// 装登录服务失败时的 `Failure::code`：这个用户开不了 linger，要管理员开。
pub const LINGER: &str = "linger";
/// 开场白之前最多跳过这么多字节的杂行。
const MAX_NOISE: u64 = 64 * 1024;

/// 桥接怎么连上远端宿主的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    /// 远端宿主的构建号。
    pub build: String,
    /// 远端宿主的进程号。
    pub pid: u32,
    /// 桥接怎么连上远端宿主的，一个不带空格的词，给人看日志用。
    pub via: String,
    /// 远端宿主会跟着 ssh 的登录被系统结束：Linux 上它在 logind 的会话 scope 里、logind 又结束登录会话里的
    /// 进程，或者它在用户服务管理器里、这个用户没开 linger。
    pub session_bound: bool,
    /// 远端的系统，`std::env::consts::OS` 的写法（`macos`、`linux`、`windows`）；开场白里没有时为空。
    pub os: String,
    /// 远端的家目录；可能有空格，放在这一行最后。
    pub home: String,
}

impl Opening {
    pub fn line(&self) -> String {
        format!(
            "{PREFIX}ok build={} pid={} via={} session={} os={} home={}\n",
            self.build,
            self.pid,
            self.via,
            u8::from(self.session_bound),
            self.os,
            self.home.replace('\n', " "),
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
        self.line_after(PREFIX)
    }

    /// 没装成登录服务时回的那一行。
    pub fn service_line(&self) -> String {
        self.line_after(SERVICE_PREFIX)
    }

    fn line_after(&self, prefix: &str) -> String {
        format!("{prefix}error {} {}\n", self.code, self.message.replace('\n', " "))
    }

    /// 前缀之后的 `rest` 是 `error <code> <message>` 时的失败。
    fn parse(rest: &str) -> Option<Self> {
        let error = rest.strip_prefix("error ")?;
        let (code, message) = error.split_once(' ').unwrap_or((error, ""));
        Some(Self { code: code.to_owned(), message: message.to_owned() })
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
    if let Some(failure) = Failure::parse(rest) {
        return Start::Failed(failure);
    }
    let rest = rest.strip_prefix("ok").unwrap_or(rest);
    let (fields, home) = rest.split_once(" home=").unwrap_or((rest, ""));
    let mut opening = Opening {
        build: String::new(),
        pid: 0,
        via: String::new(),
        session_bound: false,
        os: String::new(),
        home: home.to_owned(),
    };
    for field in fields.split_whitespace() {
        match field.split_once('=') {
            Some(("build", value)) => opening.build = value.to_owned(),
            Some(("pid", value)) => opening.pid = value.parse().unwrap_or(0),
            Some(("via", value)) => opening.via = value.to_owned(),
            Some(("session", value)) => opening.session_bound = value == "1",
            Some(("os", value)) => opening.os = value.to_owned(),
            _ => {}
        }
    }
    Start::Bridge(opening)
}

/// 远端把宿主装成了登录服务。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service {
    /// 原来的宿主没有会话，换成了登录服务起的宿主；为假时原来的宿主还在跑，它退出后才由服务起。
    pub moved: bool,
}

impl Service {
    pub fn line(&self) -> String {
        format!("{SERVICE_PREFIX}ok moved={}\n", u8::from(self.moved))
    }
}

/// 读装登录服务的结果，跳过前面的杂行；读到结尾也没有结果行时报错。
pub fn read_service(input: &mut impl BufRead) -> io::Result<Result<Service, Failure>> {
    let mut limited = input.take(MAX_NOISE);
    loop {
        let mut bytes = Vec::new();
        if limited.read_until(b'\n', &mut bytes)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no service result line"));
        }
        let line = String::from_utf8_lossy(&bytes);
        let Some(rest) = line.strip_prefix(SERVICE_PREFIX) else {
            continue;
        };
        let rest = rest.trim_end();
        if let Some(failure) = Failure::parse(rest) {
            return Ok(Err(failure));
        }
        let moved = rest.split_whitespace().any(|field| field == "moved=1");
        return Ok(Ok(Service { moved }));
    }
}
