//! 远端要跑的脚本：探测平台、把 runode 装上去、起桥接。脚本经 ssh 交给远端的登录 shell，那可能是 fish、
//! tcsh 这类不按 sh 的规矩解释的 shell，所以一律写成一行、整个包在单引号里交给 `/bin/sh -c`，脚本里
//! 不出现单引号（结束引号）、`!`（csh 的历史展开）和换行（csh 的引号里不能有）。Windows 远端的脚本另在
//! `windows` 里。

pub mod windows;

use std::fmt;

/// 远端命令按哪种系统的写法：`Unix` 交给登录 shell 里的 `/bin/sh`，`Windows` 交给 cmd（`windows`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Family {
    #[default]
    Unix,
    Windows,
}

impl Family {
    /// 这种系统的 `connect_command`。
    pub fn connect_command(self, dir: &str) -> String {
        match self {
            Self::Unix => connect_command(dir),
            Self::Windows => windows::connect_command(dir),
        }
    }

    /// 这种系统的 `run_command`。
    pub fn run_command(self, dir: &str, entry: Entry) -> String {
        match self {
            Self::Unix => run_command(dir, entry),
            Self::Windows => windows::run_command(dir, entry),
        }
    }

    /// 这种系统的 `prune_command`。
    pub fn prune_command(self, keep: &str) -> String {
        match self {
            Self::Unix => prune_command(keep),
            Self::Windows => windows::prune_command(keep),
        }
    }
}

/// runode 在远端装在家目录下的这里，每个构建一个子目录，旁边的 `runode` 是指向当前那个的链接。
pub const BIN_DIR: &str = ".runode/bin";
/// 连接脚本发现远端还没装这个构建时，打完平台信息以这个码退出。
pub const PROBE_EXIT: i32 = 42;
/// 远端可执行文件按 glibc 链接，要这个版本起。
pub const MIN_GLIBC: (u32, u32) = (2, 28);

/// 构建号换成远端的目录名：只留字母、数字和 `.`、`-`、`_`，别的换成 `_`。
pub fn install_dir_name(build: &str) -> String {
    let name: String = build
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    if name.is_empty() || name.starts_with('.') { format!("_{name}") } else { name }
}

/// 交给 ssh 的一行远端命令：脚本原样包在单引号里给 `/bin/sh -c`。
fn wrap(script: &str) -> String {
    debug_assert!(!script.contains(['\'', '!', '\n']), "脚本里有远端 shell 解释不了的字符：{script}");
    format!("/bin/sh -c '{script}'")
}

/// 构建目录里记着各个文件 SHA-256 的清单，每行是 `<十六进制>  <文件名>`（`sha256sum` 的格式）。
/// 连上时按它复核装着的文件；没有它的构建当作没装。
pub const SUMS_FILE: &str = "SHA256SUMS";

/// sh 里算 `path` 的 SHA-256（十六进制）的命令替换；`sha256sum` 和 `shasum` 都没有时是空串。
fn sh_sha256(path: &str) -> String {
    format!(r#"$( (sha256sum "{path}" 2>/dev/null || shasum -a 256 "{path}") | cut -d" " -f1)"#)
}

/// 连上远端：装着 `dir` 这个构建、`runode` 的摘要又和 `SUMS_FILE` 里记的对得上，就直接换成它的桥接
/// （`runode --ssh-bridge`）；没装、没有清单、对不上（装好后被改坏、截断）就打出 `RUNODE-PROBE` 开头的平台信息
/// （系统、架构、C 库、家目录、已经装着的构建），以 `PROBE_EXIT` 退出，由调用方重新装。每次连上都重算一遍摘要。
pub fn connect_command(dir: &str) -> String {
    wrap(&format!(
        concat!(
            r#"D="$HOME/{bin}"; B="$D/{dir}"; X="$B/runode"; "#,
            r#"if [ -x "$X" ] && [ -f "$B/{sums}" ]; then S={hash}; "#,
            r#"if [ -n "$S" ] && [ "$(cat "$B/{sums}")" = "$S  runode" ]; then exec "$X" --ssh-bridge; fi; fi; "#,
            r#"printf "RUNODE-PROBE os=%s\n" "$(uname -s)"; "#,
            r#"printf "RUNODE-PROBE arch=%s\n" "$(uname -m)"; "#,
            r#"printf "RUNODE-PROBE arm64=%s\n" "$(sysctl -n hw.optional.arm64 2>/dev/null)"; "#,
            r#"printf "RUNODE-PROBE glibc=%s\n" "$(getconf GNU_LIBC_VERSION 2>/dev/null)"; "#,
            r#"printf "RUNODE-PROBE musl=%s\n" "$( (ldd --version 2>&1 || true) | grep -ci musl)"; "#,
            r#"printf "RUNODE-PROBE home=%s\n" "$HOME"; "#,
            r#"ls "$D" 2>/dev/null | sed "s/^/RUNODE-PROBE installed=/"; "#,
            "exit {exit}"
        ),
        bin = BIN_DIR,
        dir = dir,
        sums = SUMS_FILE,
        hash = sh_sha256("$X"),
        exit = PROBE_EXIT,
    ))
}

/// 连上以后在远端起的 runode，stdin、stdout 交给它。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// 项目服务（`runode --ssh-project`）。
    Project,
    /// 远程桌面的桥接（`runode --ssh-screen`）。
    Screen,
    /// 把远端的宿主装成登录服务（`runode --ssh-service`）。
    Service,
    /// 语言服务器（`runode --ssh-lsp`，后面跟着服务器的程序名和参数）。程序名和参数原样拼进脚本，只能有字母、
    /// 数字和 `-`、`_`、`.`。
    Lsp { program: &'static str, args: &'static [&'static str] },
}

impl Entry {
    fn arg(self) -> String {
        match self {
            Self::Project => "--ssh-project".into(),
            Self::Screen => "--ssh-screen".into(),
            Self::Service => "--ssh-service".into(),
            Self::Lsp { program, args } => {
                ["--ssh-lsp", program].iter().chain(args).copied().collect::<Vec<_>>().join(" ")
            }
        }
    }
}

/// 起远端装着的 `dir` 这个构建的 `entry`；没装时以 `PROBE_EXIT` 退出，先经 `connect_command` 装好。
pub fn run_command(dir: &str, entry: Entry) -> String {
    run_installed(dir, &entry.arg())
}

fn run_installed(dir: &str, arg: &str) -> String {
    wrap(&format!(r#"X="$HOME/{BIN_DIR}/{dir}/runode"; if [ -x "$X" ]; then exec "$X" {arg}; fi; exit {PROBE_EXIT}"#))
}

/// 把 stdin 上的可执行文件装成 `dir` 这个构建：先写成临时文件，核对 SHA-256（`sha256`，十六进制）、试跑一下
/// （拦住挂成 noexec 的家目录、太旧的 glibc、缺的动态库），都过了才原子地换上，再把 `runode` 链接指过去；哪一步
/// 没成都删掉临时文件；临时文件名每次不同，被打断时留下的由 `prune_script` 删。试跑不成时 stderr 上打一行
/// `RUNODE-INSTALL exec`，后面跟着试跑报错的开头（`parse_exec_failure` 认）。`archive` 时
/// stdin 是只装着 `runode` 的 tar.gz，核对的是包。换上 `runode` 以后才写 `SUMS_FILE`：可执行文件就是 `sha256`，
/// 从核对过的包里解出来的在远端重算。换上以后删掉放了超过 `BUILD_KEEP_DAYS` 天的旧构建（`prune_script`），
/// 链接原先指着的那个留着。
pub fn install_command(dir: &str, sha256: &str, archive: bool) -> String {
    let unpack = if archive { r#"tar -xzOf "$T.in" runode > "$T" && rm -f "$T.in""# } else { r#"mv -f "$T.in" "$T""# };
    let rehash = if archive {
        format!(r#"S={}; [ -n "$S" ] || {{ rm -f "$T"; exit 1; }}; "#, sh_sha256("$T"))
    } else {
        String::new()
    };
    wrap(&format!(
        concat!(
            r#"D="$HOME/{bin}"; umask 077; mkdir -p "$D/{dir}" || exit 1; T="$D/{dir}/.part-$$"; "#,
            r#"cat > "$T.in" || {{ rm -f "$T.in"; exit 1; }}; "#,
            r#"S={hash}; "#,
            r#"if [ "$S" = "{sha}" ]; then :; else rm -f "$T.in"; echo "RUNODE-INSTALL checksum" >&2; exit 1; fi; "#,
            r#"{unpack} || {{ rm -f "$T" "$T.in"; exit 1; }}; "#,
            r#"chmod 755 "$T" || {{ rm -f "$T"; exit 1; }}; "#,
            r#"if "$T" --version > /dev/null 2> "$T.err"; then rm -f "$T.err"; else "#,
            r#"E=$(head -c 300 "$T.err" | tr "\n" " "); rm -f "$T" "$T.err"; echo "RUNODE-INSTALL exec $E" >&2; exit 1; fi; "#,
            "{rehash}",
            r#"mv -f "$T" "$D/{dir}/runode" || {{ rm -f "$T"; exit 1; }}; "#,
            r#"printf "%s  runode\n" "$S" > "$T.sums" && mv -f "$T.sums" "$D/{dir}/{sums}" || {{ rm -f "$T.sums"; exit 1; }}; "#,
            r#"P=$(readlink "$D/runode" 2>/dev/null); P="${{P%%/*}}"; "#,
            r#"ln -sf "{dir}/runode" "$D/.link-$$" && mv -f "$D/.link-$$" "$D/runode" || exit 1; "#,
            "{prune} > /dev/null; :"
        ),
        bin = BIN_DIR,
        dir = dir,
        hash = sh_sha256("$T.in"),
        sha = sha256,
        unpack = unpack,
        rehash = rehash,
        sums = SUMS_FILE,
        prune = prune_script(dir, Some(BUILD_KEEP_DAYS)),
    ))
}

/// `install_command` 和 `windows::install_command` 的 stderr 里试跑装上的 runode 不成的那一行：报了原因时是
/// 原因，没报时是空串；不是这种失败时是 `None`。
pub fn parse_exec_failure(stderr: &str) -> Option<&str> {
    stderr.lines().find_map(|line| line.strip_prefix("RUNODE-INSTALL exec")).map(str::trim)
}

/// 装上新构建时，别的构建放满这么多天才删。
pub const BUILD_KEEP_DAYS: u32 = 7;
/// 装到一半被打断留下的临时文件（Windows 上是临时目录）放满这么多天才删：正在装的那些一直在写，不会这么旧。
const PART_KEEP_DAYS: u32 = 1;
/// `days` 天折成 `find -mmin` 的分钟数：按分钟比，放满整 `days` 天就删。
fn minutes(days: u32) -> u32 {
    days * 24 * 60
}
/// 删掉一个旧构建打出的一行以它开头，后面是目录名和它占的 KiB。
const PRUNED: &str = "RUNODE-PRUNED ";

/// 删掉远端除 `keep` 和 `runode` 链接指着的以外的所有构建，打出删掉了哪些（`parse_pruned` 认）。
pub fn prune_command(keep: &str) -> String {
    wrap(&format!(
        r#"D="$HOME/{BIN_DIR}"; P=$(readlink "$D/runode" 2>/dev/null); P="${{P%%/*}}"; {}"#,
        prune_script(keep, None)
    ))
}

/// 删掉 `$D` 下的构建目录，跳过 `keep`、`$P` 和还有进程在跑着里面的 `runode` 的；给了 `days` 时只删放满这么多天的。
/// 留着的构建里放满 `PART_KEEP_DAYS` 天的 `.part-` 临时文件也删掉。
fn prune_script(keep: &str, days: Option<u32>) -> String {
    let age = days.map(|days| format!(" -mmin +{}", minutes(days))).unwrap_or_default();
    format!(
        concat!(
            r#"find "$D" -mindepth 2 -maxdepth 2 -type f -name ".part-*" -mmin +{part_minutes} -exec rm -f {{}} + 2>/dev/null; "#,
            r#"R=$( {{ ps -A -o args=; ls -l /proc/[0-9]*/exe; }} 2>/dev/null ); "#,
            r#"find "$D" -mindepth 1 -maxdepth 1 -type d{age} | while read -r E; do N="${{E##*/}}"; "#,
            r#"case "$N" in "{keep}"|"$P") continue;; esac; case "$R" in *"$E/runode"*) continue;; esac; "#,
            r#"K=$(du -sk "$E" 2>/dev/null | cut -f1); rm -rf "$E" && printf "{pruned}%s %s\n" "$N" "${{K:-0}}"; done"#
        ),
        part_minutes = minutes(PART_KEEP_DAYS),
        age = age,
        keep = keep,
        pruned = PRUNED,
    )
}

/// `prune_command` 的输出里删掉的构建，各是目录名和它占的 KiB；登录 shell 的启动文件可能先打出别的行，跳过。
pub fn parse_pruned(output: &str) -> Vec<(String, u64)> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix(PRUNED)?.split_once(' '))
        .map(|(name, kib)| (name.to_owned(), kib.trim().parse().unwrap_or(0)))
        .collect()
}

/// 上传到远端的文件放在家目录下的这里，每次上传一个子目录。
pub const UPLOADS_DIR: &str = ".runode/uploads";
/// 上传的子目录放满这么多天，下次上传时删掉。
pub const UPLOAD_KEEP_DAYS: u32 = 7;
/// 上传完打出的那一行以它开头，后面是这次上传放在远端的目录。
const UPLOADED: &str = "RUNODE-UPLOADED ";

/// 把 stdin 传到远端 `UPLOADS_DIR` 下新建的子目录 `id` 里：给了 `file` 时 stdin 就是这个文件的内容，没给时是
/// tar 包，原样解开；传完打出这个子目录的绝对路径（`parse_uploaded` 认）。`UPLOADS_DIR` 只让自己进，已经有了
/// 的也收紧，里面新建的目录和文件也只给自己。`id` 和 `file` 只能有字母、数字和 `.`、`-`、`_`，不能以 `.` 开头。
pub fn upload_command(id: &str, file: Option<&str>) -> String {
    debug_assert!([Some(id), file].into_iter().flatten().all(|name| install_dir_name(name) == name));
    let receive = match file {
        Some(file) => format!(r#"cat > "$U/{file}""#),
        None => r#"tar -xf - -C "$U""#.to_owned(),
    };
    wrap(&format!(
        concat!(
            r#"D="$HOME/{uploads}"; umask 077; mkdir -p "$D" && chmod 700 "$D" || exit 1; "#,
            r#"find "$D" -mindepth 1 -maxdepth 1 -mmin +{keep} -exec rm -rf {{}} + 2>/dev/null; "#,
            r#"U="$D/{id}"; mkdir "$U" || exit 1; "#,
            "{receive} || exit 1; ",
            r#"printf "{uploaded}%s\n" "$U""#
        ),
        uploads = UPLOADS_DIR,
        keep = minutes(UPLOAD_KEEP_DAYS),
        id = id,
        receive = receive,
        uploaded = UPLOADED,
    ))
}

/// `upload_command` 的输出里这次上传放在远端的目录；登录 shell 的启动文件可能先打出别的行，跳过。
pub fn parse_uploaded(output: &str) -> Option<&str> {
    output.lines().find_map(|line| line.strip_prefix(UPLOADED)).filter(|dir| dir.starts_with('/'))
}

/// 远端的系统。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
    Other(String),
}

/// 远端的 C 库。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Libc {
    Glibc(u32, u32),
    Musl,
    Unknown,
}

/// 探测到的远端平台。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    /// `uname -m`，Windows 上是 `PROCESSOR_ARCHITECTURE`；Apple 芯片的 Mac 上在 Rosetta 下的 shell 里也报
    /// `x86_64`，所以另看 `arm64`。
    pub arch: String,
    /// Mac 的硬件是 Apple 芯片。
    pub arm64: bool,
    pub libc: Libc,
    pub home: String,
    /// 远端已经装着的构建的目录名。
    pub installed: Vec<String>,
}

/// runode 发布了可执行文件的平台。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    LinuxX86_64,
    LinuxAarch64,
    MacArm64,
    MacX86_64,
    WindowsX86_64,
}

impl Target {
    /// 发布页上这个平台的包名里的架构名。
    pub fn arch(self) -> &'static str {
        match self {
            Self::LinuxX86_64 | Self::MacX86_64 | Self::WindowsX86_64 => "x86_64",
            Self::LinuxAarch64 => "aarch64",
            Self::MacArm64 => "arm64",
        }
    }

    pub fn is_mac(self) -> bool {
        matches!(self, Self::MacArm64 | Self::MacX86_64)
    }

    pub fn is_windows(self) -> bool {
        self == Self::WindowsX86_64
    }

    /// 环境变量名里用的写法，比如 `LINUX_X86_64`。
    pub fn env_name(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "LINUX_X86_64",
            Self::LinuxAarch64 => "LINUX_AARCH64",
            Self::MacArm64 => "MACOS_ARM64",
            Self::MacX86_64 => "MACOS_X86_64",
            Self::WindowsX86_64 => "WINDOWS_X86_64",
        }
    }

    /// 这台机器自己的平台。
    pub fn current() -> Option<Self> {
        Some(match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Self::LinuxX86_64,
            ("linux", "aarch64") => Self::LinuxAarch64,
            ("macos", "aarch64") => Self::MacArm64,
            ("macos", "x86_64") => Self::MacX86_64,
            ("windows", "x86_64") => Self::WindowsX86_64,
            _ => return None,
        })
    }
}

/// 远端为什么跑不了 runode。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Os(String),
    Arch(String),
    Musl,
    OldGlibc(u32, u32),
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os(os) => write!(f, "unsupported system: {os}"),
            Self::Arch(arch) => write!(f, "unsupported architecture: {arch}"),
            Self::Musl => f.write_str("musl libc is not supported"),
            Self::OldGlibc(major, minor) => {
                write!(f, "glibc {major}.{minor} is too old, {}.{} or later is required", MIN_GLIBC.0, MIN_GLIBC.1)
            }
        }
    }
}

impl Platform {
    /// 读连接脚本打出的 `RUNODE-PROBE` 行；没有系统那一行时为空。
    pub fn parse(output: &str) -> Option<Self> {
        let mut os = None;
        let mut platform = Self {
            os: Os::Other(String::new()),
            arch: String::new(),
            arm64: false,
            libc: Libc::Unknown,
            home: String::new(),
            installed: Vec::new(),
        };
        let mut musl = false;
        for line in output.lines() {
            let Some((key, value)) = line.strip_prefix("RUNODE-PROBE ").and_then(|rest| rest.split_once('=')) else {
                continue;
            };
            let value = value.trim();
            match key {
                "os" => {
                    os = Some(match value {
                        "Linux" => Os::Linux,
                        "Darwin" => Os::MacOs,
                        "Windows" => Os::Windows,
                        other => Os::Other(other.to_owned()),
                    })
                }
                "arch" => platform.arch = value.to_owned(),
                "arm64" => platform.arm64 = value == "1",
                "glibc" => {
                    if let Some(version) = parse_glibc(value) {
                        platform.libc = Libc::Glibc(version.0, version.1);
                    }
                }
                "musl" => musl = value.parse::<u32>().is_ok_and(|count| count > 0),
                "home" => platform.home = value.to_owned(),
                "installed" if !value.is_empty() => platform.installed.push(value.to_owned()),
                _ => {}
            }
        }
        if musl && platform.libc == Libc::Unknown {
            platform.libc = Libc::Musl;
        }
        platform.os = os?;
        Some(platform)
    }

    /// 远端该装哪个平台的 runode。
    pub fn target(&self) -> Result<Target, Unsupported> {
        match &self.os {
            Os::MacOs => Ok(if self.arm64 || self.arch == "arm64" { Target::MacArm64 } else { Target::MacX86_64 }),
            Os::Linux => {
                match self.libc {
                    Libc::Musl => return Err(Unsupported::Musl),
                    Libc::Glibc(major, minor) if (major, minor) < MIN_GLIBC => {
                        return Err(Unsupported::OldGlibc(major, minor));
                    }
                    _ => {}
                }
                match self.arch.as_str() {
                    "x86_64" | "amd64" => Ok(Target::LinuxX86_64),
                    "aarch64" | "arm64" => Ok(Target::LinuxAarch64),
                    other => Err(Unsupported::Arch(other.to_owned())),
                }
            }
            // ARM64 的 Windows 11 经系统的 x64 仿真跑 x64 的包。
            Os::Windows if self.arch.eq_ignore_ascii_case("AMD64") || self.arch.eq_ignore_ascii_case("ARM64") => {
                Ok(Target::WindowsX86_64)
            }
            Os::Windows => Err(Unsupported::Arch(self.arch.clone())),
            Os::Other(os) => Err(Unsupported::Os(os.clone())),
        }
    }
}

/// `glibc 2.31` 里的版本号。
fn parse_glibc(text: &str) -> Option<(u32, u32)> {
    let version = text.split_whitespace().last()?;
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}
