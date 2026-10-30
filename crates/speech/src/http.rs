//! 用系统的 curl 发 HTTPS 请求，本 crate 不带 TLS 库。密钥经标准输入交给 curl 的 `--config -`，
//! 不出现在命令行上，别的用户用 `ps` 看不到；请求体写进临时目录里的文件，用完删掉。

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};

pub(crate) enum Body<'a> {
    Json(&'a serde_json::Value),
    /// 表单的文字字段和一个 WAV 文件字段（字段名、内容）。
    Form {
        fields: Vec<(&'a str, String)>,
        file: (&'a str, Vec<u8>),
    },
}

pub(crate) struct Response {
    pub(crate) status: u16,
    /// 响应头 `X-Api-Status-Code`，没有时是空的（豆包在这里报业务状态）。
    pub(crate) api_status: String,
    pub(crate) body: String,
}

/// 发一个 POST。`secret_headers` 是 `名字: 值` 形式、不能出现在命令行上的头，`headers` 是别的头。
pub(crate) fn post(
    url: &str,
    secret_headers: &[String],
    headers: &[String],
    body: Body,
    timeout: Duration,
) -> anyhow::Result<Response> {
    let dir = TempDir::new()?;
    let headers_file = dir.0.join("headers.txt");
    let mut command = curl();
    command
        // 不跟重定向：curl 只在换主机时去掉 `Authorization`，别的密钥头会跟着发过去。
        .args(["--silent", "--show-error", "--config", "-"])
        .args(["--connect-timeout", "10", "--max-time"])
        .arg(timeout.as_secs().to_string())
        // 响应头另写进文件自己找：`--write-out` 的 `%header{}` 要 curl 7.84，Windows 10 自带的是 7.55。
        .arg("--dump-header")
        .arg(&headers_file)
        .args(["--write-out", "\n%{http_code}"]);
    for header in headers {
        command.arg("--header").arg(header);
    }
    match body {
        Body::Json(json) => {
            let path = dir.0.join("body.json");
            fs::write(&path, serde_json::to_vec(json)?)?;
            command.args(["--header", "Content-Type: application/json", "--data-binary"]);
            command.arg(format!("@{}", path.display()));
        }
        Body::Form { fields, file: (name, data) } => {
            for (field, value) in fields {
                command.arg("--form-string").arg(format!("{field}={value}"));
            }
            let path = dir.0.join("audio.wav");
            fs::write(&path, data)?;
            command.arg("--form").arg(format!("{name}=@{};type=audio/wav", path.display()));
        }
    }
    command.arg("--url").arg(url).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().context("cannot run curl")?;
    let config: String = secret_headers.iter().map(|header| format!("header = \"{}\"\n", quote(header))).collect();
    child.stdin.take().context("no stdin for curl")?.write_all(config.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("curl {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
    }
    let headers = fs::read(&headers_file).context("cannot read curl response headers")?;
    parse(&String::from_utf8_lossy(&output.stdout), &String::from_utf8_lossy(&headers))
}

/// 跑 curl 的命令。Windows 上 `Command` 先找 System32 再找 PATH，所以先按 PATH 找 `curl.exe`，
/// 用户自己装的新版排在系统自带的前面；也不给它开控制台窗口。
pub(crate) fn curl() -> Command {
    #[cfg(windows)]
    {
        let program = std::env::var_os("PATH")
            .and_then(|path| std::env::split_paths(&path).map(|dir| dir.join("curl.exe")).find(|exe| exe.is_file()))
            .unwrap_or_else(|| PathBuf::from("curl"));
        let mut command = Command::new(program);
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
        command
    }
    #[cfg(not(windows))]
    Command::new("curl")
}

/// 拆开 curl 的输出：响应体，最后一行是 `--write-out` 写的状态码；`headers` 是 `--dump-header`
/// 写下的响应头，有 `100 Continue` 这类中间响应时取最后一段。
fn parse(stdout: &str, headers: &str) -> anyhow::Result<Response> {
    let (body, status) = stdout.rsplit_once('\n').context("curl printed no status")?;
    let lines: Vec<&str> = headers.lines().collect();
    let last = lines.iter().rposition(|line| line.starts_with("HTTP/")).unwrap_or(0);
    let api_status = lines[last..]
        .iter()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("x-api-status-code"))
        .map_or("", |(_, value)| value.trim());
    Ok(Response { status: status.trim().parse()?, api_status: api_status.to_owned(), body: body.to_owned() })
}

/// curl 配置文件里双引号内的写法：反斜杠和双引号前面加反斜杠，换行写成 `\n`。
fn quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r")
}

/// 一次请求用的临时目录，只有自己能进，丢掉时删掉。Linux 的临时目录是大家共用的，所以目录要新建，
/// 已经有了（可能是别人预先放好、里面有符号链接）就换个名字。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg_attr(not(unix), expect(unused_mut))]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        loop {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos();
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("runode-speech-{}-{n}-{nanos}", std::process::id()));
            match builder.create(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 16 位单声道 PCM 的 WAV 文件。
pub(crate) fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend(b"RIFF");
    out.extend((36 + data).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data.to_le_bytes());
    for sample in samples {
        out.extend(((sample.clamp(-1., 1.) * f32::from(i16::MAX)) as i16).to_le_bytes());
    }
    out
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(chunk.get(1).copied().unwrap_or(0)) << 8
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_like_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_status_line_is_split_off_the_body() {
        let headers = "HTTP/1.1 200 OK\r\nX-Api-Status-Code: 20000003\r\n\r\n";
        let response = parse("{\"a\":\"x\ny\"}\n200", headers).unwrap();
        assert_eq!((response.status, response.api_status.as_str()), (200, "20000003"));
        assert_eq!(response.body, "{\"a\":\"x\ny\"}");
        assert_eq!(parse("{}\n401", "HTTP/1.1 401 Unauthorized\r\n\r\n").unwrap().api_status, "");
        assert_eq!(parse("{}\n401", "").unwrap().api_status, "");
    }

    #[test]
    fn the_api_status_comes_from_the_final_response() {
        let headers = "HTTP/1.1 100 Continue\r\nx-api-status-code: 1\r\n\r\n\
                       HTTP/1.1 200 OK\r\nx-api-status-code: 20000000\r\n\r\n";
        assert_eq!(parse("{}\n200", headers).unwrap().api_status, "20000000");
        let headers = "HTTP/1.1 200 OK\r\nX-Api-Status-Code: 20000000\r\nX-Debug: HTTP/1.1\r\n\r\n";
        assert_eq!(parse("{}\n200", headers).unwrap().api_status, "20000000");
    }

    #[test]
    fn config_values_are_quoted() {
        assert_eq!(quote(r#"Authorization: Bearer a"b\c"#), r#"Authorization: Bearer a\"b\\c"#);
    }

    /// 锁定密钥头经标准输入交给 curl 后确实发了出去，表单字段和文件也在请求里。
    #[test]
    fn posts_secret_headers_and_form_fields() {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/audio/transcriptions", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut head = String::new();
            while !head.ends_with("\r\n\r\n") {
                reader.read_line(&mut head).unwrap();
            }
            let length: usize = head
                .lines()
                .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let reply = "HTTP/1.1 200 OK\r\nX-Api-Status-Code: 20000000\r\nContent-Length: 2\r\n\r\n{}";
            reader.get_mut().write_all(reply.as_bytes()).unwrap();
            (head, String::from_utf8_lossy(&body).into_owned())
        });
        let response = post(
            &url,
            &["Authorization: Bearer s\"ecret".to_owned()],
            &["X-Plain: 1".to_owned()],
            Body::Form { fields: vec![("model", "m1".to_owned())], file: ("file", wav(&[0.5], 16_000)) },
            Duration::from_secs(10),
        )
        .unwrap();
        let (head, body) = server.join().unwrap();
        assert!(head.contains("Authorization: Bearer s\"ecret\r\n"), "{head}");
        assert!(head.contains("X-Plain: 1\r\n"), "{head}");
        assert!(body.contains("name=\"model\"\r\n\r\nm1\r\n"), "{body}");
        assert!(body.contains("filename=\"audio.wav\"") && body.contains("RIFF"), "{body}");
        assert_eq!((response.status, response.api_status.as_str(), response.body.as_str()), (200, "20000000", "{}"));
    }

    #[test]
    fn wav_header_describes_the_samples() {
        let file = wav(&[0., 1., -1.], 16_000);
        assert_eq!(file.len(), 44 + 6);
        assert_eq!(&file[40..44], &6u32.to_le_bytes());
        assert_eq!(&file[44..], &[0, 0, 0xff, 0x7f, 0x01, 0x80]);
    }
}
