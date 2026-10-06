//! 远程访问的文件怎么读写：只有自己能读（0600），写的时候先写到旁边的临时文件再换上去，读的一方
//! 不会读到写了一半的；另外是取随机数。

use std::{
    fs::OpenOptions,
    io::{self, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use ring::rand::{SecureRandom as _, SystemRandom};

/// 把 `bytes` 整个换进 `path`，权限 0600。
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let written = (|| {
        let mut file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temp)?;
        // 已经有的临时文件（上次没清掉的）沿用它原来的权限，收回来。
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// 读整个文件；文件不存在时为 `None`。
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// 读 JSON 文件；文件不存在时为 `None`。
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    read_optional(path)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)))
        .transpose()
}

/// 把 `value` 写成 JSON 整个换进 `path`，见 `write_private`。
pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_private(path, &bytes)
}

/// `N` 个系统随机字节。
pub(crate) fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    SystemRandom::new().fill(&mut bytes).map_err(|_| io::Error::other("the system random source failed"))?;
    Ok(bytes)
}

/// 路径没有时的错误：没有家目录（`runode_paths::Dirs` 的字段为 `None`）。
pub(crate) fn no_home() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no home directory to keep remote access files in")
}
