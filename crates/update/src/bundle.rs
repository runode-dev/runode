//! 装到 SSH 远端用的发布包：从 GitHub 上某个版本的 Release 下载给某个平台的包，按同一个 Release 里的
//! `SHA256SUMS` 核对，下过的按版本留在调用方给的缓存目录里，旁边记着核对过的摘要；下次重算摘要对得上就
//! 直接用，不再联网，对不上的重新下载。Linux 的包是只装着 `runode` 的
//! tar.gz，Windows 的是装着 `runode.exe`、`rn.exe` 和终端要的文件的 zip，都原样交出去；mac 的 dmg 挂上后
//! 核对 .app 的签名（`TEAM_ID` 的 Developer ID），再取出里面的可执行文件。

use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use sha2::{Digest as _, Sha256};

use crate::Error;

/// 发布包的签名要出自这个 Team。
pub const TEAM_ID: &str = "5VRL2M2A3V";

pub(crate) const RELEASES: &str = "https://github.com/runode-dev/runode/releases/download";
/// 下载一个包最多等这么久。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(crate) const SUMS_TIMEOUT: Duration = Duration::from_secs(30);

/// 同一时间只取一个包：几台主机一起部署时，下载中的 `.part`、mac 的解包目录和 dmg 都在同一个路径上，
/// 一起取会互相踩坏；排在后面的等前面的取完，直接用缓存。
static FETCHING: Mutex<()> = Mutex::new(());

/// `version` 给 Linux `arch`（`x86_64`、`aarch64`）的 tar.gz，核对过的。会阻塞到下完，下载时不断调
/// `progress(已收到的字节, 总字节)`，总字节未知时是 0。
pub fn linux_archive(version: &str, arch: &str, cache: &Path, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, Error> {
    let name = format!("Runode-{version}-linux-{arch}.tar.gz");
    let _fetching = FETCHING.lock().unwrap_or_else(PoisonError::into_inner);
    fetch_checked(version, &name, cache, progress)
}

/// `version` 给 Windows `arch`（`x86_64`）的 zip，核对过的。会阻塞到下完，下载时不断调
/// `progress(已收到的字节, 总字节)`，总字节未知时是 0。
pub fn windows_archive(version: &str, arch: &str, cache: &Path, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, Error> {
    let name = format!("Runode-{version}-windows-{arch}.zip");
    let _fetching = FETCHING.lock().unwrap_or_else(PoisonError::into_inner);
    fetch_checked(version, &name, cache, progress)
}

/// `version` 给 mac `arch`（`arm64`、`x86_64`）的 `runode` 可执行文件，取自签名核对过的 .app。只有
/// macOS 上做得到（要挂 dmg、跑 codesign）。
pub fn mac_binary(version: &str, arch: &str, cache: &Path, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, Error> {
    let binary = cache.join(version).join(format!("runode-macos-{arch}"));
    let _fetching = FETCHING.lock().unwrap_or_else(PoisonError::into_inner);
    if cached(&binary) {
        return Ok(binary);
    }
    let dmg = fetch_checked(version, &format!("Runode-{version}-macos-{arch}.dmg"), cache, progress)?;
    extract_mac_binary(&dmg, version, &binary)?;
    let _ = fs::remove_file(&dmg);
    let _ = fs::remove_file(digest_file(&dmg));
    Ok(binary)
}

#[cfg(target_os = "macos")]
fn extract_mac_binary(dmg: &Path, version: &str, to: &Path) -> Result<(), Error> {
    let dir = to.with_extension("unpack");
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|err| io(&dir, err))?;
    }
    fs::create_dir_all(&dir).map_err(|err| io(&dir, err))?;
    let extracted = crate::install::app_from_dmg(dmg, &dir, TEAM_ID, version).and_then(|app| {
        let exe = app.join("Contents/MacOS/runode");
        // 记下的是签名核对过的 .app 里那份的摘要。
        let digest = sha256(&exe).map_err(|err| io(&exe, err))?;
        let part = to.with_extension("part");
        fs::copy(&exe, &part).map_err(|err| io(&exe, err))?;
        fs::rename(&part, to).map_err(|err| io(to, err))?;
        record_digest(to, &digest)
    });
    let _ = fs::remove_dir_all(&dir);
    extracted
}

#[cfg(not(target_os = "macos"))]
fn extract_mac_binary(_dmg: &Path, _version: &str, _to: &Path) -> Result<(), Error> {
    Err(Error::Rejected("a mac release can only be unpacked on macOS".into()))
}

/// 把 Release 里叫 `name` 的文件下到 `cache/<version>/name`，和 `SHA256SUMS` 对得上才留下。
fn fetch_checked(version: &str, name: &str, cache: &Path, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, Error> {
    fetch_checked_from(&format!("{RELEASES}/v{version}"), version, name, cache, progress)
}

/// 从下载地址 `base` 下取 `name` 和 `SHA256SUMS`。
fn fetch_checked_from(
    base: &str,
    version: &str,
    name: &str,
    cache: &Path,
    progress: &dyn Fn(u64, u64),
) -> Result<PathBuf, Error> {
    let dir = cache.join(version);
    let path = dir.join(name);
    if cached(&path) {
        return Ok(path);
    }
    fs::create_dir_all(&dir).map_err(|err| io(&dir, err))?;
    let sums = crate::fetch::get(&format!("{base}/SHA256SUMS"), SUMS_TIMEOUT, &|_, _| {})?;
    let expected = expected_sum(&String::from_utf8_lossy(&sums), name)
        .ok_or_else(|| Error::Rejected(format!("{name} is not in SHA256SUMS")))?;
    let part = dir.join(format!("{name}.part"));
    crate::fetch::download(&format!("{base}/{name}"), &part, DOWNLOAD_TIMEOUT, progress)?;
    let actual = sha256(&part).map_err(|err| io(&part, err))?;
    if actual != expected {
        let _ = fs::remove_file(&part);
        return Err(Error::Rejected(format!("{name} does not match SHA256SUMS")));
    }
    fs::rename(&part, &path).map_err(|err| io(&path, err))?;
    record_digest(&path, &expected)?;
    Ok(path)
}

/// 缓存里的 `path` 还和取的时候核对过的一样：重算的摘要等于当时记下的（`record_digest`）。对不上的、没记过
/// 摘要的连同摘要一起删掉，当作没取过。
fn cached(path: &Path) -> bool {
    let digest = digest_file(path);
    let intact = path.is_file()
        && fs::read_to_string(&digest).is_ok_and(|recorded| sha256(path).is_ok_and(|actual| actual == recorded.trim()));
    if !intact {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(&digest);
    }
    intact
}

/// 记下核对过的 `path` 的摘要 `sha256`，原子地写：不会留下写了一半的摘要。
fn record_digest(path: &Path, sha256: &str) -> Result<(), Error> {
    let digest = digest_file(path);
    let part = appended(&digest, ".part");
    fs::write(&part, sha256).map_err(|err| io(&part, err))?;
    fs::rename(&part, &digest).map_err(|err| io(&digest, err))
}

/// 缓存里 `path` 的摘要放在哪：同目录下文件名加上 `.sha256`。
fn digest_file(path: &Path) -> PathBuf {
    appended(path, ".sha256")
}

fn appended(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// `sha256sum` 输出里 `name` 的校验和：每行是 `<十六进制>  <名字>`，二进制模式时名字前带 `*`。
pub fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim_start();
        (file.strip_prefix('*').unwrap_or(file) == name).then(|| sum.to_ascii_lowercase())
    })
}

/// 文件的 SHA-256，十六进制小写。
pub fn sha256(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn io(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::{
        io::{BufRead as _, BufReader, Write as _},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    use super::*;

    const NAME: &str = "Runode-0.0.1-linux-x86_64.tar.gz";

    /// 在本机起一个 HTTP 服务器，`/SHA256SUMS` 回 `body` 的校验和，`/NAME` 回 `body`；返回下载地址和
    /// `NAME` 被取过几次。
    fn serve(body: &'static [u8]) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&hits);
        let sums = {
            let hex: String = Sha256::digest(body).iter().map(|byte| format!("{byte:02x}")).collect();
            format!("{hex}  {NAME}\n").into_bytes()
        };
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream);
                let mut request = String::new();
                let _ = reader.read_line(&mut request);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                    line.clear();
                }
                let path = request.split_whitespace().nth(1).unwrap_or_default().to_owned();
                let reply: Option<&[u8]> = if path == "/SHA256SUMS" {
                    Some(&sums)
                } else if path == format!("/{NAME}") {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Some(body)
                } else {
                    None
                };
                let (status, reply) = reply.map_or(("404 Not Found", &b""[..]), |reply| ("200 OK", reply));
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len());
                let mut stream = reader.into_inner();
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(reply);
            }
        });
        (base, hits)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-bundle-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_damaged_cache_is_fetched_again() {
        let body: &'static [u8] = Box::leak(vec![7u8; 100_000].into_boxed_slice());
        let (base, hits) = serve(body);
        let cache = scratch("damaged");
        let path = fetch_checked_from(&base, "0.0.1", NAME, &cache, &|_, _| {}).unwrap();
        assert!(fs::read(&path).unwrap() == body, "the cache holds what was served");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let again = fetch_checked_from(&base, "0.0.1", NAME, &cache, &|_, _| {}).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1, "an intact cache is used as is");

        let mut damaged = fs::read(&again).unwrap();
        damaged[50_000] ^= 1;
        fs::write(&again, &damaged).unwrap();
        let path = fetch_checked_from(&base, "0.0.1", NAME, &cache, &|_, _| {}).unwrap();
        assert!(fs::read(&path).unwrap() == body, "the cache holds what was served");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_cache_without_a_recorded_digest_is_fetched_again() {
        let body: &'static [u8] = Box::leak(vec![9u8; 1_000].into_boxed_slice());
        let (base, hits) = serve(body);
        let cache = scratch("unrecorded");
        let dir = cache.join("0.0.1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(NAME), b"left over").unwrap();
        let path = fetch_checked_from(&base, "0.0.1", NAME, &cache, &|_, _| {}).unwrap();
        assert!(fs::read(&path).unwrap() == body, "the cache holds what was served");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let _ = fs::remove_dir_all(&cache);
    }
}
