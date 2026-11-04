//! 改文件的操作：不覆盖已有的、不挪进自己下面，名字按原样的字节处理。

#[cfg(unix)]
use std::io;
use std::{
    fs,
    path::{Path, PathBuf},
};

use runode_files::ops::{OpError, add_to_gitignore, child_name, child_path, copy_into, create, move_into, rename};
#[cfg(unix)]
use runode_files::ops::{remove, unchanged_name};

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

/// 开关一下改名框不改名：非 UTF-8 的名字、首尾带空白的名字填进框里再读回来都不一样了。
#[cfg(unix)]
#[test]
fn untouched_rename_keeps_lossy_names() {
    use std::os::unix::ffi::OsStrExt as _;
    let path = Path::new(std::ffi::OsStr::from_bytes(b"/p/a\xff.txt"));
    assert!(unchanged_name(path, &path.file_name().unwrap().to_string_lossy()));
    assert!(unchanged_name(Path::new("/p/ b.txt "), "b.txt"));
    assert!(!unchanged_name(path, "a.txt"));
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
    assert!(matches!(add_to_gitignore(&tmp.0, Path::new("x\n*"), false), Err(OpError::NewlineInName)));
    assert!(matches!(add_to_gitignore(&tmp.0, Path::new("a\r/b"), false), Err(OpError::NewlineInName)));
    assert_eq!(fs::read_to_string(&file).unwrap(), "target\n/a/b\\*.log\n/dist/\n/x\\ \n");
}

/// 指向目录的符号链接按文件写（不加结尾的 `/`）：git 的 `dir/` 规则只匹配真正的目录。
#[cfg(unix)]
#[test]
fn gitignore_treats_directory_symlinks_as_files() {
    let tmp = TempDir::new("gitignore-symlink");
    fs::create_dir(tmp.0.join("real")).unwrap();
    std::os::unix::fs::symlink("real", tmp.0.join("link")).unwrap();
    add_to_gitignore(&tmp.0, Path::new("link"), true).unwrap();
    add_to_gitignore(&tmp.0, Path::new("real"), true).unwrap();
    assert_eq!(fs::read(tmp.0.join(".gitignore")).unwrap(), b"/link\n/real/\n");
}

/// 名字不是合法 UTF-8 时规则照原样的字节写，git 读 .gitignore 也按字节，才匹配得到那个文件。
#[cfg(unix)]
#[test]
fn gitignore_keeps_raw_bytes_in_names() {
    use std::os::unix::ffi::OsStrExt as _;
    let tmp = TempDir::new("gitignore-raw");
    let raw = Path::new(std::ffi::OsStr::from_bytes(b"caf\xe9*"));
    add_to_gitignore(&tmp.0, raw, false).unwrap();
    add_to_gitignore(&tmp.0, raw, false).unwrap();
    assert_eq!(fs::read(tmp.0.join(".gitignore")).unwrap(), b"/caf\xe9\\*\n");
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

/// 目录里有管道时整个复制报错、不留半个副本，不会卡在打开管道上等人来写。
#[cfg(unix)]
#[test]
fn refuses_to_copy_fifos() {
    let tmp = TempDir::new("copy-fifo");
    let dir = tmp.0.join("d");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a"), "a").unwrap();
    assert!(std::process::Command::new("mkfifo").arg(dir.join("pipe")).status().unwrap().success());
    let err = copy_into(&dir, &tmp.0).unwrap_err();
    assert!(matches!(&err, OpError::Io(err) if err.kind() == io::ErrorKind::InvalidInput), "{err:?}");
    assert!(!tmp.0.join("d copy").exists());
}

/// 读不了源文件时报错，不留下空的副本。
#[cfg(unix)]
#[test]
fn failed_file_copy_leaves_no_copy() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = TempDir::new("copy-unreadable");
    let file = tmp.0.join("a.txt");
    fs::write(&file, "x").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    // root 不受权限限制，测不了。
    if fs::File::open(&file).is_ok() {
        return;
    }
    assert!(copy_into(&file, &tmp.0).is_err());
    assert!(fs::symlink_metadata(tmp.0.join("a copy.txt")).is_err());
}

/// 经符号链接指进自己下面的目录也算放进自己，挪动、复制都拦；源本身是符号链接时照旧只动链接。
#[cfg(unix)]
#[test]
fn refuses_targets_inside_the_source_through_symlinks() {
    let tmp = TempDir::new("symlink-into");
    let src = tmp.0.join("src");
    fs::create_dir_all(src.join("inner")).unwrap();
    let alias = tmp.0.join("alias");
    std::os::unix::fs::symlink(src.join("inner"), &alias).unwrap();
    assert!(matches!(copy_into(&src, &alias), Err(OpError::IntoItself)));
    assert!(matches!(move_into(&src, &alias), Err(OpError::IntoItself)));
    assert!(!src.join("inner/src").exists());

    let link = tmp.0.join("link");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    let copy = copy_into(&link, &src.join("inner")).unwrap();
    assert!(fs::symlink_metadata(&copy).unwrap().is_symlink());
    assert_eq!(move_into(&link, &src).unwrap(), src.join("link"));
}

#[cfg(unix)]
#[test]
fn removes_links_without_following_them() {
    let dir = TempDir::new("remove");
    let (target, link, nested) = (dir.0.join("target"), dir.0.join("link"), dir.0.join("tree/a/b"));
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep.txt"), "x").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    remove(&link).unwrap();
    assert!(!link.exists() && target.join("keep.txt").exists());
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("f.txt"), "x").unwrap();
    remove(&dir.0.join("tree")).unwrap();
    assert!(!dir.0.join("tree").exists());
    assert!(matches!(remove(&dir.0.join("tree")), Err(OpError::Io(err)) if err.kind() == io::ErrorKind::NotFound));
}
