//! 有上限地读文件，以及判断文件读过之后变没变的版本戳。

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _},
    path::Path,
    time::SystemTime,
};

/// 文件的大小和修改时间。两个相等就当文件没变过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Stamp {
    pub len: u64,
    pub modified: SystemTime,
}

/// `path` 现在的版本戳；文件不在或读不到修改时间时为空。
pub fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some(Stamp { len: meta.len(), modified: meta.modified().ok()? })
}

/// 有上限地读到的内容。
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Limited {
    /// 至多上限那么多字节。
    pub bytes: Vec<u8>,
    /// 文件比上限长，后面没读。
    pub cut: bool,
}

/// 读 `path` 的前 `max` 个字节，只读普通文件。`whole` 时只要整个文件：比 `max` 大就不读，交回空的、
/// 标着 `cut` 的内容。
pub fn read_limited(path: &Path, max: u64, whole: bool) -> io::Result<Limited> {
    // 打开没有写端的命名管道会一直阻塞，读的上限救不了；设备文件的大小是 0，读起来却没有尽头。
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    if whole && meta.len() > max {
        return Ok(Limited { bytes: Vec::new(), cut: true });
    }
    // 看过大小后文件还可能变大，读的时候照样设上限，多读一个字节看超没超。
    let mut bytes = Vec::new();
    open_regular(path)?.take(max + 1).read_to_end(&mut bytes)?;
    let cut = bytes.len() as u64 > max;
    if cut && whole {
        bytes.clear();
    }
    bytes.truncate(max as usize);
    Ok(Limited { bytes, cut })
}

fn not_regular() -> io::Error {
    io::Error::other("not a regular file")
}

/// `O_NONBLOCK` 的值，和 libc 里的一样（标准库没有这个常量）；没列出的平台为空，照旧阻塞地打开。
#[cfg(unix)]
const O_NONBLOCK: Option<i32> = if cfg!(target_vendor = "apple") {
    Some(0x0004)
} else if cfg!(all(
    target_os = "linux",
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "arm",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "loongarch64"
    )
)) {
    Some(0o4000)
} else {
    None
};

/// 打开 `path`，打开的不是普通文件时报错。调用方先看过路径是普通文件，可看过以后路径还可能被换成
/// 别的（指向设备的符号链接、命名管道），所以打开以后对着打开的这个再看一次，读的才一定是普通文件。
/// 不阻塞地打开：换成了没有写端的命名管道时阻塞式的打开一直等，走不到这次检查；对普通文件这个标志
/// 不起作用。
fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    if let Some(flag) = O_NONBLOCK {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(flag);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 看过是普通文件以后路径被换成没有写端的命名管道：打开不阻塞，对着打开的这个看出不是普通文件。
    #[cfg(unix)]
    #[test]
    fn fifo_swapped_in_after_the_check_does_not_block() {
        use std::{process::Command, sync::mpsc, thread, time::Duration};

        let dir = std::env::temp_dir().join(format!("runode-files-fifo-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("notes.txt");
        fs::remove_file(&fifo).ok();
        assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(open_regular(&fifo).map(drop)).ok());
        let opened = rx.recv_timeout(Duration::from_secs(5)).expect("打开命名管道阻塞了");
        assert!(opened.is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
