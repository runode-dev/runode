//! ssh 要问用户（密码、密钥的口令、2FA 验证码、首次连接时认不认主机指纹）时，经 `SSH_ASKPASS` 调 runode
//! 自己；这个进程连上桌面开着的 socket，把提示交给桌面弹框，拿到回答写到 stdout。
//!
//! ssh 调 askpass 时只给提示文字这一个参数，认出这种调用要靠环境变量 `ENV_SOCKET`，所以可执行文件要在
//! 处理任何参数之前先调 `run_if_requested`。

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::Command,
};

/// 桌面开着的 askpass socket 的路径，有它就是 askpass 调用。
pub const ENV_SOCKET: &str = "RUNODE_ASKPASS";
/// 桌面据此认出是哪次连接在问。
pub const ENV_TOKEN: &str = "RUNODE_ASKPASS_TOKEN";

const MAGIC: &str = "RUNODE-ASKPASS 1";
/// 提示和回答最长这么多字节，再长的不是 ssh 发的。
const MAX_TEXT: usize = 64 * 1024;

/// ssh 问的是什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 要用户输入、不显示出来的：密码、口令、验证码。
    Secret,
    /// 要用户答是或否：首次连接时认不认主机指纹、ssh-agent 要确认的签名。答是时回 `yes`。
    Confirm,
    /// 只是告诉用户一声，比如请触碰安全密钥；ssh 办完就结束这个进程，不等回答。
    Notice,
}

impl Kind {
    /// 按 ssh 设的 `SSH_ASKPASS_PROMPT`（`confirm`、`none`）和提示文字判断。
    pub fn of(prompt_env: Option<&str>, prompt: &str) -> Self {
        match prompt_env {
            Some("confirm") => Self::Confirm,
            Some("none") => Self::Notice,
            _ if prompt.contains("(yes/no") => Self::Confirm,
            _ => Self::Secret,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Confirm => "confirm",
            Self::Notice => "notice",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "secret" => Self::Secret,
            "confirm" => Self::Confirm,
            "notice" => Self::Notice,
            _ => return None,
        })
    }
}

/// askpass 进程交给桌面的一次询问。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub token: String,
    pub kind: Kind,
    pub prompt: String,
}

/// 桌面的回答：`None` 是用户取消了。
pub type Reply = Option<String>;

fn write_text(out: &mut impl Write, text: &str) -> io::Result<()> {
    writeln!(out, "{}", text.len())?;
    out.write_all(text.as_bytes())
}

fn read_line(input: &mut impl BufRead) -> io::Result<String> {
    let mut line = String::new();
    input.by_ref().take(256).read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(io::ErrorKind::InvalidData.into());
    }
    line.pop();
    Ok(line)
}

fn read_text(input: &mut impl BufRead) -> io::Result<String> {
    let len: usize = read_line(input)?.parse().map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if len > MAX_TEXT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; len];
    input.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| io::ErrorKind::InvalidData.into())
}

pub fn write_request(out: &mut impl Write, request: &Request) -> io::Result<()> {
    writeln!(out, "{MAGIC}")?;
    writeln!(out, "{}", request.token)?;
    writeln!(out, "{}", request.kind.name())?;
    write_text(out, &request.prompt)?;
    out.flush()
}

pub fn read_request(input: &mut impl BufRead) -> io::Result<Request> {
    if read_line(input)? != MAGIC {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let token = read_line(input)?;
    let kind = Kind::from_name(&read_line(input)?).ok_or(io::ErrorKind::InvalidData)?;
    let prompt = read_text(input)?;
    Ok(Request { token, kind, prompt })
}

pub fn write_reply(out: &mut impl Write, reply: &Reply) -> io::Result<()> {
    match reply {
        Some(answer) => {
            writeln!(out, "ok")?;
            write_text(out, answer)?;
        }
        None => writeln!(out, "cancel")?,
    }
    out.flush()
}

pub fn read_reply(input: &mut impl BufRead) -> io::Result<Reply> {
    match read_line(input)?.as_str() {
        "ok" => Ok(Some(read_text(input)?)),
        "cancel" => Ok(None),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// 让 `command` 起的 ssh 要问用户时调 `askpass`（runode 自己），经 `socket` 问桌面，`token` 标明是哪次连接。
/// 没有终端也一律走 askpass（`SSH_ASKPASS_REQUIRE=force`）。
pub fn configure(command: &mut Command, askpass: &Path, socket: &Path, token: &str) {
    command
        .env("SSH_ASKPASS", askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env(ENV_SOCKET, socket)
        .env(ENV_TOKEN, token);
    // 老版本的 ssh 没设 DISPLAY 时不用 askpass。
    if std::env::var_os("DISPLAY").is_none() {
        command.env("DISPLAY", "runode");
    }
}

/// 这个进程是 ssh 调起的 askpass 时问桌面、把回答写到 stdout，返回退出码（取消时为 1）；不是时为空。
pub fn run_if_requested() -> Option<i32> {
    let socket = std::env::var_os(ENV_SOCKET)?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let request = Request {
        token: std::env::var(ENV_TOKEN).unwrap_or_default(),
        kind: Kind::of(std::env::var("SSH_ASKPASS_PROMPT").ok().as_deref(), &prompt),
        prompt,
    };
    let answer = ask(Path::new(&socket), &request);
    Some(match answer {
        Ok(Some(answer)) => {
            let mut out = io::stdout().lock();
            if writeln!(out, "{answer}").and_then(|()| out.flush()).is_ok() { 0 } else { 1 }
        }
        Ok(None) => 1,
        Err(err) => {
            let _ = writeln!(io::stderr(), "runode askpass: {err}");
            1
        }
    })
}

fn ask(socket: &Path, request: &Request) -> io::Result<Reply> {
    #[cfg(unix)]
    let stream = std::os::unix::net::UnixStream::connect(socket)?;
    #[cfg(windows)]
    let stream = uds_windows::UnixStream::connect(socket)?;
    let mut writer = &stream;
    write_request(&mut writer, request)?;
    read_reply(&mut BufReader::new(&stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_round_trip() {
        let request = Request { token: "t".into(), kind: Kind::Secret, prompt: "Enter passphrase\nfor key:".into() };
        let mut wire = Vec::new();
        write_request(&mut wire, &request).unwrap();
        assert_eq!(read_request(&mut wire.as_slice()).unwrap(), request);
        for reply in [Some("pa ss\nword".to_owned()), None] {
            let mut wire = Vec::new();
            write_reply(&mut wire, &reply).unwrap();
            assert_eq!(read_reply(&mut wire.as_slice()).unwrap(), reply);
        }
        assert!(read_request(&mut "HELLO\n".as_bytes()).is_err());
    }

    #[test]
    fn classifies_prompts() {
        let fingerprint = "The authenticity of host 'x' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])?";
        assert_eq!(Kind::of(None, fingerprint), Kind::Confirm);
        assert_eq!(Kind::of(None, "user@host's password:"), Kind::Secret);
        assert_eq!(Kind::of(Some("none"), "Confirm user presence for key"), Kind::Notice);
        assert_eq!(Kind::of(Some("confirm"), "Allow use of key?"), Kind::Confirm);
    }
}
