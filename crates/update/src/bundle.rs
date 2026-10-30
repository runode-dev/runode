//! 装到 SSH 远端用的发布包：从 GitHub 上某个版本的 Release 下载给某个平台的包，按同一个 Release 里的
//! `SHA256SUMS` 核对，下过的按版本留在调用方给的缓存目录里，下次直接用。Linux 的包是只装着 `runode` 的
//! tar.gz，原样交出去；mac 的 dmg 挂上后核对 .app 的签名（`TEAM_ID` 的 Developer ID），再取出里面的
//! 可执行文件。

use std::{
    fs::{self, File},
    io::Read as _,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest as _, Sha256};

use crate::Error;

/// 发布包的签名要出自这个 Team。
pub const TEAM_ID: &str = "5VRL2M2A3V";

const RELEASES: &str = "https://github.com/runode-dev/runode/releases/download";
/// 下载一个包最多等这么久。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SUMS_TIMEOUT: Duration = Duration::from_secs(30);

/// `version` 给 Linux `arch`（`x86_64`、`aarch64`）的 tar.gz，核对过的。会阻塞到下完，下载时不断调
/// `progress(已收到的字节, 总字节)`，总字节未知时是 0。
pub fn linux_archive(version: &str, arch: &str, cache: &Path, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, Error> {
    let name = format!("Runode-{version}-linux-{arch}.tar.gz");
    fetch_checked(version, &name, cache, progress)
}

/// `version` 给 mac `arch`（`arm64`、`x86_64`）的 `runode` 可执行文件，取自签名核对过的 .app。只有
/// macOS 上做得到（要挂 dmg、跑 codesign）。
pub fn mac_binary(version: &str, arch: &str, cache: &Path, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, Error> {
    let binary = cache.join(version).join(format!("runode-macos-{arch}"));
    if binary.is_file() {
        return Ok(binary);
    }
    let dmg = fetch_checked(version, &format!("Runode-{version}-macos-{arch}.dmg"), cache, progress)?;
    extract_mac_binary(&dmg, version, &binary)?;
    let _ = fs::remove_file(&dmg);
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
        let part = to.with_extension("part");
        fs::copy(&exe, &part).map_err(|err| io(&exe, err))?;
        fs::rename(&part, to).map_err(|err| io(to, err))
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
    let dir = cache.join(version);
    let path = dir.join(name);
    if path.is_file() {
        return Ok(path);
    }
    fs::create_dir_all(&dir).map_err(|err| io(&dir, err))?;
    let base = format!("{RELEASES}/v{version}");
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
    Ok(path)
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
