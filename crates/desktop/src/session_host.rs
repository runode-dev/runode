//! app 进程里的终端宿主：各个终端视图经 `client` 连到同一个宿主，宿主管着每个会话的 PTY 和
//! 权威的那份 VT，见 `runode_host`。主题和要不要记命令历史跟着配置走，见 `configure`；别的
//! 进程经 `listen` 开的 Unix socket 连上来。

use std::sync::OnceLock;

use runode_config::Config;
use runode_host::{BuildId, Client, ClientMsg, Host};

/// 全进程共用的宿主，第一次用到时建好。
fn host() -> &'static Host {
    static HOST: OnceLock<Host> = OnceLock::new();
    HOST.get_or_init(Host::new)
}

/// 全进程共用的进程内连接。
pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| host().connect_in_process())
}

/// 在 runode 自己的 `run/` 目录里开宿主的 socket，让命令行这类别的进程连上来。监听不了（另一个
/// runode 已经开着、目录不对）时记一笔日志，app 照常用。
pub fn listen() {
    let dirs = runode_paths::Dirs::from_env();
    let result = dirs.create_runtime_dir().map_err(anyhow::Error::from).and_then(|_| {
        let socket = dirs.host_socket_file().ok_or_else(|| anyhow::anyhow!("the socket path is too long"))?;
        let lock = dirs.host_lock_file().ok_or_else(|| anyhow::anyhow!("no place for the host lock"))?;
        host().listen(&socket, &lock, BuildId(env!("RUNODE_BUILD").into()))?;
        tracing::info!("host listening on {}", socket.display());
        Ok(())
    });
    if let Err(err) = result {
        tracing::warn!("the host is not listening for other processes: {err:#}");
    }
}

/// 把配置里宿主关心的部分告诉它：主题（各个会话在输出流里标出换主题的位置，视图到那里再换）
/// 和要不要把命令记进历史文件。和宿主现在的一样时它什么都不做。
pub fn configure(config: &Config) {
    let client = client();
    client.send(ClientMsg::SetTheme { settings: config.term_settings() });
    client.send(ClientMsg::SetOptions { record_history: config.command_suggestions });
}
