//! Windows 的更新：把 setup.exe 下载到装着的 runode.exe 旁边的暂存目录，核对签名后留着
//! （`Installation::stage`），app 退出时静默运行它（`Staged::install`），由安装程序关掉还在跑的
//! 进程、换上新文件。宿主在 Windows 上不交接会话，所以换上新版本时终端会话随之结束。重启以更新时
//! 由安装程序装完后打开新版本，不由 app 自己重启：自己重启会赶在安装程序换文件之前把旧版本打开。
//!
//! 签名核对：新包要有 Authenticode 签名，签名者证书的指纹和在跑的 runode.exe 一样，版本号和
//! 清单一致；在跑的 runode.exe 自己没签名、不是安装程序装的（旁边没有 unins000.exe）时不更新。

use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::{Error, Release, authenticode};

/// 下载安装包最多等这么久。
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SETUP_NAME: &str = "Runode-setup.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 装着的、能自己更新的 runode.exe。
#[derive(Clone, Debug)]
pub struct Installation {
    exe: PathBuf,
    /// 它的签名者证书指纹，新包要和它一样。
    thumbprint: String,
}

impl Installation {
    /// 在跑的这份 app，能自己更新时；不能时说为什么。
    pub fn current() -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        if !exe.with_file_name("unins000.exe").is_file() {
            return Err("not installed by the setup program".into());
        }
        let signed = signature(&exe).map_err(|err| format!("the app is not signed: {err}"))?;
        Ok(Self { exe, thumbprint: signed.thumbprint })
    }

    /// 把 `release` 的安装包下载到暂存目录并核对。会阻塞到下完，最多 `ARCHIVE_TIMEOUT`。没成时删掉
    /// 暂存目录。下载期间不断调 `progress(已收到的字节, 总字节)`，总字节未知（0）。
    pub fn stage(&self, release: &Release, progress: &dyn Fn(u64, u64)) -> Result<Staged, Error> {
        let url = release.archive().ok_or(Error::NoArchive)?;
        let dir = self.exe.with_file_name(".update");
        let staged = self.stage_into(&dir, url, &release.version, progress);
        if staged.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        staged
    }

    fn stage_into(&self, dir: &Path, url: &str, version: &str, progress: &dyn Fn(u64, u64)) -> Result<Staged, Error> {
        // 上次下好却没装上留下的先删掉。
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|err| io(dir, err))?;
        }
        fs::create_dir(dir).map_err(|err| io(dir, err))?;
        let setup = dir.join(SETUP_NAME);
        crate::fetch::download(url, &setup, ARCHIVE_TIMEOUT, progress)?;
        verify(&setup, &self.thumbprint, version)?;
        tracing::info!("staged Runode {version} at {}", setup.display());
        Ok(Staged {
            dir: dir.to_owned(),
            exe: self.exe.clone(),
            setup,
            thumbprint: self.thumbprint.clone(),
            version: version.to_owned(),
        })
    }
}

/// 下好、核对过的新版本，等着装上。
#[derive(Debug)]
pub struct Staged {
    dir: PathBuf,
    /// 装着的 runode.exe，没起安装程序又要重新打开时用。
    exe: PathBuf,
    setup: PathBuf,
    thumbprint: String,
    version: String,
}

impl Staged {
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 再核对一次，静默运行安装程序后立即返回，不等它：安装程序要等这个进程退出才能换文件。从下好到
    /// 退出可能隔了几个小时，暂存目录又是用户写得了的，所以装上前再核对。已经装着不比它旧的版本时
    /// 删掉暂存目录就算办完。没装上时暂存目录照样删掉，下次重新下载；装上时安装程序还在用这个目录，
    /// 留到下次下载前清理。
    ///
    /// `relaunch` 时装完由安装程序打开新版本；没起安装程序（不用装、核对不过、起不来）时打开装着的
    /// 这份，用户要的重启照样办到。不 `relaunch` 时安装程序装完不打开。还在跑的 runode.exe（包括单独跑的
    /// 宿主）由安装程序强行关掉：无窗口的进程 Restart Manager 礼貌地关不掉，关不掉就换不了文件。
    pub fn install(self, relaunch: bool) -> Result<(), Error> {
        let started = self.start_installer(relaunch);
        if relaunch && !matches!(started, Ok(true)) {
            reopen(&self.exe);
        }
        started.map(|_| ())
    }

    /// 起了安装程序时为 `true`；不用装时为 `false`。
    fn start_installer(&self, relaunch: bool) -> Result<bool, Error> {
        if !crate::is_newer(&self.version, env!("CARGO_PKG_VERSION")) {
            let _ = fs::remove_dir_all(&self.dir);
            return Ok(false);
        }
        if let Err(err) = verify(&self.setup, &self.thumbprint, &self.version) {
            let _ = fs::remove_dir_all(&self.dir);
            return Err(err);
        }
        Command::new(&self.setup)
            .args(installer_args(relaunch))
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|err| io(&self.setup, err))?;
        tracing::info!("started the installer for Runode {}", self.version);
        Ok(true)
    }
}

/// 静默运行安装程序的参数：总是强行关掉还在跑的 runode.exe；`relaunch` 时不带 `/NORUN`，安装脚本的
/// `LaunchAfterSilentInstall` 装完打开新版本。
fn installer_args(relaunch: bool) -> Vec<&'static str> {
    let mut args = vec!["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/FORCECLOSEAPPLICATIONS"];
    if !relaunch {
        args.push("/NORUN");
    }
    args
}

/// 打开 `exe`，不等它。
fn reopen(exe: &Path) {
    match Command::new(exe).creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn() {
        Ok(_) => tracing::info!("reopened {}", exe.display()),
        Err(err) => tracing::error!("failed to reopen {}: {err}", exe.display()),
    }
}

/// `file` 的签名者证书指纹等于 `thumbprint`，版本号是 `version`。
fn verify(file: &Path, thumbprint: &str, version: &str) -> Result<(), Error> {
    let signed = signature(file).map_err(Error::Rejected)?;
    if !signed.thumbprint.eq_ignore_ascii_case(thumbprint) {
        return Err(Error::Rejected("the update is signed by a different certificate".into()));
    }
    // 安装程序的产品版本写成四段（0.2.0.0），和清单的三段按数字比，缺的段算 0。
    // 版本号读不出来时 `is_newer` 两个方向都是假，不能当成一致放过。
    let unreadable = crate::version_parts(&signed.version).is_none();
    if unreadable || crate::is_newer(&signed.version, version) || crate::is_newer(version, &signed.version) {
        return Err(Error::Rejected(format!("the update is version {}, the release says {version}", signed.version)));
    }
    Ok(())
}

fn signature(file: &Path) -> Result<authenticode::Signed, String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", authenticode::SCRIPT])
        .env("RUNODE_FILE", file)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| format!("powershell.exe: {err}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    authenticode::check(&String::from_utf8_lossy(&out.stdout))
}

fn io(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 安装程序总是静默地强行关掉还在跑的 runode.exe；只有重启以更新时装完打开新版本。
    #[test]
    fn the_installer_opens_the_app_only_when_relaunching() {
        let quit = installer_args(false);
        let restart = installer_args(true);
        for args in [&quit, &restart] {
            assert!(args.contains(&"/VERYSILENT") && args.contains(&"/FORCECLOSEAPPLICATIONS"), "{args:?}");
        }
        assert!(quit.contains(&"/NORUN"));
        assert!(!restart.contains(&"/NORUN"));
    }
}
