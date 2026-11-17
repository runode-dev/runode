//! 把这个构建的 runode 装到远端：远端的平台由连接脚本探出来（`runode_ssh::bootstrap::Platform`），要装的
//! 可执行文件按先后从这几处取：环境变量 `RUNODE_SSH_BINARY_<平台>` 或 `RUNODE_SSH_BINARY` 指的文件（开发构建
//! 没有发布包时用），远端和本机平台一样时用这个可执行文件本身，别的从发布页下载（`runode_update::bundle`）。
//! 经 ssh 的 stdin 传上去，远端核对过、试跑过才换上。Windows 远端装的是 zip（`runode.exe` 要带着 `rn.exe` 和
//! 终端要的文件），环境变量指的也得是这样的 zip。

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
    bootstrap::{Platform, Target, install_command, install_dir_name, parse_exec_failure, windows},
    process::Pipes,
};

use super::{Failure, Phase, RemoteHost, Status};

/// 开发时指定要装到远端的可执行文件，按平台的（`RUNODE_SSH_BINARY_LINUX_X86_64` 这样）先于不分平台的。
const ENV_BINARY: &str = "RUNODE_SSH_BINARY";

/// 要装上去的东西。
struct Source {
    path: PathBuf,
    /// 是只装着 `runode` 的 tar.gz；装到 Windows 上的一律是 zip。
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
    let unreadable = |err: io::Error| Failure::Retry(format!("cannot read {}: {err}", source.path.display()));
    let sums = if target.is_windows() {
        Sums::Files(zip_sums(&source.path).map_err(unreadable)?)
    } else {
        Sums::Whole(sha256(&source.path).map_err(unreadable)?)
    };
    // Windows 上每传一次另起临时目录（`part_name`）。
    let script = || match &sums {
        Sums::Files(sums) => windows::install_command(dir, &part_name(), sums).map_err(|err| Failure::Stop {
            error: format!("cannot install {} on the remote: {err}", source.path.display()),
            auth: false,
        }),
        Sums::Whole(sha) => Ok(install_command(dir, sha, source.archive)),
    };
    let mut uploads = 0;
    loop {
        uploads += 1;
        match upload(host, ssh, &source.path, &script()?, dir, log, interactive) {
            Err(Upload::Damaged) => match damaged(target, dir, uploads) {
                Some(failure) => return Err(failure),
                None => tracing::warn!("runode {dir} did not check out on {}, uploading it again", host.name),
            },
            Err(Upload::Failed(failure)) => return Err(failure),
            Ok(()) => return Ok(()),
        }
    }
}

/// 远端照着核对的摘要。
enum Sums {
    /// zip 里各个文件的（`zip_sums`），装到 Windows 上时。
    Files(Vec<(String, String)>),
    /// 整个文件的。
    Whole(String),
}

/// `upload` 没装上的原因。
enum Upload {
    /// 远端核对摘要没过（`RUNODE-INSTALL checksum`）。
    Damaged,
    Failed(Failure),
}

impl From<Failure> for Upload {
    fn from(failure: Failure) -> Self {
        Upload::Failed(failure)
    }
}

/// Windows 远端核对摘要没过时最多传这么多次。
const WINDOWS_UPLOADS: u32 = 2;

/// 远端核对摘要没过、这已是第 `uploads` 次传时怎么办；`None` 是再传一次。Windows 上传的清单是本机从核对过的
/// zip 算的，再传仍对不上多半是远端的 PowerShell 跑不了核对的脚本，接着重传也一样，所以传满 `WINDOWS_UPLOADS`
/// 次就停下等用户。
fn damaged(target: Target, dir: &str, uploads: u32) -> Option<Failure> {
    if !target.is_windows() {
        return Some(Failure::Retry("the upload arrived damaged".into()));
    }
    (uploads >= WINDOWS_UPLOADS).then(|| Failure::Stop {
        error: format!(
            "runode {dir} did not check out on the remote after {uploads} uploads: the upload may be damaged, or \
             PowerShell there may be unable to run scripts or compute file hashes"
        ),
        auth: false,
    })
}

/// 经 ssh 跑安装脚本 `script`，把 `path` 从 stdin 传上去，装成 `dir`。
fn upload(
    host: &RemoteHost,
    ssh: &Ssh,
    path: &Path,
    script: &str,
    dir: &str,
    log: &Path,
    interactive: bool,
) -> Result<(), Upload> {
    let total = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    tracing::info!("installing {} on {} as {dir} ({total} bytes)", path.display(), host.name);
    host.set_status(Status::Connecting(Phase::Uploading { done: 0, total }));
    let mut command = host.exec(ssh, script, interactive, true);
    let stderr = File::create(log).map_err(|err| Failure::Retry(format!("cannot write the ssh log: {err}")))?;
    command.stdout(Stdio::null()).stderr(stderr);
    crate::subprocess::isolate(&mut command);
    let pipes = Pipes { stdin: true, ..Pipes::default() };
    let piped = runode_ssh::process::spawn(&mut command, pipes)
        .map_err(|err| Failure::Retry(format!("cannot run ssh: {err}")))?;
    let mut child = piped.child;
    let pid = child.id();
    if let Err(failure) = host.track(pid) {
        let _ = child.wait();
        return Err(failure.into());
    }
    let sent = piped.stdin.ok_or_else(|| io::Error::other("ssh has no stdin")).and_then(|mut stdin| {
        let mut file = File::open(path)?;
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
    let status = child.wait();
    host.untrack(pid);
    let status = status.map_err(|err| Failure::Retry(err.to_string()))?;
    // 试跑的报错按字节截断，可能切在多字节字符中间，不能整个按 UTF-8 读。
    let stderr = String::from_utf8_lossy(&std::fs::read(log).unwrap_or_default()).into_owned();
    if stderr.contains("RUNODE-INSTALL checksum") {
        return Err(Upload::Damaged);
    }
    if stderr.contains("RUNODE-INSTALL busy") {
        return Err(Upload::Failed(Failure::Stop {
            error: format!(
                "runode {dir} on the remote is damaged but still running: end its processes there and connect again"
            ),
            auth: false,
        }));
    }
    if let Some(reason) = parse_exec_failure(&stderr) {
        const NOEXEC: &str = "its home directory may be mounted noexec";
        let error = if reason.is_empty() {
            format!("runode cannot run on the remote ({NOEXEC})")
        } else if reason.contains("Permission denied") {
            format!("runode cannot run on the remote ({NOEXEC}): {reason}")
        } else {
            format!("runode cannot run on the remote: {reason}")
        };
        return Err(Failure::Stop { error, auth: false }.into());
    }
    if let Err(err) = sent {
        return Err(Failure::Retry(format!("the upload was cut off: {err}")).into());
    }
    if !status.success() {
        return Err(super::classify(log, status.code()).into());
    }
    Ok(())
}

/// 要装到 `target` 上的可执行文件从哪来。
fn source(host: &RemoteHost, target: Target) -> Result<Source, Failure> {
    // 不分平台的那个指的是可执行文件，装不到 Windows 上。
    let generic = (!target.is_windows()).then(|| ENV_BINARY.to_owned());
    for key in std::iter::once(format!("{ENV_BINARY}_{}", target.env_name())).chain(generic) {
        if let Some(path) = std::env::var_os(&key).map(PathBuf::from) {
            return Ok(Source { path, archive: false });
        }
    }
    // Linux 桌面版链接着图形库，在无头的服务器上起不来；Windows 上要装的是 zip，不是这个可执行文件。
    let headless = target.is_mac() || !cfg!(feature = "linux-desktop");
    if Target::current() == Some(target) && headless && !target.is_windows() {
        let exe = std::env::current_exe().map_err(|err| Failure::Retry(err.to_string()))?;
        return Ok(Source { path: exe, archive: false });
    }
    // 发布页上同版本号的包是另一份代码，认不得这个构建的桥接参数，装上去只会一直重连。
    // shortcut: 只拦调试构建，本地打的发布构建照样下载；发布包能报出提交号、装前核对时再改成按构建号拦。
    if cfg!(debug_assertions) {
        let kind = if target.is_windows() { "a zip of a build" } else { "a build" };
        return Err(Failure::Stop {
            error: format!(
                "a development build cannot download runode for {}: set {ENV_BINARY}_{} to {kind} for it",
                target.env_name().to_lowercase(),
                target.env_name()
            ),
            auth: false,
        });
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
    } else if target.is_windows() {
        runode_update::bundle::windows_archive(version, target.arch(), &cache, &progress).map(|path| (path, true))
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

/// Windows 上装到一半的临时目录名，每次不同：同时连着的两个桌面各装各的。
fn part_name() -> String {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.subsec_nanos());
    format!(".part-{}-{nanos:x}", std::process::id())
}

/// 传了百分之几；总数不知道时按已收到的兆字节算。
fn percent(done: u64, total: u64) -> u64 {
    (done * 100).checked_div(total).unwrap_or(done >> 20)
}

/// zip 里各个文件的名字和 SHA-256（十六进制），交给远端写成清单、照着核对；读的时候按每个文件的 CRC 核对。
fn zip_sums(path: &Path) -> io::Result<Vec<(String, String)>> {
    use sha2::{Digest as _, Sha256};

    let mut archive = zip::ZipArchive::new(File::open(path)?).map_err(io::Error::other)?;
    let mut sums = Vec::with_capacity(archive.len());
    let mut buffer = vec![0; 1 << 20];
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io::Error::other)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(io::Error::other)?.into_owned();
        let mut hasher = Sha256::new();
        loop {
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        let hex = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        sums.push((name, hex));
    }
    Ok(sums)
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use sha2::{Digest as _, Sha256};
    use zip::{CompressionMethod, write::SimpleFileOptions};

    use runode_ssh::bootstrap::Target;

    use super::{Failure, WINDOWS_UPLOADS, damaged, zip_sums};

    /// 装到 Windows 上核对没过时再传一次，还不过就停下等用户、点出远端的 PowerShell；别的平台照旧过一会儿重连。
    #[test]
    fn windows_stops_after_uploads_that_do_not_check_out() {
        assert!(damaged(Target::WindowsX86_64, "0.1.16", 1).is_none());
        match damaged(Target::WindowsX86_64, "0.1.16", WINDOWS_UPLOADS) {
            Some(Failure::Stop { error, auth: false }) => assert!(error.contains("PowerShell"), "{error}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(damaged(Target::LinuxX86_64, "0.1.16", 1), Some(Failure::Retry(_))));
    }

    fn hex(data: &[u8]) -> String {
        Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// 压缩过的和原样存的文件都按解出来的内容算摘要；包里的内容坏了（CRC 对不上）时不交出摘要。
    #[test]
    fn sums_the_unpacked_files_of_a_zip() {
        let dir = std::env::temp_dir().join(format!("runode-zip-sums-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runode.zip");
        let app: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("runode.exe", deflated).unwrap();
        writer.write_all(&app).unwrap();
        writer
            .start_file("rn.exe", SimpleFileOptions::default().compression_method(CompressionMethod::Stored))
            .unwrap();
        writer.write_all(b"launcher").unwrap();
        writer.finish().unwrap();

        let sums = zip_sums(&path).unwrap();
        assert_eq!(sums, [("runode.exe".to_owned(), hex(&app)), ("rn.exe".to_owned(), hex(b"launcher"))]);
        assert!(runode_ssh::bootstrap::windows::install_command("0.1.16", ".part-1", &sums).is_ok());

        // 改掉原样存的 rn.exe 的一个字节：读到头时 CRC 对不上。
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes.windows(8).position(|window| window == b"launcher").unwrap();
        bytes[at] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(zip_sums(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
