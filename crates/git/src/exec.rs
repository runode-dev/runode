//! 跑写仓库的 git 命令：限时、杀掉整组进程、ssh 不问口令，把退出状态整理成结果。

use std::{
    ffi::OsStr,
    io::{ErrorKind, Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, PoisonError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{GitError, Result, git, git_command};

/// 一条写仓库的 git 命令最多跑这么久，到时杀掉：推送、拉取要联网，给得长些；等的是 ssh 问口令、
/// 确认主机指纹或者 gpg 等着输密码这种永远等不来的输入时，不让后台线程一直挂着。
pub(crate) const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// git 退出以后最多再等它的输出管道关上这么久：钩子留在后台的进程拿着管道时等不到头。
const PIPE_GRACE: Duration = Duration::from_secs(1);

/// 超时时先让 git 自己收拾（删掉 `index.lock` 这类锁文件）最多这么久，再强杀。
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(2);

/// 跑一条会写仓库的 git 命令，返回标准输出。和只读的 `git` 不同：不设 `GIT_OPTIONAL_LOCKS`，
/// 要拿的锁照常拿；`GIT_TERMINAL_PROMPT=0` 让要密码的远端直接报错，不卡着等输入，ssh 同样不问
/// （`batch_ssh`）；`input` 给了就写进标准输入，否则标准输入是空的。超过 `RUN_TIMEOUT` 杀掉，
/// 报超时。失败时的错误是 git 的标准错误，没有就用标准输出。
pub(crate) fn run<S: AsRef<OsStr>>(
    dir: &Path,
    args: impl IntoIterator<Item = S>,
    input: Option<&[u8]>,
) -> Result<Vec<u8>> {
    run_for(dir, args, input, RUN_TIMEOUT, &NO_ENV)
}

/// 不另设环境变量。
const NO_ENV: [(&str, &str); 0] = [];

/// 在 `dir` 里跑一条 git 命令，`input` 给了就写进标准输入，最多跑 `timeout`，另外设上 `env` 里的环境变量。
pub(crate) fn run_for<S: AsRef<OsStr>, K: AsRef<OsStr>, V: AsRef<OsStr>>(
    dir: &Path,
    args: impl IntoIterator<Item = S>,
    input: Option<&[u8]>,
    timeout: Duration,
    env: &[(K, V)],
) -> Result<Vec<u8>> {
    let mut command = git_command(dir)?;
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .env("GIT_TERMINAL_PROMPT", "0")
        // 关掉「hint:」建议：本地化后的提示不以「hint:」开头，事后滤不掉。
        .env("GIT_ADVICE", "0")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 自成一个进程组，超时时连同 git 拉起的 ssh、gpg 一起杀掉，它们拿着输出的管道。
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    batch_ssh(&mut command, dir);
    #[cfg(windows)]
    let started = std::time::SystemTime::now();
    let mut child = command.spawn().map_err(|err| GitError::Spawn(err.to_string()))?;
    // 一边写输入一边读输出，输入输出都大时谁也不会等着谁。不用作用域线程：超时杀掉以后，脱离了
    // 进程组的子孙进程可能还拿着管道，读的线程就留在那里，不等它们。
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_vec();
        thread::spawn(move || {
            // git 不读完就退出时写会失败，以它的退出状态为准。
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = Drain::spawn(child.stdout.take());
    let stderr = Drain::spawn(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                kill_group(&mut child);
                #[cfg(windows)]
                let lock = left_lock(dir, started);
                #[cfg(not(windows))]
                let lock = None;
                return Err(GitError::Timeout { secs: timeout.as_secs(), lock });
            }
            Err(err) => {
                kill_group(&mut child);
                return Err(GitError::Spawn(err.to_string()));
            }
        }
    };
    // git 写的这时都已经在管道里了。钩子拉起的后台进程继承了管道、一直不关时读不到头，最多等
    // `PIPE_GRACE`，拿到现在读到的，读的线程留给它们。
    let drained = Instant::now() + PIPE_GRACE;
    while !(stdout.is_finished() && stderr.is_finished()) && Instant::now() < drained {
        thread::sleep(Duration::from_millis(10));
    }
    output_result(status, stdout.take(), &stderr.take())
}

/// 在另一个线程里读 `pipe` 直到关上，读到的随时可取。取过一次或者丢掉以后，线程接着读、但读到的都扔掉：
/// 钩子留在后台的进程还拿着管道、一直在写时，不再为没人要的输出攒内存，也不让它写不进去。
struct Drain {
    /// `None` 是已经取走了，之后读到的不再留。
    read: Arc<Mutex<Option<Vec<u8>>>>,
    thread: JoinHandle<()>,
}

impl Drain {
    fn spawn(pipe: Option<impl Read + Send + 'static>) -> Self {
        let read = Arc::new(Mutex::new(Some(Vec::new())));
        let out = read.clone();
        let thread = thread::spawn(move || {
            let Some(mut pipe) = pipe else { return };
            let mut chunk = [0; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(read) = out.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
                            read.extend_from_slice(&chunk[..n]);
                        }
                    }
                    // 信号打断的读接着读，不然输出缺一段却当成读完了。
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        });
        Self { read, thread }
    }

    /// 管道关上了，读完了。
    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// 到现在读到的。只能取一次，再取是空的。
    fn take(&self) -> Vec<u8> {
        self.read.lock().unwrap_or_else(PoisonError::into_inner).take().unwrap_or_default()
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        self.take();
    }
}

/// 没设 `GIT_SSH_COMMAND`、`GIT_SSH` 和 `core.sshCommand` 时让 ssh 用 `BatchMode`：要输口令、
/// 确认主机指纹时直接失败，不在开着 runode 的终端上等输入。设了的是用户自己的选择，不改：
/// `GIT_SSH_COMMAND` 压过 `core.sshCommand` 和 `GIT_SSH`，这里要是设了就把用户的盖掉了。
fn batch_ssh(command: &mut Command, dir: &Path) {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if set("GIT_SSH_COMMAND") || set("GIT_SSH") || git(dir, &["config", "--get", "core.sshCommand"]).is_some() {
        return;
    }
    command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
}

/// 杀掉 `run_for` 拉起的 git 所在的整个进程组，再收掉 git。先发 TERM，git 收到会删掉自己拿着的锁文件
/// 再退出；直接 KILL 会把 `index.lock` 留下，之后的 git 命令都说有别的 git 在跑。等 `TERM_GRACE` 还没
/// 退的（不理 TERM 的钩子）再 KILL。
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        signal_group(child, libc::SIGTERM);
        let deadline = Instant::now() + TERM_GRACE;
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        signal_group(child, libc::SIGKILL);
    }
    #[cfg(windows)]
    kill_tree(child);
    let _ = child.kill();
    let _ = child.wait();
}

/// 给 git 所在的进程组发 `signal`。进程组号就是 git 的进程号（`process_group(0)`）。
#[cfg(unix)]
fn signal_group(child: &Child, signal: libc::c_int) {
    let Ok(group) = libc::pid_t::try_from(child.id()) else { return };
    // SAFETY: 只发信号，不碰内存；组已经没了时返回错误，不管它。
    unsafe { libc::killpg(group, signal) };
}

/// Windows 上强杀的 git 来不及删自己拿着的 `index.lock`，之后的写操作都说有别的 git 在跑。`started`（拉起
/// git 时）之后写的锁文件多半是被杀的这次留下的，但也可能是同时在跑的别的 git 的，那时删了会让两边同时写
/// 索引，所以只找出来告诉用户，不删。看修改时间不看创建时间：NTFS 的隧道效应让删掉后十几秒内重建的同名
/// 文件沿用旧文件的创建时间。
#[cfg(windows)]
fn left_lock(dir: &Path, started: std::time::SystemTime) -> Option<std::path::PathBuf> {
    let path = git(dir, &["rev-parse", "--git-path", "index.lock"])?;
    let lock = dir.join(crate::os_str(path.trim_ascii()));
    let modified = std::fs::metadata(&lock).and_then(|meta| meta.modified()).ok()?;
    (modified >= started).then_some(lock)
}

/// Windows 没有 TERM，连同子孙进程一起强杀（`taskkill /T /F`）。
#[cfg(windows)]
fn kill_tree(child: &Child) {
    let mut command = Command::new("taskkill");
    command.args(["/T", "/F", "/PID", &child.id().to_string()]).stdin(Stdio::null()).stdout(Stdio::null());
    crate::hide_window(&mut command);
    let _ = command.stderr(Stdio::null()).status();
}

/// git 退出以后，成功时是标准输出，失败时把标准错误（没有就用标准输出）整理成错误。
pub(crate) fn output_result(status: ExitStatus, stdout: Vec<u8>, stderr: &[u8]) -> Result<Vec<u8>> {
    if status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(stderr);
    let message: Vec<_> = stderr.lines().filter(|line| !line.starts_with("hint:")).collect();
    let message = message.join("\n").trim().to_owned();
    let message = if message.is_empty() { String::from_utf8_lossy(&stdout).trim().to_owned() } else { message };
    if message.is_empty() {
        return Err(GitError::Exited(status.to_string()));
    }
    Err(GitError::Git(message))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{fs, path::PathBuf};

    use super::*;

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-git-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(git(&dir, &["init", "-q"]).is_some());
        dir
    }

    #[cfg(unix)]
    #[test]
    fn kills_git_that_runs_past_the_timeout() {
        let dir = temp_repo("timeout");
        // `commit -a` 跑钩子时拿着 `index.lock`。
        let commit = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "x"];
        fs::write(dir.join("f"), "a").unwrap();
        assert!(git(&dir, &["add", "f"]).is_some());
        assert!(git(&dir, &commit).is_some());
        fs::write(dir.join("f"), "b").unwrap();
        // pre-commit 钩子一直不退出，像 ssh、gpg 等着输入那样；它把进程号记下来，好看它也被杀掉了。
        let pid_file = dir.join("hook.pid");
        let hooks = dir.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        fs::write(&hook, format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n", pid_file.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(git(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]).is_some());
        let started = Instant::now();
        // 给足时间让钩子跑起来、记下进程号：整套测试一起跑时机器很忙。
        let err = run_for(&dir, commit, None, Duration::from_secs(3), &NO_ENV).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(12), "{:?}", started.elapsed());
        assert!(matches!(err, GitError::Timeout { lock: None, .. }), "{err}");
        // 钩子在 git 的进程组里，跟着一起杀掉；死了以后要等被收掉，`kill -0` 才找不到它。
        let pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        let alive = || Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "钩子 {pid} 还活着");
        // 先发的 TERM 让 git 删掉了自己拿着的锁，后面的 git 命令不会说有别的 git 在跑。
        assert!(!dir.join(".git/index.lock").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Windows 上强杀的 git 留下的 `index.lock` 不删（可能是别的 git 的），在报错里指给用户。
    #[cfg(windows)]
    #[test]
    fn points_at_the_lock_left_by_a_killed_git() {
        let dir = temp_repo("timeout-lock");
        let commit = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-a", "-m", "x"];
        fs::write(dir.join("f"), "a").unwrap();
        assert!(git(&dir, &["add", "f"]).is_some());
        assert!(git(&dir, &commit).is_some());
        fs::write(dir.join("f"), "b").unwrap();
        let hooks = dir.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        // 钩子跑起来时 git 已经拿着 `index.lock`，记个标记，好分清是没等到钩子还是锁没找着。
        fs::write(hooks.join("pre-commit"), "#!/bin/sh\necho > hook-started\nexec sleep 30\n").unwrap();
        assert!(git(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]).is_some());
        // 给足时间让钩子跑起来：整套测试一起跑时 CI 的 Windows 很忙，起一个 git 和 sh 要好几秒。
        let err = run_for(&dir, commit, None, Duration::from_secs(10), &NO_ENV).unwrap_err();
        assert!(dir.join("hook-started").exists(), "钩子没跑起来：{err}");
        assert!(matches!(&err, GitError::Timeout { lock: Some(lock), .. } if lock.ends_with("index.lock")), "{err}");
        assert!(dir.join(".git/index.lock").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 钩子把进程留在后台、拿着 git 的输出管道：git 退出了就算跑完，不等那个进程。
    #[cfg(unix)]
    #[test]
    fn returns_when_git_exits_though_a_hook_left_the_pipes_open() {
        let dir = temp_repo("background-hook");
        let pid_file = dir.join("hook.pid");
        let hooks = dir.join(".git/test-hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("post-commit");
        fs::write(&hook, format!("#!/bin/sh\nsleep 30 &\necho $! > '{}'\n", pid_file.display())).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(git(&dir, &["config", "core.hooksPath", &hooks.to_string_lossy()]).is_some());
        let started = Instant::now();
        let args = ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"];
        let result = run_for(&dir, args, None, Duration::from_secs(20), &NO_ENV);
        let elapsed = started.elapsed();
        let pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
        let _ = Command::new("kill").arg(&pid).status();
        assert!(result.is_ok(), "{result:?}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// 取走以后管道里再来的不攒着，线程照样读到管道关上。
    #[test]
    fn discards_output_read_after_it_was_taken() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let drain = Drain::spawn(Some(reader));
        writer.write_all(b"a").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while drain.read.lock().unwrap().as_ref().is_some_and(Vec::is_empty) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(drain.take(), b"a");
        writer.write_all(&[b'b'; 100_000]).unwrap();
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !drain.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(drain.is_finished());
        assert!(drain.read.lock().unwrap().is_none());
    }

    #[test]
    fn keeps_the_users_ssh_command() {
        let dir = temp_repo("ssh");
        let env = |command: &Command| {
            command
                .get_envs()
                .find(|(name, _)| *name == "GIT_SSH_COMMAND")
                .and_then(|(_, value)| value.map(OsStr::to_owned))
        };
        let user_set = std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || git(&dir, &["config", "--get", "core.sshCommand"]).is_some();
        let mut command = Command::new("git");
        batch_ssh(&mut command, &dir);
        if !user_set {
            assert_eq!(env(&command), Some("ssh -o BatchMode=yes".into()));
        }
        // 仓库里配了 `core.sshCommand` 时不设，不然它被环境变量盖掉。
        assert!(git(&dir, &["config", "core.sshCommand", "ssh -i key"]).is_some());
        let mut command = Command::new("git");
        batch_ssh(&mut command, &dir);
        assert_eq!(env(&command), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
