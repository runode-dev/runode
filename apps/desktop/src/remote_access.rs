//! 远程访问（手机经网络连上宿主，见 `runode_remote`）跟着宿主活：宿主跑在哪个进程里，监听
//! 就开在哪个进程里，门禁过了的连接经 `Host::connect_pair` 接到宿主上。开关（配置项
//! `remote-access`）、端口（`remote-access-port`）和给手机看的名字（`remote-access-name`）随配置变，
//! 几秒内生效。agent 等回答时给手机的推送也在这个进程里发，设置（`remote-access-push`
//! 这几项和 `apns-*`）同样随配置变，见 `push_settings`。
//!
//! 宿主跑在 app 里时，app 每次应用配置都告诉它（见 `host_client::configure`）；单独跑的宿主
//! （`runode --host`）不管界面，自己读配置文件，文件变了就重读（`follow_config`）。app 连着单独跑的
//! 宿主时不开监听，免得两个进程抢同一个端口。
//!
//! 窗口里的手机端引导页在界面上给手机配对，见 `pairing`。

pub mod pairing;

use std::{
    path::PathBuf,
    sync::{Arc, Weak, mpsc},
    thread,
    time::{Duration, SystemTime},
};

use runode_config::Config;
use runode_host::Host;
use runode_remote::{ApnsKey, Bind, Connect, Options, PushSettings, Service};

/// 盯不了文件时，单独跑的宿主隔多久看一次配置文件、设备表和配对口令；有配对口令在等时也按它看过期没有。
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// 配置要远程访问开在哪个端口；关着时为 `None`。
pub fn wanted_port(config: &Config) -> Option<u16> {
    config.remote_access.then_some(config.remote_access_port)
}

/// 配置里推送的设置。直连 APNs 要 `apns-key-file`、`apns-key-id`、`apns-team-id`、`apns-bundle-id`
/// 四项都配了，缺几项时不直连，记一条警告。
pub fn push_settings(config: &Config) -> PushSettings {
    let key = [&config.apns_key_file, &config.apns_key_id, &config.apns_team_id, &config.apns_bundle_id];
    let direct = match key {
        [Some(file), Some(key_id), Some(team_id), Some(bundle)] => Some(ApnsKey {
            key_file: runode_paths::Dirs::from_env().expand_home(file),
            key_id: key_id.clone(),
            team_id: team_id.clone(),
            bundle: bundle.clone(),
        }),
        [None, None, None, None] => None,
        _ => {
            tracing::warn!(
                "apns-key-file, apns-key-id, apns-team-id and apns-bundle-id go together; not pushing straight to APNs"
            );
            None
        }
    };
    PushSettings {
        enabled: config.remote_access_push,
        text: config.remote_access_push_text,
        delay: config.remote_access_push_delay,
        direct,
        ..PushSettings::default()
    }
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

/// 单独跑的宿主的远程访问：读一遍配置定下开不开，之后在后台线程里盯着配置文件，变了就重读。返回的
/// `Following` 丢掉时监听停下，看配置的线程随之结束。
///
/// 同一个线程也盯着设备表和配对口令，重新判断宿主空闲时要不要留着（`Host::set_stay_up`，见 `StayUp`）：
/// 开着远程访问、又有手机可能连上来时不因空闲退出，不然手机连不上来开新会话。
pub fn follow_config(host: &Host) -> Option<Following> {
    let service = Arc::new(service(host)?);
    // 宿主进程里没有界面，系统外观无从知道；远程访问的两项和主题无关，按深色读即可。
    let config = Config::load(true);
    let dirs = runode_paths::Dirs::from_env();
    service.set(wanted_port(&config), config.remote_access_name.clone());
    service.set_push(push_settings(&config));
    let mut stay = StayUp::default();
    host.set_stay_up(stay.check(&config, &dirs));
    let weak = Arc::downgrade(&service);
    let host = host.clone();
    let (wake, events) = mpsc::channel();
    let watch_wake = wake.clone();
    let spawned = thread::Builder::new()
        .name("remote-access-config".into())
        .spawn(move || watch(&weak, &host, &dirs, config, stay, watch_wake, &events));
    if let Err(err) = spawned {
        tracing::warn!("cannot follow the config for remote access, keeping what it says now: {err}");
    }
    Some(Following { service: Some(service), wake })
}

/// `follow_config` 开着的远程访问。
pub struct Following {
    service: Option<Arc<Service>>,
    /// 叫醒看配置的线程；它平时等着文件的动静，不定时醒。
    wake: mpsc::Sender<()>,
}

impl Drop for Following {
    fn drop(&mut self) {
        // 先放掉监听，再叫醒看配置的线程，它看到监听没了就结束。
        drop(self.service.take());
        self.wake.send(()).ok();
    }
}

fn watch(
    service: &Weak<Service>,
    host: &Host,
    dirs: &runode_paths::Dirs,
    mut config: Config,
    mut stay: StayUp,
    wake: mpsc::Sender<()>,
    events: &mpsc::Receiver<()>,
) {
    let mut files = crate::config::FileWatch::new(move || {
        wake.send(()).ok();
    })
    .inspect_err(|err| {
        tracing::warn!("cannot watch the files remote access follows, checking them every second: {err}")
    })
    .ok();
    let mut seen = crate::config::watch_stamp(&config);
    // 刚开始盯时先看一遍，盯上之前改的也不漏。
    let mut wait = false;
    loop {
        if let Some(files) = &mut files {
            let mut paths = config.watch_paths();
            paths.extend(dirs.remote_access_devices_file());
            paths.extend(dirs.remote_access_pairing_file());
            files.follow(&paths);
        }
        // 等着的配对口令过期时文件不变，这时也按时看一眼。
        let woken = if !wait {
            false
        } else if files.is_some() && !stay.awaiting_pairing() {
            events.recv().is_ok()
        } else {
            events.recv_timeout(WATCH_INTERVAL).is_ok()
        };
        wait = true;
        if woken {
            thread::sleep(crate::config::WATCH_DEBOUNCE);
            while events.try_recv().is_ok() {}
        }
        let Some(service) = service.upgrade() else { return };
        let stamp = crate::config::watch_stamp(&config);
        if stamp != seen {
            config = Config::load(true);
            // 重读可能引入新的文件（比如换了主题），按新配置重新记录；读之前就盯着的用读之前取的
            // 时间，读的时候有人写进来下一轮照样重读。
            seen = crate::config::seen_after(&stamp, &config);
            service.set(wanted_port(&config), config.remote_access_name.clone());
            service.set_push(push_settings(&config));
        }
        host.set_stay_up(stay.check(&config, dirs));
    }
}

/// 文件的修改时刻和大小；没变时不必重新读。
type FileStamp = (SystemTime, u64);

fn file_stamp(path: Option<PathBuf>) -> Option<FileStamp> {
    let meta = std::fs::metadata(path?).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// 单独跑的宿主空闲时要不要留着，见 `StayUp::check`。记着上次读设备表、配对口令文件时它们的样子和读出的
/// 结论，文件没变就不再读。
#[derive(Default)]
struct StayUp {
    devices: Option<(Option<FileStamp>, bool)>,
    pairing: Option<(Option<FileStamp>, bool)>,
}

impl StayUp {
    /// 配置开着远程访问，而且有配对过的设备，或者有还在等设备的配对口令（`runode remote pair` 正在等）。
    /// 关着远程访问时照旧空闲退出。
    fn check(&mut self, config: &Config, dirs: &runode_paths::Dirs) -> bool {
        if !config.remote_access {
            self.pairing = None;
            return false;
        }
        let devices = file_stamp(dirs.remote_access_devices_file());
        let has_devices = match self.devices {
            Some((seen, has)) if seen == devices => has,
            _ => runode_remote::list_devices(dirs).is_ok_and(|devices| !devices.is_empty()),
        };
        self.devices = Some((devices, has_devices));
        // 等着的口令到点就过期，文件却不变，所以在等时每次都读。
        let pairing = file_stamp(dirs.remote_access_pairing_file());
        let pending = match self.pairing {
            Some((seen, false)) if seen == pairing => false,
            _ => runode_remote::pairing_pending(dirs),
        };
        self.pairing = Some((pairing, pending));
        has_devices || pending
    }

    /// 上次看时有配对口令在等。
    fn awaiting_pairing(&self) -> bool {
        matches!(self.pairing, Some((_, true)))
    }
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
    fn push_settings_follow_the_config() {
        assert_eq!(push_settings(&Config::default()), PushSettings::default());
        let config = Config {
            apns_key_file: Some("/keys/AuthKey_X.p8".into()),
            apns_key_id: Some("X".into()),
            apns_team_id: Some("T".into()),
            apns_bundle_id: Some("cn.example.app".into()),
            remote_access_push_text: false,
            ..Config::default()
        };
        let settings = push_settings(&config);
        assert!(!settings.text);
        let key = settings.direct.unwrap();
        assert_eq!((key.key_file.to_str(), key.bundle.as_str()), (Some("/keys/AuthKey_X.p8"), "cn.example.app"));
        // 缺了一项不直连。
        assert_eq!(push_settings(&Config { apns_team_id: None, ..config }).direct, None);
    }

    #[test]
    fn the_host_stays_up_while_a_phone_may_connect() {
        let root = std::env::temp_dir().join(format!("rdr-stay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = runode_paths::Dirs { data: Some(root.join("runode")), ..runode_paths::Dirs::default() };
        let on = Config { remote_access: true, ..Config::default() };
        let off = Config::default();
        // 一直用同一个，文件变了要看得出来，没变时沿用上次的结论。
        let mut stay = StayUp::default();
        // 开着远程访问，但没有配对过的设备、也没人在配对：照旧空闲退出。
        assert!(!stay.check(&on, &dirs));
        assert!(!stay.awaiting_pairing());
        // 正在配对。
        let ticket = runode_remote::PairingTicket::begin(&dirs, Duration::from_secs(60)).unwrap();
        assert!(stay.check(&on, &dirs));
        assert!(stay.awaiting_pairing());
        assert!(!stay.check(&off, &dirs));
        assert!(!stay.awaiting_pairing());
        assert!(stay.check(&on, &dirs));
        drop(ticket);
        assert!(!stay.check(&on, &dirs));
        assert!(!stay.awaiting_pairing());
        // 有配对过的设备。
        let devices = dirs.remote_access_devices_file().unwrap();
        std::fs::write(
            &devices,
            r#"{"version":1,"devices":[{"device_id":"00112233445566778899aabbccddeeff","name":"手机",
                "public_key":"BA","paired_at":1,"last_seen":1}]}"#,
        )
        .unwrap();
        assert!(stay.check(&on, &dirs));
        assert!(stay.check(&on, &dirs));
        assert!(!stay.check(&off, &dirs));
        // 设备都撤销了。
        std::fs::write(&devices, r#"{"version":1,"devices":[]}"#).unwrap();
        assert!(!stay.check(&on, &dirs));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
