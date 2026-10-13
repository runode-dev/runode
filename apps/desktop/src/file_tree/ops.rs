//! 文件树里改文件的操作：新建、改名、移动、复制和移到废纸篓。只碰文件系统，不碰界面；
//! 出错时交回 `OpError`，由界面换成提示。

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

/// 改文件失败的原因。
#[derive(Debug)]
pub(super) enum OpError {
    /// 目标位置已经有同名的了。
    Exists,
    /// 要把目录挪进或复制进它自己下面。
    IntoItself,
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
pub(super) fn child_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let name = Path::new(name.trim());
    let mut parts = name.components().peekable();
    parts.peek()?;
    parts.all(|part| matches!(part, Component::Normal(_))).then(|| dir.join(name))
}

/// 改名时输入的名字换成 `dir` 下的路径：和 `child_path` 一样，但只能是一段，不能挪到别的目录。
pub(super) fn child_name(dir: &Path, name: &str) -> Option<PathBuf> {
    child_path(dir, name).filter(|_| Path::new(name.trim()).components().count() == 1)
}

/// 新建空文件或目录，中间缺的目录一并建出来；已经有了时报 `Exists`。
pub(super) fn create(path: &Path, is_dir: bool) -> Result {
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
pub(super) fn rename(from: &Path, to: &Path) -> Result {
    if let Ok(existing) = fs::symlink_metadata(to)
        && !same_file(&existing, &fs::symlink_metadata(from)?)
    {
        return Err(OpError::Exists);
    }
    Ok(fs::rename(from, to)?)
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    false
}

/// 把 `src` 挪进目录 `dir`，返回挪过去的路径；本来就在 `dir` 里时不动。不能挪进它自己或者
/// 它下面的目录，`dir` 里已经有同名的时报 `Exists`。
pub(super) fn move_into(src: &Path, dir: &Path) -> Result<PathBuf> {
    let name = src.file_name().ok_or(OpError::IntoItself)?;
    let dest = dir.join(name);
    if dest == src {
        return Ok(dest);
    }
    if dir.starts_with(src) {
        return Err(OpError::IntoItself);
    }
    rename(src, &dest)?;
    Ok(dest)
}

/// 把 `src` 复制进目录 `dir`，返回副本的路径。重名时副本叫「名字 copy」「名字 copy 2」，
/// 扩展名留在最后。目录连同里面的东西一起复制，符号链接复制成链接本身。不能复制进它自己
/// 或它下面的目录，复制的过程中会一直读到新复制出来的东西。
pub(super) fn copy_into(src: &Path, dir: &Path) -> Result<PathBuf> {
    let name = src.file_name().ok_or(OpError::IntoItself)?;
    if dir.starts_with(src) {
        return Err(OpError::IntoItself);
    }
    let dest = free_name(dir, Path::new(name));
    copy_all(src, &dest)?;
    Ok(dest)
}

/// `dir` 里还没被占用的名字：`name` 本身，或者「名字 copy」「名字 copy 2」……
fn free_name(dir: &Path, name: &Path) -> PathBuf {
    let path = dir.join(name);
    if fs::symlink_metadata(&path).is_err() {
        return path;
    }
    let stem = name.file_stem().unwrap_or(name.as_os_str()).to_string_lossy();
    let ext = name.extension().map(|ext| format!(".{}", ext.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| if n == 1 { format!("{stem} copy{ext}") } else { format!("{stem} copy {n}{ext}") })
        .map(|name| dir.join(name))
        .find(|path| fs::symlink_metadata(path).is_err())
        .expect("总有一个没被占用的名字")
}

fn copy_all(src: &Path, dest: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_symlink() {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(fs::read_link(src)?, dest);
        #[cfg(not(unix))]
        return fs::copy(src, dest).map(drop);
    }
    if !meta.is_dir() {
        return fs::copy(src, dest).map(drop);
    }
    fs::create_dir(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        copy_all(&entry.path(), &dest.join(entry.file_name()))?;
    }
    Ok(())
}

/// 把仓库里的 `rel`（相对仓库根 `root`）写进根目录的 `.gitignore`：只匹配这一个路径，开头的 `/`
/// 钉在根目录上，目录结尾加 `/`，`*?[\` 和结尾的空格转义。已经有同样的一行时不再写。
pub(super) fn add_to_gitignore(root: &Path, rel: &Path, is_dir: bool) -> Result {
    let mut pattern = String::from("/");
    for (ix, part) in rel.components().enumerate() {
        if ix > 0 {
            pattern.push('/');
        }
        for c in part.as_os_str().to_string_lossy().chars() {
            if matches!(c, '*' | '?' | '[' | '\\') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
    }
    if pattern.ends_with(' ') {
        pattern.insert(pattern.len() - 1, '\\');
    }
    if is_dir {
        pattern.push('/');
    }
    let path = root.join(".gitignore");
    let existing = match fs::read_to_string(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        read => read?,
    };
    if existing.lines().any(|line| line == pattern) {
        return Ok(());
    }
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    let mut file = fs::OpenOptions::new().append(true).create(true).open(path)?;
    io::Write::write_all(&mut file, format!("{sep}{pattern}\n").as_bytes())?;
    Ok(())
}

/// 把 `path` 移到废纸篓，在访达里能放回原处。
#[cfg(target_os = "macos")]
pub(super) fn trash(path: &Path) -> Result {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|err| OpError::Io(io::Error::other(err.localizedDescription().to_string())))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn trash(_: &Path) -> Result {
    Err(OpError::Io(io::ErrorKind::Unsupported.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试一个空的临时目录，结束时删掉。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("runode-files-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn validates_new_names() {
        let dir = Path::new("/p");
        assert_eq!(child_path(dir, " a.txt "), Some(PathBuf::from("/p/a.txt")));
        assert_eq!(child_path(dir, "a/b/c.rs"), Some(PathBuf::from("/p/a/b/c.rs")));
        assert_eq!(child_path(dir, ""), None);
        assert_eq!(child_path(dir, "  "), None);
        assert_eq!(child_path(dir, "/etc/x"), None);
        assert_eq!(child_path(dir, "../x"), None);
        assert_eq!(child_path(dir, "a/../../x"), None);
        assert_eq!(child_name(dir, "b.rs"), Some(PathBuf::from("/p/b.rs")));
        assert_eq!(child_name(dir, "a/b.rs"), None);
    }

    #[test]
    fn creates_nested_entries_once() {
        let tmp = TempDir::new("create");
        let file = tmp.0.join("a/b/c.txt");
        create(&file, false).unwrap();
        assert!(file.is_file());
        assert!(matches!(create(&file, false), Err(OpError::Exists)));
        create(&tmp.0.join("d"), true).unwrap();
        assert!(tmp.0.join("d").is_dir());
    }

    #[test]
    fn renames_without_overwriting() {
        let tmp = TempDir::new("rename");
        let (a, b) = (tmp.0.join("a"), tmp.0.join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        assert!(matches!(rename(&a, &b), Err(OpError::Exists)));
        rename(&a, &tmp.0.join("A")).unwrap();
        assert_eq!(fs::read_to_string(tmp.0.join("A")).unwrap(), "a");
    }

    #[test]
    fn moves_into_dirs() {
        let tmp = TempDir::new("move");
        let (src, dir) = (tmp.0.join("src"), tmp.0.join("dir"));
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::create_dir(&dir).unwrap();
        assert!(matches!(move_into(&src, &src.join("inner")), Err(OpError::IntoItself)));
        assert_eq!(move_into(&src, &tmp.0).unwrap(), src);
        let moved = move_into(&src, &dir).unwrap();
        assert_eq!(moved, dir.join("src"));
        assert!(moved.join("inner").is_dir());
        assert!(!src.exists());
    }

    #[test]
    fn appends_to_gitignore_once() {
        let tmp = TempDir::new("gitignore");
        let file = tmp.0.join(".gitignore");
        fs::write(&file, "target").unwrap();
        add_to_gitignore(&tmp.0, Path::new("a/b*.log"), false).unwrap();
        add_to_gitignore(&tmp.0, Path::new("dist"), true).unwrap();
        add_to_gitignore(&tmp.0, Path::new("dist"), true).unwrap();
        add_to_gitignore(&tmp.0, Path::new("x "), false).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "target\n/a/b\\*.log\n/dist/\n/x\\ \n");
    }

    #[test]
    fn copies_with_free_names() {
        let tmp = TempDir::new("copy");
        let file = tmp.0.join("a.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(copy_into(&file, &tmp.0).unwrap(), tmp.0.join("a copy.txt"));
        assert_eq!(copy_into(&file, &tmp.0).unwrap(), tmp.0.join("a copy 2.txt"));
        let dir = tmp.0.join("d");
        fs::create_dir_all(dir.join("e")).unwrap();
        fs::write(dir.join("e/f"), "y").unwrap();
        // 复制到自己所在的目录可以，复制进自己下面不行。
        let copy = copy_into(&dir, &tmp.0).unwrap();
        assert_eq!(copy, tmp.0.join("d copy"));
        assert_eq!(fs::read_to_string(copy.join("e/f")).unwrap(), "y");
        assert!(matches!(copy_into(&dir, &dir.join("e")), Err(OpError::IntoItself)));
        assert!(matches!(copy_into(&dir, &dir), Err(OpError::IntoItself)));
    }
}
