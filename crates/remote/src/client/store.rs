//! 配对过的电脑：`Dirs::paired_machines_file` 里的 JSON（`{"version":1,"machines":[…]}`，只有自己能读），
//! 每台是 `PairedMachine` 的字段加上这边的设备私钥和跨网络连它用的 iroh 私钥。改的一方先拿旁边 `.lock`
//! 文件的锁再读改写，文件整个换掉写，读的一方不用锁。

use std::{
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use runode_paths::Dirs;
use runode_protocol::remote::{Bytes, DeviceId};
use serde::{Deserialize, Serialize};

use super::{PairedMachine, tunnel::TunnelKey};
use crate::files::{no_home, private_options, random, read_json, write_json};

/// 文件格式的版本，格式变了、旧版本读不懂时加一。
const FILE_VERSION: u32 = 1;

/// 一台配对过的电脑和连它用的私钥。
pub(super) struct Entry {
    pub(super) machine: PairedMachine,
    /// 设备私钥，ring 的 P-256 PKCS#8 DER。
    pub(super) key: Vec<u8>,
    /// 跨网络连它用的 iroh 私钥，和设备私钥一起生成、一起删。文件里没有或长度不对时为 `None`，由
    /// `tunnel_key` 补上或报错。
    pub(super) tunnel_key: Option<TunnelKey>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tunnel_key: Option<Bytes>,
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
            tunnel_key: self.tunnel_key.as_ref().and_then(|bytes| bytes.0.as_slice().try_into().ok()),
        })
    }

    fn new(entry: Entry) -> Self {
        let Entry { machine, key, tunnel_key } = entry;
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
            tunnel_key: tunnel_key.map(|bytes| Bytes(bytes.to_vec())),
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

/// 跨网络连 `id` 那台用的 iroh 私钥，记录里还没有时生成一把存下。存着的长度不对时报错，不悄悄换掉：换了
/// 那台电脑就认不出这边了。已经被忘掉了时报 `NotFound`。
pub(super) fn tunnel_key(dirs: &Dirs, id: &str) -> io::Result<TunnelKey> {
    let mut key = Err(io::Error::new(io::ErrorKind::NotFound, format!("no paired computer {id}")));
    update(dirs, |file| {
        let Some(stored) = file.machines.iter_mut().find(|stored| stored.id == id) else { return false };
        if let Some(bytes) = &stored.tunnel_key {
            key = bytes.0.as_slice().try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, format!("the iroh key kept for {id} is not a key"))
            });
            return false;
        }
        key = random();
        let Ok(generated) = &key else { return false };
        stored.tunnel_key = Some(Bytes(generated.to_vec()));
        true
    })?;
    key
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

#[cfg(test)]
mod tests {
    use runode_protocol::remote::FINGERPRINT_LEN;

    use super::*;

    /// 临时的数据目录，丢掉时删掉。
    struct Temp {
        root: PathBuf,
        dirs: Dirs,
    }

    impl Temp {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("rrs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
            Self { root, dirs }
        }

        fn file(&self) -> PathBuf {
            path(&self.dirs).unwrap()
        }

        fn json(&self) -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(self.file()).unwrap()).unwrap()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn entry(id: &str, tunnel_key: Option<TunnelKey>) -> Entry {
        Entry {
            machine: PairedMachine {
                id: id.into(),
                host_name: "那台电脑".into(),
                fingerprint: [1; FINGERPRINT_LEN],
                port: 7000,
                addrs: Vec::new(),
                last_addr: None,
                node: Some("node".into()),
                device_id: DeviceId([2; 16]),
                paired_at: 1,
            },
            key: vec![3; 8],
            tunnel_key,
        }
    }

    /// 配对时交进来的那把原样存下、原样读回。
    #[test]
    fn the_key_kept_at_pairing_is_read_back() {
        let temp = Temp::new("kept");
        add(&temp.dirs, entry("a", Some([9; 32]))).unwrap();
        assert_eq!(find(&temp.dirs, "a").unwrap().unwrap().tunnel_key, Some([9; 32]));
        assert_eq!(tunnel_key(&temp.dirs, "a").unwrap(), [9; 32]);
    }

    /// 记录里没有 iroh 私钥时补生成一把存下，之后读回的是同一把，别的字段不动；
    /// 两台电脑各是各的。
    #[test]
    fn a_missing_key_is_generated_once_per_machine() {
        let temp = Temp::new("missing");
        add(&temp.dirs, entry("a", None)).unwrap();
        add(&temp.dirs, entry("b", None)).unwrap();
        let before = temp.json();
        assert!(before["machines"][0].get("tunnel_key").is_none(), "{before}");
        assert_eq!(find(&temp.dirs, "a").unwrap().unwrap().tunnel_key, None);

        let a = tunnel_key(&temp.dirs, "a").unwrap();
        assert_eq!(tunnel_key(&temp.dirs, "a").unwrap(), a);
        assert_eq!(find(&temp.dirs, "a").unwrap().unwrap().tunnel_key, Some(a));
        let b = tunnel_key(&temp.dirs, "b").unwrap();
        assert_ne!(a, b);

        let mut after = temp.json();
        for machine in after["machines"].as_array_mut().unwrap() {
            assert!(machine.as_object_mut().unwrap().remove("tunnel_key").is_some(), "{machine}");
        }
        assert_eq!(after, before);
    }

    /// 忘掉了的那台没有私钥可给。
    #[test]
    fn a_forgotten_machine_has_no_key() {
        let temp = Temp::new("forgotten");
        add(&temp.dirs, entry("a", Some([9; 32]))).unwrap();
        assert!(forget(&temp.dirs, "a").unwrap());
        assert_eq!(tunnel_key(&temp.dirs, "a").unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!temp.json().to_string().contains("tunnel_key"));
    }

    /// 存着的私钥长度不对、或者整个文件读不懂时报错，文件原样不动，不换成新生成的。
    #[test]
    fn a_bad_key_or_file_is_not_replaced() {
        let temp = Temp::new("bad");
        add(&temp.dirs, entry("a", None)).unwrap();
        let mut json = temp.json();
        json["machines"][0]["tunnel_key"] = serde_json::to_value(Bytes(vec![1; 5])).unwrap();
        std::fs::write(temp.file(), serde_json::to_vec(&json).unwrap()).unwrap();
        let written = std::fs::read(temp.file()).unwrap();
        assert_eq!(find(&temp.dirs, "a").unwrap().unwrap().tunnel_key, None);
        assert_eq!(tunnel_key(&temp.dirs, "a").unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(temp.file()).unwrap(), written);

        std::fs::write(temp.file(), b"{not json").unwrap();
        assert_eq!(tunnel_key(&temp.dirs, "a").unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(temp.file()).unwrap(), b"{not json");
    }
}
