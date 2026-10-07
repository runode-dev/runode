//! 装着的 .app 能不能自己更新（`Installation::current`）、把新版本下载到它旁边的暂存目录并核对
//! 签名（`Installation::stage`），以及和装着的那份对调（`Staged::install`）。
//!
//! 暂存目录和 .app 在同一个目录里（`/Applications/.Runode.app.update`），对调才能用一次
//! `renamex_np(RENAME_SWAP)` 原子地做完：不会有哪一刻 .app 不在，退出时也只要几毫秒。

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::{BUNDLE_ID, Error, Release};

/// 下载更新包最多等这么久。
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// 装着的、能自己更新的 .app。
#[derive(Clone, Debug)]
pub struct Installation {
    /// 在跑的这份 .app。
    app: PathBuf,
    /// 它的签名的 Team ID，新包要和它一样。
    team: String,
}

impl Installation {
    /// 在跑的这份 app，能自己更新时；不能时说为什么：不是从 .app 里跑的（比如 `cargo run`），.app
    /// 没有 Developer ID 签名（自己打包的），或者 .app、它所在的目录写不了（在 dmg 里、被系统隔离
    /// 转移到只读目录，或者没有权限）。
    pub fn current() -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        let app = bundle_of(&exe).ok_or("not running from an app bundle")?;
        // 自己也要是 Developer ID 签的：开发证书签的（同一个 Team 也算）不更新，免得被同样签名的包换掉。
        let team = team_of(&app)
            .filter(|team| run(&mut satisfies(&app, &requirement(team))).is_ok())
            .ok_or("the app is not signed with a Developer ID")?;
        // 对调要在所在的目录里建删条目；.app 换到别的目录下时它自己的 `..` 也要改，所以两个都要写得了。
        for dir in [app.parent().unwrap_or(Path::new("/")), &app] {
            if !writable(dir) {
                return Err(format!("{} is not writable", dir.display()));
            }
        }
        Ok(Self { app, team })
    }

    /// 把 `release` 下载到暂存目录、解压、核对签名和版本号，成了等着 `Staged::install`。会阻塞到
    /// 下完，最多 `ARCHIVE_TIMEOUT`。没成时删掉暂存目录。
    pub fn stage(&self, release: &Release) -> Result<Staged, Error> {
        let url = release.archive().ok_or(Error::NoArchive)?;
        let dir = self.staging_dir();
        let staged = self.stage_into(&dir, url, &release.version);
        if staged.is_err() {
            let _ = fs::remove_dir_all(&dir);
        }
        staged
    }

    fn stage_into(&self, dir: &Path, url: &str, version: &str) -> Result<Staged, Error> {
        // 上次下好却没装上（app 没正常退出、对调失败）留下的先删掉。
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|err| io(dir, err))?;
        }
        fs::create_dir(dir).map_err(|err| io(dir, err))?;
        let zip = dir.join("update.zip");
        let body = crate::fetch::get(url, ARCHIVE_TIMEOUT)?;
        fs::write(&zip, body).map_err(|err| io(&zip, err))?;
        // ditto 解压时保留符号链接（.app 里的 rn）和扩展属性，签名才对得上。
        run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&zip).arg(dir))?;
        fs::remove_file(&zip).map_err(|err| io(&zip, err))?;
        let new_app = only_app_in(dir)?;
        verify(&new_app, &self.team, version)?;
        tracing::info!("staged Runode {version} at {}", new_app.display());
        Ok(Staged {
            dir: dir.to_owned(),
            new_app,
            target: self.app.clone(),
            team: self.team.clone(),
            version: version.to_owned(),
        })
    }

    /// 和 .app 同一个目录、以点开头的隐藏目录：`/Applications/.Runode.app.update`。
    fn staging_dir(&self) -> PathBuf {
        let name = self.app.file_name().unwrap_or(OsStr::new("Runode.app")).to_string_lossy();
        self.app.with_file_name(format!(".{name}.update"))
    }
}

/// 下好、核对过的新版本，等着装上。
#[derive(Debug)]
pub struct Staged {
    /// 暂存目录。
    dir: PathBuf,
    /// 暂存目录里的新 .app。
    new_app: PathBuf,
    /// 要换掉的、装着的 .app。
    target: PathBuf,
    /// 装着的那份的 Team ID，装上前再核对一次。
    team: String,
    version: String,
}

impl Staged {
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 再核对一次签名和版本号，和装着的 .app 对调，再删掉换下来的旧版本。在跑的进程照常跑完，下次
    /// 启动的是新版本。从下好到退出可能隔了几个小时，暂存目录又是用户写得了的，所以装上前再核对。
    pub fn install(self) -> Result<(), Error> {
        verify(&self.new_app, &self.team, &self.version)?;
        swap(&self.new_app, &self.target).map_err(|err| io(&self.target, err))?;
        tracing::info!("installed Runode {} at {}", self.version, self.target.display());
        if let Err(err) = fs::remove_dir_all(&self.dir) {
            tracing::warn!("failed to remove the old version in {}: {err}", self.dir.display());
        }
        Ok(())
    }
}

/// 可执行文件 `exe` 所在的 .app：`<名字>.app/Contents/MacOS/<可执行文件>`。
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let named = |dir: &Path, name: &str| dir.file_name() == Some(OsStr::new(name));
    let is_app = app.extension() == Some(OsStr::new("app"));
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then(|| app.to_owned())
}

/// `app` 的签名完好、满足 `requirement(team)`，版本号是 `version`。
fn verify(app: &Path, team: &str, version: &str) -> Result<(), Error> {
    run(&mut satisfies(app, &requirement(team))).map_err(|err| Error::Rejected(err.to_string()))?;
    let plist = app.join("Contents/Info.plist");
    let found = run(Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(&plist))?;
    let found = found.trim();
    if found != version {
        return Err(Error::Rejected(format!("the update is version {found}, the release says {version}")));
    }
    Ok(())
}

/// 用 `team` 的 Developer ID Application 证书签的 `BUNDLE_ID`。两个 `exists` 是 Developer ID 证书
/// 特有的扩展字段（中间证书是 Developer ID 的 CA、叶子证书是 Developer ID Application），和系统给
/// Developer ID 签名生成的指定要求一样；没有它们时，同一个 Team 的开发证书签的包也能通过。
fn requirement(team: &str) -> String {
    format!(
        "=anchor apple generic and identifier \"{BUNDLE_ID}\" \
         and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// 核对 `app` 的签名完好、满足 `requirement` 的 codesign 命令。
fn satisfies(app: &Path, requirement: &str) -> Command {
    let mut command = Command::new("/usr/bin/codesign");
    command.args(["--verify", "--deep", "--strict", "-R"]).arg(requirement).arg(app);
    command
}

/// `app` 的签名的 Team ID；ad-hoc 签名、没签名时为空。
fn team_of(app: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/codesign").args(["-d", "--verbose=2"]).arg(app).output().ok()?;
    parse_team(&String::from_utf8_lossy(&out.stderr))
}

/// 从 `codesign -d --verbose=2` 写在 stderr 上的签名信息里取 Team ID：其中一行是
/// `TeamIdentifier=<Team ID>`，没有 Team 时是 `TeamIdentifier=not set`。
fn parse_team(info: &str) -> Option<String> {
    info.lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

/// `dir` 里唯一的一个 .app。
fn only_app_in(dir: &Path) -> Result<PathBuf, Error> {
    let entries = fs::read_dir(dir).map_err(|err| io(dir, err))?;
    let mut apps = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension() == Some(OsStr::new("app")));
    match (apps.next(), apps.next()) {
        (Some(app), None) => Ok(app),
        _ => Err(Error::Rejected("the update should hold exactly one app".into())),
    }
}

/// 跑 `command`，成了返回 stdout；没成时带上 stderr。
fn run(command: &mut Command) -> Result<String, Error> {
    let program = command.get_program().to_string_lossy().into_owned();
    let out = command.output().map_err(|err| Error::Io(format!("{program}: {err}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Io(format!("{program}: {}", stderr.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn io(path: &Path, err: std::io::Error) -> Error {
    Error::Io(format!("{}: {err}", path.display()))
}

#[cfg(target_os = "macos")]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: 传进去的是以 NUL 结尾的路径，只查权限。
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(target_os = "macos"))]
fn writable(_path: &Path) -> bool {
    false
}

/// 原子地对调 `a` 和 `b` 两个目录，要在同一个卷上。
#[cfg(target_os = "macos")]
fn swap(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let path = |path: &Path| std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other);
    let (a, b) = (path(a)?, path(b)?);
    // SAFETY: 两个都是以 NUL 结尾的路径。
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn swap(_a: &Path, _b: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "updates are only supported on macOS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的空目录，测完删掉。
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("runode-update-{name}-{}", std::process::id()));
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

    #[test]
    fn the_bundle_is_three_levels_up_from_the_executable() {
        let exe = Path::new("/Applications/Runode.app/Contents/MacOS/runode");
        assert_eq!(bundle_of(exe).as_deref(), Some(Path::new("/Applications/Runode.app")));
        assert_eq!(bundle_of(Path::new("/src/runode/target/release/runode")), None);
        assert_eq!(bundle_of(Path::new("/Applications/Runode/Contents/MacOS/runode")), None);
    }

    #[test]
    fn the_staging_directory_is_a_hidden_sibling_of_the_app() {
        let installation = Installation { app: "/Applications/Runode.app".into(), team: "TEAM".into() };
        assert_eq!(installation.staging_dir(), Path::new("/Applications/.Runode.app.update"));
    }

    /// 对调后装着的位置是新版本，暂存的位置换成了旧版本。
    #[cfg(target_os = "macos")]
    #[test]
    fn swapping_exchanges_the_two_apps() {
        let scratch = Scratch::new("swap");
        let target = scratch.0.join("Runode.app");
        let dir = scratch.0.join(".Runode.app.update");
        let new_app = dir.join("Runode.app");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("version"), "old").unwrap();
        fs::create_dir_all(&new_app).unwrap();
        fs::write(new_app.join("version"), "new").unwrap();
        swap(&new_app, &target).unwrap();
        assert_eq!(fs::read_to_string(target.join("version")).unwrap(), "new");
        assert_eq!(fs::read_to_string(new_app.join("version")).unwrap(), "old");
    }

    /// 装上前再核对一次：暂存的包被换成了签名不对的，不对调，装着的那份不动。
    #[cfg(target_os = "macos")]
    #[test]
    fn installing_rechecks_the_signature() {
        let scratch = Scratch::new("recheck");
        let target = scratch.0.join("Runode.app");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("version"), "old").unwrap();
        let dir = scratch.0.join(".Runode.app.update");
        let new_app = ad_hoc_bundle(&dir, "0.2.0");
        let staged = Staged { dir, new_app, target: target.clone(), team: TEAM.into(), version: "0.2.0".into() };
        assert!(matches!(staged.install(), Err(Error::Rejected(_))));
        assert_eq!(fs::read_to_string(target.join("version")).unwrap(), "old");
    }

    /// ad-hoc 签名的包不是 Developer ID 签的，版本号对得上也不收。
    #[cfg(target_os = "macos")]
    #[test]
    fn an_ad_hoc_signed_update_is_rejected() {
        let scratch = Scratch::new("ad-hoc");
        let app = ad_hoc_bundle(&scratch.0, "0.2.0");
        assert!(matches!(verify(&app, TEAM, "0.2.0"), Err(Error::Rejected(_))));
        assert_eq!(team_of(&app), None);
    }

    #[test]
    fn the_team_comes_from_the_team_identifier_line() {
        let signed = "Executable=/Applications/Runode.app/Contents/MacOS/runode\nIdentifier=dev.runode.app\n\
                      TeamIdentifier=ABCDE12345\nSealed Resources version=2\n";
        assert_eq!(parse_team(signed).as_deref(), Some("ABCDE12345"));
        assert_eq!(parse_team("Identifier=dev.runode.app\nTeamIdentifier=not set\n"), None);
        assert_eq!(parse_team("code object is not signed at all\n"), None);
    }

    /// 签名要求的写法 codesign 认得。
    #[cfg(target_os = "macos")]
    #[test]
    fn the_requirement_compiles() {
        let out = Command::new("/usr/bin/csreq").arg("-r").arg(requirement(TEAM)).arg("-t").output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    const TEAM: &str = "ABCDE12345";

    /// 在 `dir` 里建一个 ad-hoc 签名的最小 .app：bundle id 是 `BUNDLE_ID`，版本号是 `version`。
    #[cfg(target_os = "macos")]
    fn ad_hoc_bundle(dir: &Path, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let app = dir.join("Runode.app");
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("runode");
        fs::write(&exe, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>\
             <key>CFBundleExecutable</key><string>runode</string>\
             <key>CFBundleShortVersionString</key><string>{version}</string>\
             </dict></plist>\n"
        );
        fs::write(app.join("Contents/Info.plist"), plist).unwrap();
        run(Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&app)).unwrap();
        app
    }

    #[test]
    fn the_update_must_hold_exactly_one_app() {
        let scratch = Scratch::new("only");
        assert!(only_app_in(&scratch.0).is_err());
        fs::create_dir(scratch.0.join("Runode.app")).unwrap();
        fs::create_dir(scratch.0.join("__MACOSX")).unwrap();
        assert_eq!(only_app_in(&scratch.0).unwrap(), scratch.0.join("Runode.app"));
        fs::create_dir(scratch.0.join("Other.app")).unwrap();
        assert!(only_app_in(&scratch.0).is_err());
    }

    /// 没签名的目录读不出 Team ID。
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unsigned_bundle_has_no_team() {
        let scratch = Scratch::new("team");
        assert_eq!(team_of(&scratch.0), None);
    }
}
