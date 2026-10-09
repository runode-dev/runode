//! 快照的公开查询：被忽略的目录连带它里面的路径；读快照时不跟着未跟踪的符号链接去读，也不
//! 执行仓库配置里的 `core.fsmonitor` 和 filter；不是 UTF-8 的文件名原样留着。

mod common;

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

use common::{TestRepo, read};
use runode_git::{DiffSide, FileStatus, RepoInfo, RepoKind, Snapshot, UntrackedCache, snapshot};

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

#[test]
fn untracked_symlinks_show_their_target_without_following_it() {
    let repo = TestRepo::new("snapshot-symlink");
    let fifo = repo.path().join(".git/fifo");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    symlink("/dev/zero", repo.path().join("zero")).unwrap();
    symlink(&fifo, repo.path().join("pipe")).unwrap();
    // 跟过去读的话，`/dev/zero` 把内存读光，命名管道一直等着；在别的线程里读，卡住时测试失败。
    let root = repo.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let snapshot = snapshot(&root, &mut UntrackedCache::default()).unwrap();
        let view = snapshot.repo().file_view(Path::new("zero"), None, &DiffSide::Worktree).unwrap().unwrap();
        tx.send((snapshot, view)).unwrap();
    });
    let (snapshot, view) = rx.recv_timeout(Duration::from_secs(30)).expect("读未跟踪的符号链接卡住了");
    // 和 git 一样，符号链接的内容是它指向的路径。
    let files: Vec<_> = snapshot
        .unstaged
        .iter()
        .map(|file| (file.path.to_string_lossy().into_owned(), file.status, file.hunks[0].lines[0].text.clone()))
        .collect();
    let fifo = fifo.to_string_lossy().into_owned();
    assert_eq!(
        files,
        [
            ("pipe".to_owned(), FileStatus::Untracked, fifo),
            ("zero".to_owned(), FileStatus::Untracked, "/dev/zero".to_owned())
        ]
    );
    assert_eq!(view.new_lines, ["/dev/zero"]);
}

#[test]
fn does_not_run_the_repository_fsmonitor() {
    let repo = TestRepo::new("snapshot-fsmonitor");
    repo.commit_file("a.txt", "one\n", "init");
    let marker = repo.path().join(".git/fsmonitor-ran");
    let script = repo.path().join(".git/fsmonitor.sh");
    std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    repo.git(&["config", "core.fsmonitor", &script.to_string_lossy()]);
    repo.write("a.txt", "two\n");
    read(&repo);
    assert!(!marker.exists(), "读快照时执行了 core.fsmonitor");
    // 对照：不关掉时 git status 会执行它，上面的断言才有意义。
    repo.git(&["status"]);
    assert!(marker.exists());
}

#[test]
fn keeps_file_names_that_are_not_utf8() {
    let repo = TestRepo::new("snapshot-latin1");
    let name = OsStr::from_bytes(b"caf\xe9.txt");
    // macOS 的文件系统不让建这样的文件，只把它记进暂存区再提交，工作区里就是删掉了它。
    repo.write("seed", "x\n");
    let blob = repo.git(&["hash-object", "-w", "seed"]);
    fs::remove_file(repo.path().join("seed")).unwrap();
    let mut cacheinfo = OsString::from(format!("100644,{blob},"));
    cacheinfo.push(name);
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["update-index", "--add", "--cacheinfo"])
        .arg(&cacheinfo)
        .status()
        .unwrap();
    assert!(status.success());
    repo.git(&["commit", "-q", "-m", "init"]);

    let path = PathBuf::from(name);
    let snapshot = read(&repo);
    assert_eq!(snapshot.statuses.get(&path), Some(&FileStatus::Deleted));
    assert_eq!(snapshot.unstaged.iter().map(|file| &file.path).collect::<Vec<_>>(), [&path]);
    snapshot.repo().stage(std::slice::from_ref(&path)).unwrap();
    assert_eq!(read(&repo).staged.iter().map(|file| &file.path).collect::<Vec<_>>(), [&path]);

    // 文件系统让建时（Linux）再试未跟踪的。
    if fs::write(repo.path().join(name), "y\n").is_ok() {
        let snapshot = read(&repo);
        assert_eq!(snapshot.statuses.get(&path), Some(&FileStatus::Untracked));
        snapshot.repo().stage(std::slice::from_ref(&path)).unwrap();
        assert_eq!(read(&repo).statuses.get(&path), Some(&FileStatus::Added));
    }
}
