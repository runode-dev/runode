//! 契约测试：同一个进程里起一份项目服务（`serve::serve`），经一对管道接成 `ProjectHost::Remote`，同样的读写
//! 分别交给它和 `ProjectHost::Local`，结果要一样。线上格式、远端那边的分派和桌面这头的转换一起守住。

use std::{
    fs,
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    thread,
};

use runode_git::{CommitOptions, DiffSide, GrepQuery, UntrackedCache};

use super::{ProjectHost, client::Client, serve};

/// 同一个进程里的项目服务，桌面这头经管道和它说话。
pub(super) fn remote() -> ProjectHost {
    let (requests, to_service) = std::io::pipe().unwrap();
    let (from_service, responses) = std::io::pipe().unwrap();
    thread::spawn(move || serve::serve(BufReader::new(requests), responses));
    ProjectHost::Remote(Client::over("contract", to_service, from_service))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        // 历史里写的是「多久以前」，提交时间钉在很久以前，两边先后读到的才一样。
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

/// 一个有提交、有改动、有未跟踪和被忽略的文件的仓库。
fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runode-contract-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("target")).unwrap();
    git(&dir, &["init", "-q"]);
    // 两边的 `ProjectRepo::commit` 都按仓库的配置署名，机器上可能没配全局的身份。
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["config", "user.email", "t@t"]);
    fs::write(dir.join(".gitignore"), "target/\n").unwrap();
    fs::write(dir.join("a.txt"), "one\ntwo\nneedle\n").unwrap();
    fs::write(dir.join("src/lib.rs"), "fn needle() {}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    fs::write(dir.join("a.txt"), "one\n2\nneedle\n").unwrap();
    fs::write(dir.join("notes.md"), "# needle\n").unwrap();
    fs::write(dir.join("target/out"), "built").unwrap();
    fs::canonicalize(&dir).unwrap()
}

#[test]
fn reads_the_same_on_both_sides() {
    let dir = fixture("read");
    let (local, remote) = (ProjectHost::Local, remote());
    let scan = |host: &ProjectHost| host.scan(dir.clone(), vec![dir.join("src")], UntrackedCache::default(), true);
    let (here, there) = (scan(&local).unwrap(), scan(&remote).unwrap());
    assert_eq!((&here.root, &here.git, &here.listings), (&there.root, &there.git, &there.listings));
    let main = here.git.as_ref().unwrap().main.repo();

    let dirs = vec![dir.clone(), dir.join("src")];
    assert_eq!(local.list_dirs(&dir, dirs.clone(), here.git.as_ref()), remote.list_dirs(&dir, dirs, None),);
    let file = dir.join("a.txt");
    assert_eq!(local.read_limited(&file, 4, false).unwrap(), remote.read_limited(&file, 4, false).unwrap());
    assert_eq!(local.stamp(&file), remote.stamp(&file));
    let paths = vec![file, dir.join("src"), dir.join("nope")];
    assert_eq!(local.kinds(paths.clone()), remote.kinds(paths));
    assert_eq!(local.list_files(&dir), remote.list_files(&dir));
    let query = GrepQuery { pattern: "needle", ignore_case: false, whole_word: true, regex: false, pathspecs: &[] };
    let go = AtomicBool::new(false);
    assert_eq!(local.grep(&dir, &query, 10, &go).unwrap(), remote.grep(&dir, &query, 10, &go).unwrap());

    let (here, there) = (local.repo(main.clone()), remote.repo(main));
    assert_eq!(here.branches(), there.branches());
    assert_eq!(here.history(10).unwrap(), there.history(10).unwrap());
    assert_eq!(here.pending_diff(false).unwrap(), there.pending_diff(false).unwrap());
    let view =
        |repo: &super::ProjectRepo| format!("{:?}", repo.file_view(Path::new("a.txt"), None, &DiffSide::Worktree));
    assert_eq!(view(&here), view(&there));
    let options = |max_bytes| runode_git::ReadOptions { max_bytes };
    let a = Path::new("a.txt");
    for max in [100, 2] {
        let new = |repo: &super::ProjectRepo| repo.new_bytes(a, &DiffSide::Worktree, options(max));
        let old = |repo: &super::ProjectRepo| repo.old_bytes(a, None, &DiffSide::Worktree, options(max));
        assert_eq!((new(&here), old(&here)), (new(&there), old(&there)));
    }
    assert_eq!(there.new_bytes(a, &DiffSide::Worktree, options(100)), Ok(Some(b"one\n2\nneedle\n".to_vec())));
    fs::remove_dir_all(&dir).ok();
}

/// 两份一样的仓库，一份经本机、一份经项目服务做同样的改动，改完的文件和 git 状态一样。
#[test]
fn writes_the_same_on_both_sides() {
    let dirs = [fixture("write-local"), fixture("write-remote")];
    let hosts = [ProjectHost::Local, remote()];
    for (host, dir) in hosts.iter().zip(&dirs) {
        let scan = host.scan(dir.clone(), Vec::new(), UntrackedCache::default(), false).unwrap();
        let repo = host.repo(scan.git.unwrap().main.repo());
        host.replace_file(&dir.join("notes.md"), b"# written\n").unwrap();
        host.create(&dir.join("docs"), true).unwrap();
        host.rename(&dir.join("notes.md"), &dir.join("docs/notes.md")).unwrap();
        repo.stage(&[PathBuf::from("a.txt")]).unwrap();
        repo.commit("second", CommitOptions::default()).unwrap();
    }
    let state = |dir: &Path| {
        let log = git(dir, &["log", "--format=%s"]);
        (git(dir, &["status", "--porcelain"]), log, fs::read_to_string(dir.join("docs/notes.md")).unwrap())
    };
    assert_eq!(state(&dirs[0]), state(&dirs[1]));
    assert_eq!(state(&dirs[0]).1, "second\nfirst\n");
    for dir in dirs {
        fs::remove_dir_all(dir).ok();
    }
}

/// 远端的监听：第一次问时开始听，改了文件再问交回改的路径，按开始听时给的写法；取消时不再等。
#[test]
fn watches_over_the_wire() {
    let dir = fixture("watch");
    let remote = remote();
    let go = AtomicBool::new(false);
    assert_eq!(remote.wait_for_changes(7, vec![dir.clone()], &go), Ok(None));
    // 监听开起来要一会儿，一直改到听到为止。
    let changes = (0..20)
        .find_map(|n| {
            fs::write(dir.join("a.txt"), format!("{n}\n")).unwrap();
            remote.wait_for_changes(7, vec![dir.clone()], &go).unwrap().filter(|changes| !changes.paths.is_empty())
        })
        .expect("heard the change");
    assert!(changes.everything || changes.paths.iter().any(|path| path == &dir.join("a.txt")), "{changes:?}");
    let stop = AtomicBool::new(true);
    assert!(remote.wait_for_changes(7, vec![dir.clone()], &stop).is_err());
    assert!(ProjectHost::Local.wait_for_changes(0, vec![dir], &go).is_err());
}

/// 不是 UTF-8 的文件名（Linux 上建得出来）不让远端的整份扫描失败：文件树里照样有它，git 状态里没有。
#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_name_does_not_break_the_remote_scan() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let dir = fixture("non-utf8");
    let name = OsStr::from_bytes(b"caf\xe9.txt");
    fs::write(dir.join(name), "x").unwrap();
    let scan = remote().scan(dir.clone(), Vec::new(), UntrackedCache::default(), false).unwrap();
    let (_, entries) = scan.listings.iter().find(|(listed, _)| listed == &dir).unwrap();
    assert!(entries.as_ref().unwrap().iter().any(|entry| entry.name == name));
    let git = scan.git.unwrap();
    assert!(git.main.statuses.contains_key(Path::new("a.txt")));
    assert!(!git.main.statuses.keys().any(|path| path.to_str().is_none()));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn every_path_in_a_request_is_visited() {
    use super::{repo::GitOp, wire::Call};

    let op = GitOp::FileView { path: "a".into(), old_path: Some("b".into()), side: DiffSide::Worktree };
    let mut call = Call::Git { root: "/r".into(), op };
    let mut seen = Vec::new();
    call.for_each_path(&mut |path| seen.push(path.clone()));
    assert_eq!(seen, [Path::new("/r"), Path::new("a"), Path::new("b")]);
}

#[cfg(windows)]
#[test]
fn remote_paths_use_forward_slashes() {
    let joined = Path::new("/home/me").join("src").join("a.rs");
    assert_eq!(super::remote_path(&joined).to_str(), Some("/home/me/src/a.rs"));
    let own = Path::new(r"\\?\C:\Users\me").join("a.rs");
    assert_eq!(super::remote_path(&own), own);
}
