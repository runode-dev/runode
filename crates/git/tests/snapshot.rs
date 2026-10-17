//! 快照的公开查询：被忽略的目录连带它里面的路径；读快照时不跟着未跟踪的符号链接去读，也不
//! 执行仓库配置里的 `core.fsmonitor`、filter 和 `gpg.program`；不是 UTF-8 的文件名原样留着。

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
use runode_git::{DiffSide, FileStatus, ReadOptions, RepoInfo, RepoKind, Snapshot, UntrackedCache, snapshot};

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

/// `.gitattributes` 指定的过滤器 `filter` 由仓库的配置给命令（`config` 写进去），读快照时不执行；
/// 过滤器设了 `required` 也照样读得出来。
fn does_not_run_repository_filters(name: &str, filter: &str, config: impl Fn(&TestRepo, &str)) {
    let repo = TestRepo::new(name);
    repo.commit_file(".gitattributes", &format!("* filter={filter}\n"), "attributes");
    repo.commit_file("a.txt", "one\n", "init");
    let marker = repo.path().join(".git/filter-ran");
    config(&repo, &format!("sh -c 'echo x >> \"{}\"; cat'", marker.display()));
    // stat 和暂存区里记的对不上，git 就要过一遍过滤器比内容。
    let later = std::time::SystemTime::now() + Duration::from_secs(60);
    fs::File::options().write(true).open(repo.path().join("a.txt")).unwrap().set_modified(later).unwrap();
    let snapshot = snapshot(repo.path(), &mut UntrackedCache::default()).expect("过滤器覆盖后状态读不出来");
    assert!(!marker.exists(), "读快照时执行了仓库配置的过滤器");
    assert!(snapshot.is_clean(), "{:?}", snapshot.unstaged);
    // 对照：不覆盖时 git status 会执行它，上面的断言才有意义。
    repo.try_git(&["status"]);
    assert!(marker.exists());
}

#[test]
fn does_not_run_the_repository_filters() {
    does_not_run_repository_filters("snapshot-filter", "evil", |repo, command| {
        repo.git(&["config", "filter.evil.clean", command]);
        repo.git(&["config", "filter.evil.process", command]);
        repo.git(&["config", "filter.evil.required", "true"]);
    });
}

/// 名字里带 `=` 的过滤器：`-c filter.a=b.clean=` 会被 git 在第一个 `=` 处切开，覆盖不到它。
#[test]
fn does_not_run_filters_with_equals_in_name() {
    does_not_run_repository_filters("snapshot-filter-equals", "a=b", |repo, command| {
        repo.git(&["config", "filter.a=b.clean", command]);
        repo.git(&["config", "filter.a=b.required", "true"]);
    });
}

#[test]
fn does_not_run_filters_from_included_config() {
    does_not_run_repository_filters("snapshot-filter-include", "evil", |repo, command| {
        let command = command.replace('\\', "\\\\").replace('"', "\\\"");
        repo.write(".git/evil.cfg", &format!("[filter \"evil\"]\n\tclean = \"{command}\"\n"));
        repo.git(&["config", "include.path", "evil.cfg"]);
    });
}

/// 仓库开了 `log.showSignature`、把 `gpg.program` 设成脚本时，读快照（`stash list` 内部跑 `log`）和
/// 读提交历史都不执行它去校验签过名的提交。
#[test]
fn does_not_run_the_repository_gpg_program() {
    let repo = TestRepo::new("snapshot-gpg");
    repo.commit_file("a.txt", "one\n", "init");
    repo.write("a.txt", "two\n");
    repo.git(&["stash", "-q"]);
    // 给 stash 的提交补上 gpgsig 头，换掉 `refs/stash`；签名不必是真的，git 见到就会拿去校验。
    let raw = repo.git(&["cat-file", "commit", "refs/stash"]);
    let (headers, message) = raw.split_once("\n\n").unwrap();
    let signature = "gpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEz\n -----END PGP SIGNATURE-----";
    repo.write(".git/signed", &format!("{headers}\n{signature}\n\n{message}\n"));
    let signed = repo.git(&["hash-object", "-t", "commit", "-w", ".git/signed"]);
    repo.git(&["update-ref", "refs/stash", &signed]);
    let marker = repo.path().join(".git/gpg-ran");
    let script = repo.path().join(".git/gpg.sh");
    std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    repo.git(&["config", "log.showSignature", "true"]);
    repo.git(&["config", "gpg.program", &script.to_string_lossy()]);
    let snapshot = read(&repo);
    assert_eq!(snapshot.info.stashes[0].id, signed);
    snapshot.repo().history(10).unwrap();
    assert!(!marker.exists(), "读快照时执行了 gpg.program");
    // 对照：不关掉时 git stash list 会执行它，上面的断言才有意义。
    repo.git(&["stash", "list"]);
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
    // 按路径找它的查询拿的是原始字节，找得到。
    let handle = snapshot.repo();
    let options = ReadOptions { max_bytes: 1024 };
    let head = DiffSide::Commit { id: "HEAD".into(), parent: None };
    assert!(handle.is_tracked(&path));
    let view = handle.file_view(&path, None, &DiffSide::Worktree).unwrap().expect("工作区那边的 diff");
    assert_eq!(view.file.status, FileStatus::Deleted);
    assert_eq!(handle.old_bytes(&path, None, &DiffSide::Worktree, options), Ok(Some(b"x\n".to_vec())));
    assert_eq!(handle.new_bytes(&path, &head, options), Ok(Some(b"x\n".to_vec())));
    handle.stage(std::slice::from_ref(&path)).unwrap();
    assert_eq!(read(&repo).staged.iter().map(|file| &file.path).collect::<Vec<_>>(), [&path]);
    assert!(handle.file_view(&path, None, &DiffSide::Index).unwrap().is_some());

    // 文件系统让建时（Linux）再试未跟踪的。
    if fs::write(repo.path().join(name), "y\n").is_ok() {
        let snapshot = read(&repo);
        assert_eq!(snapshot.statuses.get(&path), Some(&FileStatus::Untracked));
        snapshot.repo().stage(std::slice::from_ref(&path)).unwrap();
        // HEAD 里有这个文件（`x`），删除只是暂存了没提交，重新加进来的 `y` 相对 HEAD 是修改。
        assert_eq!(read(&repo).statuses.get(&path), Some(&FileStatus::Modified));
    }
}

/// 冲突的文件都在未暂存段里、标成冲突，不在暂存段里；我方删了、对方改了的那种 diff 读不出来，也照样列出。
#[test]
fn conflicts_are_listed_even_without_a_diff() {
    let repo = TestRepo::new("snapshot-conflicts");
    repo.commit_file("both.txt", "base\n", "base");
    repo.commit_file("gone.txt", "base\n", "gone base");
    repo.git(&["checkout", "-q", "-b", "feat"]);
    repo.commit_file("both.txt", "feat\n", "feat both");
    repo.commit_file("gone.txt", "feat\n", "feat gone");
    repo.git(&["checkout", "-q", "main"]);
    repo.commit_file("both.txt", "main\n", "main both");
    repo.git(&["rm", "-q", "gone.txt"]);
    repo.git(&["commit", "-q", "-m", "main gone"]);
    assert!(repo.try_git(&["merge", "-q", "feat"]).is_none());

    let snapshot = read(&repo);
    let conflicted: Vec<(String, FileStatus)> =
        snapshot.unstaged.iter().map(|file| (file.path.display().to_string(), file.status)).collect();
    assert_eq!(conflicted, [("both.txt".into(), FileStatus::Conflicted), ("gone.txt".into(), FileStatus::Conflicted)]);
    assert!(snapshot.staged.iter().all(|file| file.status != FileStatus::Conflicted), "{:?}", snapshot.staged);
}

/// `git diff` 失败（这里是对象库里的 blob 丢了）而 `git status` 照常时，不能当成没有改动、报干净，也不能
/// 当成不在仓库里：读不出来的那一段改用 `git status` 列出的文件，只有路径和状态；读得出来的那一段照常。
#[test]
fn a_failing_diff_falls_back_to_the_status() {
    let repo = TestRepo::new("snapshot-diff-fails");
    repo.commit_file("a.txt", "one\n", "init");
    repo.commit_file("b.txt", "b\n", "second");
    let blob = repo.git(&["rev-parse", ":a.txt"]);
    let object = |blob: &str| repo.path().join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    fs::remove_file(object(&blob)).unwrap();
    repo.write("a.txt", "two\n");
    repo.write("b.txt", "bb\n");
    repo.git(&["add", "b.txt"]);
    repo.write("new.txt", "new\n");
    assert_eq!(repo.status(), [" M a.txt", "M  b.txt", "?? new.txt"]);
    let snapshot = snapshot(repo.path(), &mut UntrackedCache::default()).expect("还在仓库里");
    let files = |files: &[runode_git::FileDiff]| -> Vec<_> {
        files.iter().map(|file| (file.path.to_string_lossy().into_owned(), file.status, file.truncated)).collect()
    };
    assert_eq!(files(&snapshot.staged), [("b.txt".to_owned(), FileStatus::Modified, false)]);
    assert_eq!(
        files(&snapshot.unstaged),
        [("a.txt".to_owned(), FileStatus::Modified, true), ("new.txt".to_owned(), FileStatus::Untracked, false)]
    );

    // 暂存段读不出来（HEAD 里的旧版本丢了）时一样，改名带着原来的路径。
    let old = repo.git(&["rev-parse", "HEAD:b.txt"]);
    fs::remove_file(object(&old)).unwrap();
    repo.git(&["mv", "a.txt", "moved.txt"]);
    assert_eq!(repo.status(), ["M  b.txt", "RM a.txt -> moved.txt", "?? new.txt"]);
    let snapshot = read(&repo);
    assert_eq!(
        files(&snapshot.staged),
        [("b.txt".to_owned(), FileStatus::Modified, true), ("moved.txt".to_owned(), FileStatus::Renamed, true)]
    );
    assert_eq!(snapshot.staged[1].old_path.as_deref(), Some(Path::new("a.txt")));
    assert_eq!(
        files(&snapshot.unstaged),
        [("moved.txt".to_owned(), FileStatus::Modified, true), ("new.txt".to_owned(), FileStatus::Untracked, false)]
    );
}

/// 暂存段的 diff 读不出来时，暂存了的子模块提交号照样列出来；未暂存段分不清子模块是换了提交号还是
/// 只是里面有改动，不列。
#[test]
fn a_failing_diff_keeps_a_staged_submodule() {
    let lib = TestRepo::new("snapshot-diff-fails-lib");
    lib.commit_file("a.txt", "one\n", "init");
    let repo = TestRepo::new("snapshot-diff-fails-submodule");
    repo.commit_file("top.txt", "top\n", "init");
    repo.add_submodule(&lib, "lib");
    let old = repo.git(&["rev-parse", "HEAD:top.txt"]);
    fs::remove_file(repo.path().join(".git/objects").join(&old[..2]).join(&old[2..])).unwrap();
    repo.write("top.txt", "changed\n");
    repo.git(&["-C", "lib", "commit", "-q", "--allow-empty", "-m", "next"]);
    repo.git(&["add", "top.txt", "lib"]);
    repo.git(&["-C", "lib", "commit", "-q", "--allow-empty", "-m", "again"]);
    assert_eq!(repo.status(), ["MM lib", "M  top.txt"]);
    let snapshot = read(&repo);
    let files = |files: &[runode_git::FileDiff]| -> Vec<_> {
        files.iter().map(|file| (file.path.to_string_lossy().into_owned(), file.gitlink, file.truncated)).collect()
    };
    assert_eq!(files(&snapshot.staged), [("lib".to_owned(), true, true), ("top.txt".to_owned(), false, true)]);
    // 未暂存段读得出来，子模块又换了提交号，照常列出。
    assert_eq!(files(&snapshot.unstaged), [("lib".to_owned(), true, false)]);
}

/// 未暂存段的 diff 读不出来时，子模块按检出的提交和暂存区记的比：换了提交号的列出来，只是里面有改动的
/// 不列；没检出的子模块暂存了提交号，照样认成 gitlink，不当成普通文件。
#[test]
fn a_failing_unstaged_diff_tells_submodule_commits_from_dirt() {
    let lib = TestRepo::new("snapshot-unstaged-fails-lib");
    lib.commit_file("a.txt", "one\n", "init");
    let repo = TestRepo::new("snapshot-unstaged-fails");
    repo.commit_file("top.txt", "top\n", "init");
    repo.add_submodule(&lib, "lib");
    repo.add_submodule(&lib, "dirty");
    // 暂存区里 top.txt 的 blob 丢了，工作区又改了它：未暂存段读不出来。
    repo.write("top.txt", "staged\n");
    repo.git(&["add", "top.txt"]);
    let blob = repo.git(&["rev-parse", ":top.txt"]);
    fs::remove_file(repo.path().join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    repo.write("top.txt", "changed\n");
    repo.git(&["-C", "lib", "commit", "-q", "--allow-empty", "-m", "next"]);
    repo.write("dirty/a.txt", "dirty\n");
    let snapshot = read(&repo);
    let files = |files: &[runode_git::FileDiff]| -> Vec<_> {
        files.iter().map(|file| (file.path.to_string_lossy().into_owned(), file.gitlink)).collect()
    };
    assert_eq!(files(&snapshot.unstaged), [("lib".to_owned(), true), ("top.txt".to_owned(), false)]);

    // 暂存段也读不出来（HEAD 里 top.txt 的 blob 也丢了），没检出的子模块暂存了换过的提交号。
    let old = repo.git(&["rev-parse", "HEAD:top.txt"]);
    fs::remove_file(repo.path().join(".git/objects").join(&old[..2]).join(&old[2..])).unwrap();
    repo.git(&["submodule", "deinit", "-q", "-f", "dirty"]);
    let next = repo.git(&["-C", "lib", "rev-parse", "HEAD"]);
    repo.git(&["update-index", "--cacheinfo", &format!("160000,{next},dirty")]);
    let snapshot = read(&repo);
    assert_eq!(files(&snapshot.staged), [("dirty".to_owned(), true), ("top.txt".to_owned(), false)]);
}

/// 用户配了 `diff.submodule=log` 时子模块换了提交照样列成 gitlink 的改动，不被当成没有改动。
#[test]
fn reads_submodule_changes_despite_diff_submodule_log() {
    let lib = TestRepo::new("snapshot-submodule-log-lib");
    lib.commit_file("a.txt", "one\n", "init");
    let repo = TestRepo::new("snapshot-submodule-log");
    repo.commit_file("top.txt", "top\n", "init");
    repo.add_submodule(&lib, "lib");
    repo.git(&["config", "diff.submodule", "log"]);
    repo.git(&["-C", "lib", "commit", "-q", "--allow-empty", "-m", "next"]);
    let snapshot = read(&repo);
    let files: Vec<_> = snapshot.unstaged.iter().map(|file| (file.path.clone(), file.gitlink)).collect();
    assert_eq!(files, [(PathBuf::from("lib"), true)]);
}

/// 用户配了 `diff.suppressBlankEmpty=true` 时空的上下文行照样计进行号，后面的行号不往前错一行。
#[test]
fn counts_blank_context_lines_despite_suppress_blank_empty() {
    let repo = TestRepo::new("snapshot-suppress-blank-empty");
    repo.commit_file("a.txt", "a\n\nb\nc\n", "init");
    repo.git(&["config", "diff.suppressBlankEmpty", "true"]);
    repo.write("a.txt", "a\n\nb\nC\n");
    let snapshot = read(&repo);
    let lines: Vec<_> =
        snapshot.unstaged[0].hunks[0].lines.iter().map(|line| (line.old, line.new, line.text.as_str())).collect();
    assert_eq!(
        lines,
        [
            (Some(1), Some(1), "a"),
            (Some(2), Some(2), ""),
            (Some(3), Some(3), "b"),
            (Some(4), None, "c"),
            (None, Some(4), "C"),
        ]
    );
}
