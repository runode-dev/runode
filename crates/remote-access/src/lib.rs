//! 远程访问：手机经局域网或 Tailscale 这类网络连到 Mac 上的宿主。线上格式（TLS 1.3、门禁消息、
//! 签名的拼法、配对 URI、Bonjour 的 TXT 记录）定义在 `runode_protocol::remote`，这里是 Mac 这边的
//! 实现。
//!
//! `Listener` 是开着的监听：IPv4 和 IPv6 各一个 TCP 监听，一个线程接连接，每条连接一个线程。连接
//! 先做 TLS 1.3（rustls，ring 后端，证书是第一次开时生成的自签证书，见 `identity`），再过门禁
//! （`gate`：登录验签、配对验口令），通过后检查第一帧是手机的 `Hello`，然后经调用方给的
//! `Connect` 要一条到宿主的新连接，在两边之间搬字节（`bridge`）。这里不依赖宿主：调用方是建宿主
//! 的那个进程（桌面 app 里的宿主，或者 `runode --host`），`Connect` 通常就是 `Host::connect_pair`。
//!
//! 监听跟着宿主活，开关和端口跟着配置变，`Service` 管这件事：给它想要的端口（`None` 是关），它在
//! 后台线程里开、关、换端口，开不了（端口被占、另一个 runode 正占着监听）就隔一会儿再试。单独
//! 跑的宿主升级交接时，旧宿主进程退出才放开端口和锁，新宿主就这样等到它放开再开；手机那边断了
//! 会自己重连。
//!
//! 命令行（`runode remote …`）不经宿主，直接读写数据目录里的文件：`PairingTicket` 写一个短命的
//! 配对口令文件，监听方每次有人来配对时读它；`listener_status` 看监听方开着没有（它一直锁着一个
//! 锁文件）、在哪个端口、证书指纹是什么；`list_devices`、`revoke_device` 读写设备表，监听方每秒
//! 看一次设备表，撤销的设备连着的连接随之断开。文件的位置都经 `runode_paths` 取。

mod addrs;
mod bonjour;
mod bridge;
mod devices;
mod files;
mod gate;
mod identity;
mod limit;
mod listener;
mod pairing;
mod service;
mod status;

use std::{io, os::unix::net::UnixStream, sync::Arc};

pub use addrs::{host_name, local_addresses};
pub use devices::{Device, list_devices, revoke_device};
pub use listener::{Bind, Listener, Options};
pub use pairing::{PairingProgress, PairingTicket, pairing_pending};
pub use service::Service;
pub use status::{ListenerStatus, listener_status};

/// 要一条到宿主的新连接，见 `Listener`。返回的连接还没说过话，第一帧由这边转发手机的 `Hello`。
pub type Connect = Arc<dyn Fn() -> io::Result<UnixStream> + Send + Sync>;

/// 现在的 Unix 秒。
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs())
}
