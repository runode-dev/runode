//! 连 SSH 主机：一律调系统的 OpenSSH，不内置 SSH 库，`~/.ssh/config`、跳板机、ssh-agent、2FA、安全密钥都按
//! OpenSSH 自己的规矩来。这里管拼命令行（`Ssh`，ControlMaster 复用一条连接）、要问用户时经 runode 自己
//! 当 askpass 问桌面（`askpass`）、读配置里的主机（`list_hosts`）、在远端探测平台和装 runode 的脚本
//! （`bootstrap`）、远端桥接的开场白（`bridge`），以及起 ssh 时接管道（`process`）。不碰网络下载和界面。

pub mod askpass;
pub mod bootstrap;
pub mod bridge;
mod command;
mod config;
mod destination;
mod error;
pub mod process;

pub use command::{DEFAULT_PERSIST_SECS, Ssh, parse_control_path, parse_version, supports_askpass};
pub use config::list_hosts;
pub use destination::Destination;
pub use error::{Failure, last_line};
