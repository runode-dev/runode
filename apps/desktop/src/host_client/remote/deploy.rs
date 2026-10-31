//! 把这个构建的 runode 装到远端：远端的平台由连接脚本探出来（`runode_ssh::bootstrap::Platform`），要装的
//! 可执行文件按先后从这几处取：环境变量 `RUNODE_SSH_BINARY_<平台>` 或 `RUNODE_SSH_BINARY` 指的文件（开发构建
//! 没有发布包时用），远端和本机平台一样时用这个可执行文件本身，别的从发布页下载（`runode_update::bundle`）。
//! 经 ssh 的 stdin 传上去，远端核对 SHA-256、试跑过才换上。

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{LazyLock, Mutex, PoisonError},
    time::SystemTime,
};

use runode_ssh::{
    Ssh,
    bootstrap::{Platform, Target, install_command, install_dir_name},
    process::Pipes,
};

use super::{Failure, Phase, RemoteHost, Status};

/// 开发时指定要装到远端的可执行文件，按平台的（`RUNODE_SSH_BINARY_LINUX_X86_64` 这样）先于不分平台的。
const ENV_BINARY: &str = "RUNODE_SSH_BINARY";

/// 要装上去的东西。
struct Source {
    path: PathBuf,
    /// 是只装着 `runode` 的 tar.gz。
    archive: bool,
}

/// 远端装这个构建的目录名。开发时同一个构建号可能是不同的可执行文件，按文件的大小和修改时间另起名字。
pub(crate) fn install_dir() -> String {
    let build = super::build().0;
    let dev = std::env::var_os(ENV_BINARY)
        .map(PathBuf::from)
        .or_else(|| cfg!(debug_assertions).then(|| std::env::current_exe().ok()).flatten());
    match dev.and_then(|path| identity(&path)) {
        Some((len, modified)) => install_dir_name(&format!("{build}-dev-{len:x}-{modified:x}")),
        None => install_dir_name(&build),
    }
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    Some((meta.len(), modified))
}

/// 把 runode 装到 `platform` 上，装成 `dir`；`interactive` 是要认证时能不能问用户（`RemoteHost::exec`）。
pub(super) fn install(
    host: &RemoteHost,
    ssh: &Ssh,
    platform: &Platform,
    dir: &str,
    log: &Path,
    interactive: bool,
) -> Result<(), Failure> {
    let target = platform.target().map_err(|err| Failure::Stop { error: err.to_string(), auth: false })?;
    let source = source(host, target)?;
    let sha =
        sha256(&source.path).map_err(|err| Failure::Retry(format!("cannot read {}: {err}", source.path.display())))?;
    let total = std::fs::metadata(&source.path).map(|meta| meta.len()).unwrap_or(0);
    tracing::info!("installing {} on {} as {dir} ({total} bytes)", source.path.display(), host.name);
    host.set_status(Status::Connecting(Phase::Uploading { done: 0, total }));
    let mut command = host.exec(ssh, &install_command(dir, &sha, source.archive), interactive, true);
    let stderr = File::create(log).map_err(|err| Failure::Retry(format!("cannot write the ssh log: {err}")))?;
    command.stdout(Stdio::null()).stderr(stderr);
    crate::subprocess::isolate(&mut command);
    let pipes = Pipes { stdin: true, ..Pipes::default() };
    let piped = runode_ssh::process::spawn(&mut command, pipes)
        .map_err(|err| Failure::Retry(format!("cannot run ssh: {err}")))?;
    let mut child = piped.child;
    let sent = piped.stdin.ok_or_else(|| io::Error::other("ssh has no stdin")).and_then(|mut stdin| {
        let mut file = File::open(&source.path)?;
        let mut buffer = vec![0; 1 << 20];
        let mut done = 0;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                return stdin.flush();
            }
            stdin.write_all(&buffer[..n])?;
            let before = percent(done, total);
            done += n as u64;
            // 界面只显示百分比，变了才报。
            if percent(done, total) != before {
                host.set_status(Status::Connecting(Phase::Uploading { done, total }));
            }
        }
    });
    let status = child.wait().map_err(|err| Failure::Retry(err.to_string()))?;
    let stderr = std::fs::read_to_string(log).unwrap_or_default();
    if stderr.contains("RUNODE-INSTALL checksum") {
        return Err(Failure::Retry("the upload arrived damaged".into()));
    }
    if stderr.contains("RUNODE-INSTALL exec") {
        return Err(Failure::Stop {
            error: "runode cannot run on the remote (its home directory may be mounted noexec)".into(),
            auth: false,
        });
    }
    if let Err(err) = sent {
        return Err(Failure::Retry(format!("the upload was cut off: {err}")));
    }
    if !status.success() {
        return Err(super::classify(log, status.code()));
    }
    Ok(())
}

/// 要装到 `target` 上的可执行文件从哪来。
fn source(host: &RemoteHost, target: Target) -> Result<Source, Failure> {
    for key in [format!("{ENV_BINARY}_{}", target.env_name()), ENV_BINARY.to_owned()] {
        if let Some(path) = std::env::var_os(&key).map(PathBuf::from) {
            return Ok(Source { path, archive: false });
        }
    }
    // Linux 桌面版链接着图形库，在无头的服务器上起不来。
    let headless = target.is_mac() || !cfg!(feature = "linux-desktop");
    if Target::current() == Some(target) && headless {
        let exe = std::env::current_exe().map_err(|err| Failure::Retry(err.to_string()))?;
        return Ok(Source { path: exe, archive: false });
    }
    download(host, target)
}

/// 从发布页下载这个版本给 `target` 的包，核对过才用，下过的留在 `Dirs::remote_bin_cache_dir`。
fn download(host: &RemoteHost, target: Target) -> Result<Source, Failure> {
    let version = env!("CARGO_PKG_VERSION");
    let dirs = runode_paths::Dirs::from_env();
    let cache = dirs.remote_bin_cache_dir().ok_or_else(|| Failure::Retry("no cache directory".into()))?;
    let reported = Mutex::new(None);
    let progress = |done, total| {
        // 界面只显示百分比，变了才报。
        let mut reported = reported.lock().unwrap_or_else(PoisonError::into_inner);
        if reported.replace(percent(done, total)) != Some(percent(done, total)) {
            host.set_status(Status::Connecting(Phase::Downloading { done, total }));
        }
    };
    let fetched = if target.is_mac() {
        runode_update::bundle::mac_binary(version, target.arch(), &cache, &progress).map(|path| (path, false))
    } else {
        runode_update::bundle::linux_archive(version, target.arch(), &cache, &progress).map(|path| (path, true))
    };
    match fetched {
        Ok((path, archive)) => Ok(Source { path, archive }),
        Err(err) => Err(Failure::Stop {
            error: format!("cannot get runode {version} for {}: {err}", target.env_name().to_lowercase()),
            auth: false,
        }),
    }
}

/// 传了百分之几；总数不知道时按已收到的兆字节算。
fn percent(done: u64, total: u64) -> u64 {
    (done * 100).checked_div(total).unwrap_or(done >> 20)
}

/// 文件的 SHA-256，十六进制；同一个文件（大小、修改时间不变）只算一次。
fn sha256(path: &Path) -> io::Result<String> {
    /// 路径、大小、修改时间。
    type Identity = (PathBuf, u64, u64);
    static KNOWN: LazyLock<Mutex<HashMap<Identity, String>>> = LazyLock::new(Mutex::default);
    let key = identity(path).map(|(len, modified)| (path.to_path_buf(), len, modified));
    if let Some(key) = &key
        && let Some(known) = KNOWN.lock().unwrap_or_else(PoisonError::into_inner).get(key)
    {
        return Ok(known.clone());
    }
    let hex = runode_update::bundle::sha256(path)?;
    if let Some(key) = key {
        KNOWN.lock().unwrap_or_else(PoisonError::into_inner).insert(key, hex.clone());
    }
    Ok(hex)
}
