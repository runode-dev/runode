//! 有上限地读：截断时标出来，要整个文件时太大就不读，不是普通文件的拒读。

use std::fs;

use runode_files::{Limited, read_limited, stamp};

#[test]
fn reads_up_to_the_limit() {
    let dir = std::env::temp_dir().join(format!("runode-files-read-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.txt");
    fs::write(&file, "hello").unwrap();
    assert_eq!(read_limited(&file, 5, false).unwrap(), Limited { bytes: b"hello".to_vec(), cut: false });
    assert_eq!(read_limited(&file, 3, false).unwrap(), Limited { bytes: b"hel".to_vec(), cut: true });
    assert_eq!(read_limited(&file, 3, true).unwrap(), Limited { bytes: Vec::new(), cut: true });
    assert!(read_limited(&dir, 3, false).is_err());
    assert!(read_limited(&dir.join("gone"), 3, false).is_err());
    let before = stamp(&file).unwrap();
    assert_eq!(before.len, 5);
    fs::write(&file, "hello!").unwrap();
    assert_ne!(stamp(&file), Some(before));
    fs::remove_dir_all(&dir).ok();
}

/// 指向 /dev/zero 的图片：大小是 0 却读不完，不能一直读下去。
#[cfg(unix)]
#[test]
fn device_symlinks_are_not_read() {
    let dir = std::env::temp_dir().join(format!("runode-files-dev-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let logo = dir.join("logo.png");
    fs::remove_file(&logo).ok();
    std::os::unix::fs::symlink("/dev/zero", &logo).unwrap();
    assert!(read_limited(&logo, 1024, true).is_err());
    fs::remove_dir_all(&dir).ok();
}
