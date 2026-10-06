//! 监听方开着没有、在哪：监听方开着时一直锁着 `remote_access_lock_file`，开好后把端口、证书指纹
//! 和主机名写进 `remote_access_status_file`。命令行据此拼配对 URI，或者告诉用户远程访问没开。

use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    os::unix::fs::OpenOptionsExt as _,
};

use runode_paths::Dirs;
use runode_protocol::remote::Bytes;
use serde::{Deserialize, Serialize};

use crate::files::{no_home, read_json};

/// 开着的监听方的状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerStatus {
    /// 监听方所在的进程：桌面 app，或者单独跑的宿主（`runode --host`）。
    pub pid: u32,
    pub port: u16,
    /// 证书指纹，见 `runode_protocol::remote::FINGERPRINT_LEN`。
    pub fingerprint: Bytes,
    pub host_name: String,
}

/// 开着的监听方的状态；没有监听方在跑时为 `None`。
pub fn listener_status(dirs: &Dirs) -> io::Result<Option<ListenerStatus>> {
    let lock = dirs.remote_access_lock_file().ok_or_else(no_home)?;
    let file = match File::open(&lock) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    // 拿得到共享锁说明没有监听方拿着独占锁；拿到的锁随 `file` 一起放开。
    match file.try_lock_shared() {
        Ok(()) => return Ok(None),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(err)) => return Err(err),
    }
    read_json(&dirs.remote_access_status_file().ok_or_else(no_home)?)
}

/// 监听方的锁：拿到了才开监听，一直拿到停下。另一个进程（另一个 app、或者交接时还没退出的旧宿主）
/// 拿着时返回 `None`。
pub(crate) fn lock_listener(dirs: &Dirs) -> io::Result<Option<File>> {
    dirs.create_remote_access_dir()?;
    let path = dirs.remote_access_lock_file().ok_or_else(no_home)?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}
