//! 登录时自启：macOS 写 launchd 的 LaunchAgent（`~/Library/LaunchAgents`），Linux 写 systemd 的
//! 用户服务（`$XDG_CONFIG_HOME/systemd/user`）。能拉起两样东西（`Kind`）：无界面的宿主
//! （`runode --host`，手机远程访问靠它在后台等着）和桌面 app（只有 macOS 有）。
//! 有没有装就看服务文件在不在，不另存状态；命令行（`runode service`）和设置窗口用的是同一套。

use std::{
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
    process::Command,
};

use runode_paths::Dirs;

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

/// `kind` 的服务文件放在哪；系统不支持、没有家目录时为 `None`。
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

/// 装了没有。
pub fn is_installed(kind: Kind, dirs: &Dirs) -> bool {
    service_file(kind, dirs).is_some_and(|path| path.exists())
}

/// 装上 `kind` 的登录自启，返回写的服务文件。`exe` 是 runode 可执行文件；`Kind::App` 时它得在
/// `Runode.app` 里。可以重复装，每次整个换掉。
///
/// 宿主马上就起（不用等下次登录），而且崩了会被拉起来；app 只写文件，下次登录才开，免得在设置里
/// 点一下开关就把窗口翻到前面。
pub fn install(kind: Kind, dirs: &Dirs, exe: &Path) -> io::Result<PathBuf> {
    let path = service_file(kind, dirs).ok_or_else(|| unsupported(kind))?;
    let content = match (kind, cfg!(target_os = "macos")) {
        (Kind::Host, true) => plist(kind.label(), &[&exe.to_string_lossy(), "--host"], true),
        (Kind::App, _) => {
            let bundle = app_bundle(exe).ok_or_else(|| {
                io::Error::other(format!(
                    "{} is not inside Runode.app, so it cannot be started at login",
                    exe.display()
                ))
            })?;
            plist(kind.label(), &["/usr/bin/open", "-a", &bundle.to_string_lossy()], false)
        }
        (Kind::Host, false) => unit(exe),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, content)?;
    match (kind, cfg!(target_os = "macos")) {
        (Kind::Host, true) => {
            // 已经加载过时 bootstrap 会报错，先卸掉旧的。
            let _ = launchctl(&["bootout", &launchd_target(kind)?]);
            launchctl(&["bootstrap", &launchd_domain()?, &path.to_string_lossy()])?;
        }
        (Kind::Host, false) => {
            // 用户服务默认只在有人登录时才起；开 linger 才能开机就起，没有登录会话时也得先开它，
            // systemd 才会给这个用户起一个服务管理器。没有权限就算了，调用方用 `starts_at_boot` 看结果。
            let _ = run("loginctl", &["enable-linger"]);
            // 刚开 linger 时服务管理器还在起来，头几次连不上它的总线，隔一会儿再试。
            let enable = || -> io::Result<()> {
                let mut result = systemctl(&["daemon-reload"]);
                for _ in 0..MANAGER_TRIES {
                    if result.is_ok() {
                        break;
                    }
                    std::thread::sleep(MANAGER_RETRY);
                    result = systemctl(&["daemon-reload"]);
                }
                result?;
                systemctl(&["enable", "--now", SYSTEMD_UNIT])
            };
            enable().map_err(|err| {
                io::Error::other(format!(
                    "{err}. If no one is logged in as this user, run `loginctl enable-linger $USER` and try again"
                ))
            })?;
        }
        (Kind::App, _) => {}
    }
    Ok(path)
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

/// 去掉 `kind` 的登录自启：停掉并删掉服务文件。原来就没装时返回 `false`。
pub fn uninstall(kind: Kind, dirs: &Dirs) -> io::Result<bool> {
    let path = service_file(kind, dirs).ok_or_else(|| unsupported(kind))?;
    if !path.exists() {
        return Ok(false);
    }
    if cfg!(target_os = "macos") {
        // 没加载过（app 那种只写了文件的）时会报错，不碍事。
        let _ = launchctl(&["bootout", &launchd_target(kind)?]);
    } else {
        let _ = systemctl(&["disable", "--now", SYSTEMD_UNIT]);
    }
    std::fs::remove_file(&path)?;
    if !cfg!(target_os = "macos") {
        let _ = systemctl(&["daemon-reload"]);
    }
    Ok(true)
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

/// launchd 的 LaunchAgent。`restart_on_crash` 时只在异常退出后重启：宿主空闲了自己退出是正常的，
/// 不能一退出就拉回来。
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
        out.push_str("\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n");
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

/// systemd 用户服务。
fn unit(exe: &Path) -> String {
    // ExecStart 里的 % 是说明符，`"` 和 `\` 要转义。
    let exe = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%");
    format!(
        "[Unit]\nDescription=runode terminal host\n\n[Service]\nExecStart=\"{exe}\" --host\nRestart=on-failure\n\n\
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
        assert!(host.contains("<key>SuccessfulExit</key>\n\t\t<false/>"), "{host}");
        let app = plist("dev.runode.app-login", &["/usr/bin/open", "-a", "/Applications/Runode.app"], false);
        assert!(app.contains("<key>RunAtLoad</key>") && !app.contains("KeepAlive"), "{app}");
    }

    #[test]
    fn the_unit_quotes_the_path() {
        let unit = unit(Path::new("/home/a b/%u/runode"));
        assert!(unit.contains("ExecStart=\"/home/a b/%%u/runode\" --host\n"), "{unit}");
        assert!(unit.contains("WantedBy=default.target"));
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
