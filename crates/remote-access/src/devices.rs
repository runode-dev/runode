//! 配对过的设备表：`remote_access_devices_file` 里的 JSON（0600），每台设备的标识、名字、公钥、
//! 配对和最近一次登录的时刻。监听方配对时加、登录时更新 `last_seen`，命令行撤销时删；改的一方都
//! 先拿 `remote_access_devices_lock_file` 的锁，读的一方不用（文件整个换掉写，读不到一半的）。

use std::{fs::OpenOptions, io, os::unix::fs::OpenOptionsExt as _, path::PathBuf};

use runode_paths::Dirs;
use runode_protocol::remote::{Bytes, DeviceId};
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

#[derive(Default, Serialize, Deserialize)]
struct Table {
    version: u32,
    devices: Vec<Device>,
}

/// 所有配对过的设备，按配对的先后。
pub fn list_devices(dirs: &Dirs) -> io::Result<Vec<Device>> {
    Ok(read(dirs)?.devices)
}

/// 撤销设备 `id`：从表里删掉，它连着的连接几秒内断开。表里没有它时返回 false。
pub fn revoke_device(dirs: &Dirs, id: DeviceId) -> io::Result<bool> {
    update(dirs, |table| {
        let before = table.devices.len();
        table.devices.retain(|device| device.device_id != id);
        table.devices.len() != before
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
