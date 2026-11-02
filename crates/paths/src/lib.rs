//! runode 读写的目录和文件的位置，统一从环境变量算出来。
//!
//! runode 自己的东西在所有系统上都放在同一个根目录：`~/.runode`，不跟着 `XDG_CONFIG_HOME` 走。
//! 配置文件和要留着的数据（窗口布局的存档、自己加的命令、看板）直接放在根目录，
//! 随时能重新生成的缓存（shell 集成脚本、首个终端的尺寸、宿主进程的日志）放在根目录的
//! `cache/` 里，宿主进程的 socket 和锁放在根目录的 `run/` 里（Linux 上再按机器分一层），远程访问的
//! 证书、设备表和配对口令放在根目录的 `remote-access/` 里。
//! Ghostty 自己的配置和主题在 Ghostty 约定的位置读。

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// 宿主进程的 socket、锁和日志的文件名（不含扩展名）。调试构建用另一个名字，免得开发版
/// 连上装好的那个版本的宿主，或者反过来。
const HOST_NAME: &str = if cfg!(debug_assertions) { "host-dev" } else { "host" };
/// 远程桌面的采集进程的 socket、锁和日志的名字，开发版和装好的版本分开。
const SCREEN_NAME: &str = if cfg!(debug_assertions) { "screen-dev" } else { "screen" };

/// runode 的数据目录在家目录下的名字。
const DATA_DIR: &str = ".runode";

/// 看板的库名。调试构建的宿主另用一份：两边的宿主各自在跑，共用一份库时定时会触发两次。
const TODOS_DB: &str = if cfg!(debug_assertions) { "todos-dev.db" } else { "todos.db" };

/// 窗口布局的存档名。调试构建另用一份：存档记的会话号只在写它的那个宿主里有，读到另一边的
/// 接不上，就会在原目录新开终端、把同一场 agent 对话再接回一次。
const WINDOWS_FILE: &str = if cfg!(debug_assertions) { "windows-dev.json" } else { "windows.json" };

/// `sockaddr_un.sun_path` 的长度（各系统里最短的那个），含结尾的 NUL；socket 路径的
/// 字节数要比它小，否则绑定和连接都会失败。
const SUN_PATH_LEN: usize = 104;

/// 各个目录的位置。环境变量没设、为空，或者连家目录都没有时，对应的字段为 `None`，
/// 由它算出的文件也都是 `None`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dirs {
    /// 家目录，即 `HOME`；Windows 上是 `USERPROFILE`，没设时才用 `HOME`：MSYS2、Emacs 这类各自改
    /// `HOME` 的环境里，runode 的目录还是同一处。`HOME` 下已经有 runode 的数据时仍用 `HOME`，
    /// 配置、看板和配对过的设备不丢；别的进程在 `USERPROFILE` 下建了目录也不改。
    pub home: Option<PathBuf>,
    /// 用户配置的根目录：`$XDG_CONFIG_HOME`，没设时 `~/.config`。Ghostty 的配置在这下面。
    pub config: Option<PathBuf>,
    /// runode 自己的根目录，即家目录下的 `.runode`。配置文件和要留着的数据放在这里。
    pub data: Option<PathBuf>,
    /// 可以重新生成的缓存，即 `data` 下的 `cache`。shell 会执行这里的集成脚本，所以放在
    /// 用户自己的目录里，而不是可能多人共用的临时目录。
    pub cache: Option<PathBuf>,
    /// 这台机器的标识：Linux 上是 machine-id 的前 12 个字符，别的系统和读不到时为空。`runtime_dir` 按它
    /// 分开：家目录在 NFS 上被几台机器共享时，NFS 把文件锁变成跨机器的锁，不分开的话一台机器上的宿主
    /// 会让别的机器都起不了宿主。不用主机名：它可以随时改（DHCP、NetworkManager），改了就找不到在跑的
    /// 宿主。
    pub machine: Option<String>,
}

impl Dirs {
    /// 按当前进程的环境变量算出各个目录。
    pub fn from_env() -> Self {
        Self::from_vars(|key| std::env::var_os(key))
    }

    /// 按 `var` 给出的环境变量算出各个目录，空值当作没设；`machine` 另从系统读。
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Self {
        let var = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = if cfg!(windows) {
            match (var("USERPROFILE"), var("HOME")) {
                (_, Some(home)) if home.join(DATA_DIR).is_dir() => Some(home),
                (profile, home) => profile.or(home),
            }
        } else {
            var("HOME")
        };
        let config = var("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|home| home.join(".config")));
        let data = home.as_ref().map(|home| home.join(DATA_DIR));
        let cache = data.as_ref().map(|data| data.join("cache"));
        Self { home, config, data, cache, machine: machine_id() }
    }

    /// runode 的配置文件。
    pub fn config_file(&self) -> Option<PathBuf> {
        self.data_file("config.conf")
    }

    /// 宿主进程和各个界面之间的 Unix socket，在 `host_dir` 里。路径太长、放不进
    /// `sun_path` 时为 `None`。
    pub fn host_socket_file(&self) -> Option<PathBuf> {
        let path = self.host_dir()?.join(format!("{HOST_NAME}.sock"));
        fits_sun_path(&path).then_some(path)
    }

    /// 宿主自己开监听用的 socket 和保证只有一个宿主进程的锁文件，放在同一个 `host_dir` 里；两个路径一起取，
    /// 中途老位置的宿主退出也不会一个在老位置、一个在新位置。socket 路径放不进 `sun_path` 时为 `None`。
    pub fn host_listen_files(&self) -> Option<(PathBuf, PathBuf)> {
        let dir = self.host_dir()?;
        let socket = dir.join(format!("{HOST_NAME}.sock"));
        fits_sun_path(&socket).then(|| (socket, dir.join(format!("{HOST_NAME}.lock"))))
    }

    /// 宿主的 socket 和锁所在的目录，一般就是 `runtime_dir`。不分机器的老位置（`data` 下的 `run`）的锁有
    /// 宿主拿着时用老位置：升级前起的宿主还在那里跑，命令行和界面照样连得上它、交接时接得过来，新起的宿主
    /// 也去拿它那把锁，不会同时跑两个。看的是锁不是 socket：宿主删掉旧 socket、重新绑定的空当里锁一直拿着。
    /// 家目录在 NFS 上共享时锁也跨机器，老位置的 socket 在却连不上（`refused`）说明拿锁的宿主在别的机器上，
    /// 这台机器照样用新位置。那个宿主退出、锁放开之后就都用新位置。新位置的 socket 路径太长、老位置放得下时
    /// 也用老位置。
    fn host_dir(&self) -> Option<PathBuf> {
        let dir = self.runtime_dir()?;
        if self.machine.is_none() {
            return Some(dir);
        }
        let legacy = self.data_file("run")?;
        let socket = format!("{HOST_NAME}.sock");
        let too_long = !fits_sun_path(&dir.join(&socket)) && fits_sun_path(&legacy.join(&socket));
        let here = || locked(&legacy.join(format!("{HOST_NAME}.lock"))) && !refused(&legacy.join(&socket));
        Some(if too_long || here() { legacy } else { dir })
    }

    /// 宿主进程的日志。宿主没有终端可写，出了问题只能看这里。
    pub fn host_log_file(&self) -> Option<PathBuf> {
        self.cache_file(&format!("{HOST_NAME}.log"))
    }

    /// 远程桌面的采集进程（`runode --screen`）的 Unix socket，在 `runtime_dir` 里。路径太长、放不进
    /// `sun_path` 时为 `None`。
    pub fn screen_socket_file(&self) -> Option<PathBuf> {
        let path = self.runtime_dir()?.join(format!("{SCREEN_NAME}.sock"));
        fits_sun_path(&path).then_some(path)
    }

    /// 保证只有一个采集进程的锁文件，和 socket 放在一起。
    pub fn screen_lock_file(&self) -> Option<PathBuf> {
        Some(self.runtime_dir()?.join(format!("{SCREEN_NAME}.lock")))
    }

    /// 采集进程换过显示模式、还没换回去的屏幕，和 socket 放在一起；采集进程没换回去就退出时由别的进程照着换回去。
    pub fn screen_modes_file(&self) -> Option<PathBuf> {
        Some(self.runtime_dir()?.join(format!("{SCREEN_NAME}-modes")))
    }

    /// 采集进程的日志。
    pub fn screen_log_file(&self) -> Option<PathBuf> {
        self.cache_file(&format!("{SCREEN_NAME}.log"))
    }

    /// 不在 .app 里的 runode 拉起采集进程时用的那一份可执行文件，在 `data` 下以 `SCREEN_NAME` 为名的目录里。
    /// 它的路径不随版本变：macOS 按路径记命令行程序的屏幕录制和辅助功能授权。
    pub fn screen_exe_file(&self) -> Option<PathBuf> {
        Some(self.data_file(SCREEN_NAME)?.join("runode"))
    }

    /// 放宿主进程的 socket 和锁的目录，即 `data` 下的 `run`；有 `machine` 时是 `run` 下以它为名的子目录。
    /// 别的用户不能进这个目录，见 `create_runtime_dir`。
    pub fn runtime_dir(&self) -> Option<PathBuf> {
        let run = self.data_file("run")?;
        Some(match &self.machine {
            Some(machine) => run.join(machine),
            None => run,
        })
    }

    /// 建好 `runtime_dir`，并把它的权限设成只有自己能进（0700）；已经存在时也重设一遍权限。
    /// 能连上 socket 的进程就能读写所有终端，所以目录本身要挡住别的用户。
    ///
    /// 已经存在的不是真目录（比如是指向别处的符号链接），或者属主和上一级目录（runode 的根
    /// 目录，用户自己的）不一样时拒绝，不去改它的权限：那多半是别人布下的。
    pub fn create_runtime_dir(&self) -> std::io::Result<PathBuf> {
        create_private_dir(self.runtime_dir())
    }

    /// 远程访问的东西：服务端证书和私钥、配对过的设备表、监听方的锁和状态、命令行交给监听方的
    /// 配对口令，即 `data` 下的 `remote-access`。只有自己能进，见 `create_remote_access_dir`。
    /// 调试构建和发布构建共用：两者只有一个能占着端口，手机配对一次两边都认。
    pub fn remote_access_dir(&self) -> Option<PathBuf> {
        self.data_file("remote-access")
    }

    /// 建好 `remote_access_dir`，权限和检查同 `create_runtime_dir`：里面有私钥和配对口令。
    pub fn create_remote_access_dir(&self) -> std::io::Result<PathBuf> {
        create_private_dir(self.remote_access_dir())
    }

    /// 远程访问的自签证书（DER），见 `remote_access_dir`。
    pub fn remote_access_cert_file(&self) -> Option<PathBuf> {
        self.remote_access_file("cert.der")
    }

    /// 证书的私钥（PKCS#8 DER）。
    pub fn remote_access_key_file(&self) -> Option<PathBuf> {
        self.remote_access_file("key.der")
    }

    /// 跨网络连接（iroh）的 Ed25519 私钥，32 字节；它的公钥就是手机拨号用的地址。
    pub fn remote_access_tunnel_key_file(&self) -> Option<PathBuf> {
        self.remote_access_file("tunnel.key")
    }

    /// 配对过的设备表（JSON）。
    pub fn remote_access_devices_file(&self) -> Option<PathBuf> {
        self.remote_access_file("devices.json")
    }

    /// 这台电脑配对过的别的电脑（JSON），连它们用的设备私钥也在里面。
    pub fn paired_machines_file(&self) -> Option<PathBuf> {
        self.remote_access_file("machines.json")
    }

    /// 改设备表时拿着的锁：监听方和命令行都会改它，设备表本身是整个换掉写的，锁不住。
    pub fn remote_access_devices_lock_file(&self) -> Option<PathBuf> {
        self.remote_access_file("devices.lock")
    }

    /// 命令行（`runode remote pair`）交给监听方的配对口令，用完就删。
    pub fn remote_access_pairing_file(&self) -> Option<PathBuf> {
        self.remote_access_file("pairing.json")
    }

    /// 监听方开着时一直锁着的锁，保证只有一个监听方；命令行据此知道它在不在。
    pub fn remote_access_lock_file(&self) -> Option<PathBuf> {
        self.remote_access_file("listener.lock")
    }

    /// 监听方开好后写的状态（端口、证书指纹、主机名），命令行拼配对 URI 时读。
    pub fn remote_access_status_file(&self) -> Option<PathBuf> {
        self.remote_access_file("listener.json")
    }

    /// 经 SSH 装到这台机器上的 runode，即 `data` 下的 `bin`：每个构建一个子目录，旁边的 `runode` 是指向当前
    /// 那个的链接，远端会话里的 shell 经它找到命令行。目录的写法由连过来的桌面定（`runode_ssh::bootstrap`）。
    pub fn deployed_bin_dir(&self) -> Option<PathBuf> {
        self.data_file("bin")
    }

    /// 远端的桥接（`runode --ssh-bridge`）的日志：它的标准输出是协议，出了问题只能看这里。
    pub fn ssh_bridge_log_file(&self) -> Option<PathBuf> {
        self.cache_file("ssh-bridge.log")
    }

    /// 远端的远程桌面桥接（`runode --ssh-screen`）的日志：它的标准输出是协议，出了问题只能看这里。
    pub fn ssh_screen_log_file(&self) -> Option<PathBuf> {
        self.cache_file("ssh-screen.log")
    }

    /// 指向最近一次经 SSH 连进来时转发的 ssh-agent 的链接，在 `runtime_dir` 里。宿主和它起的 shell 一直用这个
    /// 路径：每次连接转发来的 agent socket 都不一样，连接一断就失效。
    pub fn ssh_agent_link(&self) -> Option<PathBuf> {
        Some(self.runtime_dir()?.join("ssh-agent.sock"))
    }

    /// 连远端主机时 OpenSSH 的 ControlMaster socket，在 `runtime_dir` 下的 `ssh` 里，文件名留给 OpenSSH 按
    /// 连接算（`%C`，展开后 40 个字符）。OpenSSH 先在这个路径加 `.` 和 16 个随机字符上建 socket 再改名，
    /// 那个路径放不进 `sun_path` 时为 `None`，由调用方不复用连接。
    pub fn ssh_control_path(&self) -> Option<PathBuf> {
        let dir = self.runtime_dir()?.join("ssh");
        fits_sun_path(&dir.join(format!("{}.{}", "0".repeat(40), "0".repeat(16)))).then(|| dir.join("%C"))
    }

    /// 建好 `ssh_control_path` 所在的目录，只有自己能进：能连上 ControlMaster 的就能借这条连接在远端跑命令。
    pub fn create_ssh_control_dir(&self) -> std::io::Result<PathBuf> {
        create_private_dir(self.runtime_dir().map(|dir| dir.join("ssh")))
    }

    /// 桌面开着的 askpass socket：ssh 要问密码时调起的 runode 经它问桌面。按进程分开，几个 app 同时开着时
    /// 各问各的。
    pub fn askpass_socket_file(&self, pid: u32) -> Option<PathBuf> {
        let path = self.runtime_dir()?.join(format!("askpass-{pid}.sock"));
        fits_sun_path(&path).then_some(path)
    }

    /// 连远端主机时 ssh 写在 stderr 上的话，按连接一个文件，连不上时据此说清原因。
    pub fn ssh_log_dir(&self) -> Option<PathBuf> {
        self.cache_file("ssh")
    }

    /// 下载来、要装到远端主机上的 runode，按版本和平台存，下次连别的主机不用再下。
    pub fn remote_bin_cache_dir(&self) -> Option<PathBuf> {
        self.cache_file("remote-bin")
    }

    /// 用户自己的配色主题目录，按名字找主题时最先找这里。
    pub fn themes_dir(&self) -> Option<PathBuf> {
        self.data_file("themes")
    }

    /// 窗口布局的存档。
    pub fn windows_file(&self) -> Option<PathBuf> {
        self.data_file(WINDOWS_FILE)
    }

    /// 用户给经 SSH 连的主机起的显示名，按连接目标记。
    pub fn host_names_file(&self) -> Option<PathBuf> {
        self.data_file("host-names.json")
    }

    /// 经 SSH 连的主机上次连上时报的系统，按连接目标记。
    pub fn host_systems_file(&self) -> Option<PathBuf> {
        self.data_file("host-systems.json")
    }

    /// Linux 上 xdg-desktop-portal 交来的远程桌面恢复令牌：用户在屏幕上允许过一次，之后再开就不再问。
    pub fn screen_restore_token_file(&self) -> Option<PathBuf> {
        self.data_file("screen-restore-token")
    }

    /// 用户自己加的命令：通用的，和按项目目录分开的，见 `runode_protocol::TaskSourceKind`。
    pub fn tasks_file(&self) -> Option<PathBuf> {
        self.data_file("tasks.json")
    }

    /// 看板：派给 agent 的任务和定时，SQLite 库，只由宿主读写。
    pub fn todos_db(&self) -> Option<PathBuf> {
        self.data_file(TODOS_DB)
    }

    /// 用户自己加的任务模板，一个模板一个 `.md` 文件，新建任务那一页上列出来。
    pub fn todo_templates_dir(&self) -> Option<PathBuf> {
        self.data_file("templates")
    }

    /// 语音转文字的识别模型，按模型分子目录。下载一次几百 MB 到 1 GB 多，所以放根目录不放缓存。
    pub fn speech_models_dir(&self) -> Option<PathBuf> {
        self.data_file("speech-models")
    }

    /// Git 面板 AI 写提交说明的预设：用哪个 agent、加什么参数、提示词模板，分所有仓库的默认值和
    /// 按仓库根目录分开的。
    pub fn commit_message_file(&self) -> Option<PathBuf> {
        self.data_file("commit-message.json")
    }

    /// Git 面板 AI 写提交说明时 agent 在里面跑的空目录，每次清空重建。
    pub fn ai_message_dir(&self) -> Option<PathBuf> {
        self.cache_file("ai-message")
    }

    /// 用户自己的 agent 识别规则，`<agent 短名>.toml` 覆盖内置的同名规则。
    pub fn agent_detection_dir(&self) -> Option<PathBuf> {
        self.data_file("agent-detection")
    }

    /// 解出 shell 集成脚本的目录。
    pub fn shell_integration_dir(&self) -> Option<PathBuf> {
        self.cache_file("shell-integration")
    }

    /// 上次第一个终端的尺寸，启动时按它提前启动 shell。
    pub fn prespawn_size_file(&self) -> Option<PathBuf> {
        self.cache_file("first-terminal-size")
    }

    /// 写出的终端屏幕内容。里面可能有密钥，所以放在用户自己的缓存目录，不放多人共用的临时目录。
    pub fn screen_files_dir(&self) -> Option<PathBuf> {
        self.cache_file("screens")
    }

    /// 侧栏里 GitHub 仓库的 owner 头像，按 owner 的名字存一份。
    pub fn github_avatar_file(&self, owner: &str) -> Option<PathBuf> {
        self.cache_file(&format!("github-avatars/{owner}.png"))
    }

    /// Ghostty 配置文件的位置，按 Ghostty 的加载顺序：XDG 目录的旧名与新名，macOS 上再加
    /// Application Support 的旧名与新名。
    pub fn ghostty_config_files(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.config.iter().map(|config| config.join("ghostty")).collect();
        if cfg!(target_os = "macos") {
            dirs.extend(self.home.iter().map(|home| home.join("Library/Application Support/com.mitchellh.ghostty")));
        }
        dirs.iter().flat_map(|dir| [dir.join("config"), dir.join("config.ghostty")]).collect()
    }

    /// Ghostty 用户自己的配色主题目录。
    pub fn ghostty_themes_dir(&self) -> Option<PathBuf> {
        Some(self.config.as_ref()?.join("ghostty/themes"))
    }

    /// 把开头的 `~/` 换成家目录；不以 `~/` 开头或没有家目录时原样返回。
    pub fn expand_home(&self, path: &str) -> PathBuf {
        match (path.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        }
    }

    /// `path` 是不是家目录本身。
    pub fn is_home(&self, path: &Path) -> bool {
        self.home.as_deref() == Some(path)
    }

    fn data_file(&self, name: &str) -> Option<PathBuf> {
        Some(self.data.as_ref()?.join(name))
    }

    fn cache_file(&self, name: &str) -> Option<PathBuf> {
        Some(self.cache.as_ref()?.join(name))
    }

    fn remote_access_file(&self, name: &str) -> Option<PathBuf> {
        Some(self.remote_access_dir()?.join(name))
    }
}

/// `lock` 这个锁文件这时有进程锁着（宿主用 `File::try_lock` 拿锁）。文件不在、打不开时算没有。试的是共享锁：
/// NFS 上排他锁要文件以写方式打开，只读打开时报错，就看不出有人锁着。
fn locked(lock: &Path) -> bool {
    std::fs::File::open(lock).is_ok_and(|file| matches!(file.try_lock_shared(), Err(std::fs::TryLockError::WouldBlock)))
}

/// `socket` 在，连上去却被拒：没有进程在这台机器上听它。
#[cfg(unix)]
fn refused(socket: &Path) -> bool {
    matches!(std::os::unix::net::UnixStream::connect(socket), Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused)
}

#[cfg(not(unix))]
fn refused(_: &Path) -> bool {
    false
}

/// socket 路径的字节数小于 `SUN_PATH_LEN`。
fn fits_sun_path(path: &Path) -> bool {
    path.as_os_str().len() < SUN_PATH_LEN
}

/// 这台机器的 machine-id（systemd 装机时生成，重启不变）的前 12 个字符；不是 Linux、读不到或者不像（容器里
/// 常常是空的）时为空。
fn machine_id() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    // machine-id 装机后不变，一个进程只读一次。
    static ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|file| parse_machine_id(&std::fs::read_to_string(file).ok()?))
    })
    .clone()
}

fn parse_machine_id(text: &str) -> Option<String> {
    let id = text.trim();
    (id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) && id.bytes().any(|byte| byte != b'0'))
        .then(|| id[..12].to_ascii_lowercase())
}

/// 建好 `dir`（`None` 时报没有家目录），权限设成只有自己能进（0700），见 `Dirs::create_runtime_dir`。
#[cfg(unix)]
fn create_private_dir(dir: Option<PathBuf>) -> std::io::Result<PathBuf> {
    use std::{
        io::{Error, ErrorKind},
        os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _},
    };

    let dir = dir.ok_or_else(|| Error::new(ErrorKind::NotFound, "no home directory"))?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.file_type().is_dir() {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{} is not a directory", dir.display())));
    }
    let parent = dir.parent().ok_or_else(|| Error::new(ErrorKind::NotFound, "the directory has no parent"))?;
    if meta.uid() != std::fs::metadata(parent)?.uid() {
        return Err(Error::new(ErrorKind::PermissionDenied, format!("{} belongs to another user", dir.display())));
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// 建好 `dir`（`None` 时报没有家目录），只让自己、SYSTEM 和管理员能进（`acl::restrict`）；已经存在的
/// 不是真目录（比如是目录联接）时拒绝。
#[cfg(windows)]
fn create_private_dir(dir: Option<PathBuf>) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};

    let dir = dir.ok_or_else(|| Error::new(ErrorKind::NotFound, "no home directory"))?;
    std::fs::create_dir_all(&dir)?;
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{} is not a directory", dir.display())));
    }
    acl::restrict(&dir)?;
    Ok(dir)
}

#[cfg(windows)]
mod acl;

/// 建好 runode 自己放私密东西的目录 `dir`（连同还没有的上一级），只让自己进：Unix 上 0700，Windows 上只给
/// 自己、SYSTEM 和管理员；已经有了时也收紧一遍。已有的不是真目录、Unix 上属主和上一级目录不一样时拒绝。
pub fn create_private(dir: &Path) -> std::io::Result<()> {
    create_private_dir(Some(dir.to_owned())).map(drop)
}

/// `dir` 只让自己、SYSTEM 和管理员进：属主是这几方之一，DACL 受保护、允许访问的项只给这三方。读不了它的
/// 安全信息时算不是。
#[cfg(windows)]
pub fn is_private_dir(dir: &Path) -> bool {
    acl::is_private(dir).unwrap_or(false)
}

/// `path` 在 `home` 下时写成 `~`，或者 `~` 加 `separator` 再接家目录下的那段；不在 `home` 下时为空。分隔符由调用方
/// 定：要能原样交回给认 `~/` 的地方（命令行的选择器）就写 `/`，只给人看的用本机的。
pub fn home_relative(path: &Path, home: &Path, separator: char) -> Option<String> {
    let rest = path.strip_prefix(home).ok()?;
    Some(if rest.as_os_str().is_empty() { "~".into() } else { format!("~{separator}{}", rest.display()) })
}

/// 把 `contents` 整个换进用户的文件 `path`（配置文件这类用户手写过的）：先写到
/// 同一目录的临时文件再改名换上，写到一半崩溃或磁盘满时原文件原样还在，不会被截成空的。`path` 是
/// 符号链接时换的是它最终指向的文件，链接留着，dotfiles 管理器靠它；原文件的权限照旧，新建的文件只有自己能读。目录要已经有了。
pub fn replace_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    replace(path, contents, true)
}

/// 把 `contents` 整个换进只给自己看的文件 `path`（设备表、密钥这类）：和 `replace_file` 一样先写临时文件
/// 再换上去，换上后不管原文件什么权限，都只有自己能读写。
pub fn replace_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    replace(path, contents, false)
}

/// `keep_permissions` 时换上的文件照旧用原文件的权限，否则只有自己能读写（Unix 上 0600，Windows 上随
/// 所在目录）。
fn replace(path: &Path, contents: &[u8], keep_permissions: bool) -> std::io::Result<()> {
    use std::{
        io::Write as _,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    // 不用 canonicalize：链接悬空（指向的文件还没有）时它报错，这里照样顺着链接写到它指向的地方。
    // 40 是 Linux 和 macOS 解析链接的层数上限。
    let mut target = path.to_owned();
    for _ in 0..40 {
        let Ok(link) = std::fs::read_link(&target) else { break };
        target = target.parent().map_or_else(|| link.clone(), |dir| dir.join(&link));
    }
    // 走满了还是链接（链接成环或链太长）就报错，不然下面的改名会把这一层链接换成普通文件。
    if std::fs::read_link(&target).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{}: too many levels of symbolic links", path.display()),
        ));
    }
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let temp =
        target.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    // 临时文件一开始就只有自己能读：先按 umask 建再收紧权限的话，中间别的用户能把它打开并一直拿着
    // 描述符，之后写进去的私密内容（设备表、口令）就漏了。原文件在的话下面再换回它的权限。
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // 建不出临时文件时直接返回，不去删同名的那个：它不是这里建的。
    let mut file = options.open(&temp)?;
    let written = (|| {
        if keep_permissions && let Ok(meta) = std::fs::metadata(&target) {
            file.set_permissions(meta.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 `vars` 算出的目录，不分机器（`machine` 为空），各系统上都一样。
    fn dirs(vars: &[(&str, &str)]) -> Dirs {
        let vars: Vec<(String, OsString)> = vars.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        Dirs { machine: None, ..Dirs::from_vars(|key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())) }
    }

    /// Windows 上 `USERPROFILE` 优先，别处只认 `HOME`。
    #[test]
    fn the_home_directory_comes_from_the_platforms_variable() {
        let both = dirs(&[("HOME", "/msys/home/me"), ("USERPROFILE", "/users/me")]);
        let expected = if cfg!(windows) { "/users/me" } else { "/msys/home/me" };
        assert_eq!(both.home, Some(expected.into()));
        let profile = dirs(&[("USERPROFILE", "/users/me")]);
        assert_eq!(profile.home, cfg!(windows).then(|| "/users/me".into()));
        assert_eq!(dirs(&[("HOME", "/home/me")]).home, Some("/home/me".into()));
    }

    /// Windows 上 `HOME` 下已有数据时接着用 `HOME`，`USERPROFILE` 下后来有了目录也一样。
    #[cfg(windows)]
    #[test]
    fn existing_data_under_home_is_kept() {
        let root = std::env::temp_dir().join(format!("runode-paths-home-{}", std::process::id()));
        let (profile, home) = (root.join("profile"), root.join("home"));
        std::fs::create_dir_all(home.join(DATA_DIR)).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let both = |profile: &Path, home: &Path| {
            dirs(&[("USERPROFILE", profile.to_str().unwrap()), ("HOME", home.to_str().unwrap())]).home
        };
        assert_eq!(both(&profile, &home), Some(home.clone()));
        std::fs::create_dir_all(profile.join(DATA_DIR)).unwrap();
        assert_eq!(both(&profile, &home), Some(home.clone()));
        std::fs::remove_dir_all(home.join(DATA_DIR)).unwrap();
        assert_eq!(both(&profile, &home), Some(profile.clone()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn everything_lives_under_dot_runode() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        assert_eq!(dirs.config_file(), Some("/home/me/.runode/config.conf".into()));
        assert_eq!(dirs.windows_file(), Some(PathBuf::from("/home/me/.runode").join(WINDOWS_FILE)));
        assert_eq!(dirs.todos_db(), Some(PathBuf::from("/home/me/.runode").join(TODOS_DB)));
        assert_eq!(dirs.themes_dir(), Some("/home/me/.runode/themes".into()));
        assert_eq!(dirs.agent_detection_dir(), Some("/home/me/.runode/agent-detection".into()));
        assert_eq!(dirs.shell_integration_dir(), Some("/home/me/.runode/cache/shell-integration".into()));
        assert_eq!(dirs.prespawn_size_file(), Some("/home/me/.runode/cache/first-terminal-size".into()));
        assert_eq!(dirs.screen_files_dir(), Some("/home/me/.runode/cache/screens".into()));
        assert_eq!(dirs.ghostty_themes_dir(), Some("/home/me/.config/ghostty/themes".into()));
    }

    #[test]
    fn remote_access_files_share_one_directory() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        let dir = PathBuf::from("/home/me/.runode/remote-access");
        assert_eq!(dirs.remote_access_dir(), Some(dir.clone()));
        for (file, name) in [
            (dirs.remote_access_cert_file(), "cert.der"),
            (dirs.remote_access_key_file(), "key.der"),
            (dirs.remote_access_devices_file(), "devices.json"),
            (dirs.remote_access_devices_lock_file(), "devices.lock"),
            (dirs.remote_access_pairing_file(), "pairing.json"),
            (dirs.remote_access_lock_file(), "listener.lock"),
            (dirs.remote_access_status_file(), "listener.json"),
        ] {
            assert_eq!(file, Some(dir.join(name)));
        }
        assert_eq!(Dirs::default().remote_access_cert_file(), None);
    }

    #[test]
    fn host_files_live_in_the_runtime_dir() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        assert_eq!(dirs.runtime_dir(), Some("/home/me/.runode/run".into()));
        assert_eq!(dirs.host_socket_file(), Some(format!("/home/me/.runode/run/{HOST_NAME}.sock").into()));
        assert_eq!(
            dirs.host_listen_files(),
            Some((
                format!("/home/me/.runode/run/{HOST_NAME}.sock").into(),
                format!("/home/me/.runode/run/{HOST_NAME}.lock").into()
            ))
        );
        assert_eq!(dirs.host_log_file(), Some(format!("/home/me/.runode/cache/{HOST_NAME}.log").into()));
        assert_eq!(dirs.screen_socket_file(), Some(format!("/home/me/.runode/run/{SCREEN_NAME}.sock").into()));
        assert_eq!(dirs.screen_exe_file(), Some(format!("/home/me/.runode/{SCREEN_NAME}/runode").into()));
        assert_eq!(Dirs::default().host_socket_file(), None);
    }

    #[test]
    fn machine_ids_are_shortened_and_checked() {
        assert_eq!(parse_machine_id("13337582D4BD4955A11400D2F91B3FBF\n"), Some("13337582d4bd".into()));
        assert_eq!(parse_machine_id(""), None);
        assert_eq!(parse_machine_id("uninitialized\n"), None);
        assert_eq!(parse_machine_id(&"0".repeat(32)), None);
        assert_eq!(parse_machine_id(&"g".repeat(32)), None);
        assert_eq!(Dirs::from_vars(|_| None).machine.is_some(), cfg!(target_os = "linux") && machine_id().is_some());
    }

    /// 分机器时宿主的文件在 `run/<machine>` 里；老位置的锁有宿主拿着时留在老位置，不管 socket 在不在。
    #[cfg(unix)]
    #[test]
    fn host_files_stay_where_a_running_host_holds_the_lock() {
        // macOS 的临时目录太长，socket 路径放不进 `sun_path`。
        let root = Path::new("/tmp").join(format!("runode-paths-machine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs { machine: Some("13337582d4bd".into()), ..dirs(&[("HOME", root.to_str().unwrap())]) };
        let run = root.join(".runode/run");
        let dir = dirs.create_runtime_dir().unwrap();
        assert_eq!(dir, run.join("13337582d4bd"));
        let files = |dir: &Path| Some((dir.join(format!("{HOST_NAME}.sock")), dir.join(format!("{HOST_NAME}.lock"))));
        assert_eq!(dirs.host_socket_file(), Some(dir.join(format!("{HOST_NAME}.sock"))));
        assert_eq!(dirs.host_listen_files(), files(&dir));
        // 老位置只剩崩掉的宿主留下的 socket 和锁文件：没人拿着锁，用新位置。
        let legacy_socket = std::os::unix::net::UnixListener::bind(run.join(format!("{HOST_NAME}.sock"))).unwrap();
        let legacy_lock = std::fs::File::create(run.join(format!("{HOST_NAME}.lock"))).unwrap();
        assert_eq!(dirs.host_listen_files(), files(&dir));
        // 升级前的宿主在老位置拿着锁、听着 socket；它删掉 socket 重新绑定的空当里也一样。
        legacy_lock.try_lock().unwrap();
        assert_eq!(dirs.host_socket_file(), Some(run.join(format!("{HOST_NAME}.sock"))));
        assert_eq!(dirs.host_listen_files(), files(&run));
        // socket 在却没人听：拿锁的宿主在共享家目录的别的机器上，这台机器用新位置。
        drop(legacy_socket);
        assert_eq!(dirs.host_listen_files(), files(&dir));
        std::fs::remove_file(run.join(format!("{HOST_NAME}.sock"))).unwrap();
        assert_eq!(dirs.host_listen_files(), files(&run));
        // 它退出、锁放开之后用新位置。
        drop(legacy_lock);
        assert_eq!(dirs.host_listen_files(), files(&dir));
        // 别的东西（集成脚本的口令文件、采集进程的 socket）一直在新位置。
        assert_eq!(dirs.screen_socket_file(), Some(dir.join(format!("{SCREEN_NAME}.sock"))));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn socket_paths_must_fit_in_sun_path() {
        // 家目录拼上 socket 的相对路径刚好放满 `sun_path`（留一个字节给 NUL）；再长就放不下了。
        let fits =
            format!("/{}", "x".repeat(SUN_PATH_LEN - 2 - "/.runode/run/".len() - HOST_NAME.len() - ".sock".len()));
        let socket = dirs(&[("HOME", &fits)]).host_socket_file().unwrap();
        assert_eq!(socket.as_os_str().len(), SUN_PATH_LEN - 1);
        let too_long = format!("{fits}x");
        let dirs = dirs(&[("HOME", &too_long)]);
        assert_eq!(dirs.host_socket_file(), None);
        assert_eq!(dirs.host_listen_files(), None);
        // 日志不受这个限制。
        assert!(dirs.host_log_file().is_some());
        // 分机器的目录放不下、老位置放得下时用老位置。
        let shorter = &fits[..fits.len() - 3];
        let machine = Dirs { machine: Some("13337582d4bd".into()), ..super::tests::dirs(&[("HOME", shorter)]) };
        assert_eq!(machine.host_socket_file(), Some(PathBuf::from(format!("{shorter}/.runode/run/{HOST_NAME}.sock"))));
    }

    #[cfg(unix)]
    #[test]
    fn the_runtime_dir_is_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!("runode-paths-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = dirs(&[("HOME", root.to_str().unwrap())]);
        let dir = dirs.create_runtime_dir().unwrap();
        assert_eq!(dir, root.join(".runode/run"));
        let mode = |dir: &Path| std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        // 已经存在、权限被放宽了的目录也收回来。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        dirs.create_runtime_dir().unwrap();
        assert_eq!(mode(&dir), 0o700);
        // 换成指向别处的符号链接：拒绝，也不去改链接指向的目录的权限。
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        assert_eq!(dirs.create_runtime_dir().unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(mode(&elsewhere), 0o755);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn xdg_config_home_moves_only_ghostty() {
        let dirs = dirs(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/xdg")]);
        assert_eq!(dirs.config_file(), Some("/home/me/.runode/config.conf".into()));
        assert_eq!(
            dirs.ghostty_config_files()[..2],
            [PathBuf::from("/xdg/ghostty/config"), "/xdg/ghostty/config.ghostty".into()]
        );
        // 家目录不跟着变。
        assert_eq!(dirs.expand_home("~/x"), PathBuf::from("/home/me/x"));
    }

    #[test]
    fn empty_variables_count_as_unset() {
        let dirs = dirs(&[("HOME", ""), ("XDG_CONFIG_HOME", "")]);
        assert_eq!(dirs, Dirs::default());
        assert_eq!(dirs.config_file(), None);
        assert!(dirs.ghostty_config_files().is_empty());
        assert_eq!(dirs.expand_home("~/x"), PathBuf::from("~/x"));
    }

    #[test]
    fn recognizes_the_home_directory() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        assert!(dirs.is_home(Path::new("/home/me")));
        assert!(!dirs.is_home(Path::new("/home")));
        assert!(!Dirs::default().is_home(Path::new("/")));
    }
}
