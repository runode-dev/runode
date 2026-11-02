//! 拼调系统 ssh 的命令行。连接都经 OpenSSH 的 ControlMaster 复用：认证一次，之后探测、部署、桥接、
//! 端口转发都走同一条连接，不再问密码、不再弹 2FA。Windows 自带的 OpenSSH 不支持 ControlMaster，
//! 那里每次调用各连各的。

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use crate::Destination;

/// Windows 上不给 ssh 开控制台窗口（`CREATE_NO_WINDOW`）：桌面 app 和宿主都没有控制台，不然每调一次 ssh 就
/// 闪一个黑窗；要问用户的经 askpass 问，用不着控制台。
fn no_window(command: &mut Command) {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(command, windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

/// 主连接空闲多久后自己退出（秒），用户没设时用这个。
pub const DEFAULT_PERSIST_SECS: u32 = 600;

/// 怎么调 ssh。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ssh {
    /// ssh 程序，默认 `ssh`，按 PATH 找。
    pub program: PathBuf,
    /// ControlMaster 的 socket 路径，可以带 OpenSSH 的 `%C`；为空时不复用连接。
    pub control: Option<PathBuf>,
    /// 主连接空闲多久后退出（秒）。
    pub persist_secs: u32,
}

impl Ssh {
    /// 所有调用都带的选项：命令行上的 `-o` 压过配置文件，用户自己的 ControlPath 不受影响。
    fn base(&self) -> Command {
        let mut command = Command::new(&self.program);
        no_window(&mut command);
        match &self.control {
            Some(path) => command.arg("-o").arg(control_path_option(path)),
            None => command.args(["-o", "ControlPath=none", "-o", "ControlMaster=no"]),
        };
        command.args([
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "LogLevel=ERROR",
        ]);
        command
    }

    /// `ssh -V`：版本打在 stderr 上，由 `parse_version` 读。
    pub fn version(&self) -> Command {
        let mut command = Command::new(&self.program);
        no_window(&mut command);
        command.arg("-V");
        command
    }

    /// 起主连接：认证完就转到后台（`-f`），之后的调用经 `control` 复用它。`batch` 时不问用户，要认证就失败。
    pub fn master(&self, dest: &Destination, batch: bool) -> Command {
        let mut command = self.connecting();
        command.args(["-M", "-N", "-f", "-T", "-o", "ControlMaster=yes"]);
        command.arg("-o").arg(format!("ControlPersist={}", self.persist_secs));
        if batch {
            command.args(["-o", "BatchMode=yes"]);
        }
        command.args(dest.args());
        command
    }

    /// 自己可能建连接的调用：用户配置里的 `LocalForward` 这类转发不在 runode 的连接上重复绑定。`forward`
    /// 不能带它，带了连要加的那条转发也一起清掉。
    fn connecting(&self) -> Command {
        let mut command = self.base();
        command.args(["-o", "ClearAllForwardings=yes"]);
        command
    }

    /// 问主连接还在不在：退出码为 0 就在。
    pub fn check(&self, dest: &Destination) -> Command {
        self.control_command("check", dest)
    }

    /// 让主连接退出。
    pub fn exit(&self, dest: &Destination) -> Command {
        self.control_command("exit", dest)
    }

    fn control_command(&self, op: &str, dest: &Destination) -> Command {
        let mut command = self.base();
        command.args(["-O", op]).args(dest.args());
        command
    }

    /// 在远端跑一条命令，stdin 和 stdout 原样是二进制数据（`-e none` 关掉转义字符，`-T` 不分配终端）。
    /// 有主连接时复用它（`batch` 时不问用户）；`remote` 交给远端的登录 shell 解释。
    pub fn exec(&self, dest: &Destination, remote: &str, batch: bool) -> Command {
        self.exec_with(dest, remote, batch, &[])
    }

    /// 在远端跑一条命令，传的数据压缩：传 runode 本身这类大文件时用。复用主连接时压不压由主连接定，这里的选项不起作用。
    pub fn exec_compressed(&self, dest: &Destination, remote: &str, batch: bool) -> Command {
        self.exec_with(dest, remote, batch, &["-o", "Compression=yes"])
    }

    /// 选项要在目标之前：之后的参数都算进远端命令。
    fn exec_with(&self, dest: &Destination, remote: &str, batch: bool, options: &[&str]) -> Command {
        let mut command = self.connecting();
        command.args(["-T", "-e", "none", "-o", "ControlMaster=no"]);
        if batch {
            command.args(["-o", "BatchMode=yes"]);
        }
        command.args(options).args(dest.args()).arg(remote);
        command
    }

    /// 在主连接上加（`cancel` 时去掉）一条本地端口转发：本机的 `local` 端口转到远端的 `remote` 端口。
    pub fn forward(&self, dest: &Destination, local: u16, remote: u16, cancel: bool) -> Command {
        let mut command = self.base();
        command.args(["-O", if cancel { "cancel" } else { "forward" }]);
        command.arg("-L").arg(format!("127.0.0.1:{local}:localhost:{remote}"));
        command.args(dest.args());
        command
    }
}

/// `ControlPath=…`：路径里的 `%` 除了 `%C` 都要写成 `%%`，免得被 OpenSSH 当成别的占位符；带空白时加上
/// 引号，OpenSSH 按空白切开 `-o` 的值。
fn control_path_option(path: &Path) -> String {
    let text = path.to_string_lossy().replace('%', "%%").replace("%%C", "%C");
    if text.contains(char::is_whitespace) { format!("ControlPath=\"{text}\"") } else { format!("ControlPath={text}") }
}

/// 读 `ssh -V` 的输出（它写在 stderr 上），比如 `OpenSSH_9.6p1, LibreSSL 3.3.6` 是 (9, 6)；Windows 自带的写成
/// `OpenSSH_for_Windows_8.1p1`。
pub fn parse_version(text: &str) -> Option<(u32, u32)> {
    let rest = &text[text.find("OpenSSH_")? + "OpenSSH_".len()..];
    let rest = rest.strip_prefix("for_Windows_").unwrap_or(rest);
    let (major, rest) = rest.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// 这个版本的 ssh 能不能强制走 askpass（`SSH_ASKPASS_REQUIRE`，OpenSSH 8.4 起）。不能时只能用密钥和 agent。
pub fn supports_askpass(version: (u32, u32)) -> bool {
    version >= (8, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_percent_signs_except_the_hash() {
        assert_eq!(control_path_option(Path::new("/a%b/ssh-%C")), "ControlPath=/a%%b/ssh-%C");
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(control_path_option(Path::new("/Users/J Smith/%C")), "ControlPath=\"/Users/J Smith/%C\"");
    }
}
