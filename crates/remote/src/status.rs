//! 监听方开着没有、在哪：监听方开着时一直锁着 `remote_access_lock_file`，开好后把端口、证书指纹
//! 和主机名写进 `remote_access_status_file`，之后每有设备连上或断开就把正连着的设备重写进去。命令行
//! 据此拼配对 URI，或者告诉用户远程访问没开；桌面 app 的标题栏据此显示连着哪些设备。

use std::{
    fs::{File, TryLockError},
    io,
};

use runode_paths::Dirs;
use runode_protocol::remote::{Bytes, DeviceId};
use serde::{Deserialize, Serialize};

use crate::files::{no_home, private_options, read_json};

/// 开着的监听方的状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerStatus {
    pub port: u16,
    /// 证书指纹，见 `runode_protocol::remote::FINGERPRINT_LEN`。
    pub fingerprint: Bytes,
    pub host_name: String,
    /// 正连着（过了门禁）的设备，每台一次，按连上的先后。
    #[serde(default)]
    pub connected: Vec<DeviceId>,
    /// 跨网络连接的地址，没开成时为空，见 `runode_protocol::remote::PairingUri::node`。
    #[serde(default)]
    pub node: Option<String>,
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
///
/// 拿到锁就删掉上一个监听方没清掉的状态文件（它没正常停下）：从这里到开好监听、写上新状态之间锁已经
/// 在手里，`listener_status` 不然会把上一个的端口和指纹当成开着的报出去。
pub(crate) fn lock_listener(dirs: &Dirs) -> io::Result<Option<File>> {
    dirs.create_remote_access_dir()?;
    let path = dirs.remote_access_lock_file().ok_or_else(no_home)?;
    let file = private_options().create(true).truncate(false).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Error(err)) => return Err(err),
    }
    match std::fs::remove_file(dirs.remote_access_status_file().ok_or_else(no_home)?) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(Some(file)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::write_json;

    /// 上一个监听方没正常停下留了状态文件：新的拿到锁、还没写上自己的状态时，不报那份旧的。
    #[test]
    fn a_stale_status_is_not_reported_once_the_lock_is_taken() {
        let root = std::env::temp_dir().join(format!("rra-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        dirs.create_remote_access_dir().unwrap();
        let stale = ListenerStatus {
            port: 7000,
            fingerprint: Bytes(vec![1; 32]),
            host_name: "old".to_owned(),
            connected: Vec::new(),
            node: None,
        };
        write_json(&dirs.remote_access_status_file().unwrap(), &stale).unwrap();

        let _lock = lock_listener(&dirs).unwrap().unwrap();

        assert_eq!(listener_status(&dirs).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
