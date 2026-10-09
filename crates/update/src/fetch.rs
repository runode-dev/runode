//! 经 NSURLSession 取一个网址的内容：系统的代理设置、证书和重定向都照系统的来。

use std::time::Duration;

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
    use std::sync::mpsc;

    use block2::RcBlock;
    use objc2_foundation::{
        NSData, NSError, NSHTTPURLResponse, NSString, NSURL, NSURLResponse, NSURLSession, NSURLSessionConfiguration,
    };

    let Some(target) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return Err(Error::Network(format!("bad URL {url}")));
    };
    // 不留 cookie 和缓存：清单每次都要新的。
    let config = NSURLSessionConfiguration::ephemeralSessionConfiguration();
    config.setTimeoutIntervalForRequest(IDLE_TIMEOUT);
    config.setTimeoutIntervalForResource(timeout.as_secs_f64());
    let session = NSURLSession::sessionWithConfiguration(&config);
    let (done, result) = mpsc::channel();
    let handler = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
        // SAFETY: 完成回调给的三个指针要么为空，要么在回调期间有效。
        let (data, response, error) = unsafe { (data.as_ref(), response.as_ref(), error.as_ref()) };
        let outcome = match (error, response.and_then(|response| response.downcast_ref::<NSHTTPURLResponse>())) {
            (Some(error), _) => Err(Error::Network(error.localizedDescription().to_string())),
            (None, Some(http)) if !(200..300).contains(&http.statusCode()) => {
                Err(Error::Network(format!("HTTP {}", http.statusCode())))
            }
            (None, _) => Ok(data.map(NSData::to_vec).unwrap_or_default()),
        };
        let _ = done.send(outcome);
    });
    // SAFETY: 回调的签名和 NSURLSession 要的一致，它只在别的线程上被调一次。
    let task = unsafe { session.dataTaskWithURL_completionHandler(&target, &handler) };
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

#[cfg(not(target_os = "macos"))]
pub(crate) fn get(_url: &str, _timeout: Duration, _progress: &dyn Fn(u64, u64)) -> Result<Vec<u8>, Error> {
    Err(Error::Network("downloading updates is only supported on macOS".into()))
}
