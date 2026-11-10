//! 远端的 `runode --ssh-project`：从 stdin 一条条读桌面的请求，每条起一个线程办，结果写回 stdout；stdin
//! 关了（桌面断开）就退出。`Call::Cancel` 不起线程，当场让在办的那条停下。经远程访问配对的电脑要读写这台
//! 电脑上的项目时，同一套服务跑在它那条连接上（`serve_on`）。

use std::{
    collections::HashMap,
    io::{self, BufRead, BufReader, Read, Write},
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
    places: Places,
}

/// 项目服务在这台机器上用的目录。
#[derive(Default)]
pub(super) struct Places {
    /// 放上传的目录，没有 home 目录时为空。
    pub uploads: Option<PathBuf>,
    /// 移到回收站时用的 home 目录回收站（`Dirs::trash_dir`）。
    pub home_trash: Option<PathBuf>,
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

/// 在任意一对读写流上跑项目服务，`input` 读完（对面断开）时返回退出码。
pub(crate) fn serve_on(input: impl Read, output: impl Write + Send + 'static) -> i32 {
    serve(BufReader::new(input), output)
}

/// 从 `input` 一条条读请求，回话写进 `output`；`input` 读完（桌面断开）时返回退出码。传来的文件放在 home
/// 目录下的 `UPLOADS_DIR`，移到回收站用这台机器的 `Dirs::trash_dir`。
pub(super) fn serve(input: impl BufRead, output: impl Write + Send + 'static) -> i32 {
    let dirs = runode_paths::Dirs::from_env();
    let uploads = dirs.home.as_ref().map(|home| home.join(runode_ssh::bootstrap::UPLOADS_DIR));
    serve_with(input, output, Places { uploads, home_trash: dirs.trash_dir() })
}

/// `serve`，用的目录换成 `places`。
pub(super) fn serve_with(mut input: impl BufRead, output: impl Write + Send + 'static, places: Places) -> i32 {
    let output = Arc::new(Mutex::new(output));
    let caches = Shared::new(Mutex::new(Caches { places, ..Caches::default() }));
    // 在办的请求各自的停止标记，按编号记。
    let running = Arc::new(Mutex::new(HashMap::<u64, Arc<AtomicBool>>::new()));
    let _stop_all = StopAll(running.clone());
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
            let (mut reply, blob) = handle(request.call, content, &caches, &stop);
            reply.drop_unsendable();
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

/// `serve_with` 返回时把还在办的请求都叫停：回话已经没人收，停得下的（按内容找、等改动）不再白跑。
struct StopAll(Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>);

impl Drop for StopAll {
    fn drop(&mut self) {
        for stop in self.0.lock().unwrap_or_else(PoisonError::into_inner).values() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// `path` 在不在、是不是目录（`Reply::Kinds`），跟着符号链接走。
pub(super) fn kind(path: &std::path::Path) -> Option<bool> {
    std::fs::metadata(path).ok().map(|meta| meta.is_dir())
}

/// `dir` 所在仓库的 `RepoSummary`；不在仓库里时不再问别的。
pub(super) fn repo_badge(dir: &std::path::Path) -> super::RepoSummary {
    let Some(branch) = runode_git::current_branch(dir) else {
        return super::RepoSummary::default();
    };
    let real = |path: PathBuf| runode_host::canonicalize(&path).unwrap_or(path);
    let location = runode_git::repo_location(dir);
    let mut common_git_dir = location.as_ref().map(|location| real(location.common_dir.clone()));
    let mut main_worktree = location.and_then(|location| location.main_worktree).map(real);
    let home = runode_paths::Dirs::from_env().home.map(real);
    if under_home_repo(&real(dir.to_path_buf()), main_worktree.as_deref(), home.as_deref()) {
        (common_git_dir, main_worktree) = (None, None);
    }
    super::RepoSummary { branch: Some(branch), origin: runode_git::origin_url(dir), common_git_dir, main_worktree }
}

/// `dir` 不是家目录、所在仓库的主工作树却是家目录：家目录是管配置文件的仓库，下面没进别的仓库的目录不归成它的
/// 项目，也不在家目录下建工作树。路径都解开了符号链接。
fn under_home_repo(
    dir: &std::path::Path,
    main_worktree: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> bool {
    home.is_some_and(|home| main_worktree == Some(home) && dir != home)
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
        Call::Edit(op) => {
            let home_trash = lock().places.home_trash.clone();
            (Reply::Edited(op.run(home_trash.as_deref())), Vec::new())
        }
        Call::ListFiles { root } => {
            let files = runode_git::list_files(&root).unwrap_or_else(|| runode_files::walk_files(&root));
            let files = files.into_iter().filter(|rel| root.join(rel).symlink_metadata().is_ok()).collect();
            (Reply::Files(files), Vec::new())
        }
        Call::Grep { root, pattern, ignore_case, whole_word, regex, pathspecs, limit } => {
            let query =
                runode_git::GrepQuery { pattern: &pattern, ignore_case, whole_word, regex, pathspecs: &pathspecs };
            match runode_git::grep(&root, &query, limit, stop) {
                Ok(found) => (Reply::Found(found), Vec::new()),
                Err(err) => (Reply::Failed(err.to_string()), Vec::new()),
            }
        }
        Call::Stamp { path } => (Reply::Stamp(runode_files::stamp(&path)), Vec::new()),
        Call::Ports => (Reply::Ports(crate::system_probe::listening_ports()), Vec::new()),
        Call::RepoBadge { dir } => (Reply::RepoBadge(repo_badge(&dir)), Vec::new()),
        Call::Kinds { paths } => (Reply::Kinds(paths.iter().map(|path| kind(path)).collect()), Vec::new()),
        Call::Write { path } => match runode_paths::replace_file(&path, &content) {
            Ok(()) => (Reply::Stamp(runode_files::stamp(&path)), Vec::new()),
            Err(err) => (Reply::Failed(err.to_string()), Vec::new()),
        },
        Call::Upload(op) => {
            let uploads = lock().places.uploads.clone();
            match op.run(uploads.as_deref(), &content) {
                Ok(path) => (Reply::Upload(path), Vec::new()),
                Err(err) => (Reply::Failed(err.to_string()), Vec::new()),
            }
        }
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
        let stamped = handle(Call::Stamp { path: file }, Vec::new(), &caches, &GO);
        assert!(matches!(stamped, (Reply::Stamp(Some(stamp)), _) if stamp == written));
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

    /// 对面断开时在等改动的请求马上停下，不等满 `WATCH_HOLD`。
    #[test]
    fn a_closed_connection_stops_the_calls_in_flight() {
        let dir = std::env::temp_dir().join(format!("runode-project-closed-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (requests, mut to_service) = std::io::pipe().unwrap();
        let (mut from_service, responses) = std::io::pipe().unwrap();
        let service = std::thread::spawn(move || serve_with(BufReader::new(requests), responses, Places::default()));
        let watch = |id| Request { id, call: Call::Watch { token: 1, dirs: vec![dir.clone()] }, blob: 0 };
        wire::write(&mut to_service, &watch(1), &[]).unwrap();
        let mut from_service_lines = BufReader::new(&mut from_service);
        let started = wire::read_line::<Response>(&mut from_service_lines).unwrap().unwrap();
        assert_eq!(started.id, 1);
        wire::write(&mut to_service, &watch(2), &[]).unwrap();
        let begun = Instant::now();
        drop(to_service);
        assert_eq!(service.join().unwrap(), 0);
        let waited = wire::read_line::<Response>(&mut from_service_lines).unwrap().unwrap();
        assert_eq!(waited.id, 2);
        assert!(begun.elapsed() < WATCH_HOLD / 2, "waited {:?}", begun.elapsed());
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

    #[test]
    fn dirs_below_a_home_repo_are_not_its_project() {
        use std::path::Path;
        let (home, notes) = (Path::new("/home/u"), Path::new("/home/u/notes"));
        assert!(super::under_home_repo(notes, Some(home), Some(home)));
        assert!(!super::under_home_repo(home, Some(home), Some(home)));
        assert!(!super::under_home_repo(notes, Some(notes), Some(home)));
        assert!(!super::under_home_repo(notes, Some(home), None));
        assert!(!super::under_home_repo(notes, None, Some(home)));
    }

    /// 侧栏要的分支、`origin` 和主工作树：仓库里都回，不在仓库里时都为空。
    #[test]
    fn repo_badge_reports_branch_and_origin() {
        let dir = std::env::temp_dir().join(format!("runode-project-badge-{}", std::process::id()));
        let repo = dir.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap();
        git(&["init", "-q", "-b", "topic"]);
        git(&["remote", "add", "origin", "git@github.com:runode-dev/runode.git"]);
        let (reply, _) = handle(Call::RepoBadge { dir: repo.clone() }, Vec::new(), &Shared::default(), &GO);
        let Reply::RepoBadge(badge) = reply else { panic!("{reply:?}") };
        assert_eq!(badge.branch.as_deref(), Some("topic"));
        assert_eq!(badge.origin.as_deref(), Some("git@github.com:runode-dev/runode.git"));
        assert_eq!(badge.common_git_dir, Some(runode_host::canonicalize(&repo.join(".git")).unwrap()));
        assert_eq!(badge.main_worktree, Some(runode_host::canonicalize(&repo).unwrap()));
        let (reply, _) = handle(Call::RepoBadge { dir: dir.clone() }, Vec::new(), &Shared::default(), &GO);
        assert!(matches!(reply, Reply::RepoBadge(badge) if badge == crate::project_host::RepoSummary::default()));
        fs::remove_dir_all(&dir).ok();
    }
}
