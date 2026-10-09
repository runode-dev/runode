//! 配对过的设备表：`remote_access_devices_file` 里的 JSON（0600），每台设备的标识、名字、公钥、
//! 配对和最近一次登录的时刻。监听方配对时加、登录时更新 `last_seen`，命令行撤销时删；改的一方都
//! 先拿 `remote_access_devices_lock_file` 的锁，读的一方不用（文件整个换掉写，读不到一半的）。
//!
//! 同一个文件里还有手机登记的推送（`PushRegistration`，见 `runode_protocol::push`），按设备一条，和
//! `Device` 分开放：`runode remote list --json` 把 `Device` 整个打印出来，token 不该跟着出去。撤销设备时
//! 一起删。旧版本读得了带着它的表（不认识的字段忽略），但它改表时会把它丢掉，手机下次连上时重新登记。

use std::{fs::OpenOptions, io, os::unix::fs::OpenOptionsExt as _, path::PathBuf};

use runode_paths::Dirs;
use runode_protocol::{
    push::ApnsEnv,
    remote::{Bytes, DeviceId},
};
use serde::{Deserialize, Serialize};

use crate::files::{no_home, read_json, write_json};

/// 设备表格式的版本，格式变了、旧版本读不懂时加一。
const TABLE_VERSION: u32 = 1;

/// 一台配对过的设备。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub device_id: DeviceId,
    /// 设备配对时报的名字。
    pub name: String,
    /// 设备的 P-256 公钥，X9.63 未压缩格式，登录时用它验签。
    pub public_key: Bytes,
    /// 配对的时刻，Unix 秒。
    pub paired_at: u64,
    /// 最近一次登录的时刻，Unix 秒；配对后还没登录过时等于 `paired_at`。
    pub last_seen: u64,
}

/// 一台设备登记的推送，见 `runode_protocol::ClientMsg::PushRegister`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRegistration {
    pub device_id: DeviceId,
    /// push-to-start token，十六进制。
    pub token: String,
    pub env: ApnsEnv,
    /// App 的 bundle id。
    pub bundle: String,
    /// 手机给这台电脑编的 UUID，推送时原样带回。
    pub machine: String,
    /// 手机上显示的这台电脑的名字。
    pub machine_name: String,
    /// 最近一次登记的时刻，Unix 秒。
    pub updated_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Table {
    version: u32,
    devices: Vec<Device>,
    #[serde(default)]
    push: Vec<PushRegistration>,
}

/// 所有配对过的设备，按配对的先后。
pub fn list_devices(dirs: &Dirs) -> io::Result<Vec<Device>> {
    Ok(read(dirs)?.devices)
}

/// 撤销设备 `id`：从表里删掉，连同它登记的推送，它连着的连接几秒内断开。表里没有它时返回 false。
pub fn revoke_device(dirs: &Dirs, id: DeviceId) -> io::Result<bool> {
    let mut revoked = false;
    update(dirs, |table| {
        let before = (table.devices.len(), table.push.len());
        table.devices.retain(|device| device.device_id != id);
        table.push.retain(|push| push.device_id != id);
        revoked = table.devices.len() != before.0;
        (table.devices.len(), table.push.len()) != before
    })?;
    Ok(revoked)
}

/// 各台设备登记的推送，按登记的先后。
pub fn push_registrations(dirs: &Dirs) -> io::Result<Vec<PushRegistration>> {
    Ok(read(dirs)?.push)
}

/// 记下（或者换掉）设备 `registration.device_id` 登记的推送。表里没有这台设备（刚被撤销）时不记，
/// 返回 false。
pub(crate) fn set_push(dirs: &Dirs, registration: PushRegistration) -> io::Result<bool> {
    update(dirs, |table| {
        if !table.devices.iter().any(|device| device.device_id == registration.device_id) {
            return false;
        }
        table.push.retain(|push| push.device_id != registration.device_id);
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

/// 设备 `id` 登记的 push-to-start token 还是 `token` 时，把它的环境改成 `env`：推送时发现登记的环境
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

/// 设备 `id` 登记的 push-to-start token 还是 `token` 时删掉这条登记：两个环境的 APNs 都不认它。登记
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
    Ok(read(dirs)?.devices.into_iter().find(|device| device.device_id == id))
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

/// 记下设备 `id` 在 `now` 登录过。
pub(crate) fn touch(dirs: &Dirs, id: DeviceId, now: u64) -> io::Result<()> {
    update(dirs, |table| {
        let Some(device) = table.devices.iter_mut().find(|device| device.device_id == id) else { return false };
        device.last_seen = now;
        true
    })
    .map(drop)
}

/// 设备表文件现在的样子（修改时刻和大小），没变时不必重新读，见 `listener` 里撤销的检查。
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
    let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(&lock_path)?;
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
}
