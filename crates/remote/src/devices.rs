//! 配对过的设备表：`remote_access_devices_file` 里的 JSON（0600），每台设备的标识、名字、公钥、
//! 配对和最近一次登录的时刻。监听方配对时加、登录时更新 `last_seen`，命令行撤销时删；改的一方都
//! 先拿 `remote_access_devices_lock_file` 的锁，读的一方不用（文件整个换掉写，读不到一半的）。
//!
//! 同一个文件里还有手机登记的推送（`PushRegistration`），按设备一条，和
//! `Device` 分开放：`runode remote list --json` 把 `Device` 整个打印出来，token 不该跟着出去。撤销设备时
//! 一起删。
//!
//! 每台设备最近一次登录的来源也这样另外放，不打印出去：局域网的地址（`SeenFrom`）和经隧道来的
//! 公钥（`TunnelSeen`）各留最近一个，回环地址不记。监听重开时据此认出连过的手机。

use std::{io, net::IpAddr, path::PathBuf};

use iroh::EndpointId;
use runode_paths::Dirs;
use runode_protocol::{
    push::{ApnsEnv, PushService},
    remote::{Bytes, DeviceId, DeviceKind},
};
use serde::{Deserialize, Serialize};

use crate::{
    files::{no_home, private_options, read_json, write_json},
    limit::Source,
};

/// 设备表格式的版本，格式变了、旧版本读不懂时加一。
const TABLE_VERSION: u32 = 1;

/// 一台配对过的设备。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub device_id: DeviceId,
    /// 设备配对时报的名字。
    pub name: String,
    /// 配对时报的设备种类，定下它过了门禁之后能开哪些连接、发哪些请求。
    #[serde(default)]
    pub kind: DeviceKind,
    /// 设备的 P-256 公钥，X9.63 未压缩格式，登录时用它验签。
    pub public_key: Bytes,
    /// 配对的时刻，Unix 秒。
    pub paired_at: u64,
    /// 最近一次登录的时刻，Unix 秒；配对后还没登录过时等于 `paired_at`。
    pub last_seen: u64,
}

/// 一台设备登记的推送。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRegistration {
    pub device_id: DeviceId,
    /// 推送的凭据：APNs device token（十六进制），官方 App 是中转服务给的句柄。
    pub token: String,
    pub env: ApnsEnv,
    /// 推送经哪家送到手机。
    #[serde(default)]
    pub service: PushService,
    /// App 的 bundle id。
    pub bundle: String,
    /// 手机给这台电脑编的 UUID，推送时原样带回。
    pub machine: String,
    /// 手机上显示的这台电脑的名字。
    pub machine_name: String,
    /// 最近一次登记的时刻，Unix 秒。
    pub updated_at: u64,
}

/// 一台设备最近一次登录时从哪个地址连来。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SeenFrom {
    device_id: DeviceId,
    ip: IpAddr,
}

/// 一台设备最近一次经隧道登录时用的公钥。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TunnelSeen {
    device_id: DeviceId,
    /// 对端的 iroh 公钥，十六进制。
    node: String,
}

/// 一台设备上次登录时从哪儿来：最近的地址和最近的隧道公钥。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LastSeen {
    ip: Option<IpAddr>,
    node: Option<EndpointId>,
}

impl LastSeen {
    /// `source` 值得记下，又和上次记的不一样。
    pub(crate) fn is_new(&self, source: Source) -> bool {
        match source {
            Source::Ip(ip) => {
                source.identifies_device() && self.ip.map(|seen| seen.to_canonical()) != Some(ip.to_canonical())
            }
            Source::Tunnel(node) => self.node != Some(node),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Table {
    version: u32,
    devices: Vec<Device>,
    #[serde(default)]
    push: Vec<PushRegistration>,
    #[serde(default)]
    seen_from: Vec<SeenFrom>,
    #[serde(default)]
    tunnel_from: Vec<TunnelSeen>,
}

/// 从 `ip` 来的登录值不值得记下（`Source::identifies_device`）。
fn worth_remembering(ip: IpAddr) -> bool {
    Source::from(ip).identifies_device()
}

/// 所有配对过的设备，按配对的先后。
pub fn list_devices(dirs: &Dirs) -> io::Result<Vec<Device>> {
    Ok(read(dirs)?.devices)
}

/// 撤销设备 `id`：从表里删掉，连同它登记的推送，它连着的连接几秒内断开。表里没有它时返回 false。
pub fn revoke_device(dirs: &Dirs, id: DeviceId) -> io::Result<bool> {
    let mut revoked = false;
    update(dirs, |table| {
        let lens =
            |table: &Table| (table.devices.len(), table.push.len(), table.seen_from.len(), table.tunnel_from.len());
        let before = lens(table);
        table.devices.retain(|device| device.device_id != id);
        table.push.retain(|push| push.device_id != id);
        table.seen_from.retain(|seen| seen.device_id != id);
        table.tunnel_from.retain(|seen| seen.device_id != id);
        revoked = table.devices.len() != before.0;
        lens(table) != before
    })?;
    Ok(revoked)
}

/// 各台设备登记的推送，按登记的先后。
pub fn push_registrations(dirs: &Dirs) -> io::Result<Vec<PushRegistration>> {
    Ok(read(dirs)?.push)
}

/// 记下（或者换掉）设备 `registration.device_id` 登记的推送，别的设备登记的同一个 token 一起换掉：
/// 手机删掉这台电脑再重新配对后是一台新设备，旧的那条还登记着同一个 token，每条通知会推两遍。
/// 表里没有这台设备（刚被撤销）时不记，返回 false。
pub(crate) fn set_push(dirs: &Dirs, registration: PushRegistration) -> io::Result<bool> {
    update(dirs, |table| {
        if !table.devices.iter().any(|device| device.device_id == registration.device_id) {
            return false;
        }
        table.push.retain(|push| push.device_id != registration.device_id && push.token != registration.token);
        table.push.push(registration);
        true
    })
}

/// 删掉设备 `id` 登记的推送。本来就没有时返回 false。
pub(crate) fn clear_push(dirs: &Dirs, id: DeviceId) -> io::Result<bool> {
    update(dirs, |table| {
        let before = table.push.len();
        table.push.retain(|push| push.device_id != id);
        table.push.len() != before
    })
}

/// 设备 `id` 登记的 device token 还是 `token` 时，把它的环境改成 `env`：推送时发现登记的环境
/// 不对、换一个推成了。登记已经换了（手机又登记过）时不动，返回 false。
pub(crate) fn set_push_env(dirs: &Dirs, id: DeviceId, token: &str, env: ApnsEnv) -> io::Result<bool> {
    update(dirs, |table| {
        let Some(push) = table.push.iter_mut().find(|push| push.device_id == id && push.token == token) else {
            return false;
        };
        let changed = push.env != env;
        push.env = env;
        changed
    })
}

/// 设备 `id` 登记的 device token 还是 `token` 时删掉这条登记：两个环境的 APNs 都不认它。登记
/// 已经换了时不动，返回 false。
pub(crate) fn drop_push_token(dirs: &Dirs, id: DeviceId, token: &str) -> io::Result<bool> {
    update(dirs, |table| {
        let before = table.push.len();
        table.push.retain(|push| push.device_id != id || push.token != token);
        table.push.len() != before
    })
}

/// 表里的设备 `id`。
pub(crate) fn find(dirs: &Dirs, id: DeviceId) -> io::Result<Option<Device>> {
    Ok(find_with_address(dirs, id)?.map(|(device, _)| device))
}

/// 表里的设备 `id`，连同它上次登录时从哪儿来。记着的公钥解析不了时当作没记。
pub(crate) fn find_with_address(dirs: &Dirs, id: DeviceId) -> io::Result<Option<(Device, LastSeen)>> {
    let table = read(dirs)?;
    let last = LastSeen {
        ip: table.seen_from.iter().find(|seen| seen.device_id == id).map(|seen| seen.ip),
        node: table.tunnel_from.iter().find(|seen| seen.device_id == id).and_then(|seen| seen.node.parse().ok()),
    };
    Ok(table.devices.into_iter().find(|device| device.device_id == id).map(|device| (device, last)))
}

/// 加一台新配对的设备。
pub(crate) fn add(dirs: &Dirs, device: Device) -> io::Result<()> {
    update(dirs, |table| {
        table.devices.retain(|known| known.device_id != device.device_id);
        table.devices.push(device);
        true
    })
    .map(drop)
}

/// 记下设备 `id` 在 `now` 从 `from` 登录过。地址换掉它上次的地址，隧道公钥换掉它上次的公钥，另一样
/// 留着；回环地址不记，这台设备记着的回环地址一起删掉。
pub(crate) fn touch(dirs: &Dirs, id: DeviceId, now: u64, from: Source) -> io::Result<()> {
    update(dirs, |table| {
        let Some(device) = table.devices.iter_mut().find(|device| device.device_id == id) else { return false };
        device.last_seen = now;
        match from {
            Source::Ip(ip) if from.identifies_device() => {
                table.seen_from.retain(|seen| seen.device_id != id);
                table.seen_from.push(SeenFrom { device_id: id, ip });
            }
            Source::Ip(_) => table.seen_from.retain(|seen| seen.device_id != id || worth_remembering(seen.ip)),
            Source::Tunnel(node) => {
                table.tunnel_from.retain(|seen| seen.device_id != id);
                table.tunnel_from.push(TunnelSeen { device_id: id, node: node.to_string() });
            }
        }
        true
    })
    .map(drop)
}

/// 表里各台设备上次登录时的来源，带是哪台设备：地址和隧道公钥各一个。回环地址和解析不了的公钥跳过。
pub(crate) fn remembered(dirs: &Dirs) -> io::Result<Vec<(DeviceId, Source)>> {
    let table = read(dirs)?;
    let ips = table
        .seen_from
        .into_iter()
        .filter(|seen| worth_remembering(seen.ip))
        .map(|seen| (seen.device_id, Source::from(seen.ip)));
    let nodes = table
        .tunnel_from
        .into_iter()
        .filter_map(|seen| Some((seen.device_id, Source::Tunnel(seen.node.parse().ok()?))));
    Ok(ips.chain(nodes).collect())
}

/// 删掉表里记着的回环地址。本来就没有时返回 false。
pub(crate) fn forget_loopback(dirs: &Dirs) -> io::Result<bool> {
    update(dirs, |table| {
        let before = table.seen_from.len();
        table.seen_from.retain(|seen| worth_remembering(seen.ip));
        table.seen_from.len() != before
    })
}

/// 设备表文件现在的样子（修改时刻和大小），没变时不必重新读。
pub(crate) fn stamp(dirs: &Dirs) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(dirs.remote_access_devices_file()?).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn path(dirs: &Dirs) -> io::Result<PathBuf> {
    dirs.remote_access_devices_file().ok_or_else(no_home)
}

fn read(dirs: &Dirs) -> io::Result<Table> {
    let table: Table = read_json(&path(dirs)?)?.unwrap_or_default();
    if table.version > TABLE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the paired device table is version {}, newer than this runode reads", table.version),
        ));
    }
    Ok(table)
}

/// 拿着锁读出设备表交给 `change`，它返回 true 时写回去。返回 `change` 的结果。
fn update(dirs: &Dirs, change: impl FnOnce(&mut Table) -> bool) -> io::Result<bool> {
    dirs.create_remote_access_dir()?;
    let lock_path = dirs.remote_access_devices_lock_file().ok_or_else(no_home)?;
    let lock = private_options().create(true).truncate(false).write(true).open(&lock_path)?;
    lock.lock()?;
    let mut table = read(dirs)?;
    let changed = change(&mut table);
    if changed {
        table.version = TABLE_VERSION;
        write_json(&path(dirs)?, &table)?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 几个线程同时各加一台设备：`update` 拿着锁读改写，谁加的都不会被别人写回去的旧表盖掉。
    #[test]
    fn devices_added_at_the_same_time_are_all_kept() {
        let root = std::env::temp_dir().join(format!("rra-devices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        let barrier = std::sync::Barrier::new(16);
        std::thread::scope(|scope| {
            for n in 0..16u8 {
                let (dirs, barrier) = (&dirs, &barrier);
                scope.spawn(move || {
                    let device = Device {
                        device_id: DeviceId([n; 16]),
                        name: format!("phone {n}"),
                        kind: DeviceKind::Mobile,
                        public_key: Bytes(vec![4; 65]),
                        paired_at: 0,
                        last_seen: 0,
                    };
                    barrier.wait();
                    add(dirs, device).unwrap();
                });
            }
        });
        let mut ids: Vec<u8> = list_devices(&dirs).unwrap().iter().map(|device| device.device_id.0[0]).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..16).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 同一台手机重新配对成了新设备、登记同一个 token 时，旧设备那条登记换掉；别的 token 不动。
    #[test]
    fn a_token_is_registered_once() {
        let root = std::env::temp_dir().join(format!("rra-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        let push = |n: u8, token: &str| PushRegistration {
            device_id: DeviceId([n; 16]),
            token: token.into(),
            env: ApnsEnv::Production,
            service: PushService::Apns,
            bundle: "dev.runode.ios".into(),
            machine: "mac".into(),
            machine_name: "Mac".into(),
            updated_at: 0,
        };
        for n in 1..=3 {
            add(
                &dirs,
                Device {
                    device_id: DeviceId([n; 16]),
                    name: format!("phone {n}"),
                    kind: DeviceKind::Mobile,
                    public_key: Bytes(vec![4; 65]),
                    paired_at: 0,
                    last_seen: 0,
                },
            )
            .unwrap();
        }
        assert!(set_push(&dirs, push(1, "aa")).unwrap());
        assert!(set_push(&dirs, push(3, "cc")).unwrap());
        assert!(set_push(&dirs, push(2, "aa")).unwrap());
        assert_eq!(push_registrations(&dirs).unwrap(), [push(3, "cc"), push(2, "aa")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 登记里没有 `service` 的旧表照样读得了，读成 APNs。
    #[test]
    fn registrations_without_a_service_are_apns() {
        let root = std::env::temp_dir().join(format!("rra-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        let device = DeviceId([1; 16]);
        let old = serde_json::json!({
            "version": 1,
            "devices": [],
            "push": [{
                "device_id": device, "token": "aa", "env": "production", "bundle": "dev.runode.mobile",
                "machine": "mac", "machine_name": "Mac", "updated_at": 0,
            }],
        });
        dirs.create_remote_access_dir().unwrap();
        std::fs::write(path(&dirs).unwrap(), old.to_string()).unwrap();
        let registrations = push_registrations(&dirs).unwrap();
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].service, PushService::Apns);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn phone(n: u8) -> Device {
        Device {
            device_id: DeviceId([n; 16]),
            name: format!("phone {n}"),
            kind: DeviceKind::Mobile,
            public_key: Bytes(vec![4; 65]),
            paired_at: 0,
            last_seen: 0,
        }
    }

    fn ip(text: &str) -> Source {
        text.parse::<IpAddr>().unwrap().into()
    }

    fn node(n: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[n; 32]).public()
    }

    /// 每台设备的地址和隧道公钥各只留最近一个，撤销时一起删；没有这两项的旧表照样读得了。
    #[test]
    fn only_the_latest_address_of_each_device_is_kept() {
        let root = std::env::temp_dir().join(format!("rra-seen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        let (one, two) = (DeviceId([1; 16]), DeviceId([2; 16]));
        add(&dirs, phone(1)).unwrap();
        add(&dirs, phone(2)).unwrap();
        assert!(remembered(&dirs).unwrap().is_empty());
        touch(&dirs, one, 10, ip("192.0.2.1")).unwrap();
        touch(&dirs, two, 10, ip("192.0.2.2")).unwrap();
        touch(&dirs, one, 20, ip("2001:db8::1")).unwrap();
        assert_eq!(remembered(&dirs).unwrap(), [(two, ip("192.0.2.2")), (one, ip("2001:db8::1"))]);
        // 隧道公钥另记一个，换掉上次的公钥，不动地址。
        touch(&dirs, two, 30, Source::Tunnel(node(1))).unwrap();
        touch(&dirs, two, 40, Source::Tunnel(node(2))).unwrap();
        assert_eq!(
            remembered(&dirs).unwrap(),
            [(two, ip("192.0.2.2")), (one, ip("2001:db8::1")), (two, Source::Tunnel(node(2)))]
        );
        let (device, last) = find_with_address(&dirs, two).unwrap().unwrap();
        assert_eq!(device.last_seen, 40);
        assert!(!last.is_new(ip("192.0.2.2")));
        assert!(!last.is_new(Source::Tunnel(node(2))));
        assert!(last.is_new(Source::Tunnel(node(1))));
        assert!(last.is_new(ip("192.0.2.3")));
        assert!(!last.is_new(ip("127.0.0.1")));
        // 回环地址只改登录的时刻，上次的地址留着。
        touch(&dirs, one, 50, ip("127.0.0.1")).unwrap();
        assert_eq!(read(&dirs).unwrap().seen_from.len(), 2);
        assert_eq!(find(&dirs, one).unwrap().unwrap().last_seen, 50);
        // 没登记的设备不记。
        touch(&dirs, DeviceId([3; 16]), 20, ip("192.0.2.3")).unwrap();
        touch(&dirs, DeviceId([3; 16]), 20, Source::Tunnel(node(3))).unwrap();
        assert_eq!(remembered(&dirs).unwrap().len(), 3);
        assert!(revoke_device(&dirs, two).unwrap());
        assert_eq!(remembered(&dirs).unwrap(), [(one, ip("2001:db8::1"))]);
        assert!(read(&dirs).unwrap().tunnel_from.is_empty());
        // 设备本身的样子（`runode remote list --json` 打印的）不带来源。
        touch(&dirs, one, 60, Source::Tunnel(node(1))).unwrap();
        let printed = serde_json::to_value(&list_devices(&dirs).unwrap()[0]).unwrap().to_string();
        assert!(!printed.contains("2001:db8::1") && !printed.contains(&node(1).to_string()), "{printed}");

        std::fs::write(path(&dirs).unwrap(), r#"{"version":1,"devices":[]}"#).unwrap();
        assert!(remembered(&dirs).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回环地址不当作来源记着：`touch` 删掉这台设备旧的回环地址，`forget_loopback` 删掉表里所有的，
    /// `remembered` 跳过还留着的和解析不了的公钥。
    #[test]
    fn loopback_addresses_are_not_remembered() {
        let root = std::env::temp_dir().join(format!("rra-loopback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        let (one, two) = (DeviceId([1; 16]), DeviceId([2; 16]));
        let old = serde_json::json!({
            "version": 1,
            "devices": [phone(1), phone(2)],
            "seen_from": [
                { "device_id": one, "ip": "127.0.0.1" },
                { "device_id": two, "ip": "::1" },
                { "device_id": two, "ip": "::ffff:127.0.0.1" },
                { "device_id": two, "ip": "192.0.2.2" },
            ],
            "tunnel_from": [
                { "device_id": one, "node": "not a key" },
                { "device_id": two, "node": node(2).to_string() },
            ],
        });
        dirs.create_remote_access_dir().unwrap();
        std::fs::write(path(&dirs).unwrap(), old.to_string()).unwrap();
        assert_eq!(remembered(&dirs).unwrap(), [(two, ip("192.0.2.2")), (two, Source::Tunnel(node(2)))]);
        assert!(find_with_address(&dirs, one).unwrap().unwrap().1.is_new(Source::Tunnel(node(1))));

        touch(&dirs, one, 10, ip("::1")).unwrap();
        assert_eq!(read(&dirs).unwrap().seen_from.len(), 3);
        assert!(forget_loopback(&dirs).unwrap());
        let seen: Vec<IpAddr> = read(&dirs).unwrap().seen_from.into_iter().map(|seen| seen.ip).collect();
        assert_eq!(seen, ["192.0.2.2".parse::<IpAddr>().unwrap()]);
        assert!(!forget_loopback(&dirs).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }
}
