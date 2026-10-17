//! 登录时自启：macOS 写 launchd 的 LaunchAgent（`~/Library/LaunchAgents`），Linux 写 systemd 的
//! 用户服务（用户服务管理器自己查找的 `$XDG_CONFIG_HOME/systemd/user`，见 `systemd_unit_files`）。能拉起两样东西（`Kind`）：无界面的宿主
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
/// 这里给的是登录项不可用时写的 LaunchAgent。Linux 上是 `systemd_unit_files` 里装着的那份，都没装时
/// 是第一个候选；真装时写到哪由服务管理器定，见 `install`。
pub fn service_file(kind: Kind, dirs: &Dirs) -> Option<PathBuf> {
    if !kind.supported() {
        return None;
    }
    if cfg!(target_os = "macos") {
        Some(dirs.home.as_ref()?.join("Library/LaunchAgents").join(format!("{}.plist", kind.label())))
    } else {
        let files = systemd_unit_files(dirs);
        files.iter().find(|file| file.exists()).or(files.first()).cloned()
    }
}

/// systemd 单元可能放的位置：本进程的 `XDG_CONFIG_HOME` 下和 `~/.config` 下。用户服务管理器只在它
/// 自己的 `XDG_CONFIG_HOME` 下找，而它的环境来自 PAM，不一定有 shell 配置里改的那个值；没改时两个
/// 是同一处。只看文件在不在，`is_installed` 不用每次去问服务管理器。
fn systemd_unit_files(dirs: &Dirs) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = [dirs.config.clone(), dirs.home.as_ref().map(|home| home.join(".config"))]
        .into_iter()
        .flatten()
        .map(|config| config.join("systemd/user").join(SYSTEMD_UNIT))
        .collect();
    files.dedup();
    files
}

/// 服务管理器的环境（`systemctl --user show-environment` 的输出，一行一个 `KEY=VALUE`）里 `key` 的值。
/// 值里带空格等字符时有两种引法：有的 systemd 把整行括进双引号，里面的 `"`、`\`、`` ` ``、`$` 前加反斜杠；
/// 更新的只引值，写成 `KEY=$'…'`，里面是 C 风格的转义（`unescape_ansi_c`）。
fn manager_var(env: &str, key: &str) -> Option<String> {
    env.lines().find_map(|line| {
        if let Some(quoted) = line.strip_prefix('"').and_then(|line| line.strip_suffix('"')) {
            let mut out = String::new();
            let mut chars = quoted.chars();
            while let Some(c) = chars.next() {
                out.push(if c == '\\' { chars.next()? } else { c });
            }
            return out.strip_prefix(key)?.strip_prefix('=').map(str::to_owned);
        }
        let value = line.strip_prefix(key)?.strip_prefix('=')?;
        match value.strip_prefix("$'").and_then(|value| value.strip_suffix('\'')) {
            Some(quoted) => unescape_ansi_c(quoted),
            None => Some(value.to_owned()),
        }
    })
}

/// 还原 `$'…'` 里的转义：systemd 的 `cescape_char` 写的 `\a\b\f\n\r\t\v`、`\\`、`\'`、`\"` 和三位八进制
/// （控制字符和不成 UTF-8 的字节），另外也认 bash 的 `\xHH`；不认得的照 bash 原样留着反斜杠。
fn unescape_ansi_c(quoted: &str) -> Option<String> {
    fn digits(chars: &mut std::iter::Peekable<std::str::Chars>, radix: u32, max: usize, mut n: u32) -> u32 {
        for _ in 0..max {
            match chars.next_if(|c| c.is_digit(radix)) {
                Some(c) => n = n * radix + c.to_digit(radix).unwrap_or(0),
                None => break,
            }
        }
        n
    }
    let mut out = Vec::new();
    let mut chars = quoted.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            continue;
        }
        let byte = match chars.next()? {
            'a' => 0x07,
            'b' => 0x08,
            'f' => 0x0c,
            'n' => b'\n',
            'r' => b'\r',
            't' => b'\t',
            'v' => 0x0b,
            c @ ('\\' | '\'' | '"') => c as u8,
            c @ '0'..='7' => digits(&mut chars, 8, 2, c.to_digit(8).unwrap_or(0)) as u8,
            'x' if chars.peek().is_some_and(char::is_ascii_hexdigit) => digits(&mut chars, 16, 2, 0) as u8,
            c => {
                out.push(b'\\');
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                continue;
            }
        };
        out.push(byte);
    }
    String::from_utf8(out).ok()
}

/// 服务管理器会去找的那份服务文件：它在自己环境里的 `XDG_CONFIG_HOME` 下找，没有时在 `~/.config`
/// 下找。只看环境不看 `UnitPath`：较旧的 systemd 把还不存在的目录从 `UnitPath` 里删掉，从没装过
/// 用户单元的账号上就查不到。`home` 是服务管理器环境里没有 `HOME` 时用的家目录。
fn manager_unit_file(manager_env: &str, home: Option<&Path>) -> Option<PathBuf> {
    let manager = Dirs::from_vars(|key| match manager_var(manager_env, key) {
        Some(value) => Some(value.into()),
        None if key == "HOME" => home.map(Into::into),
        None => None,
    });
    Some(manager.config?.join("systemd/user").join(SYSTEMD_UNIT))
}

/// 问服务管理器在哪找单元，挑 `systemd_unit_files` 里它会找的那个；不在候选里时报错说清楚，免得
/// 写了文件服务管理器却说找不到。连不上服务管理器时先开 linger、等它起来。
fn systemd_unit_file(dirs: &Dirs) -> io::Result<PathBuf> {
    // 用户服务默认只在有人登录时才起；开 linger 才能开机就起，没有登录会话时也得先开它，
    // systemd 才会给这个用户起一个服务管理器。没有权限就算了，调用方用 `starts_at_boot` 看结果。
    let _ = run("loginctl", &["enable-linger"]);
    let ask = || capture("systemctl", &["--user", "show-environment"]);
    // 刚开 linger 时服务管理器还在起来，头几次连不上它的总线，隔一会儿再试。
    let mut env = ask();
    for _ in 0..MANAGER_TRIES {
        // 没有 systemctl（不是 systemd 的系统）时等也没用。
        if env.as_ref().map_or_else(|err| err.kind() == io::ErrorKind::NotFound, |_| true) {
            break;
        }
        std::thread::sleep(MANAGER_RETRY);
        env = ask();
    }
    let env = env.map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            return io::Error::new(io::ErrorKind::NotFound, "systemctl was not found; starting at login needs systemd");
        }
        io::Error::other(format!(
            "{err}. If no one is logged in as this user, run `loginctl enable-linger $USER` and try again"
        ))
    })?;
    let candidates = systemd_unit_files(dirs);
    match manager_unit_file(&env, dirs.home.as_deref()) {
        Some(file) if candidates.contains(&file) => Ok(file),
        file => {
            let dir = |file: &PathBuf| file.parent().map(|dir| dir.display().to_string());
            Err(io::Error::other(format!(
                "the systemd user manager looks for units in {}, not in {}: it runs with a different \
                 XDG_CONFIG_HOME than this shell",
                file.as_ref().and_then(dir).unwrap_or_else(|| "an unknown directory".into()),
                candidates.iter().filter_map(dir).collect::<Vec<_>>().join(" or "),
            )))
        }
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
    // systemd 只在它自己的单元目录里找，先问清楚再决定写到哪。
    let path = if cfg!(target_os = "macos") { path } else { systemd_unit_file(dirs)? };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 启用失败时换回原来的服务文件（原来没有就删掉），免得 `is_installed` 说装好了。
    let previous = match std::fs::read(&path) {
        Ok(previous) => Some(previous),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    // 先写临时文件再换上：写到一半失败时原来的服务文件还在，回滚有东西可换。
    runode_paths::replace_file(&path, content.as_bytes())?;
    match (kind, cfg!(target_os = "macos")) {
        (Kind::Host, true) => {
            // 已经加载过时 bootstrap 会报错；也不先 bootout，那会把托管着会话的宿主停掉。不带 `-k` 的
            // kickstart 只在它没在跑时拉起来；它已经在跑时可能报错，不碍事。
            let loaded = launchd_target(kind).and_then(|target| {
                if launchctl(&["print", &target]).is_ok() {
                    let _ = launchctl(&["kickstart", &target]);
                    return Ok(());
                }
                launchctl(&["bootstrap", &launchd_domain()?, &path.to_string_lossy()])
            });
            if let Err(err) = loaded {
                // 没加载上的服务不在 launchd 里，换回文件即可，不用 bootout。
                let _ = match &previous {
                    Some(previous) => runode_paths::replace_file(&path, previous),
                    None => std::fs::remove_file(&path),
                };
                return Err(err);
            }
        }
        (Kind::Host, false) => {
            // 走到 `enable` 前单元原来是不是启用着，没走到时为空；回滚时原来没启用的要撤掉这次的 `enable`。
            let mut was_enabled = None;
            let mut enable = || -> io::Result<()> {
                systemctl(&["daemon-reload"])?;
                // reload 成了才问，免得连不上时当成原来没启用。启用看的是 wants 目录里的链接，
                // 换了服务文件不影响。
                was_enabled = Command::new("systemctl")
                    .args(["--user", "is-enabled", SYSTEMD_UNIT])
                    .output()
                    .ok()
                    .and_then(|output| Enablement::parse(&String::from_utf8_lossy(&output.stdout)));
                // 不用 `enable --now`、`restart`：`start` 对跑着的服务什么也不做，不会打断它托管的会话。
                systemctl(&["enable", SYSTEMD_UNIT])?;
                systemctl(&["start", SYSTEMD_UNIT])
            };
            enable().inspect_err(|_| {
                match &previous {
                    Some(previous) => {
                        // 同 `None` 那支，先照这次的服务文件 `disable` 再换回原来的。原来只在运行时启用的，
                        // 这次的 `enable` 加了持久的链接，也要撤掉，不然只管这次开机的服务成了每次登录都起。
                        if matches!(was_enabled, Some(Enablement::Disabled | Enablement::Runtime)) {
                            let _ = systemctl(&["disable", SYSTEMD_UNIT]);
                        }
                        let _ = runode_paths::replace_file(&path, previous);
                        // 有的 systemd 版本的 `disable` 连运行时的链接一起删，照原来的服务文件补回来；
                        // 链接还在时这一步什么也不做。
                        if was_enabled == Some(Enablement::Runtime) {
                            let _ = systemctl(&["enable", "--runtime", SYSTEMD_UNIT]);
                        }
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
    // Linux 上候选的位置都看，在不同的环境里装过时一起删掉。
    let files: Vec<PathBuf> = if cfg!(target_os = "macos") { vec![path] } else { systemd_unit_files(dirs) };
    let files: Vec<PathBuf> = files.into_iter().filter(|file| file.exists()).collect();
    if files.is_empty() {
        return Ok(false);
    }
    // macOS 上删掉文件就够了，下次登录 launchd 不再加载它；不 bootout，那会停掉正在跑的宿主。
    if !cfg!(target_os = "macos") {
        let _ = systemctl(&["disable", SYSTEMD_UNIT]);
    }
    for file in &files {
        std::fs::remove_file(file)?;
    }
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

/// systemd 单元在 `install` 这次 `enable` 之前的启用状态，回滚时据此只撤这次加的链接。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Enablement {
    /// 没启用（`disabled`）。
    Disabled,
    /// 持久地启用着（`enabled`），这次的 `enable` 没加什么。
    Enabled,
    /// 只在运行时启用着（`enabled-runtime`）：链接在运行时目录里，重启后就没了。
    Runtime,
}

impl Enablement {
    /// 认 `systemctl is-enabled` 的回答。别的回答（`masked`、`linked` 这些）和问不出来一样为 `None`，
    /// 回滚时不动链接。
    fn parse(answer: &str) -> Option<Self> {
        match answer.trim() {
            "disabled" => Some(Self::Disabled),
            "enabled" => Some(Self::Enabled),
            "enabled-runtime" => Some(Self::Runtime),
            _ => None,
        }
    }
}

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
    capture(program, args).map(drop)
}

/// 跑 `program`，成了时返回它的标准输出。
fn capture(program: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
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
    fn only_a_clear_answer_counts_as_how_the_unit_was_enabled() {
        assert_eq!(Enablement::parse("disabled\n"), Some(Enablement::Disabled));
        assert_eq!(Enablement::parse("enabled\n"), Some(Enablement::Enabled));
        // 只在运行时启用的单元回滚时要撤掉这次加的持久链接，不能当成问不出来。
        assert_eq!(Enablement::parse("enabled-runtime\n"), Some(Enablement::Runtime));
        assert_eq!(Enablement::parse("masked\n"), None);
        assert_eq!(Enablement::parse(""), None);
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

    #[test]
    fn the_unit_goes_where_the_systemd_manager_looks() {
        // shell 里把 XDG_CONFIG_HOME 改成了 ~/.xdg，服务管理器的环境里没有它，只在 ~/.config 下找。
        let dirs = Dirs::from_vars(|key| match key {
            "HOME" => Some("/home/a b".into()),
            "XDG_CONFIG_HOME" => Some("/home/a b/.xdg".into()),
            _ => None,
        });
        let candidates = systemd_unit_files(&dirs);
        assert_eq!(
            candidates,
            [
                Path::new("/home/a b/.xdg/systemd/user/runode-host.service"),
                Path::new("/home/a b/.config/systemd/user/runode-host.service"),
            ]
        );
        // 服务管理器的环境里没有 XDG_CONFIG_HOME：用 ~/.config，哪怕那里的目录还没建。
        let home = Path::new("/home/a b");
        let manager = "HOME=/home/a b\nLANG=C.UTF-8\n";
        assert_eq!(manager_unit_file(manager, None).as_ref(), Some(&candidates[1]));
        assert_eq!(manager_unit_file("LANG=C.UTF-8\n", Some(home)).as_ref(), Some(&candidates[1]));
        // 服务管理器也有这个 XDG_CONFIG_HOME（比如经 pam_env 设的）时照它来；较新的 systemd 给带空格的行加引号。
        let manager = "\"HOME=/home/a b\"\n\"XDG_CONFIG_HOME=/home/a b/.xdg\"\n";
        assert_eq!(manager_unit_file(manager, Some(home)).as_ref(), Some(&candidates[0]));
        assert_eq!(manager_var(r#""A=x \"y\" \$z""#, "A").as_deref(), Some(r#"x "y" $z"#));
        // 更新的 systemd 只引值：`$'…'`，里面是 C 风格转义。
        let manager = "HOME=$'/home/a b'\nXDG_CONFIG_HOME=$'/home/a b/.xdg'\n";
        assert_eq!(manager_unit_file(manager, None).as_ref(), Some(&candidates[0]));
        assert_eq!(manager_var(r#"A=$'it\'s \\ \"q\"'"#, "A").as_deref(), Some(r#"it's \ "q""#));
        assert_eq!(manager_var(r"A=$'a\tb\nc\033d\303\251\x41\q'", "A").as_deref(), Some("a\tb\nc\x1bdéA\\q"));
        // 原样写的 `$'` 开头的值在旧格式里引了 `$`，不当成转义。
        assert_eq!(manager_var(r#""A=\$'x'""#, "A").as_deref(), Some("$'x'"));
        // 只是前缀相同的变量不算。
        assert_eq!(manager_var("XDG_CONFIG_HOMEX=/x\n", "XDG_CONFIG_HOME"), None);
        // 没改 XDG_CONFIG_HOME 时只有一个候选。
        let plain = Dirs::from_vars(|key| (key == "HOME").then(|| "/home/me".into()));
        assert_eq!(systemd_unit_files(&plain), [Path::new("/home/me/.config/systemd/user/runode-host.service")]);
    }
}
