//! 各个测试文件共用的辅助。`TestRepo` 是测试用的临时仓库：建在系统临时目录里，用完删掉。
//! 身份、签名、钩子这些写进仓库自己的配置，不依赖本机的全局配置；分支名显式给 `main`，
//! 不看 `init.defaultBranch`。
#![allow(dead_code, reason = "每个测试文件各自编译成一个 crate，只用到这里的一部分")]

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use runode_git_status::{Snapshot, UntrackedCache, snapshot};

pub struct TestRepo {
    dir: PathBuf,
}

impl TestRepo {
    /// 在 `main` 分支上的空仓库，还没有提交。
    pub fn new(name: &str) -> Self {
        let repo = Self::empty_dir(name);
        repo.git(&["init", "-q", "-b", "main"]);
        repo.configure();
        repo
    }

    /// 当远端用的 bare 仓库。
    pub fn bare(name: &str) -> Self {
        let repo = Self::empty_dir(name);
        repo.git(&["init", "-q", "--bare", "-b", "main"]);
        repo
    }

    /// 从 `remote` 克隆一份。
    pub fn clone_of(remote: &TestRepo, name: &str) -> Self {
        let repo = Self::empty_dir(name);
        let status = Command::new("git")
            .args(["clone", "-q"])
            .arg(remote.path())
            .arg(repo.path())
            .output()
            .unwrap();
        assert!(status.status.success(), "git clone: {}", String::from_utf8_lossy(&status.stderr));
        repo.configure();
        repo
    }

    fn empty_dir(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("runode-git-status-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn configure(&self) {
        for (key, value) in [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("core.hooksPath", "no-hooks"),
            ("pull.rebase", "false"),
            ("core.autocrlf", "false"),
        ] {
            self.git(&["config", key, value]);
        }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// 跑 git，失败时测试失败；返回去掉结尾空白的标准输出（开头的制表符要留着比）。
    pub fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git").arg("-C").arg(&self.dir).args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim_end().to_owned()
    }

    /// 跑 git，失败时为空。
    pub fn try_git(&self, args: &[&str]) -> Option<String> {
        let output = Command::new("git").arg("-C").arg(&self.dir).args(args).output().unwrap();
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim_end().to_owned())
    }

    pub fn write(&self, path: &str, content: &str) {
        let full = self.dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).unwrap()
    }

    /// 写好文件、只暂存它并提交。
    pub fn commit_file(&self, path: &str, content: &str, message: &str) {
        self.write(path, content);
        self.git(&["add", "--", path]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// `git status --porcelain` 的短格式，按行列出，方便断言。
    pub fn status(&self) -> Vec<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).collect()
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn read(repo: &TestRepo) -> Snapshot {
    snapshot(repo.path(), &mut UntrackedCache::default()).unwrap()
}
