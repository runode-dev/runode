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

/// 原文件拒绝某个用户读的 ACL 项，换上后还在。
#[cfg(target_os = "macos")]
#[test]
fn keeps_the_extended_acl() {
    let dir = scratch("acl");
    let file = dir.join("config.conf");
    std::fs::write(&file, "old\n").unwrap();
    let status =
        std::process::Command::new("chmod").arg("+a").arg("user:nobody deny read").arg(&file).status().unwrap();
    assert!(status.success());

    replace_file(&file, b"new\n").unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
    let listing = std::process::Command::new("ls").arg("-le").arg(&file).output().unwrap();
    let listing = String::from_utf8_lossy(&listing.stdout);
    // 目录服务一时答不上时 `ls` 把用户写成 UUID，不写名字，所以只认这一项。
    assert!(
        listing.lines().any(|line| line.trim_start().starts_with("0: ") && line.ends_with(" deny read")),
        "{listing}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// 原文件的 POSIX ACL 换上后还在，属组的权限位仍是 mask。没装 `setfacl` 或文件系统不支持 ACL 时跳过。
#[cfg(target_os = "linux")]
#[test]
fn keeps_the_posix_acl() {
    let dir = scratch("acl");
    let file = dir.join("config.conf");
    std::fs::write(&file, "old\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let set = std::process::Command::new("setfacl").arg("-m").arg("u:nobody:r").arg(&file).status();
    if !set.is_ok_and(|status| status.success()) {
        std::fs::remove_dir_all(&dir).unwrap();
        return;
    }
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;

    replace_file(&file, b"new\n").unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
    let listing = std::process::Command::new("getfacl").arg("-c").arg(&file).output().unwrap();
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.contains("user:nobody:r--"), "{listing}");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, mode);
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

#[test]
fn a_symlink_loop_is_an_error_and_stays_a_link() {
    let dir = scratch("loop");
    let link = dir.join("config.conf");
    symlink("config.conf", &link).unwrap();

    assert!(replace_file(&link, b"new\n").is_err());

    assert_eq!(std::fs::read_link(&link).unwrap(), PathBuf::from("config.conf"));
    assert_eq!(entries(&dir), ["config.conf"]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_chain_longer_than_the_link_limit_is_an_error() {
    let dir = scratch("chain");
    std::fs::write(dir.join("link-41"), "old\n").unwrap();
    for i in 0..41 {
        symlink(format!("link-{}", i + 1), dir.join(format!("link-{i}"))).unwrap();
    }

    assert!(replace_file(&dir.join("link-0"), b"new\n").is_err());

    for i in 0..41 {
        assert!(std::fs::symlink_metadata(dir.join(format!("link-{i}"))).unwrap().file_type().is_symlink());
    }
    assert_eq!(std::fs::read_to_string(dir.join("link-41")).unwrap(), "old\n");
    assert_eq!(entries(&dir).len(), 42);
    std::fs::remove_dir_all(&dir).unwrap();
}
