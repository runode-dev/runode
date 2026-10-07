//! 远程访问（手机经网络连上宿主，见 `runode_remote_access`）跟着宿主活：宿主跑在哪个进程里，监听
//! 就开在哪个进程里，门禁过了的连接经 `Host::connect_pair` 接到宿主上。开关（配置项
//! `remote-access`）、端口（`remote-access-port`）和给手机看的名字（`remote-access-name`）随配置变，
//! 几秒内生效。
//!
//! 宿主跑在 app 里时，app 每次应用配置都告诉它（见 `host_client::configure`）；单独跑的宿主
//! （`runode --host`）不管界面，自己读配置文件，文件变了就重读（`follow_config`）。app 连着单独跑的
//! 宿主时不开监听，免得两个进程抢同一个端口。
//!
//! 设置窗口和手机端引导页在界面上给手机配对，见 `pairing`。

pub mod pairing;

use std::{
    sync::{Arc, Weak},
    thread,
    time::Duration,
};

use runode_config::Config;
use runode_host::Host;
use runode_remote_access::{Bind, Connect, Options, Service};

/// 单独跑的宿主隔多久看一次配置文件有没有变。
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// 配置要远程访问开在哪个端口；关着时为 `None`。
pub fn wanted_port(config: &Config) -> Option<u16> {
    config.remote_access.then_some(config.remote_access_port)
}

/// 门禁过了的连接接到 `host` 上的远程访问，先关着，由调用方按配置 `set`。起不了后台线程时
/// 记日志，返回 `None`。
pub fn service(host: &Host) -> Option<Service> {
    let host = host.clone();
    let connect: Connect = Arc::new(move || host.connect_pair());
    let options = Options {
        dirs: runode_paths::Dirs::from_env(),
        port: 0,
        bind: Bind::All,
        host_name: None,
        advertise: true,
        connect,
    };
    Service::start(options).map_err(|err| tracing::warn!("cannot start remote access: {err}")).ok()
}

/// 单独跑的宿主的远程访问：读一遍配置定下开不开，之后在后台线程里每隔 `WATCH_INTERVAL` 看一眼
/// 配置文件，变了就重读。返回的 `Service` 丢掉时监听停下，看配置的线程随之结束。
///
/// 同一个线程每次也重新判断宿主空闲时要不要留着（`Host::set_stay_up`，见 `stays_up`）：开着远程
/// 访问、又有手机可能连上来时不因空闲退出，不然手机连不上来开新会话。
pub fn follow_config(host: &Host) -> Option<Arc<Service>> {
    let service = Arc::new(service(host)?);
    // 宿主进程里没有界面，系统外观无从知道；远程访问的两项和主题无关，按深色读即可。
    let config = Config::load(true);
    let dirs = runode_paths::Dirs::from_env();
    service.set(wanted_port(&config), config.remote_access_name.clone());
    host.set_stay_up(stays_up(&config, &dirs));
    let weak = Arc::downgrade(&service);
    let host = host.clone();
    let spawned =
        thread::Builder::new().name("remote-access-config".into()).spawn(move || watch(&weak, &host, &dirs, config));
    if let Err(err) = spawned {
        tracing::warn!("cannot follow the config for remote access, keeping what it says now: {err}");
    }
    Some(service)
}

fn watch(service: &Weak<Service>, host: &Host, dirs: &runode_paths::Dirs, mut config: Config) {
    let mut seen = crate::config::watch_stamp(&config);
    loop {
        thread::sleep(WATCH_INTERVAL);
        let Some(service) = service.upgrade() else { return };
        let stamp = crate::config::watch_stamp(&config);
        if stamp != seen {
            config = Config::load(true);
            // 重读可能引入新的文件（比如换了主题），按新配置重新记录。
            seen = crate::config::watch_stamp(&config);
            service.set(wanted_port(&config), config.remote_access_name.clone());
        }
        host.set_stay_up(stays_up(&config, dirs));
    }
}

/// 单独跑的宿主空闲时要不要留着：配置开着远程访问，而且有配对过的设备，或者有还在等设备的配对
/// 口令（`runode remote pair` 正在等）。关着远程访问时照旧空闲退出。
fn stays_up(config: &Config, dirs: &runode_paths::Dirs) -> bool {
    config.remote_access
        && (runode_remote_access::list_devices(dirs).is_ok_and(|devices| !devices.is_empty())
            || runode_remote_access::pairing_pending(dirs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_port_is_the_protocol_default() {
        assert_eq!(Config::default().remote_access_port, runode_protocol::remote::DEFAULT_PORT);
        assert_eq!(wanted_port(&Config::default()), None);
        let config = Config { remote_access: true, remote_access_port: 9000, ..Config::default() };
        assert_eq!(wanted_port(&config), Some(9000));
    }

    #[test]
    fn the_host_stays_up_while_a_phone_may_connect() {
        let root = std::env::temp_dir().join(format!("rdr-stay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = runode_paths::Dirs { data: Some(root.join("runode")), ..runode_paths::Dirs::default() };
        let on = Config { remote_access: true, ..Config::default() };
        let off = Config::default();
        // 开着远程访问，但没有配对过的设备、也没人在配对：照旧空闲退出。
        assert!(!stays_up(&on, &dirs));
        // 正在配对。
        let ticket = runode_remote_access::PairingTicket::begin(&dirs, Duration::from_secs(60)).unwrap();
        assert!(stays_up(&on, &dirs));
        assert!(!stays_up(&off, &dirs));
        drop(ticket);
        assert!(!stays_up(&on, &dirs));
        // 有配对过的设备。
        std::fs::write(
            dirs.remote_access_devices_file().unwrap(),
            r#"{"version":1,"devices":[{"device_id":"00112233445566778899aabbccddeeff","name":"手机",
                "public_key":"BA","paired_at":1,"last_seen":1}]}"#,
        )
        .unwrap();
        assert!(stays_up(&on, &dirs));
        assert!(!stays_up(&off, &dirs));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
