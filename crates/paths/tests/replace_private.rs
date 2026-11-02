//! 覆盖写只给自己看的文件：换上后只有自己能读写，几个线程同时写也不留临时文件。

use std::path::PathBuf;

use runode_paths::replace_private;

/// 这个测试自己的空目录。
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-replace-private-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn tightens_the_permissions_of_an_open_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch("mode");
    let file = dir.join("devices.json");
    std::fs::write(&file, "old").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    replace_private(&file, b"new").unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn threads_writing_the_same_file_do_not_trip_each_other() {
    let dir = scratch("threads");
    let path = dir.join("shared.json");
    let contents: Vec<Vec<u8>> = (0..8u8).map(|n| vec![b'a' + n; 1000 * (usize::from(n) + 1)]).collect();
    std::thread::scope(|scope| {
        for bytes in &contents {
            let path = &path;
            scope.spawn(move || {
                for _ in 0..50 {
                    replace_private(path, bytes).unwrap();
                }
            });
        }
    });
    assert!(contents.contains(&std::fs::read(&path).unwrap()));
    // 临时文件都换上去了，没有剩下的。
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
