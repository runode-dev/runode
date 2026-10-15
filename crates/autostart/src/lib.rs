//! 登录时自启：macOS 写 launchd 的 LaunchAgent（`~/Library/LaunchAgents`），Linux 写 systemd 的
//! 用户服务（`$XDG_CONFIG_HOME/systemd/user`）。能拉起两样东西（`Kind`）：无界面的宿主
//! （`runode --host`，手机远程访问靠它在后台等着）和桌面 app（只有 macOS 有）。app 在 macOS 13 起
//! 登记成系统的登录项（`SMAppService`），更早的系统上也写 LaunchAgent。
//! 有没有装就看服务文件在不在、登录项登没登记，不另存状态；命令行（`runode service`）和设置页用的是同一套。

use std::{
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
    process::Command,
};

use runode_paths::Dirs;

#[cfg(target_os = "macos")]
mod login_item;

/// 登录时拉起什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 无界面的终端宿主 `runode --host`。
    Host,
    /// 桌面 app（`Runode.app`），只有 macOS。
    App,
}

impl Kind {
    pub const ALL: [Self; 2] = [Self::Host, Self::App];

    /// 命令行里的写法。
    pub fn name(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::App => "app",
        }
    }

    /// 这个系统上能不能装。
    pub fn supported(self) -> bool {
        match self {
            Self::Host => cfg!(any(target_os = "macos", target_os = "linux")),
            Self::App => cfg!(target_os = "macos"),
        }
    }

    /// launchd 的 Label。
    fn label(self) -> &'static str {
        match self {
            Self::Host => "dev.runode.host-login",
            Self::App => "dev.runode.app-login",
        }
    }
}

/// `kind` 的服务文件放在哪；系统不支持、没有家目录时为 `None`。app 登记成登录项时不写服务文件，
/// 这里给的是登录项不可用时写的 LaunchAgent。
pub fn service_file(kind: Kind, dirs: &Dirs) -> Option<PathBuf> {
    if !kind.supported() {
        return None;
    }
    if cfg!(target_os = "macos") {
        Some(dirs.home.as_ref()?.join("Library/LaunchAgents").join(format!("{}.plist", kind.label())))
    } else {
        Some(dirs.config.as_ref()?.join("systemd/user/runode-host.service"))
    }
}

/// 装了没有。app 登记成登录项的系统上，从 macOS 12 升上来时留下的 LaunchAgent 也算：它照样在登录时开 app。
pub fn is_installed(kind: Kind, dirs: &Dirs) -> bool {
    let has_file = service_file(kind, dirs).is_some_and(|path| path.exists());
    #[cfg(target_os = "macos")]
    if kind == Kind::App && login_item::available() {
        return login_item::is_enabled() || has_file;
    }
    has_file
}

/// 装上 `kind` 的登录自启，返回写的服务文件；app 登记成登录项时没有服务文件，返回 `None`。`exe` 是
/// runode 可执行文件；`Kind::App` 时它得在 `Runode.app` 里，登记的就是这个 app。可以重复装，每次整个
/// 换掉服务文件。
///
/// 宿主马上就起（不用等下次登录），而且崩了会被拉起来；app 下次登录才开，免得在设置里
/// 点一下开关就把窗口翻到前面。宿主已经在服务管理器里跑着时不重启它：它托管着会话，一重启会话就丢了，
/// 新的服务文件等它下次起来时才用上。
pub fn install(kind: Kind, dirs: &Dirs, exe: &Path) -> io::Result<Option<PathBuf>> {
    let path = service_file(kind, dirs).ok_or_else(|| unsupported(kind))?;
    let bundle = || {
        app_bundle(exe).ok_or_else(|| {
            io::Error::other(format!("{} is not inside Runode.app, so it cannot be started at login", exe.display()))
        })
    };
    #[cfg(target_os = "macos")]
    if kind == Kind::App && login_item::available() {
        bundle()?;
        login_item::register()?;
        // macOS 12 时写的 LaunchAgent 删掉，不然登录时 app 开两回。
        remove_if_there(&path)?;
        return Ok(None);
    }
    let content = match (kind, cfg!(target_os = "macos")) {
        (Kind::Host, true) => plist(kind.label(), &[&exe.to_string_lossy(), "--host"], true),
        (Kind::App, _) => plist(kind.label(), &["/usr/bin/open", "-a", &bundle()?.to_string_lossy()], false),
        (Kind::Host, false) => unit(exe),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 启用失败时换回原来的服务文件（原来没有就删掉），免得 `is_installed` 说装好了。
    let previous = match std::fs::read(&path) {
        Ok(previous) => Some(previous),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    std::fs::write(&path, content)?;
    match (kind, cfg!(target_os = "macos")) {
        (Kind::Host, true) => {
            // 已经加载过时 bootstrap 会报错；也不先 bootout，那会把托管着会话的宿主停掉。不带 `-k` 的
            // kickstart 只在它没在跑时拉起来；它已经在跑时可能报错，不碍事。
            let target = launchd_target(kind)?;
            if launchctl(&["print", &target]).is_ok() {
                let _ = launchctl(&["kickstart", &target]);
            } else {
                launchctl(&["bootstrap", &launchd_domain()?, &path.to_string_lossy()])?;
            }
        }
        (Kind::Host, false) => {
            // 用户服务默认只在有人登录时才起；开 linger 才能开机就起，没有登录会话时也得先开它，
            // systemd 才会给这个用户起一个服务管理器。没有权限就算了，调用方用 `starts_at_boot` 看结果。
            let _ = run("loginctl", &["enable-linger"]);
            // 走到 `enable` 前单元原来是不是启用着，没走到时为空；回滚时原来没启用的要撤掉这次的 `enable`。
            let mut was_enabled = None;
            // 刚开 linger 时服务管理器还在起来，头几次连不上它的总线，隔一会儿再试。
            let mut enable = || -> io::Result<()> {
                let mut result = systemctl(&["daemon-reload"]);
                for _ in 0..MANAGER_TRIES {
                    // 没有 systemctl（不是 systemd 的系统）时等也没用。
                    if result.as_ref().map_or_else(|err| err.kind() == io::ErrorKind::NotFound, |()| true) {
                        break;
                    }
                    std::thread::sleep(MANAGER_RETRY);
                    result = systemctl(&["daemon-reload"]);
                }
                result?;
                // 连得上服务管理器了才问，免得连不上时当成原来没启用。启用看的是 wants 目录里的链接，
                // 换了服务文件不影响。只有明确答 `disabled` 才算没启用，问不出来时回滚不动链接。
                was_enabled =
                    Command::new("systemctl").args(["--user", "is-enabled", SYSTEMD_UNIT]).output().ok().and_then(
                        |output| match String::from_utf8_lossy(&output.stdout).trim() {
                            "disabled" => Some(false),
                            "enabled" => Some(true),
                            _ => None,
                        },
                    );
                // 不用 `enable --now`、`restart`：`start` 对跑着的服务什么也不做，不会打断它托管的会话。
                systemctl(&["enable", SYSTEMD_UNIT])?;
                systemctl(&["start", SYSTEMD_UNIT])
            };
            enable().map_err(|err| {
                match &previous {
                    Some(previous) => {
                        // 同 `None` 那支，先照这次的服务文件 `disable` 再换回原来的。
                        if was_enabled == Some(false) {
                            let _ = systemctl(&["disable", SYSTEMD_UNIT]);
                        }
                        let _ = std::fs::write(&path, previous);
                    }
                    None => {
                        // `enable` 成了、`start` 没成时 default.target.wants 里留着链接；`disable` 照着服务
                        // 文件的 [Install] 删，所以先删链接再删文件。
                        let _ = systemctl(&["disable", SYSTEMD_UNIT]);
                        let _ = std::fs::remove_file(&path);
                    }
                }
                // systemd 还记着刚才 reload 进去的新定义，换回文件后再 reload 一次，之后起的才是原来那份。
                let _ = systemctl(&["daemon-reload"]);
                if err.kind() == io::ErrorKind::NotFound {
                    return io::Error::new(
                        io::ErrorKind::NotFound,
                        "systemctl was not found; starting at login needs systemd",
                    );
                }
                io::Error::other(format!(
                    "{err}. If no one is logged in as this user, run `loginctl enable-linger $USER` and try again"
                ))
            })?;
        }
        (Kind::App, _) => {}
    }
    Ok(Some(path))
}

/// Linux 上这个用户开着 linger，即宿主不用等人登录、开机就起（`install` 会试着开）。卸载时不关它：
/// 别的用户服务可能也靠它。
pub fn starts_at_boot() -> bool {
    let Ok(user) = std::env::var("USER") else {
        return false;
    };
    Command::new("loginctl")
        .args(["show-user", &user, "--property=Linger", "--value"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "yes")
}

/// 去掉 `kind` 的登录自启：删掉服务文件、取消开机启用，app 还从登录项里去掉。原来就没装时返回 `false`。
///
/// 正在跑的宿主不停：它托管着会话，停了会话就丢了；它空闲时自己会退出，或者跟着这次登录结束。
pub fn uninstall(kind: Kind, dirs: &Dirs) -> io::Result<bool> {
    let path = service_file(kind, dirs).ok_or_else(|| unsupported(kind))?;
    #[cfg(target_os = "macos")]
    if kind == Kind::App && login_item::available() {
        let registered = login_item::is_registered();
        if registered {
            login_item::unregister()?;
        }
        // 从 macOS 12 升上来时留下的 LaunchAgent 一起删，见 `is_installed`。
        return Ok(remove_if_there(&path)? || registered);
    }
    if !path.exists() {
        return Ok(false);
    }
    // macOS 上删掉文件就够了，下次登录 launchd 不再加载它；不 bootout，那会停掉正在跑的宿主。
    if !cfg!(target_os = "macos") {
        let _ = systemctl(&["disable", SYSTEMD_UNIT]);
    }
    std::fs::remove_file(&path)?;
    if !cfg!(target_os = "macos") {
        let _ = systemctl(&["daemon-reload"]);
    }
    Ok(true)
}

/// 删掉 `path`，返回原来在不在。
#[cfg(target_os = "macos")]
fn remove_if_there(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

const SYSTEMD_UNIT: &str = "runode-host.service";
/// 刚开 linger 后等 systemd 起用户服务管理器：最多试这么多次、每次隔这么久。
const MANAGER_TRIES: u32 = 10;
const MANAGER_RETRY: std::time::Duration = std::time::Duration::from_millis(500);

fn unsupported(kind: Kind) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        match kind {
            Kind::Host => "starting at login is only supported on macOS and Linux",
            Kind::App => "the app only exists on macOS, so there is no app to start at login",
        },
    )
}

/// `exe` 所在的 `.app` 目录。
fn app_bundle(exe: &Path) -> Option<&Path> {
    exe.ancestors().find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
}

/// launchd 的 LaunchAgent。`restart_on_crash` 时只在崩溃（被信号杀掉）后重启：宿主空闲了自己退出、
/// 抢不到宿主锁（已经有一个宿主在跑）时返回非零，都不能拉回来，不然 launchd 会每十秒重启一次。
fn plist(label: &str, args: &[&str], restart_on_crash: bool) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let _ = writeln!(out, "\t<key>Label</key>\n\t<string>{}</string>", xml_escape(label));
    out.push_str("\t<key>ProgramArguments</key>\n\t<array>\n");
    for arg in args {
        let _ = writeln!(out, "\t\t<string>{}</string>", xml_escape(arg));
    }
    out.push_str("\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n");
    if restart_on_crash {
        out.push_str("\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>Crashed</key>\n\t\t<true/>\n\t</dict>\n");
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

/// systemd 用户服务。`on-abnormal` 只在被信号杀掉、超时时重启，返回非零（比如抢不到宿主锁）不算。
fn unit(exe: &Path) -> String {
    // ExecStart 里的 % 是说明符，$ 会被当成环境变量替换，`"` 和 `\` 要转义。
    let exe = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$");
    format!(
        "[Unit]\nDescription=runode terminal host\n\n[Service]\nExecStart=\"{exe}\" --host\nRestart=on-abnormal\n\n\
         [Install]\nWantedBy=default.target\n"
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// launchd 里这个用户的图形会话，即 `gui/<uid>`。
fn launchd_domain() -> io::Result<String> {
    let output = Command::new("id").arg("-u").output()?;
    Ok(format!("gui/{}", String::from_utf8_lossy(&output.stdout).trim()))
}

fn launchd_target(kind: Kind) -> io::Result<String> {
    Ok(format!("{}/{}", launchd_domain()?, kind.label()))
}

fn launchctl(args: &[&str]) -> io::Result<()> {
    run("launchctl", args)
}

fn systemctl(args: &[&str]) -> io::Result<()> {
    run("systemctl", &[&["--user"], args].concat())
}

fn run(program: &str, args: &[&str]) -> io::Result<()> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(format!("`{program} {}` failed: {}", args.join(" "), stderr.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_escapes_paths_and_restarts_only_after_a_crash() {
        let host = plist("dev.runode.host-login", &["/Apps/R&D/runode", "--host"], true);
        assert!(host.contains("<string>/Apps/R&amp;D/runode</string>"), "{host}");
        assert!(host.contains("<key>KeepAlive</key>\n\t<dict>\n\t\t<key>Crashed</key>\n\t\t<true/>"), "{host}");
        assert!(!host.contains("SuccessfulExit"), "{host}");
        let app = plist("dev.runode.app-login", &["/usr/bin/open", "-a", "/Applications/Runode.app"], false);
        assert!(app.contains("<key>RunAtLoad</key>") && !app.contains("KeepAlive"), "{app}");
    }

    #[test]
    fn the_unit_quotes_the_path() {
        let unit = unit(Path::new("/home/a b/%u/runode"));
        assert!(unit.contains("ExecStart=\"/home/a b/%%u/runode\" --host\n"), "{unit}");
        let unit = super::unit(Path::new("/opt/$HOME/run\"o\\de"));
        assert!(unit.contains("ExecStart=\"/opt/$$HOME/run\\\"o\\\\de\" --host\n"), "{unit}");
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("\nRestart=on-abnormal\n"), "{unit}");
    }

    #[test]
    fn the_app_is_found_by_its_bundle() {
        assert_eq!(
            app_bundle(Path::new("/Applications/Runode.app/Contents/MacOS/runode")),
            Some(Path::new("/Applications/Runode.app"))
        );
        assert_eq!(app_bundle(Path::new("/home/me/.local/lib/runode/runode")), None);
    }

    #[test]
    fn service_files_live_in_the_users_launch_directories() {
        let dirs = Dirs::from_vars(|key| (key == "HOME").then(|| "/home/me".into()));
        let host = service_file(Kind::Host, &dirs).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(host, Path::new("/home/me/Library/LaunchAgents/dev.runode.host-login.plist"));
        } else {
            assert_eq!(host, Path::new("/home/me/.config/systemd/user/runode-host.service"));
        }
        assert_eq!(service_file(Kind::App, &dirs).is_some(), cfg!(target_os = "macos"));
    }
}
