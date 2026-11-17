//! 改项目里文件的操作：新建、改名、移动、复制和移到废纸篓。出错时交回 `OpError`，由调用方换成提示。

use std::{
    ffi::{OsStr, OsString},
    fs, io,
    path::{Component, Path, PathBuf},
};

/// 改文件失败的原因。
#[derive(Debug)]
pub enum OpError {
    /// 目标位置已经有同名的了。
    Exists,
    /// 要把目录挪进或复制进它自己下面。
    IntoItself,
    /// 名字里有换行或回车，没法写进按行分的 `.gitignore`。
    NewlineInName,
    Io(io::Error),
}

impl From<io::Error> for OpError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::AlreadyExists { Self::Exists } else { Self::Io(err) }
    }
}

type Result<T = ()> = std::result::Result<T, OpError>;

/// 在 `dir` 下新建时输入的名字换成路径。可以带 `/` 一次建出几层，但不能是绝对路径，
/// 也不能有 `.`、`..` 跳出 `dir`；空的或者不合规时为空。
pub fn child_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = Path::new(name.trim());
    let mut parts = name.components().peekable();
    parts.peek()?;
    parts.all(|part| matches!(part, Component::Normal(_))).then(|| dir.join(name))
}

/// 改名时输入的名字换成 `dir` 下的路径：只能是一段，不能挪到别的目录；不合规时为空。
pub fn child_name(dir: &Path, name: &str) -> Option<PathBuf> {
    child_path(dir, name).filter(|_| Path::new(name.trim()).components().count() == 1)
}

/// 改名框里的 `name` 和打开时填进去的一样（都去掉首尾空白比）。框里填的是有损的名字：非 UTF-8 的字节
/// 换成了 `�`，读回来又去了首尾空白，原样拿去改名会把这样的名字悄悄改掉。
pub fn unchanged_name(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|old| old.to_string_lossy().trim() == name.trim())
}

/// 新建空文件或目录，中间缺的目录一并建出来；已经有了时报 `Exists`。
pub fn create(path: &Path, is_dir: bool) -> Result {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if is_dir {
        fs::create_dir(path)?;
    } else {
        fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    }
    Ok(())
}

/// 把 `from` 改名成 `to`。`to` 已经有了时报 `Exists`，只改大小写除外：不分大小写的文件系统上
/// `to` 查到的就是 `from` 自己。
pub fn rename(from: &Path, to: &Path) -> Result {
    if fs::symlink_metadata(to).is_ok() {
        if !same_file(from, to)? {
            return Err(OpError::Exists);
        }
        return Ok(fs::rename(from, to)?);
    }
    Ok(rename_noreplace(from, to)?)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

/// 把 `from` 改名成 `to`，`to` 已经有了时报 `AlreadyExists`：查和改是一步，不会盖掉别的操作刚建出来的同名文件。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (c_from, c_to) = (c_path(from)?, c_path(to)?);
    // SAFETY: 两个都是以 NUL 结尾的路径。
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(c_from.as_ptr(), c_to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::renameat2(libc::AT_FDCWD, c_from.as_ptr(), libc::AT_FDCWD, c_to.as_ptr(), libc::RENAME_NOREPLACE)
    };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // 文件系统不支持不覆盖的改名（网络卷、FAT 这类）时退回先查后改。
    if matches!(err.raw_os_error(), Some(libc::ENOTSUP | libc::ENOSYS | libc::EINVAL)) {
        return rename_checked(from, to);
    }
    Err(err)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    rename_checked(from, to)
}

/// shortcut: 先查后改，查和改之间别人建出来的同名文件会被盖掉；只在系统或文件系统没有不覆盖的改名时用，
/// 要支持这样的平台时换成它自己的原子改名。
fn rename_checked(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    fs::rename(from, to)
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let (a, b) = (fs::symlink_metadata(a)?, fs::symlink_metadata(b)?);
    Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
}

/// Windows 上取不到稳定的文件号，按规范化后的路径比：规范化给的是盘上的真名，只差大小写的两个名字
/// 规范化后一样。
#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(fs::canonicalize(a)? == fs::canonicalize(b)?)
}

/// 把 `src` 挪进目录 `dir`，返回挪过去的路径；本来就在 `dir` 里时不动。不能挪进它自己或者
/// 它下面的目录，`dir` 里已经有同名的时报 `Exists`。`dir` 在另一个卷上时先复制过去再删掉 `src`，
/// 删 `src` 失败时报错，复制出来的那份留着。
pub fn move_into(src: &Path, dir: &Path) -> Result<PathBuf> {
    let name = src.file_name().ok_or(OpError::IntoItself)?;
    let dest = dir.join(name);
    if dest == src {
        return Ok(dest);
    }
    if inside(src, dir) {
        return Err(OpError::IntoItself);
    }
    match rename(src, &dest) {
        Err(OpError::Io(err)) if err.kind() == io::ErrorKind::CrossesDevices => move_across(src, &dest)?,
        result => result?,
    }
    Ok(dest)
}

/// 跨卷挪动：把 `src` 复制成 `dest` 再删掉 `src`，`dest` 已经有了时报 `Exists`。复制失败时 `src` 不动、
/// 不留半个副本；删 `src` 失败时 `dest` 已是完整的一份，留着它，只报错。
fn move_across(src: &Path, dest: &Path) -> Result {
    copy_all(src, dest)?;
    remove(src)
}

/// 把 `src` 复制进目录 `dir`，返回副本的路径。重名时副本叫「名字 copy」「名字 copy 2」，
/// 扩展名留在最后。目录连同里面的东西一起复制，符号链接复制成链接本身。不能复制进它自己
/// 或它下面的目录，复制的过程中会一直读到新复制出来的东西。
pub fn copy_into(src: &Path, dir: &Path) -> Result<PathBuf> {
    let name = src.file_name().ok_or(OpError::IntoItself)?;
    if inside(src, dir) {
        return Err(OpError::IntoItself);
    }
    // 挑好的名字不算占住：两次粘贴同时进行时会挑到同一个。建的时候不覆盖已有的，撞上了换下一个名字。
    for _ in 0..8 {
        let dest = free_name(dir, Path::new(name));
        match copy_all(src, &dest) {
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|()| dest).map_err(OpError::Io),
        }
    }
    Err(OpError::Exists)
}

/// `dir` 是 `src` 自己或者在它下面。文件树把指向目录的符号链接也当目的地，所以再按真实路径比一次；
/// `src` 本身是符号链接时挪动、复制的是链接本身，只按字面路径比。
fn inside(src: &Path, dir: &Path) -> bool {
    if dir.starts_with(src) {
        return true;
    }
    if fs::symlink_metadata(src).is_ok_and(|meta| meta.is_symlink()) {
        return false;
    }
    match (src.canonicalize(), dir.canonicalize()) {
        (Ok(src), Ok(dir)) => dir.starts_with(src),
        _ => false,
    }
}

/// `dir` 里还没被占用的名字：`name` 本身，或者「名字 copy」「名字 copy 2」……
fn free_name(dir: &Path, name: &Path) -> PathBuf {
    let path = dir.join(name);
    if fs::symlink_metadata(&path).is_err() {
        return path;
    }
    (1..)
        .map(|n| if n == 1 { " copy".to_owned() } else { format!(" copy {n}") })
        .map(|insert| dir.join(insert_before_extension(name.as_os_str(), &insert)))
        .find(|path| fs::symlink_metadata(path).is_err())
        .expect("总有一个没被占用的名字")
}

/// 在 `name` 的扩展名前插上 `insert`，没有扩展名时接在最后；名字里不成 UTF-8 的字节原样保留。
fn insert_before_extension(name: &OsStr, insert: &str) -> OsString {
    let path = Path::new(name);
    let mut out = path.file_stem().unwrap_or(name).to_owned();
    out.push(insert);
    if let Some(ext) = path.extension() {
        out.push(".");
        out.push(ext);
    }
    out
}

/// 把 `src` 连同里面的东西复制成 `dest`，权限照旧；`dest` 已经有了时报 `AlreadyExists`、不碰它。
/// 失败时只清掉这次自己建出来的。
fn copy_all(src: &Path, dest: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_symlink() {
        let target = fs::read_link(src)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, dest);
        #[cfg(windows)]
        return if is_symlink_dir(&meta) {
            std::os::windows::fs::symlink_dir(target, dest)
        } else {
            std::os::windows::fs::symlink_file(target, dest)
        };
    }
    if meta.is_file() {
        return copy_file(src, dest);
    }
    // 管道、套接字和设备文件不复制：打开管道会一直等到有人来写，设备文件可能读不到头。
    if !meta.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{}: not a regular file", src.display())));
    }
    // 复制的过程中只有自己进得去，里面的都复制完了才设上源的权限：只读的目录先设上就写不进去了。
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dest)?;
    // 中途失败时把这次建出来的半个副本清掉，免得留下残缺的目录，再试一次又多出一份。
    copy_entries(src, dest).inspect_err(|_| {
        let _ = fs::remove_dir_all(dest);
    })?;
    // 设不了权限的文件系统（FAT 这类）照样算复制成功。
    let _ = fs::set_permissions(dest, meta.permissions());
    Ok(())
}

fn copy_entries(src: &Path, dest: &Path) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        copy_all(&entry.path(), &dest.join(entry.file_name()))?;
    }
    Ok(())
}

/// 复制一个文件，`dest` 已经有了时报 `AlreadyExists`，不截断它（`fs::copy` 会截断）。
#[cfg(target_os = "macos")]
fn copy_file(src: &Path, dest: &Path) -> io::Result<()> {
    use std::os::{fd::AsRawFd as _, unix::fs::OpenOptionsExt as _};
    // `<sys/clonefile.h>` 的两个标志，libc 没有导出。不带 `CLONE_ACL` 时副本的 ACL 随目标目录，源上的 ACL 丢掉。
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_ACL: u32 = 0x0004;
    let (c_src, c_dest) = (c_path(src)?, c_path(dest)?);
    // 同一个卷上克隆，连同权限、ACL 和扩展属性；要么建好、要么什么都没建。
    // SAFETY: 两个都是以 NUL 结尾的路径。
    if unsafe { libc::clonefile(c_src.as_ptr(), c_dest.as_ptr(), CLONE_NOFOLLOW | CLONE_ACL) } == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if !matches!(err.raw_os_error(), Some(libc::EXDEV | libc::ENOTSUP)) {
        return Err(err);
    }
    // 克隆不了时自己独占地建出 `dest` 再往里复制，出错时删的一定是这次建的。复制完权限、ACL 前只有自己读得到。
    let input = fs::File::open(src)?;
    let out = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dest)?;
    // SAFETY: 两个描述符都开着，不传状态。
    let copied = unsafe {
        libc::fcopyfile(
            input.as_raw_fd(),
            out.as_raw_fd(),
            std::ptr::null_mut(),
            libc::COPYFILE_METADATA | libc::COPYFILE_DATA,
        )
    };
    let result = if copied == 0 { Ok(()) } else { Err(io::Error::last_os_error()) };
    drop(out);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

#[cfg(not(target_os = "macos"))]
fn copy_file(src: &Path, dest: &Path) -> io::Result<()> {
    let mut input = fs::File::open(src)?;
    let meta = input.metadata()?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    // 复制完设上源的权限之前只有自己读得到。
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(dest)?;
    let result = io::copy(&mut input, &mut out).and_then(|_| out.set_permissions(meta.permissions()));
    // 修改时间跟着源走；有的文件系统设不了，设不了不算复制失败。
    if result.is_ok()
        && let Ok(modified) = meta.modified()
    {
        let _ = out.set_modified(modified);
    }
    // `dest` 是上面刚建的，复制到一半出错时删掉，不留半个副本。Windows 上要先关了才删得掉。
    drop(out);
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

/// 把仓库里的 `rel`（相对仓库根 `root`）写进根目录的 `.gitignore`：只匹配这一个路径，开头的 `/`
/// 钉在根目录上，目录结尾加 `/`，`*?[\` 和结尾的空格转义。已经有同样的一行时不再写。名字里有换行或回车时
/// 报 `NewlineInName`：写进去会拆成几行，成了别的规则。
pub fn add_to_gitignore(root: &Path, rel: &Path, is_dir: bool) -> Result {
    if rel.as_os_str().as_encoded_bytes().iter().any(|b| matches!(b, b'\n' | b'\r')) {
        return Err(OpError::NewlineInName);
    }
    // 按原样的字节拼，不经 UTF-8：名字不是合法 UTF-8 时 lossy 转出的规则匹配不到那个文件。
    let mut pattern = b"/".to_vec();
    for (ix, part) in rel.components().enumerate() {
        if ix > 0 {
            pattern.push(b'/');
        }
        for &b in part.as_os_str().as_encoded_bytes() {
            if matches!(b, b'*' | b'?' | b'[' | b'\\') {
                pattern.push(b'\\');
            }
            pattern.push(b);
        }
    }
    if pattern.ends_with(b" ") {
        pattern.insert(pattern.len() - 1, b'\\');
    }
    // 文件树把指向目录的符号链接也当目录，但 git 的 `dir/` 规则只匹配真正的目录，链接要按文件写。
    if is_dir && !fs::symlink_metadata(root.join(rel)).is_ok_and(|meta| meta.is_symlink()) {
        pattern.push(b'/');
    }
    let path = root.join(".gitignore");
    let existing = match fs::read(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        read => read?,
    };
    if existing.split(|b| *b == b'\n').any(|line| line.strip_suffix(b"\r").unwrap_or(line) == pattern) {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new().append(true).create(true).open(path)?;
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        pattern.insert(0, b'\n');
    }
    pattern.push(b'\n');
    io::Write::write_all(&mut file, &pattern)?;
    Ok(())
}

/// 删掉 `path`，不进废纸篓、不能恢复：是目录时连同里面的一起删，是符号链接时只删链接本身。
pub fn remove(path: &Path) -> Result {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else if is_symlink_dir(&meta) {
        // 指向目录的链接要按目录删，删掉的只是链接。
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// `meta` 是指向目录的链接（包括目录联接）。只有 Windows 的链接分指向文件和指向目录两种，别的系统上总是 false。
#[cfg(windows)]
fn is_symlink_dir(meta: &fs::Metadata) -> bool {
    std::os::windows::fs::FileTypeExt::is_symlink_dir(&meta.file_type())
}

#[cfg(not(windows))]
fn is_symlink_dir(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(target_os = "linux")]
mod freedesktop;
#[cfg(windows)]
mod recycle_bin;

/// 把 `path` 移到废纸篓（回收站），在访达、资源管理器或文件管理器里能放回原处。`home_trash` 是 Linux 上家目录的
/// 回收站（`Dirs::trash_dir`），别的系统不用。找不到能用的回收站时报 `Unsupported`。
#[cfg(target_os = "macos")]
pub fn trash(path: &Path, _home_trash: Option<&Path>) -> Result {
    use objc2_foundation::NSFileManager;

    let url = file_url(path).map_err(OpError::Io)?;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|err| OpError::Io(io::Error::other(err.localizedDescription().to_string())))
}

/// `path` 原样的字节拼成的文件 URL，不经 UTF-8：名字不是合法 UTF-8 时不会指到名字恰好是替换字符的那个文件上。
#[cfg(target_os = "macos")]
fn file_url(path: &Path) -> io::Result<objc2::rc::Retained<objc2_foundation::NSURL>> {
    let c = c_path(path)?;
    // SAFETY: `c` 在调用期间一直活着，并且以 NUL 结尾。不按目录拼：废纸篓只按路径找，指向目录的符号链接
    // 不带结尾的斜杠也不会顺着链接删到目标。
    Ok(unsafe {
        objc2_foundation::NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
            std::ptr::NonNull::new(c.as_ptr().cast_mut()).expect("CString 的指针不为空"),
            false,
            None,
        )
    })
}

#[cfg(target_os = "linux")]
pub fn trash(path: &Path, home_trash: Option<&Path>) -> Result {
    freedesktop::trash(path, home_trash).map_err(OpError::Io)
}

#[cfg(windows)]
pub fn trash(path: &Path, _home_trash: Option<&Path>) -> Result {
    recycle_bin::trash(path).map_err(OpError::Io)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn trash(_: &Path, _: Option<&Path>) -> Result {
    Err(OpError::Io(io::ErrorKind::Unsupported.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 插在扩展名前，不成 UTF-8 的字节不被换成替换字符。
    #[cfg(unix)]
    #[test]
    fn inserting_keeps_the_bytes_of_the_name() {
        use std::os::unix::ffi::OsStrExt as _;
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        assert_eq!(insert_before_extension(name, " copy").as_bytes(), b"caf\xe9 copy.txt");
        assert_eq!(insert_before_extension(OsStr::new("Makefile"), ".2"), "Makefile.2");
        assert_eq!(insert_before_extension(OsStr::new(".env"), " copy"), ".env copy");
    }

    #[cfg(windows)]
    #[test]
    fn trash_recycles_on_a_fixed_disk() {
        let path = std::env::temp_dir().join(format!("runode-trash-{}.txt", std::process::id()));
        fs::write(&path, "x").unwrap();
        trash(&path, None).unwrap();
        assert!(!path.exists());
    }

    /// 关了回收站的盘上系统会直接删掉，不能让它删。要先在临时目录所在的盘上关掉回收站，所以默认不跑。
    #[cfg(windows)]
    #[test]
    #[ignore = "needs the recycle bin turned off on the temp directory's drive"]
    fn trash_refuses_when_the_recycle_bin_is_off() {
        let path = std::env::temp_dir().join(format!("runode-trash-off-{}.txt", std::process::id()));
        fs::write(&path, "x").unwrap();
        let err = trash(&path, None).unwrap_err();
        assert!(matches!(err, OpError::Io(err) if err.kind() == io::ErrorKind::Unsupported));
        assert!(path.exists());
        fs::remove_file(&path).unwrap();
    }

    /// 网络共享上没有回收站，不能让系统直接删掉。经本机的管理共享 `C$` 访问，有的机器关了它，所以默认不跑。
    #[cfg(windows)]
    #[test]
    #[ignore = "needs the C$ admin share"]
    fn trash_refuses_network_shares() {
        let local = std::env::temp_dir().join(format!("runode-trash-share-{}.txt", std::process::id()));
        fs::write(&local, "x").unwrap();
        let rest = local.to_string_lossy().strip_prefix(r"C:\").unwrap().to_owned();
        let shared = PathBuf::from(format!(r"\\localhost\C$\{rest}"));
        let err = trash(&shared, None).unwrap_err();
        assert!(matches!(err, OpError::Io(err) if err.kind() == io::ErrorKind::Unsupported));
        assert!(local.exists());
        fs::remove_file(&local).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trash_urls_keep_raw_bytes() {
        use std::os::unix::ffi::OsStrExt as _;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/a\xff"));
        let url = file_url(path).unwrap();
        // SAFETY: `fileSystemRepresentation` 返回以 NUL 结尾、跟着 `url` 活着的 C 字符串。
        let back = unsafe { std::ffi::CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };
        assert_eq!(back.to_bytes(), b"/tmp/a\xff");
    }

    /// 复制和改名都不覆盖已有的：同时粘贴同一个文件时各得一个副本，改名不盖掉别的文件。
    #[test]
    fn copies_and_renames_never_overwrite() {
        let tmp = std::env::temp_dir().join(format!("runode-files-no-overwrite-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let (a, b) = (tmp.join("a"), tmp.join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        assert_eq!(copy_file(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(rename_noreplace(&a, &b).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&b).unwrap(), "b");

        let dir = tmp.join("out");
        fs::create_dir(&dir).unwrap();
        let copies: std::collections::HashSet<_> = std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..6).map(|_| scope.spawn(|| copy_into(&a, &dir).unwrap())).collect();
            jobs.into_iter().map(|job| job.join().unwrap()).collect()
        });
        assert_eq!(copies.len(), 6);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 6);
        assert!(copies.iter().all(|copy| fs::read_to_string(copy).unwrap() == "a"));
        fs::remove_dir_all(&tmp).ok();
    }

    fn scratch(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("runode-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    /// 跨卷挪动后东西都在新位置，原处没了。
    #[test]
    fn moving_across_volumes_copies_then_removes() {
        let tmp = scratch("move-across");
        let file = tmp.join("a.txt");
        fs::write(&file, "a").unwrap();
        let dir = tmp.join("d");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/b.txt"), "b").unwrap();
        let out = tmp.join("out");
        fs::create_dir(&out).unwrap();

        move_across(&file, &out.join("a.txt")).unwrap();
        move_across(&dir, &out.join("d")).unwrap();
        assert!(fs::symlink_metadata(&file).is_err());
        assert!(fs::symlink_metadata(&dir).is_err());
        assert_eq!(fs::read_to_string(out.join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(out.join("d/sub/b.txt")).unwrap(), "b");
        fs::remove_dir_all(&tmp).ok();
    }

    /// 挪过去的目录权限照旧，只有自己进得去的目录不会变成别人也能进。
    #[cfg(unix)]
    #[test]
    fn moving_across_volumes_keeps_directory_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = scratch("move-across-mode");
        let src = tmp.join("private");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o700)).unwrap();
        let dest = tmp.join("out");

        move_across(&src, &dest).unwrap();
        assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "a");
        fs::remove_dir_all(&tmp).ok();
    }

    /// 源复制不了、还没建出 `dest` 时，别人在 `dest` 放的东西不动。
    #[test]
    fn moving_across_volumes_leaves_a_dest_it_did_not_make() {
        let tmp = scratch("move-across-missing");
        let (src, dest) = (tmp.join("gone"), tmp.join("b"));
        fs::write(&dest, "b").unwrap();
        assert!(move_across(&src, &dest).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "b");
        fs::remove_dir_all(&tmp).ok();
    }

    /// 源读不了、`dest` 已经有了时报错，`dest` 原样留着。
    #[cfg(unix)]
    #[test]
    fn copying_an_unreadable_file_leaves_an_existing_dest() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = scratch("copy-unreadable");
        let (src, dest) = (tmp.join("a"), tmp.join("b"));
        fs::write(&src, "a").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        fs::write(&dest, "b").unwrap();
        assert!(copy_all(&src, &dest).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "b");
        fs::remove_dir_all(&tmp).ok();
    }

    /// 复制出来的文件权限照旧，同一个卷上克隆和跨卷复制都是。
    #[cfg(unix)]
    #[test]
    fn copying_keeps_file_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = scratch("copy-mode");
        let src = tmp.join("a");
        fs::write(&src, "a").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
        copy_all(&src, &tmp.join("b")).unwrap();
        assert_eq!(fs::metadata(tmp.join("b")).unwrap().permissions().mode() & 0o777, 0o640);
        fs::remove_dir_all(&tmp).ok();
    }

    /// 复制出来的文件带着源上拒绝某个用户读的 ACL 项。
    #[cfg(target_os = "macos")]
    #[test]
    fn copying_keeps_the_acl() {
        let tmp = scratch("copy-acl");
        let src = tmp.join("a");
        fs::write(&src, "a").unwrap();
        let status = std::process::Command::new("chmod").arg("+a").arg("user:nobody deny read").arg(&src).status();
        assert!(status.unwrap().success());
        copy_all(&src, &tmp.join("b")).unwrap();
        let listing = std::process::Command::new("ls").arg("-le").arg(tmp.join("b")).output().unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.lines().any(|line| line.ends_with(" deny read")), "{listing}");
        fs::remove_dir_all(&tmp).ok();
    }

    /// 目标已经有了时不碰它，源也不动。
    #[test]
    fn moving_across_volumes_never_overwrites() {
        let tmp = scratch("move-across-exists");
        let (src, dest) = (tmp.join("a"), tmp.join("b"));
        fs::write(&src, "a").unwrap();
        fs::write(&dest, "b").unwrap();
        assert!(matches!(move_across(&src, &dest), Err(OpError::Exists)));
        assert_eq!(fs::read_to_string(&src).unwrap(), "a");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "b");
        fs::remove_dir_all(&tmp).ok();
    }

    /// 复制到一半失败时不留半个副本，源不动。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn moving_across_volumes_keeps_the_source_when_copying_fails() {
        use std::os::unix::fs::FileTypeExt as _;
        let tmp = scratch("move-across-fails");
        let src = tmp.join("d");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        let fifo = c_path(&src.join("pipe")).unwrap();
        // SAFETY: 以 NUL 结尾的路径。
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let dest = tmp.join("out");

        assert!(matches!(move_across(&src, &dest), Err(OpError::Io(_))));
        assert!(fs::symlink_metadata(&dest).is_err());
        assert_eq!(fs::read_to_string(src.join("a.txt")).unwrap(), "a");
        assert!(fs::symlink_metadata(src.join("pipe")).unwrap().file_type().is_fifo());
        fs::remove_dir_all(&tmp).ok();
    }
}
