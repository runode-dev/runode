//! 按给的路径建私有目录：新建的和已经有了的都只让自己进，指到别处的链接不认。

#![cfg(unix)]

use std::{os::unix::fs::PermissionsExt as _, path::PathBuf};

use runode_paths::create_private;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-create-private-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn new_and_existing_directories_end_up_private() {
    let root = scratch("modes");
    let fresh = root.join("a/b");
    create_private(&fresh).unwrap();
    assert_eq!(mode(&fresh), 0o700);

    let open = root.join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    create_private(&open).unwrap();
    assert_eq!(mode(&open), 0o700);

    let link = root.join("link");
    std::os::unix::fs::symlink(&open, &link).unwrap();
    assert!(create_private(&link).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}
