//! 按 freedesktop.org 的回收站规范把文件移到回收站，文件管理器里能还原：和家目录在同一个文件系统上的进家目录的
//! 回收站，别的进那个文件系统顶层的 `.Trash/<uid>` 或 `.Trash-<uid>`。

use std::{
    fs,
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

/// 把 `path` 移到回收站。`home_trash` 是家目录的回收站（`Dirs::trash_dir`），没有时只试文件系统顶层的。
pub(super) fn trash(path: &Path, home_trash: Option<&Path>) -> io::Result<()> {
    // 上一级规范化成真实路径，经链接进了别的文件系统时才找得到那个文件系统的顶层；被删的这一项本身是链接时移走的
    // 仍是链接。
    let path = std::path::absolute(path)?;
    let name = path.file_name().ok_or_else(unsupported)?.to_owned();
    let path = fs::canonicalize(path.parent().ok_or_else(unsupported)?)?.join(name);
    let dev = fs::symlink_metadata(&path)?.dev();
    if let Some(home) = home_trash {
        // 家目录的回收站还没建时按它所在的目录比：建出来以后也在那个文件系统上。
        let home_dev = home.ancestors().find_map(|dir| fs::metadata(dir).ok()).map(|meta| meta.dev());
        if home_dev == Some(dev) {
            return move_into(home, &path, &path);
        }
    }
    let top = mount_top(&path, dev);
    let rel = path.strip_prefix(&top).unwrap_or(&path).to_path_buf();
    // SAFETY: getuid 不会失败，也不碰内存。
    let uid = unsafe { libc::getuid() };
    let shared = top.join(".Trash");
    if shared_trash_usable(&shared) {
        let dir = shared.join(uid.to_string());
        if make_private_dir(&dir).is_ok() && move_into(&dir, &path, &rel).is_ok() {
            return Ok(());
        }
    }
    let own = top.join(format!(".Trash-{uid}"));
    make_private_dir(&own).map_err(|_| unsupported())?;
    move_into(&own, &path, &rel)
}

/// 找不到能用的回收站：调用方提示这个系统上移不到回收站。
fn unsupported() -> io::Error {
    io::ErrorKind::Unsupported.into()
}

/// `path` 所在文件系统的顶层：往上走到上一级换了设备号的那一层。
fn mount_top(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.parent().unwrap_or(path).to_path_buf();
    while let Some(parent) = top.parent() {
        match fs::metadata(parent) {
            Ok(meta) if meta.dev() == dev => top = parent.to_path_buf(),
            _ => break,
        }
    }
    top
}

/// 管理员建好的共用回收站 `$topdir/.Trash`：规范要求是真的目录（不是链接）、带粘滞位，否则不用。
fn shared_trash_usable(dir: &Path) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & 0o1000 != 0)
}

/// 建只有自己能进的目录。已经有了时要是真的目录（不是链接）、属主是自己：共用的 `$topdir` 人人可写时，别的用户
/// 事先建好的同名目录不能用，不然移进去的文件他能删、能看，还能把里面的 `files` 换成链接引到别处。
fn make_private_dir(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(err) if err.kind() != io::ErrorKind::AlreadyExists => Err(err),
        _ => {
            let meta = fs::symlink_metadata(dir)?;
            // SAFETY: getuid 不会失败，也不碰内存。
            let mine = meta.uid() == unsafe { libc::getuid() };
            if meta.is_dir() && mine { Ok(()) } else { Err(unsupported()) }
        }
    }
}

/// 把 `path` 移进回收站 `trash` 的 `files`，在 `info` 里记下原来的位置 `recorded`（家目录的回收站记绝对路径，
/// 文件系统顶层的记相对顶层的路径）。名字被占了时换成 `名字.2.扩展名` 这样的；先用独占创建占下 `info` 里的
/// 那一份，两个同时删同名文件的不会互相盖掉。
fn move_into(trash: &Path, path: &Path, recorded: &Path) -> io::Result<()> {
    let (files, info) = (trash.join("files"), trash.join("info"));
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&files)?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&info)?;
    let name = path.file_name().ok_or_else(unsupported)?;
    let content = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", url_escape(recorded), deletion_date());
    for n in 1..10_000 {
        let candidate = numbered(name, n);
        let mut info_name = candidate.clone();
        info_name.push(".trashinfo");
        let info_file = info.join(info_name);
        let mut file = match fs::OpenOptions::new().write(true).create_new(true).open(&info_file) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let moved =
            file.write_all(content.as_bytes()).and_then(|()| super::rename_noreplace(path, &files.join(&candidate)));
        match moved {
            Ok(()) => return Ok(()),
            Err(err) => {
                let _ = fs::remove_file(&info_file);
                if err.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// 回收站里的第 `n` 个同名：第一个原样，之后在扩展名前插上编号。
fn numbered(name: &std::ffi::OsStr, n: u32) -> std::ffi::OsString {
    if n == 1 {
        return name.to_owned();
    }
    let path = Path::new(name);
    let stem = path.file_stem().unwrap_or(name).to_string_lossy();
    match path.extension() {
        Some(ext) => format!("{stem}.{n}.{}", ext.to_string_lossy()).into(),
        None => format!("{stem}.{n}").into(),
    }
}

/// 规范要的写法：路径按字节转义成 URL 那样，字母数字和 `/-._~` 照写，别的写成 `%XX`。
fn url_escape(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut out = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 现在的本地时间，写成规范要的 `YYYY-MM-DDThh:mm:ss`。
fn deletion_date() -> String {
    // SAFETY: `tm` 是全零初始化的普通结构，localtime_r 只往里写；`now` 在调用期间一直活着。
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-trash-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn moves_into_the_home_trash_and_records_where_it_was() {
        let root = temp_dir("home");
        let trash_dir = root.join("Trash");
        let file = root.join("a b.txt");
        fs::write(&file, "x").unwrap();
        trash(&file, Some(&trash_dir)).unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read_to_string(trash_dir.join("files/a b.txt")).unwrap(), "x");
        let info = fs::read_to_string(trash_dir.join("info/a b.txt.trashinfo")).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"));
        assert!(info.contains("a%20b.txt\nDeletionDate="));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_second_file_with_the_same_name_gets_a_number() {
        let root = temp_dir("twice");
        let trash_dir = root.join("Trash");
        for _ in 0..2 {
            fs::write(root.join("notes.md"), "x").unwrap();
            trash(&root.join("notes.md"), Some(&trash_dir)).unwrap();
        }
        assert!(trash_dir.join("files/notes.md").exists());
        assert!(trash_dir.join("files/notes.2.md").exists());
        assert!(trash_dir.join("info/notes.2.md.trashinfo").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn paths_are_escaped_byte_by_byte() {
        assert_eq!(url_escape(Path::new("/tmp/a b/项.txt")), "/tmp/a%20b/%E9%A1%B9.txt");
    }
}
