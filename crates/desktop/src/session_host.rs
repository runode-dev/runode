//! app 进程里的终端宿主：各个终端视图经 `client` 连到同一个宿主，宿主管着每个会话的 PTY 和
//! 权威的那份 VT，见 `runode_host`。主题和要不要记命令历史跟着配置走，见 `configure`。

use std::sync::OnceLock;

use runode_config::Config;
use runode_host::{Client, ClientMsg, Host};

/// 全进程共用的进程内连接；第一次用到时建好宿主。
pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| Host::new().connect_in_process())
}

/// 把配置里宿主关心的部分告诉它：主题（各个会话在输出流里标出换主题的位置，视图到那里再换）
/// 和要不要把命令记进历史文件。和宿主现在的一样时它什么都不做。
pub fn configure(config: &Config) {
    let client = client();
    client.send(ClientMsg::SetTheme { settings: config.term_settings() });
    client.send(ClientMsg::SetOptions { record_history: config.command_suggestions });
}
