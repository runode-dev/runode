//! 快照的公开查询：被忽略的目录连带它里面的路径。

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use runode_git_status::{RepoInfo, RepoKind, Snapshot};

#[test]
fn ignored_dirs_cover_their_contents() {
    let snapshot = Snapshot {
        root: "/repo".into(),
        git_dir: "/repo/.git".into(),
        prefix: "".into(),
        kind: RepoKind::Main,
        staged: Vec::new(),
        unstaged: Vec::new(),
        statuses: HashMap::new(),
        ignored: HashSet::from(["target".into(), "out/gen".into()]),
        info: RepoInfo::default(),
    };
    assert!(snapshot.is_ignored(Path::new("target")));
    assert!(snapshot.is_ignored(Path::new("target/debug/x")));
    assert!(snapshot.is_ignored(Path::new("out/gen/a.rs")));
    assert!(!snapshot.is_ignored(Path::new("out")));
    assert!(!snapshot.is_ignored(Path::new("targets/x")));
}
