//! 照 Kitty 的规矩读图形协议按文件传的图：只读普通文件，不读 `/proc`、`/sys` 和 `/dev`（`/dev/shm`
//! 除外）下的东西；临时文件只认临时目录里、路径带 `TEMPORARY_FILE_MARK` 的，读完删掉。

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

use super::Medium;

/// 临时文件（`t=t`）的真实路径里必须有的一段。
const TEMPORARY_FILE_MARK: &[u8] = b"tty-graphics-protocol";

/// 读出 `path` 从第 `offset` 个字节起的 `size` 个字节，`size` 是 0 时读到末尾；要读的超过 `max`
/// 个字节时不读，返回 `ErrorKind::FileTooLarge`。路径必须是绝对的。临时文件过了位置和名字的检查
/// 以后，不论读没读成（包括超过 `max`）都删掉。
pub(super) fn read_file(medium: Medium, path: &[u8], offset: u64, size: u64, max: u64) -> io::Result<Vec<u8>> {
    if path.first() != Some(&b'/') || path.contains(&0) {
        return Err(invalid("not an absolute path"));
    }
    // 打开 `/dev/fd/N`、`/proc/self/fd/N` 等于拿到宿主自己的描述符，真实路径是宿主打开的那个文件，
    // 打开以后的检查认不出来，读它还会挪动宿主的读写位置；所以先按请求的路径拒掉，带 `..` 的
    // 一并拒掉，免得绕过字面的比较。
    // shortcut: 指向这些路径的符号链接挡不住，要封住得用 `openat2(RESOLVE_NO_MAGICLINKS)` 或
    // `O_NOFOLLOW_ANY`，遇到真实程序传进链接或安全要求收紧时再换。
    let path = Path::new(OsStr::from_bytes(path));
    if is_system_path(path) || path.components().any(|part| part == Component::ParentDir) {
        return Err(invalid("a system file"));
    }
    // 先打开再按打开的这个文件查，中途换掉路径上的链接也骗不过。O_NONBLOCK 让打开 FIFO 时不等
    // 写端，不卡住会话的线程。
    let mut file =
        OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC).open(path)?;
    let real = real_path(&file)?;
    if is_system_path(&real) {
        return Err(invalid("a system file"));
    }
    if medium == Medium::File {
        return read_open(&mut file, offset, size, max);
    }
    if !in_temp_dir(&real) {
        return Err(invalid("a temporary file outside the temporary directories"));
    }
    if memchr::memmem::find(real.as_os_str().as_bytes(), TEMPORARY_FILE_MARK).is_none() {
        return Err(invalid("a temporary file not named as one"));
    }
    let result = read_open(&mut file, offset, size, max);
    if let Err(err) = fs::remove_file(&real) {
        tracing::debug!("failed to delete a Kitty graphics temporary file: {err}");
    }
    result
}

/// `/proc`、`/sys` 和 `/dev`（`/dev/shm` 除外）下的路径。按路径的各段比，`//` 和 `/./` 骗不过；
/// macOS 的根卷默认不分大小写，`/DEV` 一样进得了 devfs，所以那边各段不分大小写地比。
fn is_system_path(path: &Path) -> bool {
    let same = |part: &OsStr, name: &str| {
        let part = part.as_bytes();
        if cfg!(target_os = "macos") { part.eq_ignore_ascii_case(name.as_bytes()) } else { part == name.as_bytes() }
    };
    let mut parts = path.components().filter_map(|part| match part {
        Component::Normal(part) => Some(part),
        _ => None,
    });
    match parts.next() {
        Some(top) if same(top, "proc") || same(top, "sys") => true,
        Some(top) if same(top, "dev") => !parts.next().is_some_and(|next| same(next, "shm")),
        _ => false,
    }
}

fn read_open(file: &mut File, offset: u64, size: u64, max: u64) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("not a regular file"));
    }
    if metadata.len() == 0 {
        return Err(invalid("an empty file"));
    }
    let wanted = if size > 0 { size } else { metadata.len().saturating_sub(offset) };
    if wanted > max {
        return Err(too_large());
    }
    file.seek(SeekFrom::Start(offset))?;
    // 文件在读的时候可能还在变大，读到末尾时也只多读一个字节来认出超了上限。
    let limit = if size > 0 { size } else { max.saturating_add(1) };
    let mut data = Vec::new();
    file.take(limit).read_to_end(&mut data)?;
    let read = data.len() as u64;
    if size > 0 && read < size {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "shorter than the requested size"));
    }
    if read > max {
        return Err(too_large());
    }
    Ok(data)
}

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::FileTooLarge, "more than allowed to read")
}

/// 打开的文件现在的真实路径：链接都解开了。
#[cfg(target_os = "macos")]
fn real_path(file: &File) -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::fd::AsRawFd, os::unix::ffi::OsStringExt};

    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH 往 `buf` 里写以 NUL 结尾的路径，最长 PATH_MAX 个字节，`buf` 正好这么大。
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    buf.truncate(len);
    Ok(PathBuf::from(OsString::from_vec(buf)))
}

/// 打开的文件现在的真实路径：链接都解开了。
#[cfg(target_os = "linux")]
fn real_path(file: &File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;

    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// 真实路径 `path` 在 Kitty 认的临时目录里。
pub(super) fn in_temp_dir(path: &Path) -> bool {
    path.starts_with("/tmp")
        || path.starts_with("/dev/shm")
        || (cfg!(target_os = "macos") && path.starts_with("/private/tmp"))
        || fs::canonicalize(std::env::temp_dir()).is_ok_and(|dir| path.starts_with(dir))
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}
