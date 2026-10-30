//! 读目录：目录在前、按名字排，只有一个子目录的链条读到头，被忽略的子目录不往下看。

use std::{fs, path::PathBuf};

use runode_files::{DirEntry, list_dirs};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-files-listing-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn lists_dirs_first_and_follows_single_child_chains() {
    let root = temp("chain");
    fs::create_dir_all(root.join("a/b/c")).unwrap();
    fs::create_dir_all(root.join("target/x")).unwrap();
    fs::write(root.join("Z.txt"), "").unwrap();
    fs::write(root.join(".DS_Store"), "").unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    let listings = list_dirs(vec![root.clone()], |dir| dir.ends_with("target"));
    let names =
        |entries: &[DirEntry]| entries.iter().map(|e| e.name.to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert_eq!(listings[0].0, root);
    assert_eq!(names(listings[0].1.as_deref().unwrap()), ["a", "target", "Z.txt"]);
    let read: Vec<_> = listings.iter().map(|(dir, _)| dir.strip_prefix(&root).unwrap().to_path_buf()).collect();
    assert_eq!(read, ["", "a", "a/b", "a/b/c"].map(PathBuf::from));
    assert_eq!(list_dirs(vec![root.join("gone")], |_| false), [(root.join("gone"), None)]);
    fs::remove_dir_all(&root).ok();
}
