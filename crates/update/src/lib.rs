//! 桌面 app 的自动更新：从 GitHub 上最新的 Release 读版本清单（`latest.json`，见 `Release`），
//! 比版本号，把这台 Mac 的架构对应的 zip 下载、解压到装着的 Runode.app 旁边的暂存目录里，核对
//! 签名后留着（`Installation::stage`），等 app 退出时和装着的那份原子地对调（`Staged::install`）。
//! 正在跑的进程不受影响，下次启动就是新版本。
//!
//! 只有用 Developer ID 签过名的 .app 才自己更新：新包也要是 Developer ID Application 证书签的，
//! 和在跑的这份出自同一个 Team ID、是同一个 bundle id（`codesign --verify -R`），这就认定了是同一个
//! 发布者的构建，不另做签名。下好时核对一次，退出时装上前再核对一次。
//! 自己打包的（ad-hoc 签名）、没打包成 .app 的和放在写不了的位置（dmg 里、被系统隔离转移到只读
//! 目录）的都不更新，见 `Installation::current`。
//!
//! 下载经 NSURLSession，跟着系统的代理设置和证书。app 自己下载的文件不带隔离属性，换上去以后
//! Gatekeeper 也不会再拦一次。

mod fetch;
mod install;

use std::{cmp::Ordering, collections::BTreeMap, fmt, time::Duration};

use serde::Deserialize;

pub use install::{Installation, Staged};

/// 最新版本的清单：GitHub 上「最新的 Release」（不含预发布）里的 `latest.json`，由发版的 workflow
/// 生成。经这个固定的下载地址取，不经 GitHub 的 API，没有未登录时每小时 60 次的限制。
pub const MANIFEST_URL: &str = "https://github.com/runode-dev/runode/releases/latest/download/latest.json";
/// 新包的签名里要是这个 bundle id，和桌面 app 的 `CFBundleIdentifier` 一样（桌面的测试对着两边）。
pub const BUNDLE_ID: &str = "dev.runode.app";

/// 取清单最多等这么久。
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 一个发布的版本，就是 `latest.json` 的格式。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Release {
    /// 版本号，比如 `0.2.0`，和新包的 `CFBundleShortVersionString` 一样。
    pub version: String,
    /// Release 的网页：更新说明、手动下载。
    pub page: String,
    /// 各架构的更新包（zip），按 `lipo -archs` 的架构名（`arm64`、`x86_64`）。
    #[serde(default)]
    pub archives: BTreeMap<String, String>,
}

impl Release {
    /// 读清单。版本号不是用点隔开的数字时算读不懂。
    pub fn parse(json: &[u8]) -> Result<Self, Error> {
        let release: Self = serde_json::from_slice(json).map_err(|err| Error::Manifest(err.to_string()))?;
        if version_parts(&release.version).is_none() {
            return Err(Error::Manifest(format!("{:?} is not a version number", release.version)));
        }
        Ok(release)
    }

    /// 这台 Mac 的架构（见 `arch`）的更新包；没有时只能手动下载。
    pub fn archive(&self) -> Option<&str> {
        self.archives.get(arch()).map(String::as_str)
    }
}

/// 取最新版本的清单。会阻塞到取到或者出错，最多 `MANIFEST_TIMEOUT`。
pub fn latest() -> Result<Release, Error> {
    Release::parse(&fetch::get(MANIFEST_URL, MANIFEST_TIMEOUT, &|_, _| {})?)
}

/// 这台 Mac 的架构，按清单（`lipo -archs`）的叫法。
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

/// `candidate` 比 `current` 新：按点隔开的各段数字比，缺的段算 0（`0.2` 和 `0.2.0` 一样）。有一边
/// 不是这种写法时不算新。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(candidate), Some(current)) = (version_parts(candidate), version_parts(current)) else {
        return false;
    };
    let len = candidate.len().max(current.len());
    let part = |parts: &[u64], i: usize| parts.get(i).copied().unwrap_or(0);
    (0..len).map(|i| part(&candidate, i).cmp(&part(&current, i))).find(|order| order.is_ne()) == Some(Ordering::Greater)
}

fn version_parts(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// 查、下载或者核对更新时出的错。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// 连不上、超时，或者 HTTP 状态不是 2xx。
    Network(String),
    /// 清单读不懂。
    Manifest(String),
    /// 清单里没有这台 Mac 的架构的更新包。
    NoArchive,
    /// 下载的包签名不对，或者版本和清单对不上。
    Rejected(String),
    /// 读写文件、跑 ditto 或 codesign 出错。
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(reason) => write!(f, "network: {reason}"),
            Self::Manifest(reason) => write!(f, "unreadable release manifest: {reason}"),
            Self::NoArchive => write!(f, "no update for {} in the release", arch()),
            Self::Rejected(reason) => write!(f, "the downloaded update was rejected: {reason}"),
            Self::Io(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for Error {}
