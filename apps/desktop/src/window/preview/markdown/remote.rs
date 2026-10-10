//! 排版视图里的网络图片（`http`、`https`）：外调 curl 在几个固定的下载线程里下载，限 10 秒、限
//! `runode_preview::MAX_IMAGE_BYTES` 字节，只收图片类型；同一个地址整个 app 只下一次，下好的结果留着
//! 给之后打开的文档用，下不了的不留，下次打开文档再试。主机是本机或局域网地址的不下载：网址里写的
//! 主机先查一遍，再自己解析出地址查一遍，解析到的地址交给 curl（`--resolve`）不让它再解析；重定向
//! 自己一跳一跳地跟，每跳都这样查。

use std::{
    collections::HashMap,
    io::Read as _,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs as _},
    panic::{self, AssertUnwindSafe},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, LazyLock, Mutex, PoisonError,
        mpsc::{self, Receiver, Sender},
    },
};

use futures::{FutureExt as _, channel::oneshot, future::Shared};
use gpui::SvgRenderer;
use runode_preview::{Content, ImageFormat, MAX_IMAGE_BYTES};

use super::{Picture, picture_of};

/// 下载的总时长上限，秒。
const TIMEOUT_SECS: &str = "10";
/// 最多跟几次重定向（GitHub 上的图片地址大多要跳一次）。
const MAX_REDIRECTS: usize = 5;
/// 同时下载的个数：这么多个线程从队列里取地址，一篇文档里的图片再多也只起这么多个 curl。
const WORKERS: usize = 4;

/// 下载中或下完的图片，等到的是画好的图，下不了时为空。
pub(super) type Pending = Shared<oneshot::Receiver<Option<Picture>>>;

/// 按地址记着的下载。shortcut: 下好的图（连同 GPUI 里解码的结果）一直留到 app 退出，关标签也不放；
/// 打开的网络图片多到占内存时再按用量清。
static DOWNLOADS: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

type Job = Box<dyn FnOnce() + Send>;

/// 下载队列，第一次用时起好 `WORKERS` 个线程。
static QUEUE: LazyLock<Sender<Job>> = LazyLock::new(|| {
    let (queue, jobs) = mpsc::channel::<Job>();
    let jobs = Arc::new(Mutex::new(jobs));
    for _ in 0..WORKERS {
        let jobs = jobs.clone();
        std::thread::spawn(move || {
            while let Some(job) = next_job(&jobs) {
                job();
            }
        });
    }
    queue
});

fn next_job(jobs: &Mutex<Receiver<Job>>) -> Option<Job> {
    jobs.lock().unwrap_or_else(PoisonError::into_inner).recv().ok()
}

/// 地址 `url` 的图片：第一次要时排进下载队列，之后都等同一个结果。本机、局域网的地址不下，直接为空。
pub(super) fn fetch(url: &str, svg: &SvgRenderer) -> Pending {
    let svg = svg.clone();
    fetch_with(&DOWNLOADS, url, move |url| {
        download(url).and_then(|(format, bytes)| picture_of(Content::Image { format, bytes }, &svg))
    })
}

/// `fetch` 去掉 curl 的部分：按地址记在 `downloads` 里，由下载线程跑 `work`；跑出来为空时把这个地址
/// 从表里去掉，下次再要时重下。
fn fetch_with(
    downloads: &'static Mutex<HashMap<String, Pending>>,
    url: &str,
    work: impl FnOnce(&str) -> Option<Picture> + Send + 'static,
) -> Pending {
    if is_local_host(url) {
        let (done, result) = oneshot::channel();
        let _ = done.send(None);
        return result.shared();
    }
    let mut map = downloads.lock().unwrap_or_else(PoisonError::into_inner);
    map.entry(url.to_owned())
        .or_insert_with(|| {
            let (done, result) = oneshot::channel();
            let url = url.to_owned();
            let job: Job = Box::new(move || {
                // 解码坏图之类 panic 了当作下不了：地址照常去掉好重试，下载线程也不跟着退出。
                let picture = panic::catch_unwind(AssertUnwindSafe(|| work(&url))).ok().flatten();
                if picture.is_none() {
                    downloads.lock().unwrap_or_else(PoisonError::into_inner).remove(&url);
                }
                let _ = done.send(picture);
            });
            let _ = QUEUE.send(job);
            result.shared()
        })
        .clone()
}

/// 网址里写的主机是本机或局域网：`localhost`、`.localhost`、`.local` 结尾的名字，`is_local_ip` 那些
/// 地址段的 IPv4（`127.1`、`0x7f000001` 这类简写同样认）和 IPv6 字面量，方括号里认不出的地址，以及
/// 带百分号编码的主机。解析出来的地址另由 `public_address` 查。
fn is_local_host(url: &str) -> bool {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    if let Some(inner) = host.strip_prefix('[') {
        let inner = inner.split(']').next().unwrap_or_default();
        return inner.parse::<Ipv6Addr>().ok().is_none_or(|ip| is_local_ip(IpAddr::V6(ip)));
    }
    let host = host.split(':').next().unwrap_or_default().trim_end_matches('.').to_ascii_lowercase();
    // curl 会解开主机里的百分号编码，正常的图片地址用不着，一律不下。
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.contains('%') {
        return true;
    }
    loose_ipv4(&host).is_some_and(|ip| is_local_ip(IpAddr::V4(ip)))
}

/// 本机或局域网的地址：回环、私有、链路本地、未指定、组播、广播，运营商级 NAT 段 100.64.0.0/10
/// （Tailscale 的设备也在这段），IPv6 的唯一本地地址 fc00::/7，以及映射到这些 IPv4 的 IPv6 地址。
fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || a == 0
                || (a == 100 && b & 0xc0 == 64)
        }
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => is_local_ip(IpAddr::V4(v4)),
            None => {
                let first = ip.segments()[0];
                ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || first & 0xfe00 == 0xfc00
                    || first & 0xffc0 == 0xfe80
            }
        },
    }
}

/// 网址的主机（方括号去掉）和端口，没写端口时按协议取 80 或 443；不是 http、https 时为空。
fn host_and_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (host, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (host, after) = inner.split_once(']')?;
            (host, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = match port {
        Some(port) if !port.is_empty() => port.parse().ok()?,
        _ => default,
    };
    (!host.is_empty()).then(|| (host.to_owned(), port))
}

/// `addresses` 都不是本机或局域网时取第一个；有一个是就为空（一个名字同时解析到公网和内网的不下）。
fn first_public(addresses: impl IntoIterator<Item = IpAddr>) -> Option<IpAddr> {
    let mut first = None;
    for ip in addresses {
        if is_local_ip(ip) {
            return None;
        }
        first.get_or_insert(ip);
    }
    first
}

/// 要下载的地址：网址里写的主机和它解析出来的地址都不是本机或局域网时，给出主机、端口和该连的地址。
fn public_address(url: &str) -> Option<(String, u16, IpAddr)> {
    // curl 把 `\` 当成用户名里的字符，`is_local_host` 和 `host_and_port` 却在它这里断开主机，
    // `http://example.com\@127.0.0.1/` 就会查一个主机、连另一个；空白和控制字符也一样不收。
    if url.chars().any(|c| c == '\\' || c.is_whitespace() || c.is_control()) || is_local_host(url) {
        return None;
    }
    let (host, port) = host_and_port(url)?;
    let ip = first_public((host.as_str(), port).to_socket_addrs().ok()?.map(|address| address.ip()))?;
    Some((host, port, ip))
}

/// 照 `inet_aton` 的写法解 IPv4：一到四段，每段十进制、`0x` 开头的十六进制或 `0` 开头的八进制，最后
/// 一段填满剩下的字节（`127.1` 是 127.0.0.1）。curl 也这样认主机，不这样解就能用简写绕过。
fn loose_ipv4(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in &parts {
        let (digits, radix) = if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            (hex, 16)
        } else if part.len() > 1 && part.starts_with('0') {
            (&part[1..], 8)
        } else {
            (*part, 10)
        };
        if digits.is_empty() && radix != 16 {
            return None;
        }
        values.push(if digits.is_empty() { 0 } else { u32::from_str_radix(digits, radix).ok()? });
    }
    let (&last, head) = values.split_last()?;
    // 前几段各占一个字节，最后一段占剩下的位。
    let rest_bits = 8 * (4 - head.len() as u32);
    if head.iter().any(|&value| value > 255) || (rest_bits < 32 && last >> rest_bits != 0) {
        return None;
    }
    let ip = head.iter().enumerate().fold(last, |ip, (ix, &value)| ip | value << (24 - 8 * ix));
    Some(Ipv4Addr::from(ip))
}

/// 下载 `url`，返回图片格式和字节；出错、超时、太大、不是图片、落到本机或局域网地址时为空。
/// 重定向自己跟，每一跳都重新查地址。
fn download(url: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        match fetch_once(&url)? {
            Hop::Done(kind, bytes) => return Some((image_type(&kind, &url)?, bytes)),
            Hop::Redirect(next) => url = next,
        }
    }
    None
}

/// 一次请求的结果：拿到正文（连同内容类型），或者要跳到别的地址。
enum Hop {
    Done(String, Vec<u8>),
    Redirect(String),
}

/// 请求一次 `url`，不跟重定向。连的是 `public_address` 查过的那个地址，curl 不再自己解析。
fn fetch_once(url: &str) -> Option<Hop> {
    let (host, port, ip) = public_address(url)?;
    let ip = match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let mut command = Command::new("curl");
    // `-q` 得是第一个参数，不读用户的 ~/.curlrc（里面的代理、跟重定向等会绕开这里的检查）；`-g` 不展开
    // 网址里的 `{}`、`[]`，一个网址只请求一次。
    command.args(["-q", "-g", "-sf", "--proto", "=http,https"]);
    // 主机本身是 IP 时 curl 不解析，`--resolve` 也写不对 IPv6 的主机，用不着它。
    if host.parse::<IpAddr>().is_err() && loose_ipv4(&host).is_none() {
        command.args(["--resolve", &format!("{host}:{port}:{ip}")]);
    }
    let mut child = command
        .args(["--max-time", TIMEOUT_SECS])
        .args(["--max-filesize", &MAX_IMAGE_BYTES.to_string()])
        // 正文写到标准输出；状态码、内容类型和重定向的目标各一行写到标准错误。
        .args(["-w", "%{stderr}%{http_code}\n%{content_type}\n%{redirect_url}", "--", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    let read = child.stdout.take()?.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_IMAGE_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let mut out = String::new();
    child.stderr.take()?.read_to_string(&mut out).ok()?;
    if !child.wait().ok()?.success() {
        return None;
    }
    let mut lines = out.lines();
    let (code, kind, next) = (lines.next()?, lines.next().unwrap_or_default(), lines.next().unwrap_or_default());
    Some(if code.starts_with('3') && !next.is_empty() {
        Hop::Redirect(next.to_owned())
    } else {
        Hop::Done(kind.to_owned(), bytes)
    })
}

/// 按服务器给的内容类型认图片格式。raw.githubusercontent.com 这类把图片当成 `text/plain`、
/// `application/octet-stream` 发，这时按网址路径的扩展名认；别的类型（网页之类）不收。
fn image_type(content_type: &str, url: &str) -> Option<ImageFormat> {
    let mime = content_type.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        "image/x-icon" | "image/vnd.microsoft.icon" => ImageFormat::Ico,
        "image/svg+xml" => ImageFormat::Svg,
        "" | "text/plain" | "application/octet-stream" => {
            let path = url.split(['?', '#']).next().unwrap_or_default();
            return runode_preview::image_format(Path::new(path));
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn local_and_private_hosts_are_not_fetched() {
        for url in [
            "http://localhost/a.png",
            "http://LOCALHOST:8080/a.png",
            "http://app.localhost/a.png",
            "https://printer.local./a.png",
            "http://127.0.0.1/a.png",
            "http://127.1/a.png",
            "http://0x7f000001/a.png",
            "http://2130706433/a.png",
            "http://user@10.1.2.3:80/a.png",
            "http://172.16.0.1/a.png",
            "http://172.31.255.255/a.png",
            "http://192.168.1.1/a.png",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0/a.png",
            "http://[::1]/a.png",
            "http://[::]/a.png",
            "http://[fc00::1]/a.png",
            "http://[fd12:3456::1]/a.png",
            "http://[fe80::1]/a.png",
            "http://[::ffff:127.0.0.1]/a.png",
            "http://[fe80::1%25en0]/a.png",
            "http://%31%32%37.0.0.1/a.png",
        ] {
            assert!(is_local_host(url), "{url}");
        }
        for url in [
            "https://raw.githubusercontent.com/a/b/logo.png",
            "https://img.shields.io/badge/x.svg",
            "http://8.8.8.8/a.png",
            "http://172.32.0.1/a.png",
            "http://[2606:4700::1111]/a.png",
            "https://local.example.com/a.png",
            "https://example.com/localhost/a.png",
        ] {
            assert!(!is_local_host(url), "{url}");
        }
    }

    #[test]
    fn resolved_addresses_must_be_public() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        assert_eq!(first_public([ip("8.8.8.8"), ip("1.1.1.1")]), Some(ip("8.8.8.8")));
        // 一个名字解析到的地址里只要有内网的就不下：换着解析（DNS rebinding）也躲不过去。
        assert_eq!(first_public([ip("8.8.8.8"), ip("192.168.1.1")]), None);
        assert_eq!(first_public([]), None);
        for local in ["100.64.0.1", "100.127.255.254", "224.0.0.1", "255.255.255.255", "ff02::1", "::ffff:10.0.0.1"] {
            assert!(is_local_ip(ip(local)), "{local}");
        }
        for public in ["100.63.255.255", "100.128.0.1", "2606:4700::1111"] {
            assert!(!is_local_ip(ip(public)), "{public}");
        }
    }

    /// curl 把 `\\` 当成用户名里的字符，按它断开主机会看错主机，这类网址直接不下。
    #[test]
    fn urls_with_backslash_or_whitespace_are_refused() {
        for url in [
            "http://example.com\\@127.0.0.1:8080/a.png",
            "http://example.com/a b.png",
            "http://example.com/a\n.png",
            "http://example.com/a\u{7f}.png",
        ] {
            assert_eq!(public_address(url), None, "{url}");
        }
    }

    #[test]
    fn urls_split_into_host_and_port() {
        let split = |url: &str| host_and_port(url);
        assert_eq!(split("https://example.com/a.png"), Some(("example.com".into(), 443)));
        assert_eq!(split("http://user@example.com:8080/a"), Some(("example.com".into(), 8080)));
        assert_eq!(split("HTTP://example.com?x=1"), Some(("example.com".into(), 80)));
        assert_eq!(split("https://[2606:4700::1111]:8443/a"), Some(("2606:4700::1111".into(), 8443)));
        assert_eq!(split("https://[2606:4700::1111]/a"), Some(("2606:4700::1111".into(), 443)));
        assert_eq!(split("ftp://example.com/a"), None);
        assert_eq!(split("https://example.com:99999/a"), None);
    }

    /// 下不了的不记着，再要时重下；下好的记着，再要时不再下。
    #[test]
    fn failed_downloads_are_retried() {
        static DOWNLOADS: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        let url = "https://example.com/a.png";
        let picture = || Picture::Bitmap(Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![1])));
        let run = |result: Option<Picture>| {
            move |_: &str| {
                RUNS.fetch_add(1, Ordering::SeqCst);
                result
            }
        };
        assert!(futures::executor::block_on(fetch_with(&DOWNLOADS, url, run(None))).unwrap().is_none());
        assert!(futures::executor::block_on(fetch_with(&DOWNLOADS, url, run(Some(picture())))).unwrap().is_some());
        assert!(futures::executor::block_on(fetch_with(&DOWNLOADS, url, run(None))).unwrap().is_some());
        assert_eq!(RUNS.load(Ordering::SeqCst), 2);
        // 下载时 panic 了当作下不了，再要时重下。
        let broken = "https://example.com/broken.png";
        let panics = |_: &str| -> Option<Picture> { panic!("坏图") };
        assert!(futures::executor::block_on(fetch_with(&DOWNLOADS, broken, panics)).unwrap().is_none());
        assert!(futures::executor::block_on(fetch_with(&DOWNLOADS, broken, run(Some(picture())))).unwrap().is_some());
        assert_eq!(RUNS.load(Ordering::SeqCst), 3);
        // 本机地址不排进下载队列。
        assert!(
            futures::executor::block_on(fetch_with(&DOWNLOADS, "http://localhost/a.png", run(None))).unwrap().is_none()
        );
        assert_eq!(RUNS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn only_image_types_are_accepted() {
        assert_eq!(image_type("image/png", "https://x/a"), Some(ImageFormat::Png));
        assert_eq!(image_type("image/SVG+xml; charset=utf-8", "https://x/a"), Some(ImageFormat::Svg));
        assert_eq!(image_type("text/html; charset=utf-8", "https://x/a.png"), None);
        // 当成纯文本发的图片按扩展名认，不认识的扩展名不收。
        assert_eq!(image_type("text/plain; charset=utf-8", "https://raw/x/logo.svg?raw=1"), Some(ImageFormat::Svg));
        assert_eq!(image_type("application/octet-stream", "https://x/file.zip"), None);
    }
}
