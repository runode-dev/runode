//! Linux 的更新：桌面版是一个 AppImage 文件。把新版本的 AppImage 下载到它旁边的隐藏文件里，按同一个版本的
//! Release 里的 `SHA256SUMS` 核对（`Installation::stage`），app 退出时改名换上（`Staged::install`）。改名是原子的，
//! 在跑的进程（连同单独跑的宿主）还挂着旧的镜像，照常跑完，下次启动的是新版本。
//!
//! Linux 上没有代码签名，新包可信只靠经 HTTPS 从 GitHub 取的校验和。`SHA256SUMS` 按版本号从固定的发布地址取，
//! 不跟着清单里的下载地址走，清单被换掉时也换不了校验和。

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    Error, Release,
    bundle::{self, SUMS_TIMEOUT},
};

/// 下载更新包最多等这么久。
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// 装着的、能自己更新的 AppImage。
#[derive(Clone, Debug)]
pub struct Installation {
    image: PathBuf,
    /// 启动时的那个文件（设备号、inode）：退出前文件被换过时不再覆盖。
    identity: (u64, u64),
}

impl Installation {
    /// 在跑的这份 app 的 AppImage 文件 `image` 能自己更新时；不能时说为什么：它所在的目录写不了时换不上。
    pub fn appimage(image: &Path) -> Result<Self, String> {
        let meta = fs::metadata(image).map_err(|err| format!("{}: {err}", image.display()))?;
        let dir = image.parent().ok_or("the AppImage has no directory")?;
        if !writable(dir) {
            return Err(format!("{} is not writable", dir.display()));
        }
        Ok(Self { image: image.to_owned(), identity: (meta.dev(), meta.ino()) })
    }

    /// 把 `release` 的 AppImage 下载到暂存文件、按 `SHA256SUMS` 核对，成了等着 `Staged::install`。会阻塞到下完，
    /// 最多 `ARCHIVE_TIMEOUT`。没成时删掉暂存文件。下载时在这个线程上不断调 `progress(已收到的字节, 总字节)`，
    /// 总字节未知时是 0。
    pub fn stage(&self, release: &Release, progress: &dyn Fn(u64, u64)) -> Result<Staged, Error> {
        let url = release.archive().ok_or(Error::NoArchive)?;
        let name = archive_name(&release.version);
        if url.rsplit('/').next() != Some(name.as_str()) {
            return Err(Error::Rejected(format!("the manifest points at {url}, not {name}")));
        }
        let staged = self.staging_file();
        let result = self.stage_into(&staged, url, &name, &release.version, progress);
        if result.is_err() {
            let _ = fs::remove_file(&staged);
        }
        result
    }

    fn stage_into(
        &self,
        staged: &Path,
        url: &str,
        name: &str,
        version: &str,
        progress: &dyn Fn(u64, u64),
    ) -> Result<Staged, Error> {
        let sums = crate::fetch::get(&format!("{}/v{version}/SHA256SUMS", bundle::RELEASES), SUMS_TIMEOUT, &|_, _| {})?;
        let sum = bundle::expected_sum(&String::from_utf8_lossy(&sums), name)
            .ok_or_else(|| Error::Rejected(format!("{name} is not in SHA256SUMS")))?;
        // 上次下好却没装上（app 没正常退出）留下的先删掉。
        let _ = fs::remove_file(staged);
        crate::fetch::download(url, staged, ARCHIVE_TIMEOUT, progress)?;
        check_sum(staged, &sum)?;
        fs::set_permissions(staged, fs::Permissions::from_mode(0o755)).map_err(|err| io(staged, err))?;
        tracing::info!("staged Runode {version} at {}", staged.display());
        Ok(Staged {
            staged: staged.to_owned(),
            target: self.image.clone(),
            identity: self.identity,
            sum,
            version: version.to_owned(),
        })
    }

    /// 和 AppImage 同一个目录、以点开头的隐藏文件：`.Runode.AppImage.update`。同一个目录里改名才是原子的。
    fn staging_file(&self) -> PathBuf {
        let name = self.image.file_name().map_or("Runode.AppImage".into(), |name| name.to_string_lossy());
        self.image.with_file_name(format!(".{name}.update"))
    }
}

/// 下好、核对过的新版本，等着装上。
#[derive(Debug)]
pub struct Staged {
    staged: PathBuf,
    target: PathBuf,
    identity: (u64, u64),
    sum: String,
    version: String,
}

impl Staged {
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 再核对一次校验和，改名换掉装着的 AppImage。从下好到退出可能隔了几个小时，暂存文件又是用户写得了的，
    /// 所以换上前再核对。这期间装着的文件被换过（用户自己装了别的版本）时不覆盖，删掉暂存文件就算办完。没换上时
    /// 装着的那份不动，暂存文件照样删掉，下次重新下载。
    pub fn install(self) -> Result<(), Error> {
        let current = fs::metadata(&self.target).map(|meta| (meta.dev(), meta.ino()));
        if current.as_ref().ok() != Some(&self.identity) {
            tracing::info!(
                "{} was replaced since Runode started, not installing {}",
                self.target.display(),
                self.version
            );
            let _ = fs::remove_file(&self.staged);
            return Ok(());
        }
        let installed = check_sum(&self.staged, &self.sum)
            .and_then(|()| fs::rename(&self.staged, &self.target).map_err(|err| io(&self.target, err)));
        if installed.is_err() {
            let _ = fs::remove_file(&self.staged);
        }
        installed?;
        tracing::info!("installed Runode {} at {}", self.version, self.target.display());
        Ok(())
    }
}

/// 版本 `version` 给这台机器的架构的 AppImage 在 Release 里的名字。
fn archive_name(version: &str) -> String {
    format!("Runode-{version}-linux-{}.AppImage", std::env::consts::ARCH)
}

fn check_sum(path: &Path, expected: &str) -> Result<(), Error> {
    let actual = bundle::sha256(path).map_err(|err| io(path, err))?;
    if actual == expected {
        Ok(())
    } else {
        Err(Error::Rejected(format!("{} does not match SHA256SUMS", path.display())))
    }
}

/// 目录 `dir` 里能建文件：试着建一个再删掉。
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".runode-write-check-{}", std::process::id()));
    let created = fs::File::create_new(&probe).is_ok();
    if created {
        let _ = fs::remove_file(&probe);
    }
    created
}

fn io(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的空目录，测完删掉。
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("runode-update-linux-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn staged(scratch: &Scratch, contents: &[u8]) -> (Installation, Staged) {
        let image = scratch.0.join("Runode.AppImage");
        fs::write(&image, b"old").unwrap();
        let installation = Installation::appimage(&image).unwrap();
        let file = installation.staging_file();
        fs::write(&file, contents).unwrap();
        let sum = bundle::sha256(&file).unwrap();
        let staged =
            Staged { staged: file, target: image, identity: installation.identity, sum, version: "9.9.9".into() };
        (installation, staged)
    }

    #[test]
    fn installing_replaces_the_appimage() {
        let scratch = Scratch::new("replace");
        let (installation, staged) = staged(&scratch, b"new");
        let file = staged.staged.clone();
        staged.install().unwrap();
        assert_eq!(fs::read(&installation.image).unwrap(), b"new");
        assert!(!file.exists());
    }

    /// 下好以后暂存文件被改过时不换上。
    #[test]
    fn a_tampered_download_is_not_installed() {
        let scratch = Scratch::new("tampered");
        let (installation, staged) = staged(&scratch, b"new");
        fs::write(&staged.staged, b"evil").unwrap();
        let file = staged.staged.clone();
        assert!(matches!(staged.install(), Err(Error::Rejected(_))));
        assert_eq!(fs::read(&installation.image).unwrap(), b"old");
        assert!(!file.exists());
    }

    /// 启动以后 AppImage 被换成了别的文件（用户自己装了别的版本）时不覆盖。
    #[test]
    fn a_replaced_appimage_is_left_alone() {
        let scratch = Scratch::new("replaced");
        let (installation, staged) = staged(&scratch, b"new");
        let other = scratch.0.join("other");
        fs::write(&other, b"mine").unwrap();
        fs::rename(&other, &installation.image).unwrap();
        let file = staged.staged.clone();
        staged.install().unwrap();
        assert_eq!(fs::read(&installation.image).unwrap(), b"mine");
        assert!(!file.exists());
    }

    /// 清单里的下载地址不是这个版本、这个架构的 AppImage 时不下载。
    #[test]
    fn a_manifest_pointing_elsewhere_is_rejected() {
        let scratch = Scratch::new("elsewhere");
        let image = scratch.0.join("Runode.AppImage");
        fs::write(&image, b"old").unwrap();
        let installation = Installation::appimage(&image).unwrap();
        let release = Release {
            version: "9.9.9".into(),
            page: "https://example.com".into(),
            archives: [(crate::arch().to_owned(), "https://example.com/evil.AppImage".to_owned())].into(),
        };
        assert!(matches!(installation.stage(&release, &|_, _| {}), Err(Error::Rejected(_))));
    }
}
