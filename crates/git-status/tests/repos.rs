//! 主仓库里的子仓库：子模块（含子模块里的子模块）和嵌套仓库各读一份，主仓库里子模块那一条
//! 只在记着的提交号变了时算改动；找子仓库的层数有上限，被忽略的目录不往里找。

mod common;

use std::path::{Path, PathBuf};

use common::{TestRepo, paths_of, read_all};
use runode_git_status::{FileStatus, RepoKind, Section, hunk_actionable};

fn layout(repos: &runode_git_status::Repos) -> Vec<(String, RepoKind)> {
    repos.iter().map(|repo| (repo.prefix.to_string_lossy().into_owned(), repo.kind)).collect()
}

#[test]
fn reads_submodules_and_nested_repositories_separately() {
    let lib = TestRepo::new("repos-lib");
    lib.commit_file("lib.txt", "one\n", "init");
    let repo = TestRepo::new("repos-main");
    repo.commit_file("top.txt", "top\n", "init");
    repo.add_submodule(&lib, "libs/lib");
    repo.nested("tools/inner");
    repo.write("libs/lib/lib.txt", "two\n");
    repo.write("tools/inner/a.txt", "two\n");
    repo.write("top.txt", "changed\n");

    let repos = read_all(&repo);
    assert_eq!(
        layout(&repos),
        [
            ("".into(), RepoKind::Main),
            ("libs/lib".into(), RepoKind::Submodule),
            ("tools/inner".into(), RepoKind::Nested)
        ]
    );
    // 子模块里改了文件、提交号没变：主仓库里不算改动，改动在子模块自己那份里。嵌套仓库在主
    // 仓库里只是个未跟踪的目录，也不列进改动。
    assert_eq!(paths_of(&repos.main.unstaged), ["top.txt"]);
    assert_eq!(repos.main.statuses[Path::new("tools/inner")], FileStatus::Untracked);
    let (lib_repo, inner) = (&repos.subs[0], &repos.subs[1]);
    assert_eq!(lib_repo.root, repo.path().join("libs/lib"));
    assert!(lib_repo.git_dir.ends_with(".git/modules/libs/lib"), "{}", lib_repo.git_dir.display());
    assert_eq!(paths_of(lib_repo.files(Section::Unstaged)), ["lib.txt"]);
    assert_eq!(lib_repo.info.branch.as_deref(), Some("main"));
    assert_eq!(paths_of(&inner.unstaged), ["a.txt"]);
    assert_eq!(inner.root, repo.path().join("tools/inner"));
    assert!(inner.git_dir.ends_with("tools/inner/.git"));

    // 路径按最深的仓库认。
    let (found, rel) = repos.locate(Path::new("libs/lib/src/x.rs"));
    assert_eq!((found.prefix.as_path(), rel), (Path::new("libs/lib"), Path::new("src/x.rs")));
    assert_eq!(repos.locate(Path::new("libs/other.txt")).0.kind, RepoKind::Main);
    let statuses: Vec<_> = {
        let mut statuses: Vec<_> = repos.statuses().collect();
        statuses.sort_by(|a, b| a.0.cmp(&b.0));
        statuses
    };
    assert_eq!(
        statuses,
        [
            (PathBuf::from("libs/lib/lib.txt"), FileStatus::Modified),
            ("tools/inner".into(), FileStatus::Untracked),
            ("tools/inner/a.txt".into(), FileStatus::Modified),
            ("top.txt".into(), FileStatus::Modified),
        ]
    );
    assert_eq!(repos.position(&repo.path().join("tools/inner")), Some(2));
    assert_eq!(repos.get(1).map(|sub| sub.kind), Some(RepoKind::Submodule));

    // 子模块里提交以后提交号变了，主仓库里那一条照常列出来，暂存它就记下新的提交号。
    lib_repo.repo().stage_all().unwrap();
    lib_repo.repo().commit("bump", Default::default()).unwrap();
    let repos = read_all(&repo);
    assert_eq!(paths_of(&repos.main.unstaged), ["libs/lib", "top.txt"]);
    assert!(repos.subs[0].is_clean());
    // 子模块那一条是 gitlink：不能按块操作，丢弃什么也不做。
    let pointer = &repos.main.unstaged[0];
    assert!(pointer.gitlink && !hunk_actionable(pointer));
    assert!(!repos.main.unstaged[1].gitlink);
    repos.main.repo().discard(std::slice::from_ref(pointer)).unwrap();
    assert_eq!(paths_of(&read_all(&repo).main.unstaged), ["libs/lib", "top.txt"]);
    repos.main.repo().stage(&[PathBuf::from("libs/lib")]).unwrap();
    assert_eq!(repo.status(), ["M  libs/lib", " M top.txt", "?? tools/inner/"]);
    assert_eq!(paths_of(&read_all(&repo).main.staged), ["libs/lib"]);
}

#[test]
fn reads_gitlinks_missing_from_gitmodules() {
    let repo = TestRepo::new("repos-gitlink");
    repo.commit_file("top.txt", "top\n", "init");
    repo.nested("wt/inner");
    // 直接 `git add` 进来的仓库：索引里是 gitlink，`.gitmodules` 里没有。
    repo.git(&["-c", "advice.addEmbeddedRepo=false", "add", "wt/inner"]);
    repo.git(&["commit", "-q", "-m", "embed"]);
    assert!(!repo.path().join(".gitmodules").exists());
    repo.write("wt/inner/a.txt", "two\n");

    let repos = read_all(&repo);
    assert_eq!(layout(&repos), [("".into(), RepoKind::Main), ("wt/inner".into(), RepoKind::Submodule)]);
    assert_eq!(paths_of(&repos.subs[0].unstaged), ["a.txt"]);
    // 提交号没变，主仓库里不算改动，状态里也不留。
    assert!(repos.main.is_clean());
    assert!(!repos.main.statuses.contains_key(Path::new("wt/inner")));
}

#[test]
fn reads_submodules_of_submodules() {
    let deep = TestRepo::new("repos-deep");
    deep.commit_file("deep.txt", "d\n", "init");
    let middle = TestRepo::new("repos-middle");
    middle.commit_file("middle.txt", "m\n", "init");
    middle.add_submodule(&deep, "deep");
    let repo = TestRepo::new("repos-outer");
    repo.commit_file("top.txt", "top\n", "init");
    repo.add_submodule(&middle, "mid");
    repo.git(&["-c", "protocol.file.allow=always", "submodule", "update", "-q", "--init", "--recursive"]);
    repo.write("mid/deep/deep.txt", "changed\n");

    let repos = read_all(&repo);
    assert_eq!(
        layout(&repos),
        [("".into(), RepoKind::Main), ("mid".into(), RepoKind::Submodule), ("mid/deep".into(), RepoKind::Submodule)]
    );
    assert_eq!(paths_of(&repos.subs[1].unstaged), ["deep.txt"]);
    assert!(repos.subs[0].is_clean() && repos.main.is_clean());
    assert!(!repos.is_clean());
    assert_eq!((repos.added(), repos.removed()), (1, 1));
}

#[test]
fn skips_uninitialized_submodules() {
    let lib = TestRepo::new("repos-uninit-lib");
    lib.commit_file("lib.txt", "one\n", "init");
    let origin = TestRepo::new("repos-uninit-origin");
    origin.commit_file("top.txt", "top\n", "init");
    origin.add_submodule(&lib, "lib");
    // 克隆下来的子模块没检出，只是个空目录。
    let clone = TestRepo::clone_of(&origin, "repos-uninit-clone");
    let repos = read_all(&clone);
    assert_eq!(repos.count(), 1);
    assert!(repos.main.is_clean());
}

#[test]
fn limits_how_deep_it_looks() {
    let repo = TestRepo::new("repos-depth");
    repo.write(".gitignore", "vendor/\n");
    repo.commit_file("top.txt", "top\n", "init");
    for path in ["a", "a/b", "a/b/c", "a/b/c/d"] {
        repo.nested(path);
    }
    // 被忽略的目录里的仓库不找。
    repo.nested("vendor/pkg");
    let repos = read_all(&repo);
    // 往下找三层：a、a/b、a/b/c，第四层的 a/b/c/d 不读。
    assert_eq!(
        layout(&repos),
        [
            ("".into(), RepoKind::Main),
            ("a".into(), RepoKind::Nested),
            ("a/b".into(), RepoKind::Nested),
            ("a/b/c".into(), RepoKind::Nested),
        ]
    );
    assert!(repos.is_ignored(Path::new("vendor/pkg/a.txt")));
    assert!(!repos.is_ignored(Path::new("a/b/a.txt")));
}

/// 系统临时目录里一个还不存在的目录，给 `git worktree add` 用；用完删掉。
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("runode-git-status-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn reads_other_worktrees() {
    let repo = TestRepo::new("repos-worktrees");
    repo.commit_file("a.txt", "one\n", "init");
    let (feat, detached) = (TempDir::new("wt-feat"), TempDir::new("wt-detached"));
    repo.git(&["worktree", "add", "-q", "-b", "feat", &feat.0.to_string_lossy()]);
    repo.git(&["worktree", "add", "-q", "--detach", &detached.0.to_string_lossy()]);
    std::fs::write(feat.0.join("a.txt"), "changed\n").unwrap();

    let repos = read_all(&repo);
    let real = |path: &Path| std::fs::canonicalize(path).unwrap();
    let worktrees: Vec<_> = repos.worktrees.iter().map(|wt| (real(&wt.root), wt.kind)).collect();
    let mut expected = vec![(real(&feat.0), RepoKind::Worktree), (real(&detached.0), RepoKind::Worktree)];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(worktrees, expected);
    let by_root = |root: &Path| repos.worktrees.iter().find(|wt| real(&wt.root) == real(root)).unwrap();
    let feat_repo = by_root(&feat.0);
    assert_eq!(paths_of(&feat_repo.unstaged), ["a.txt"]);
    assert_eq!(feat_repo.info.branch.as_deref(), Some("feat"));
    assert_eq!(feat_repo.prefix, feat_repo.root);
    assert!(feat_repo.is_linked_worktree() && !repos.main.is_linked_worktree());
    let detached_repo = by_root(&detached.0);
    assert_eq!((detached_repo.info.branch.as_deref(), detached_repo.info.head.is_some()), (None, true));
    // 其他工作树不参与按主仓库目录算的标记和统计。
    assert_eq!(repos.count(), 3);
    assert!(repos.is_clean());
    assert_eq!(repos.statuses().count(), 0);
    assert_eq!(repos.get(1).map(|wt| wt.kind), Some(RepoKind::Worktree));

    // 从链接工作树里读：主仓库是它，主工作树成了其他工作树之一。
    let from_feat = runode_git_status::snapshot_repos(&feat.0, &mut Default::default(), Default::default()).unwrap();
    assert_eq!(real(&from_feat.main.root), real(&feat.0));
    assert!(from_feat.worktrees.iter().any(|wt| real(&wt.root) == real(repo.path())));
    assert_eq!(from_feat.worktrees.len(), 2);
    assert!(from_feat.main.is_linked_worktree());
    let primary = from_feat.worktrees.iter().find(|wt| real(&wt.root) == real(repo.path())).unwrap();
    assert!(!primary.is_linked_worktree());

    // 不要其他工作树时不读。
    let options = runode_git_status::ReadOptions { worktrees: false };
    let local = runode_git_status::snapshot_repos(repo.path(), &mut Default::default(), options).unwrap();
    assert!(local.worktrees.is_empty());

    // 目录被删掉的工作树 git 记作 prunable，跳过。
    std::fs::remove_dir_all(&detached.0).unwrap();
    assert_eq!(read_all(&repo).worktrees.len(), 1);

    // 有改动时不强制删不掉，强制删得掉。
    let handle = repos.main.repo();
    assert!(handle.remove_worktree(&feat.0, false).is_err());
    handle.remove_worktree(&feat.0, true).unwrap();
    assert!(!feat.0.exists());
    assert!(read_all(&repo).worktrees.is_empty());
}
