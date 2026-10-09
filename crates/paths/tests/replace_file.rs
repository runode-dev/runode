//! 覆盖写用户的文件：内容整个换掉，符号链接留着、写到它指向的文件，权限照旧，不留临时文件。

#![cfg(unix)]

use std::{
    os::unix::fs::{PermissionsExt as _, symlink},
    path::PathBuf,
};

use runode_paths::replace_file;

/// 这个测试自己的空目录。
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-replace-file-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn replaces_the_contents_and_keeps_the_permissions() {
    let dir = scratch("plain");
    let file = dir.join("config.conf");
    std::fs::write(&file, "font-size = 14\nmuch longer than what replaces it\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

    replace_file(&file, b"font-size = 16\n").unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "font-size = 16\n");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(entries(&dir), ["config.conf"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn creates_a_file_that_is_not_there_yet() {
    let dir = scratch("new");
    let file = dir.join("AGENTS.md");
    replace_file(&file, b"hello\n").unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
    assert_eq!(entries(&dir), ["AGENTS.md"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn writes_through_a_symlink_and_keeps_the_link() {
    let dir = scratch("link");
    let dotfiles = dir.join("dotfiles");
    std::fs::create_dir(&dotfiles).unwrap();
    let real = dotfiles.join("config.conf");
    std::fs::write(&real, "old\n").unwrap();
    // 链接到链接，第二层用相对路径，像 dotfiles 管理器常做的那样。
    let middle = dir.join("middle.conf");
    symlink(&real, &middle).unwrap();
    let link = dir.join("config.conf");
    symlink("middle.conf", &link).unwrap();

    replace_file(&link, b"new\n").unwrap();

    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), PathBuf::from("middle.conf"));
    assert!(std::fs::symlink_metadata(&middle).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new\n");
    assert_eq!(entries(&dotfiles), ["config.conf"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_dangling_symlink_gets_its_target_created() {
    let dir = scratch("dangling");
    let real = dir.join("real.conf");
    let link = dir.join("config.conf");
    symlink(&real, &link).unwrap();

    replace_file(&link, b"made\n").unwrap();

    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "made\n");
    std::fs::remove_dir_all(&dir).unwrap();
}
