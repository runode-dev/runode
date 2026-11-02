//! 远程访问的文件怎么读写：只有自己能读（Unix 上 0600，Windows 上靠目录的权限），写的时候先写到旁边的
//! 临时文件再换上去，读的一方不会读到写了一半的；另外是取随机数。

use std::{fs::OpenOptions, io, path::Path};

use ring::rand::{SecureRandom as _, SystemRandom};

/// 新建的文件只有自己能读写（0600）。
#[cfg(unix)]
pub(crate) fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// 新文件继承 `remote_access_dir` 的权限：只有本人、SYSTEM 和管理员能进。
#[cfg(windows)]
pub(crate) fn private_options() -> OpenOptions {
    OpenOptions::new()
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

/// 把 `value` 写成 JSON 整个换进 `path`，只有自己能读写。
pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    runode_paths::replace_private(path, &bytes)
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
