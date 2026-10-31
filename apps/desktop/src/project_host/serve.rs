//! 远端的 `runode --ssh-project`：从 stdin 一条条读桌面的请求，每条起一个线程办，结果写回 stdout；stdin
//! 关了（桌面断开）就退出。`Call::Cancel` 不起线程，当场让在办的那条停下。

use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use runode_files::Changes;

use runode_git::{Repos, UntrackedCache};

use super::{
    scan,
    wire::{self, Call, RemoteScan, Reply, Request, Response},
};

/// 两样缓存各最多按这么多个目录留着，多了全部丢掉重算。
const MAX_CACHES: usize = 16;
/// `Call::Watch` 没有改动时最多等这么久就交回空的，桌面接着再问。
pub(super) const WATCH_HOLD: Duration = Duration::from_secs(25);
/// 监听这么久没人来取就停掉：桌面那边的监听已经不要了。
const WATCH_IDLE: Duration = Duration::from_secs(120);
/// 等到第一个改动后再攒这么久才交回：保存文件、提交这类操作会连着来一串事件。
const WATCH_BATCH: Duration = Duration::from_millis(100);
/// 攒着的路径多过这么多时改成整个重读，桌面一直不来取时不至于越攒越多。
const MAX_PENDING: usize = 4096;
/// 等改动时每隔这么久看一次要不要停。
const STOP_POLL: Duration = Duration::from_millis(200);

/// 按读的目录留着的未跟踪文件缓存，按文件树根留着的最近一次 git 状态（列目录时判断忽略），和桌面的各个监听。
#[derive(Default)]
struct Caches {
    untracked: HashMap<PathBuf, UntrackedCache>,
    git: HashMap<PathBuf, Option<Repos>>,
    watches: HashMap<u64, Watched>,
}

/// 攒着还没交给桌面的改动，来了改动时叫醒等着的那条 `Call::Watch`。
type Pending = Arc<(Mutex<Option<Changes>>, Condvar)>;

struct Watched {
    pending: Pending,
    polled: Instant,
    _watch: runode_files::Watch,
}

type Shared = Arc<Mutex<Caches>>;

pub(crate) fn run() -> i32 {
    serve(io::stdin().lock(), io::stdout())
}

/// 从 `input` 一条条读请求，回话写进 `output`；`input` 读完（桌面断开）时返回退出码。
pub(super) fn serve(mut input: impl BufRead, output: impl Write + Send + 'static) -> i32 {
    let output = Arc::new(Mutex::new(output));
    let caches = Shared::default();
    // 在办的请求各自的停止标记，按编号记。
    let running = Arc::new(Mutex::new(HashMap::<u64, Arc<AtomicBool>>::new()));
    loop {
        let request = match wire::read_line::<Request>(&mut input) {
            Ok(Some(request)) => request,
            Ok(None) => return 0,
            Err(err) => {
                tracing::warn!("bad request: {err}");
                return 1;
            }
        };
        let Ok(content) = wire::read_blob(&mut input, request.blob) else {
            return 0;
        };
        if let Call::Cancel { id } = request.call {
            if let Some(stop) = running.lock().unwrap_or_else(PoisonError::into_inner).get(&id) {
                stop.store(true, Ordering::Relaxed);
            }
            continue;
        }
        let stop = Arc::new(AtomicBool::new(false));
        running.lock().unwrap_or_else(PoisonError::into_inner).insert(request.id, stop.clone());
        let (output, caches, running) = (output.clone(), caches.clone(), running.clone());
        let spawned = thread::Builder::new().name("project-call".into()).spawn(move || {
            let (reply, blob) = handle(request.call, content, &caches, &stop);
            running.lock().unwrap_or_else(PoisonError::into_inner).remove(&request.id);
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

/// `path` 在不在、是不是目录（`Reply::Kinds`），跟着符号链接走。
pub(super) fn kind(path: &std::path::Path) -> Option<bool> {
    std::fs::metadata(path).ok().map(|meta| meta.is_dir())
}

/// 办一个请求，`content` 是请求后面跟着的字节；`stop` 置位时停得下的停下，交回已经找到的。
fn handle(call: Call, content: Vec<u8>, caches: &Shared, stop: &AtomicBool) -> (Reply, Vec<u8>) {
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
            let scan = RemoteScan {
                dir: scan.dir,
                root: scan.root,
                git: scan.git,
                listings: scan.listings,
                watch_git_dirs: scan.watch_git_dirs,
                cost_ms,
            };
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
        Call::Edit(op) => (Reply::Edited(op.run()), Vec::new()),
        Call::ListFiles { root } => {
            let files = runode_git::list_files(&root).unwrap_or_else(|| crate::file_tree::walk_files(&root));
            let files = files.into_iter().filter(|rel| root.join(rel).symlink_metadata().is_ok()).collect();
            (Reply::Files(files), Vec::new())
        }
        Call::Grep { root, pattern, ignore_case, whole_word, regex, pathspecs, limit } => {
            let query =
                runode_git::GrepQuery { pattern: &pattern, ignore_case, whole_word, regex, pathspecs: &pathspecs };
            match runode_git::grep(&root, &query, limit, stop) {
                Ok(found) => (Reply::Found(found), Vec::new()),
                Err(err) => (Reply::Failed(err.message), Vec::new()),
            }
        }
        Call::Stamp { path } => (Reply::Stamp(runode_files::stamp(&path)), Vec::new()),
        Call::Ports => (Reply::Ports(crate::window::status_bar::listening_ports()), Vec::new()),
        Call::Kinds { paths } => (Reply::Kinds(paths.iter().map(|path| kind(path)).collect()), Vec::new()),
        Call::Write { path } => match runode_paths::replace_file(&path, &content) {
            Ok(()) => (Reply::Stamp(runode_files::stamp(&path)), Vec::new()),
            Err(err) => (Reply::Failed(err.to_string()), Vec::new()),
        },
        Call::Watch { token, dirs } => (wait_for_changes(token, dirs, caches, stop), Vec::new()),
        Call::Cancel { .. } => (Reply::Failed("nothing to cancel".into()), Vec::new()),
        Call::Git { root, op } => match runode_git::Repo::open(&root).map(|repo| op.run(&repo)) {
            Some(Ok((value, blob))) => (Reply::Git(value), blob),
            Some(Err(err)) => (Reply::Failed(err.to_string()), Vec::new()),
            None => (Reply::Failed(format!("{} is not in a git repository", root.display())), Vec::new()),
        },
    }
}

/// 办 `Call::Watch`。
fn wait_for_changes(token: u64, dirs: Vec<PathBuf>, caches: &Shared, stop: &AtomicBool) -> Reply {
    let lock = || caches.lock().unwrap_or_else(PoisonError::into_inner);
    let known = {
        let mut caches = lock();
        caches.watches.retain(|_, watched| watched.polled.elapsed() < WATCH_IDLE);
        caches.watches.get_mut(&token).map(|watched| {
            watched.polled = Instant::now();
            watched.pending.clone()
        })
    };
    let Some(pending) = known else {
        // 开始听要走一遍目录，不占着缓存的锁。
        let pending = Pending::default();
        let Some(watch) = start_watch(dirs, pending.clone()) else {
            return Reply::Failed("cannot watch the project".into());
        };
        lock().watches.insert(token, Watched { pending, polled: Instant::now(), _watch: watch });
        return Reply::Changed { started: true, changes: Changes::default() };
    };
    let (changes, ready) = &*pending;
    let deadline = Instant::now() + WATCH_HOLD;
    let mut waiting = changes.lock().unwrap_or_else(PoisonError::into_inner);
    while waiting.is_none() && !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        waiting = ready.wait_timeout(waiting, STOP_POLL).unwrap_or_else(PoisonError::into_inner).0;
    }
    if waiting.is_some() {
        drop(waiting);
        thread::sleep(WATCH_BATCH);
        waiting = changes.lock().unwrap_or_else(PoisonError::into_inner);
    }
    Reply::Changed { started: false, changes: waiting.take().unwrap_or_default() }
}

/// 递归地听 `dirs`，改动并进 `pending`。事件里的路径按 `dirs` 原来的写法交回：桌面按它给的路径比。
fn start_watch(dirs: Vec<PathBuf>, pending: Pending) -> Option<runode_files::Watch> {
    let real: Vec<(PathBuf, PathBuf)> =
        dirs.iter().map(|dir| (std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()), dir.clone())).collect();
    let everything = dirs.clone();
    runode_files::watch(dirs, move |mut batch| {
        for path in &mut batch.paths {
            if let Some((real, given)) = real.iter().find(|(real, given)| real != given && path.starts_with(real))
                && let Ok(rel) = path.strip_prefix(real)
            {
                *path = given.join(rel);
            }
        }
        let (changes, ready) = &*pending;
        let mut changes = changes.lock().unwrap_or_else(PoisonError::into_inner);
        let changes = changes.get_or_insert_default();
        changes.merge(batch);
        if changes.paths.len() > MAX_PENDING {
            *changes = Changes { paths: everything.clone(), everything: true };
        }
        ready.notify_all();
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor};

    use super::*;
    use crate::project_host::repo::GitOp;

    /// 不停下的请求用的停止标记。
    static GO: AtomicBool = AtomicBool::new(false);

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
            let (reply, blob) = handle(call, Vec::new(), &caches, &GO);
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

    /// 改文件和写回在远端照本机的做：写进去的字节读得回、版本戳跟着变；不覆盖已有的，原因带回来。
    #[test]
    fn edits_happen_where_the_project_is() {
        use crate::project_host::edit::{FileOp, OpFailure};

        let dir = std::env::temp_dir().join(format!("runode-project-edit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let caches = Shared::default();
        let file = dir.join("a/b.txt");
        let created =
            handle(Call::Edit(FileOp::Create { path: file.clone(), is_dir: false }), Vec::new(), &caches, &GO);
        assert!(matches!(created, (Reply::Edited(Ok(None)), _)));
        let again = handle(Call::Edit(FileOp::Create { path: file.clone(), is_dir: false }), Vec::new(), &caches, &GO);
        assert!(matches!(again, (Reply::Edited(Err(OpFailure::Exists)), _)));
        let (Reply::Stamp(Some(written)), _) =
            handle(Call::Write { path: file.clone() }, b"hello".to_vec(), &caches, &GO)
        else {
            panic!("写回没交回版本戳")
        };
        assert_eq!(fs::read(&file).unwrap(), b"hello");
        let stamped = handle(Call::Stamp { path: file.clone() }, Vec::new(), &caches, &GO);
        assert!(matches!(stamped, (Reply::Stamp(Some(stamp)), _) if stamp == written));
        let removed = handle(Call::Edit(FileOp::Remove { path: dir.join("a") }), Vec::new(), &caches, &GO);
        assert!(matches!(removed, (Reply::Edited(Ok(None)), _)) && !file.exists());
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
            let (reply, blob) = handle(Call::Git { root: dir.clone(), op }, Vec::new(), &caches, &GO);
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

    /// 停下的按内容找交回已经找到的，不把整个仓库找完。
    #[test]
    fn a_stopped_grep_gives_up() {
        let dir = std::env::temp_dir().join(format!("runode-project-grep-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).status().unwrap().success());
        fs::write(dir.join("a.txt"), "needle\n").unwrap();
        let grep = || Call::Grep {
            root: dir.clone(),
            pattern: "needle".into(),
            ignore_case: false,
            whole_word: false,
            regex: false,
            pathspecs: Vec::new(),
            limit: 10,
        };
        let caches = Shared::default();
        assert!(matches!(handle(grep(), Vec::new(), &caches, &GO), (Reply::Found(found), _) if found.len() == 1));
        let stopped = AtomicBool::new(true);
        assert!(matches!(handle(grep(), Vec::new(), &caches, &stopped), (Reply::Found(found), _) if found.is_empty()));
        fs::remove_dir_all(&dir).ok();
    }

    /// 问路径在不在时按请求的顺序回，目录和文件分得开。
    #[test]
    fn kinds_tell_files_dirs_and_missing_paths_apart() {
        let dir = std::env::temp_dir().join(format!("runode-project-kinds-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        let paths = vec![dir.join("a.txt"), dir.join("nope"), dir.clone()];
        let (reply, _) = handle(Call::Kinds { paths }, Vec::new(), &Shared::default(), &GO);
        assert!(matches!(reply, Reply::Kinds(kinds) if kinds == [Some(false), None, Some(true)]));
        fs::remove_dir_all(&dir).ok();
    }
}
