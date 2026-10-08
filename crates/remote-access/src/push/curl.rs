//! 经 curl 发 HTTPS 请求：APNs 只说 HTTP/2，标准库没有 HTTP 客户端，系统自带的 curl 正好会。每个请求
//! 起一个 curl，URL、请求头（含签名的 JWT）和请求体都写成 curl 的配置从 stdin 交给它
//! （`--config -`），不出现在命令行参数里，别的进程用 `ps` 看不到。响应连头带体从 stdout 读回来
//! （`--include`）。

use std::{
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
};

/// 一个请求最多等这么多秒（curl 的 `--max-time`）。
const MAX_TIME_SECS: &str = "15";

/// 要发的一个请求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) method: &'static str,
    pub(crate) url: String,
    /// 名字和值；名字小写。
    pub(crate) headers: Vec<(&'static str, String)>,
    /// JSON 请求体；没有时不发体。
    pub(crate) body: Option<Vec<u8>>,
}

/// 收到的响应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// 名字转成小写。
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    /// 名字是 `name`（小写）的头。
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

/// 怎么运行 curl：平常是 PATH 上的 `curl`，测试换成假的。
#[derive(Clone, Debug)]
pub(crate) struct Curl {
    pub(crate) program: PathBuf,
}

impl Default for Curl {
    fn default() -> Self {
        Self { program: "curl".into() }
    }
}

impl Curl {
    /// 看 curl 能不能用：运行得了，而且编进了 HTTP/2（`curl --version` 的 Features 里有 `HTTP2`）。
    pub(crate) fn check(&self) -> Result<(), String> {
        let output = Command::new(&self.program)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|err| format!("cannot run {}: {err}", self.program.display()))?;
        let text = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            return Err(format!("{} --version failed: {}", self.program.display(), output.status));
        }
        let http2 = text
            .lines()
            .filter_map(|line| line.strip_prefix("Features:"))
            .any(|features| features.split_whitespace().any(|feature| feature == "HTTP2"));
        if !http2 {
            return Err(format!("{} has no HTTP/2 support", self.program.display()));
        }
        Ok(())
    }

    /// 发 `request`，等到响应。curl 运行不了、连不上、超时这类没有拿到响应的情况是 `Err`。
    pub(crate) fn send(&self, request: &Request) -> Result<Response, String> {
        let mut child = Command::new(&self.program)
            // `-q` 必须在最前面：不读 `~/.curlrc`，免得里面的 `fail`、`location` 改了行为。走代理时
            // `--include` 会先输出代理 CONNECT 的应答，`--suppress-connect-headers` 去掉它，解析看到的
            // 头一块才是 APNs 的响应。
            .args(["-q", "--http2", "--silent", "--show-error", "--max-time", MAX_TIME_SECS, "--proto", "=https"])
            .args(["--suppress-connect-headers", "--include", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("cannot run {}: {err}", self.program.display()))?;
        let config = config(request);
        let stdin = child.stdin.take();
        // 一边写配置一边读输出，谁也不会等着谁。
        let output = std::thread::scope(|scope| {
            if let Some(mut stdin) = stdin {
                scope.spawn(move || {
                    // curl 不读完就退出时写会失败，以它的退出状态为准。
                    let _ = stdin.write_all(config.as_bytes());
                });
            }
            child.wait_with_output()
        })
        .map_err(|err| format!("cannot run {}: {err}", self.program.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("curl failed ({}): {}", output.status, stderr.trim()));
        }
        parse(&output.stdout)
    }
}

/// curl 的配置：一行一项，值写成带引号的字符串。
fn config(request: &Request) -> String {
    let mut out = format!("url = {}\nrequest = {}\n", quoted(&request.url), quoted(request.method));
    for (name, value) in &request.headers {
        out.push_str(&format!("header = {}\n", quoted(&format!("{name}: {value}"))));
    }
    if let Some(body) = &request.body {
        out.push_str(&format!("header = {}\n", quoted("content-type: application/json")));
        // `data-raw` 不把开头的 `@` 当成文件名。
        out.push_str(&format!("data-raw = {}\n", quoted(&String::from_utf8_lossy(body))));
    }
    out
}

/// curl 配置里带引号的字符串：反斜杠、引号和控制字符转义。JSON 请求体里没有原样的换行，引号和反斜杠
/// 却很多。
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 解 `--include` 的输出：状态行、头、空行、体。前面有 1xx 的临时响应时跳过它们。
fn parse(output: &[u8]) -> Result<Response, String> {
    let mut rest = output;
    loop {
        let end = rest
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "curl printed no complete response head".to_owned())?;
        let head = String::from_utf8_lossy(&rest[..end]);
        let body = &rest[end + 4..];
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| format!("unexpected status line from curl: {status_line}"))?;
        if (100..200).contains(&status) {
            rest = body;
            continue;
        }
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        return Ok(Response { status, headers, body: body.to_vec() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_quotes_everything() {
        let request = Request {
            method: "POST",
            url: "https://api.push.apple.com/4/broadcasts/apps/dev.runode.mobile".into(),
            headers: vec![("apns-channel-id", "ab+/c==".into()), ("authorization", "bearer x.y.z".into())],
            body: Some(br#"{"a":"q\"uo\\te"}"#.to_vec()),
        };
        assert_eq!(
            config(&request),
            "url = \"https://api.push.apple.com/4/broadcasts/apps/dev.runode.mobile\"\nrequest = \"POST\"\n\
             header = \"apns-channel-id: ab+/c==\"\nheader = \"authorization: bearer x.y.z\"\n\
             header = \"content-type: application/json\"\ndata-raw = \"{\\\"a\\\":\\\"q\\\\\\\"uo\\\\\\\\te\\\"}\"\n"
        );
        let delete = Request { method: "DELETE", url: "https://x".into(), headers: Vec::new(), body: None };
        assert_eq!(config(&delete), "url = \"https://x\"\nrequest = \"DELETE\"\n");
    }

    #[test]
    fn responses_are_parsed() {
        let response = parse(b"HTTP/2 201 \r\napns-request-id: 1\r\nApns-Channel-Id: dGVzdA==\r\n\r\n").unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(response.header("apns-channel-id"), Some("dGVzdA=="));
        assert!(response.body.is_empty());

        let response = parse(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 400 Bad Request\r\nx: y\r\n\r\n{\"reason\":\"BadDeviceToken\"}",
        )
        .unwrap();
        assert_eq!((response.status, response.body.as_slice()), (400, br#"{"reason":"BadDeviceToken"}"#.as_slice()));
        assert!(parse(b"HTTP/2 200\r\n").is_err());
        assert!(parse(b"garbage\r\n\r\n").is_err());
    }
}
