//! 主仓库里的子仓库：子模块（含子模块里的子模块）和嵌套仓库各读一份，主仓库里子模块那一条
//! 只在记着的提交号变了时算改动；找子仓库的层数有上限，被忽略的目录不往里找。

mod common;

use std::path::{Path, PathBuf};

use common::{TestRepo, paths_of, read_all};
use runode_git::{FileStatus, RepoKind, Section, hunk_actionable};

/// 各个仓库的前缀（分隔符一律写成 `/`）和种类。
fn layout(repos: &runode_git::Repos) -> Vec<(String, RepoKind)> {
    repos.iter().map(|repo| (repo.prefix.to_string_lossy().replace('\\', "/"), repo.kind)).collect()
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
    assert_eq!((repos.subs[1].added(), repos.subs[1].removed()), (1, 1));
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
    // 往下找的层数有上限：a、a/b、a/b/c 读，再下一层的 a/b/c/d 不读。
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
        let dir = std::env::temp_dir().join(format!("runode-git-{name}-{}", std::process::id()));
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
    let from_feat = runode_git::snapshot_repos(&feat.0, &mut Default::default(), true).unwrap();
    assert_eq!(real(&from_feat.main.root), real(&feat.0));
    assert!(from_feat.worktrees.iter().any(|wt| real(&wt.root) == real(repo.path())));
    assert_eq!(from_feat.worktrees.len(), 2);
    assert!(from_feat.main.is_linked_worktree());
    let primary = from_feat.worktrees.iter().find(|wt| real(&wt.root) == real(repo.path())).unwrap();
    assert!(!primary.is_linked_worktree());

    // 不要其他工作树时不读。
    let local = runode_git::snapshot_repos(repo.path(), &mut Default::default(), false).unwrap();
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

#[test]
fn adds_a_worktree_inside_the_repo_without_dirtying_it() {
    let repo = TestRepo::new("repos-add-worktree");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = runode_git::Repo::open(repo.path()).unwrap();
    let worktree = handle.root.join(".worktrees").join("fix-login");

    handle.add_worktree(&worktree, "runode/fix-login").unwrap();
    assert_eq!(std::fs::read_to_string(worktree.join("a.txt")).unwrap(), "one\n");
    assert_eq!(repo.git(&["-C", &worktree.to_string_lossy(), "branch", "--show-current"]).trim(), "runode/fix-login");
    // 新工作树的目录写进了 info/exclude，主仓库的状态里看不到它；`.gitignore` 没被碰。
    assert!(repo.status().is_empty(), "{:?}", repo.status());
    assert!(!repo.path().join(".gitignore").exists());
    let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.ends_with("/.worktrees/\n"), "{exclude:?}");

    // 第二个工作树不再重复写；分支已经有了时 git 拒绝。
    handle.add_worktree(&handle.root.join(".worktrees").join("second"), "runode/second").unwrap();
    assert_eq!(std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap(), exclude);
    assert!(handle.add_worktree(&handle.root.join(".worktrees").join("again"), "runode/fix-login").is_err());

    // 干净时不强制也删得掉，有改动时删不掉。
    handle.remove_worktree(&worktree, false).unwrap();
    assert!(!worktree.exists());
    let second = handle.root.join(".worktrees").join("second");
    std::fs::write(second.join("a.txt"), "changed\n").unwrap();
    assert!(handle.remove_worktree(&second, false).is_err());

    // 没有新提交的分支删得掉；有没合并的提交、或者还检出在工作树里的删不掉。
    handle.delete_merged_branch("runode/fix-login").unwrap();
    assert!(!repo.git(&["branch", "--list"]).contains("runode/fix-login"));
    assert!(handle.delete_merged_branch("runode/second").is_err());
    repo.git(&["-C", &second.to_string_lossy(), "commit", "-qam", "work"]);
    handle.remove_worktree(&second, false).unwrap();
    assert!(handle.delete_merged_branch("runode/second").is_err());
    assert!(handle.delete_merged_branch("-D").is_err());
}

#[cfg(unix)]
#[test]
fn removes_the_new_worktree_and_branch_when_info_exclude_cannot_be_written() {
    let repo = TestRepo::new("repos-add-worktree-no-exclude");
    repo.commit_file("a.txt", "one\n", "init");
    // info/exclude 指向不存在的目录时追加写不进去，git 读它时只当没有。
    let exclude = repo.path().join(".git/info/exclude");
    std::fs::remove_file(&exclude).ok();
    std::os::unix::fs::symlink(repo.path().join("missing/exclude"), &exclude).unwrap();
    let handle = runode_git::Repo::open(repo.path()).unwrap();
    let worktree = handle.root.join(".worktrees").join("a");

    assert!(handle.add_worktree(&worktree, "runode/a").is_err());
    assert!(!worktree.exists());
    assert!(!repo.git(&["branch", "--list"]).contains("runode/a"));
    assert!(!repo.git(&["worktree", "list"]).contains(".worktrees"));
}

#[test]
fn leaves_info_exclude_alone_when_the_worktree_dir_is_ignored() {
    let repo = TestRepo::new("repos-add-worktree-ignored");
    repo.commit_file(".gitignore", ".worktrees/\n", "init");
    let handle = runode_git::Repo::open(repo.path()).unwrap();
    let before = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap_or_default();

    handle.add_worktree(&handle.root.join(".worktrees").join("a"), "runode/a").unwrap();
    assert_eq!(std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap_or_default(), before);
    assert!(repo.status().is_empty(), "{:?}", repo.status());
}

/// 相对的路径按仓库根算，和 `git worktree add` 一样，也照样写进 info/exclude。
#[test]
fn excludes_a_worktree_given_by_a_relative_path() {
    let repo = TestRepo::new("repos-add-worktree-relative");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = runode_git::Repo::open(repo.path()).unwrap();

    handle.add_worktree(std::path::Path::new(".worktrees/new"), "runode/new").unwrap();
    assert!(repo.path().join(".worktrees/new/a.txt").exists());
    assert!(repo.status().is_empty(), "{:?}", repo.status());
}

/// 路径里有 `..` 时按工作树真正所在的目录写：`scratch/../new` 建在 `new`，要排除的是 `new`，
/// 不能把不相干的 `scratch` 藏起来。
#[test]
fn excludes_the_real_top_dir_of_a_path_with_parent_components() {
    let repo = TestRepo::new("repos-add-worktree-dotdot");
    repo.commit_file("a.txt", "one\n", "init");
    repo.write("scratch/notes.txt", "x\n");
    let handle = runode_git::Repo::open(repo.path()).unwrap();

    handle.add_worktree(std::path::Path::new("scratch/../new"), "runode/new").unwrap();
    assert!(repo.path().join("new/a.txt").exists());
    assert_eq!(repo.status(), ["?? scratch/notes.txt"]);
    let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.ends_with("/new/\n"), "{exclude:?}");
}

/// 目录名里的通配符按字面写进 info/exclude：`work[1]` 不能写成匹配 `work1` 的规则。
#[test]
fn excludes_worktree_dirs_whose_names_look_like_globs() {
    let repo = TestRepo::new("repos-add-worktree-glob");
    repo.commit_file("a.txt", "one\n", "init");
    let handle = runode_git::Repo::open(repo.path()).unwrap();
    repo.write("work1/x.txt", "x\n");

    handle.add_worktree(&handle.root.join("work[1]").join("new"), "runode/new").unwrap();
    assert_eq!(repo.status(), ["?? work1/x.txt"]);
    let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.ends_with("/work\\[1\\]/\n"), "{exclude:?}");
}
