//! 取一个网址：小的（清单）读进内存，更新包直接下载成文件。macOS 经 NSURLSession，系统的代理设置、
//! 证书和重定向都照系统的来；Windows 交给系统自带的 curl.exe。

use std::{path::Path, time::Duration};

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2_foundation::{NSError, NSURL, NSURLResponse, NSURLSession, NSURLSessionTask};

use crate::Error;

/// 一次请求等这么久没有新的数据就算超时；整个下载的上限由调用方给。
#[cfg(target_os = "macos")]
const IDLE_TIMEOUT: f64 = 30.0;

/// 下载时多久报一次进度。
#[cfg(target_os = "macos")]
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// 取 `url` 的内容，阻塞到取完或者出错，最多 `timeout`。HTTP 状态不是 2xx 时算出错。下载期间每隔
/// `PROGRESS_INTERVAL` 调一次 `progress(已收到的字节, 总字节)`，服务器没说总大小时总字节是 0。
#[cfg(target_os = "macos")]
pub(crate) fn get(url: &str, timeout: Duration, progress: &dyn Fn(u64, u64)) -> Result<Vec<u8>, Error> {
    use block2::RcBlock;
    use objc2_foundation::NSData;

    perform(url, timeout, progress, |session, target, done| {
        let handler = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            // SAFETY: 完成回调给的三个指针要么为空，要么在回调期间有效。
            let (data, response, error) = unsafe { (data.as_ref(), response.as_ref(), error.as_ref()) };
            let outcome = match failure(error, response) {
                Some(err) => Err(err),
                None => Ok(data.map(NSData::to_vec).unwrap_or_default()),
            };
            let _ = done.send(outcome);
        });
        // SAFETY: 回调的签名和 NSURLSession 要的一致，它只在别的线程上被调一次。
        Retained::into_super(unsafe { session.dataTaskWithURL_completionHandler(target, &handler) })
    })
}

/// 把 `url` 下载成文件 `to`，和 `get` 一样阻塞、报进度、判 HTTP 状态，但内容由系统边收边写进临时
/// 文件，不经内存。HTTP 状态不是 2xx 时不留 `to`。
#[cfg(target_os = "macos")]
pub(crate) fn download(url: &str, to: &Path, timeout: Duration, progress: &dyn Fn(u64, u64)) -> Result<(), Error> {
    use block2::RcBlock;

    let to = to.to_owned();
    perform(url, timeout, progress, move |session, target, done| {
        let handler = RcBlock::new(move |location: *mut NSURL, response: *mut NSURLResponse, error: *mut NSError| {
            // SAFETY: 完成回调给的三个指针要么为空，要么在回调期间有效。
            let (location, response, error) = unsafe { (location.as_ref(), response.as_ref(), error.as_ref()) };
            // 系统在回调返回后就删掉临时文件，所以就在这里挪走。
            let outcome = match failure(error, response) {
                Some(err) => Err(err),
                None => location
                    .and_then(NSURL::to_file_path)
                    .ok_or_else(|| Error::Network("the download produced no file".into()))
                    .and_then(|from| {
                        move_file(&from, &to).map_err(|err| Error::Io(format!("{}: {err}", to.display())))
                    }),
            };
            let _ = done.send(outcome);
        });
        // SAFETY: 同 `get`。
        Retained::into_super(unsafe { session.downloadTaskWithURL_completionHandler(target, &handler) })
    })
}

/// `get` 和 `download` 共用的部分：建会话，由 `start` 建好任务、把结果交给它给的 `Sender`，在这个
/// 线程上等结果、隔一会儿报一次进度。
#[cfg(target_os = "macos")]
fn perform<T>(
    url: &str,
    timeout: Duration,
    progress: &dyn Fn(u64, u64),
    start: impl FnOnce(&NSURLSession, &NSURL, std::sync::mpsc::Sender<Result<T, Error>>) -> Retained<NSURLSessionTask>,
) -> Result<T, Error> {
    use std::sync::mpsc;

    use objc2_foundation::{NSString, NSURLSessionConfiguration};

    let Some(target) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return Err(Error::Network(format!("bad URL {url}")));
    };
    // 不留 cookie 和缓存：清单每次都要新的。
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    config.setTimeoutIntervalForRequest(IDLE_TIMEOUT);
    config.setTimeoutIntervalForResource(timeout.as_secs_f64());
    let session = NSURLSession::sessionWithConfiguration(&config);
    let (done, result) = mpsc::channel();
    let task = start(&session, &target, done);
    task.resume();
    let outcome = loop {
        match result.recv_timeout(PROGRESS_INTERVAL) {
            Ok(outcome) => break outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let (received, expected) = (task.countOfBytesReceived(), task.countOfBytesExpectedToReceive());
                progress(received.max(0) as u64, expected.max(0) as u64);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err(Error::Network("the request was dropped".into())),
        }
    };
    session.finishTasksAndInvalidate();
    outcome
}

/// 完成回调里的错：请求本身出错，或者 HTTP 状态不是 2xx。
#[cfg(target_os = "macos")]
fn failure(error: Option<&NSError>, response: Option<&NSURLResponse>) -> Option<Error> {
    use objc2_foundation::NSHTTPURLResponse;

    if let Some(error) = error {
        return Some(Error::Network(error.localizedDescription().to_string()));
    }
    let status = response?.downcast_ref::<NSHTTPURLResponse>()?.statusCode();
    (!(200..300).contains(&status)).then(|| Error::Network(format!("HTTP {status}")))
}

/// 把 `from` 挪成 `to`。系统的临时目录和暂存目录（在 .app 旁边）可能不在同一个卷上，rename 不了时
/// 复制过去，`from` 留给系统删。
#[cfg(target_os = "macos")]
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => std::fs::copy(from, to).map(drop),
        moved => moved,
    }
}

/// Windows 上交给系统自带的 curl.exe（Windows 10 起有）；只许 https，代理照环境变量。
#[cfg(windows)]
pub(crate) fn get(url: &str, timeout: Duration, _progress: &dyn Fn(u64, u64)) -> Result<Vec<u8>, Error> {
    let out = curl(timeout).arg(url).output().map_err(|err| Error::Network(format!("curl.exe: {err}")))?;
    if !out.status.success() {
        return Err(Error::Network(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(out.stdout)
}

/// 下载成文件，每 250 毫秒报一次已收到的字节（总字节未知，报 0）；失败时不留 `to`。
#[cfg(windows)]
pub(crate) fn download(url: &str, to: &Path, timeout: Duration, progress: &dyn Fn(u64, u64)) -> Result<(), Error> {
    use std::{io::Read as _, process::Stdio, thread};

    let mut child = curl(timeout)
        .arg("-o")
        .arg(to)
        .arg(url)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error::Network(format!("curl.exe: {err}")))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                progress(std::fs::metadata(to).map_or(0, |meta| meta.len()), 0);
                thread::sleep(Duration::from_millis(250));
            }
            Err(err) => return Err(Error::Network(err.to_string())),
        }
    };
    if !status.success() {
        let mut reason = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut reason);
        }
        let _ = std::fs::remove_file(to);
        return Err(Error::Network(reason.trim().to_owned()));
    }
    Ok(())
}

#[cfg(windows)]
fn curl(timeout: Duration) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;

    let mut command = std::process::Command::new("curl.exe");
    command
        .args(["-fsSL", "--retry", "2", "--proto", "=https", "--max-time"])
        .arg(timeout.as_secs().to_string())
        .creation_flags(0x0800_0000);
    command
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn get(_url: &str, _timeout: Duration, _progress: &dyn Fn(u64, u64)) -> Result<Vec<u8>, Error> {
    Err(unsupported())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn download(_url: &str, _to: &Path, _timeout: Duration, _progress: &dyn Fn(u64, u64)) -> Result<(), Error> {
    Err(unsupported())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn unsupported() -> Error {
    Error::Network("downloading updates is only supported on macOS".into())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    use super::*;

    /// 在本机起一个只回一次话的 HTTP 服务器，回 `status` 和 `body`，返回它的网址。
    fn serve_once(status: &'static str, body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/update.zip", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            // 读完请求头再回话。
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                line.clear();
            }
            let mut stream = reader.into_inner();
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        });
        url
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-fetch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_download_lands_in_the_given_file() {
        let body: &'static [u8] = Box::leak(vec![7u8; 300_000].into_boxed_slice());
        let url = serve_once("200 OK", body);
        let dir = scratch("ok");
        let zip = dir.join("update.zip");
        download(&url, &zip, Duration::from_secs(30), &|_, _| {}).unwrap();
        assert_eq!(std::fs::read(&zip).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_status_leaves_no_file() {
        let url = serve_once("404 Not Found", b"missing");
        let dir = scratch("404");
        let zip = dir.join("update.zip");
        assert_eq!(download(&url, &zip, Duration::from_secs(30), &|_, _| {}), Err(Error::Network("HTTP 404".into())));
        assert!(!zip.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_replaces_the_target() {
        let dir = scratch("move");
        let (from, to) = (dir.join("from"), dir.join("to"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        move_file(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
