//! runode 读写的目录和文件的位置，统一从环境变量算出来。
//!
//! runode 自己的东西在所有系统上都放在同一个根目录：`~/.runode`，不跟着 `XDG_CONFIG_HOME` 走。
//! 配置文件和要留着的数据（窗口布局的存档、命令历史、自己加的项目命令）直接放在根目录，
//! 随时能重新生成的缓存（shell 集成脚本、首个终端的尺寸、宿主进程的日志）放在根目录的
//! `cache/` 里，宿主进程的 socket 和锁放在根目录的 `run/` 里，远程访问的证书、设备表和配对口令放在
//! 根目录的 `remote-access/` 里。
//! Ghostty 自己的配置和主题照旧在它原来的位置读。

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// 宿主进程的 socket、锁和日志的文件名（不含扩展名）。调试构建用另一个名字，免得开发版
/// 连上装好的那个版本的宿主，或者反过来。
const HOST_NAME: &str = if cfg!(debug_assertions) { "host-dev" } else { "host" };

/// `sockaddr_un.sun_path` 的长度（macOS 上最短，104 字节），含结尾的 NUL；socket 路径的
/// 字节数要比它小，否则绑定和连接都会失败。
const SUN_PATH_LEN: usize = 104;

/// 各个目录的位置。环境变量没设、为空，或者连家目录都没有时，对应的字段为 `None`，
/// 由它算出的文件也都是 `None`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dirs {
    /// 家目录，即 `HOME`。
    pub home: Option<PathBuf>,
    /// 用户配置的根目录：`$XDG_CONFIG_HOME`，没设时 `~/.config`。Ghostty 的配置在这下面。
    pub config: Option<PathBuf>,
    /// runode 自己的根目录，即家目录下的 `.runode`。配置文件和要留着的数据放在这里。
    pub data: Option<PathBuf>,
    /// 可以重新生成的缓存，即 `data` 下的 `cache`。shell 会执行这里的集成脚本，所以放在
    /// 用户自己的目录里，而不是可能多人共用的临时目录。
    pub cache: Option<PathBuf>,
}

impl Dirs {
    /// 按当前进程的环境变量算出各个目录。
    pub fn from_env() -> Self {
        Self::from_vars(|key| std::env::var_os(key))
    }

    /// 按 `var` 给出的环境变量算出各个目录，空值当作没设。
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Self {
        let var = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var("HOME");
        let config = var("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|home| home.join(".config")));
        let data = home.as_ref().map(|home| home.join(".runode"));
        let cache = data.as_ref().map(|data| data.join("cache"));
        Self { home, config, data, cache }
    }

    /// runode 的配置文件。
    pub fn config_file(&self) -> Option<PathBuf> {
        self.data_file("config.conf")
    }

    /// 宿主进程和各个界面之间的 Unix socket，在 `runtime_dir` 里。路径太长、放不进
    /// `sun_path` 时为 `None`。
    pub fn host_socket_file(&self) -> Option<PathBuf> {
        let path = self.runtime_dir()?.join(format!("{HOST_NAME}.sock"));
        (path.as_os_str().len() < SUN_PATH_LEN).then_some(path)
    }

    /// 保证只有一个宿主进程的锁文件，和 socket 放在一起。
    pub fn host_lock_file(&self) -> Option<PathBuf> {
        Some(self.runtime_dir()?.join(format!("{HOST_NAME}.lock")))
    }

    /// 宿主进程的日志。宿主没有终端可写，出了问题只能看这里。
    pub fn host_log_file(&self) -> Option<PathBuf> {
        self.cache_file(&format!("{HOST_NAME}.log"))
    }

    /// 放宿主进程的 socket 和锁的目录，即 `data` 下的 `run`。别的用户不能进这个目录，见
    /// `create_runtime_dir`。
    pub fn runtime_dir(&self) -> Option<PathBuf> {
        self.data_file("run")
    }

    /// 建好 `runtime_dir`，并把它的权限设成只有自己能进（0700）；已经存在时也重设一遍权限。
    /// 能连上 socket 的进程就能读写所有终端，所以目录本身要挡住别的用户。
    ///
    /// 已经存在的不是真目录（比如是指向别处的符号链接），或者属主和上一级目录（runode 的根
    /// 目录，用户自己的）不一样时拒绝，不去改它的权限：那多半是别人布下的。
    #[cfg(unix)]
    pub fn create_runtime_dir(&self) -> std::io::Result<PathBuf> {
        create_private_dir(self.runtime_dir())
    }

    /// 远程访问的东西：服务端证书和私钥、配对过的设备表、监听方的锁和状态、命令行交给监听方的
    /// 配对口令、推送用着的广播频道，即 `data` 下的 `remote-access`。只有自己能进，见 `create_remote_access_dir`。
    /// 调试构建和发布构建共用：两者只有一个能占着端口，手机配对一次两边都认。
    pub fn remote_access_dir(&self) -> Option<PathBuf> {
        self.data_file("remote-access")
    }

    /// 建好 `remote_access_dir`，权限和检查同 `create_runtime_dir`：里面有私钥和配对口令。
    #[cfg(unix)]
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

    /// 配对过的设备表（JSON）。
    pub fn remote_access_devices_file(&self) -> Option<PathBuf> {
        self.remote_access_file("devices.json")
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

    /// 推送 Live Activity 时正用着的广播频道（JSON）：监听方重启后据此接着用或收起。
    pub fn remote_access_push_channels_file(&self) -> Option<PathBuf> {
        self.remote_access_file("push-channels.json")
    }

    /// 用户自己的配色主题目录，按名字找主题时最先找这里。
    pub fn themes_dir(&self) -> Option<PathBuf> {
        self.data_file("themes")
    }

    /// 窗口布局的存档。
    pub fn windows_file(&self) -> Option<PathBuf> {
        self.data_file("windows.json")
    }

    /// 用户自己加的项目命令：通用的，和按项目目录分开的，见 `runode_protocol::TaskSourceKind`。
    pub fn tasks_file(&self) -> Option<PathBuf> {
        self.data_file("tasks.json")
    }

    /// Git 面板 AI 写提交说明的预设：用哪个 agent、加什么参数、提示词模板，分所有仓库的默认值和
    /// 按仓库根目录分开的。
    pub fn commit_message_file(&self) -> Option<PathBuf> {
        self.data_file("commit-message.json")
    }

    /// runode 自己记的命令历史。
    pub fn history_file(&self) -> Option<PathBuf> {
        self.data_file("history.jsonl")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(vars: &[(&str, &str)]) -> Dirs {
        let vars: Vec<(String, OsString)> = vars.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        Dirs::from_vars(|key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    }

    #[test]
    fn everything_lives_under_dot_runode() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        assert_eq!(dirs.config_file(), Some("/home/me/.runode/config.conf".into()));
        assert_eq!(dirs.windows_file(), Some("/home/me/.runode/windows.json".into()));
        assert_eq!(dirs.history_file(), Some("/home/me/.runode/history.jsonl".into()));
        assert_eq!(dirs.themes_dir(), Some("/home/me/.runode/themes".into()));
        assert_eq!(dirs.agent_detection_dir(), Some("/home/me/.runode/agent-detection".into()));
        assert_eq!(dirs.shell_integration_dir(), Some("/home/me/.runode/cache/shell-integration".into()));
        assert_eq!(dirs.prespawn_size_file(), Some("/home/me/.runode/cache/first-terminal-size".into()));
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
            (dirs.remote_access_push_channels_file(), "push-channels.json"),
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
        assert_eq!(dirs.host_lock_file(), Some(format!("/home/me/.runode/run/{HOST_NAME}.lock").into()));
        assert_eq!(dirs.host_log_file(), Some(format!("/home/me/.runode/cache/{HOST_NAME}.log").into()));
        assert_eq!(Dirs::default().host_socket_file(), None);
    }

    #[test]
    fn socket_paths_must_fit_in_sun_path() {
        // 家目录下的 `/.runode/run/host-dev.sock` 共 26 字节（release 构建的 `host.sock` 短 4
        // 字节），家目录拼上它正好 103 字节；再长就放不下了。
        let fits =
            format!("/{}", "x".repeat(SUN_PATH_LEN - 2 - "/.runode/run/".len() - HOST_NAME.len() - ".sock".len()));
        let socket = dirs(&[("HOME", &fits)]).host_socket_file().unwrap();
        assert_eq!(socket.as_os_str().len(), SUN_PATH_LEN - 1);
        let too_long = format!("{fits}x");
        let dirs = dirs(&[("HOME", &too_long)]);
        assert_eq!(dirs.host_socket_file(), None);
        // 锁文件和日志不受这个限制。
        assert!(dirs.host_lock_file().is_some());
        assert!(dirs.host_log_file().is_some());
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
