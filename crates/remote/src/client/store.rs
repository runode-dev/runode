//! 配对过的电脑：`Dirs::paired_machines_file` 里的 JSON（`{"version":1,"machines":[…]}`，只有自己能读），
//! 每台是 `PairedMachine` 的字段加上这边的设备私钥。改的一方先拿旁边 `.lock` 文件的锁再读改写，文件整个
//! 换掉写，读的一方不用锁。

use std::{
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use runode_paths::Dirs;
use runode_protocol::remote::{Bytes, DeviceId};
use serde::{Deserialize, Serialize};

use super::PairedMachine;
use crate::files::{no_home, private_options, read_json, write_json};

/// 文件格式的版本，格式变了、旧版本读不懂时加一。
const FILE_VERSION: u32 = 1;

/// 一台配对过的电脑和连它用的私钥。
pub(super) struct Entry {
    pub(super) machine: PairedMachine,
    /// 设备私钥，ring 的 P-256 PKCS#8 DER。
    pub(super) key: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    host_name: String,
    fingerprint: Bytes,
    port: u16,
    addrs: Vec<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_addr: Option<SocketAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<String>,
    device_id: DeviceId,
    paired_at: u64,
    key: Bytes,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    version: u32,
    machines: Vec<Stored>,
}

impl Stored {
    /// 指纹的长度不对（文件被改坏了）时为 `None`。
    fn entry(&self) -> Option<Entry> {
        Some(Entry {
            machine: PairedMachine {
                id: self.id.clone(),
                host_name: self.host_name.clone(),
                fingerprint: self.fingerprint.0.as_slice().try_into().ok()?,
                port: self.port,
                addrs: self.addrs.clone(),
                last_addr: self.last_addr,
                node: self.node.clone(),
                device_id: self.device_id,
                paired_at: self.paired_at,
            },
            key: self.key.0.clone(),
        })
    }

    fn new(entry: Entry) -> Self {
        let Entry { machine, key } = entry;
        Self {
            id: machine.id,
            host_name: machine.host_name,
            fingerprint: Bytes(machine.fingerprint.to_vec()),
            port: machine.port,
            addrs: machine.addrs,
            last_addr: machine.last_addr,
            node: machine.node,
            device_id: machine.device_id,
            paired_at: machine.paired_at,
            key: Bytes(key),
        }
    }
}

/// 所有配对过的电脑，按配对的先后；读不懂的那几条跳过。
pub(super) fn list(dirs: &Dirs) -> io::Result<Vec<Entry>> {
    Ok(read(dirs)?.machines.iter().filter_map(Stored::entry).collect())
}

pub(super) fn find(dirs: &Dirs, id: &str) -> io::Result<Option<Entry>> {
    Ok(read(dirs)?.machines.iter().find(|stored| stored.id == id).and_then(Stored::entry))
}

/// 记下一台新配对的电脑。
pub(super) fn add(dirs: &Dirs, entry: Entry) -> io::Result<()> {
    update(dirs, |file| {
        file.machines.retain(|stored| stored.id != entry.machine.id);
        file.machines.push(Stored::new(entry));
        true
    })
    .map(drop)
}

/// 删掉 `id` 那台；本来就没有时返回 false。
pub(super) fn forget(dirs: &Dirs, id: &str) -> io::Result<bool> {
    update(dirs, |file| {
        let before = file.machines.len();
        file.machines.retain(|stored| stored.id != id);
        file.machines.len() != before
    })
}

/// 连上 `id` 那台以后记下它这次报的主机名、跨网络的地址和连上的地址（经跨网络连接时为空，不动上次的），
/// 返回记下以后的样子；已经被忘掉了时为 `None`。
pub(super) fn connected(
    dirs: &Dirs,
    id: &str,
    host_name: &str,
    node: Option<&str>,
    addr: Option<SocketAddr>,
) -> io::Result<Option<PairedMachine>> {
    let mut machine = None;
    update(dirs, |file| {
        let Some(stored) = file.machines.iter_mut().find(|stored| stored.id == id) else { return false };
        let last_addr = addr.or(stored.last_addr);
        let changed = stored.host_name != host_name || stored.node.as_deref() != node || stored.last_addr != last_addr;
        stored.host_name = host_name.into();
        stored.node = node.map(Into::into);
        stored.last_addr = last_addr;
        machine = stored.entry().map(|entry| entry.machine);
        changed
    })?;
    Ok(machine)
}

fn path(dirs: &Dirs) -> io::Result<PathBuf> {
    dirs.paired_machines_file().ok_or_else(no_home)
}

fn read(dirs: &Dirs) -> io::Result<File> {
    let file: File = read_json(&path(dirs)?)?.unwrap_or_default();
    if file.version > FILE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the paired computer list is version {}, newer than this runode reads", file.version),
        ));
    }
    Ok(file)
}

/// 拿着锁读出文件交给 `change`，它返回 true 时写回去。返回 `change` 的结果。
fn update(dirs: &Dirs, change: impl FnOnce(&mut File) -> bool) -> io::Result<bool> {
    dirs.create_remote_access_dir()?;
    let path = path(dirs)?;
    let lock = private_options().create(true).truncate(false).write(true).open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut file = read(dirs)?;
    let changed = change(&mut file);
    if changed {
        file.version = FILE_VERSION;
        write_json(&path, &file)?;
    }
    Ok(changed)
}
