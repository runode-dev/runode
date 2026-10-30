//! 远端的 `runode --ssh-project`：从 stdin 一条条读桌面的请求，每条起一个线程办，结果写回 stdout；stdin
//! 关了（桌面断开）就退出。

use std::{
    collections::HashMap,
    io::{self, BufReader},
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    thread,
};

use runode_git::{Repos, UntrackedCache};

use super::{
    scan,
    wire::{self, Call, RemoteScan, Reply, Request, Response},
};

/// 两样缓存各最多按这么多个目录留着，多了全部丢掉重算。
const MAX_CACHES: usize = 16;

/// 按读的目录留着的未跟踪文件缓存，和按文件树根留着的最近一次 git 状态（列目录时判断忽略）。
#[derive(Default)]
struct Caches {
    untracked: HashMap<PathBuf, UntrackedCache>,
    git: HashMap<PathBuf, Option<Repos>>,
}

type Shared = Arc<Mutex<Caches>>;

pub(crate) fn run() -> i32 {
    let output = Arc::new(Mutex::new(io::stdout()));
    let caches = Shared::default();
    let mut input = BufReader::new(io::stdin().lock());
    loop {
        let request = match wire::read_line::<Request>(&mut input) {
            Ok(Some(request)) => request,
            Ok(None) => return 0,
            Err(err) => {
                tracing::warn!("bad request: {err}");
                return 1;
            }
        };
        let (output, caches) = (output.clone(), caches.clone());
        let spawned = thread::Builder::new().name("project-call".into()).spawn(move || {
            let (reply, blob) = handle(request.call, &caches);
            let response = Response { id: request.id, reply, blob: blob.len() };
            let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);
            // 编不了的结果改回一句原因；还写不出去说明桌面已经断开，stdin 随后也会关，在那里退出。
            if let Err(err) = wire::write(&mut *output, &response, &blob) {
                let failed = Response { id: request.id, reply: Reply::Failed(err.to_string()), blob: 0 };
                let _ = wire::write(&mut *output, &failed, &[]);
            }
        });
        if let Err(err) = spawned {
            tracing::warn!("cannot start a thread: {err}");
            return 1;
        }
    }
}

fn handle(call: Call, caches: &Shared) -> (Reply, Vec<u8>) {
    let lock = || caches.lock().unwrap_or_else(PoisonError::into_inner);
    match call {
        Call::Scan { dir, expanded, worktrees } => {
            let untracked = lock().untracked.remove(&dir).unwrap_or_default();
            let scan = scan(dir.clone(), expanded, untracked, worktrees);
            let mut caches = lock();
            if caches.untracked.len() >= MAX_CACHES {
                caches.untracked.clear();
            }
            if caches.git.len() >= MAX_CACHES {
                caches.git.clear();
            }
            caches.untracked.insert(dir, scan.untracked);
            caches.git.insert(scan.root.clone(), scan.git.clone());
            drop(caches);
            let cost_ms = u64::try_from(scan.cost.as_millis()).unwrap_or(u64::MAX);
            let scan = RemoteScan { dir: scan.dir, root: scan.root, git: scan.git, listings: scan.listings, cost_ms };
            (Reply::Scan(Box::new(scan)), Vec::new())
        }
        Call::Read { path, max, whole } => match runode_files::read_limited(&path, max, whole) {
            Ok(read) => (Reply::Read { cut: read.cut }, read.bytes),
            Err(err) => (Reply::Failed(err.to_string()), Vec::new()),
        },
        Call::ListDirs { root, dirs } => {
            let git = lock().git.get(&root).cloned().flatten();
            (Reply::Listed(crate::file_tree::list_dirs(dirs, git.as_ref())), Vec::new())
        }
        Call::Git { root, op } => match runode_git::Repo::open(&root).map(|repo| op.run(&repo)) {
            Some(Ok(value)) => (Reply::Git(value), Vec::new()),
            Some(Err(err)) => (Reply::Failed(err.to_string()), Vec::new()),
            None => (Reply::Failed(format!("{} is not in a git repository", root.display())), Vec::new()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor};

    use super::*;
    use crate::project_host::repo::GitOp;

    /// 一来一回过一遍线上格式：读文件的内容跟在回话后面，列目录、读项目照本机的结果交回。
    #[test]
    fn answers_survive_the_wire() {
        let dir = std::env::temp_dir().join(format!("runode-project-serve-{}", std::process::id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("notes.md"), "# 笔记\n").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        let caches = Shared::default();
        let calls = [
            Call::Scan { dir: dir.clone(), expanded: vec![dir.join("src")], worktrees: false },
            Call::ListDirs { root: dir.clone(), dirs: vec![dir.join("src")] },
            Call::Read { path: dir.join("notes.md"), max: 4, whole: false },
            Call::Read { path: dir.join("missing.md"), max: 4, whole: false },
        ];
        let mut stream = Vec::new();
        for (id, call) in calls.into_iter().enumerate() {
            let (reply, blob) = handle(call, &caches);
            wire::write(&mut stream, &Response { id: id as u64, reply, blob: blob.len() }, &blob).unwrap();
        }
        let mut input = Cursor::new(stream);
        let mut next = || {
            let response = wire::read_line::<Response>(&mut input).unwrap().unwrap();
            let blob = wire::read_blob(&mut input, response.blob).unwrap();
            (response.reply, blob)
        };
        let Reply::Scan(scan) = next().0 else { panic!("读项目没交回结果") };
        assert_eq!(scan.root, dir);
        assert!(scan.listings.iter().any(|(listed, entries)| *listed == dir.join("src") && entries.is_some()));
        let Reply::Listed(listed) = next().0 else { panic!("列目录没交回结果") };
        let [(listed, Some(entries))] = &listed[..] else { panic!("列目录交回的不对：{listed:?}") };
        assert_eq!(*listed, dir.join("src"));
        assert!(entries.iter().any(|entry| entry.name == "main.rs"));
        assert!(matches!(next(), (Reply::Read { cut: true }, blob) if blob == "# 笔".as_bytes()[..4]));
        assert!(matches!(next(), (Reply::Failed(_), blob) if blob.is_empty()));
        fs::remove_dir_all(&dir).ok();
    }

    /// git 操作在远端照本机的跑，结果原样带回：暂存后读得出暂存区的 diff，不在仓库里时报原因。
    #[test]
    fn git_runs_where_the_project_is() {
        let dir = std::env::temp_dir().join(format!("runode-project-git-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git").args(args).current_dir(&dir).status().unwrap().success())
        };
        git(&["init", "-q"]);
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let caches = Shared::default();
        let run = |op| {
            let mut stream = Vec::new();
            let (reply, blob) = handle(Call::Git { root: dir.clone(), op }, &caches);
            wire::write(&mut stream, &Response { id: 0, reply, blob: blob.len() }, &blob).unwrap();
            wire::read_line::<Response>(&mut Cursor::new(stream)).unwrap().unwrap().reply
        };
        let Reply::Git(staged) = run(GitOp::Stage { paths: vec![PathBuf::from("a.txt")] }) else {
            panic!("暂存没交回结果")
        };
        assert_eq!(serde_json::from_value::<runode_git::Result>(staged).unwrap(), Ok(()));
        let Reply::Git(diff) = run(GitOp::PendingDiff { staged: true }) else { panic!("diff 没交回结果") };
        assert!(serde_json::from_value::<runode_git::Result<String>>(diff).unwrap().unwrap().contains("+hello"));
        fs::remove_dir_all(dir.join(".git")).unwrap();
        assert!(matches!(run(GitOp::StageAll), Reply::Failed(_)));
        fs::remove_dir_all(&dir).ok();
    }
}
