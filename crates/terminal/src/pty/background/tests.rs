use std::{
    io::{BufRead as _, BufReader, Write as _},
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::Duration,
};

use super::*;

/// 让 `helper_process` 真的干活的环境变量：`listen` 时监听一个端口，`sleep` 时只是等着。
const HELPER_MODE: &str = "RUNODE_BACKGROUND_TEST_HELPER";
/// helper 等多久就自己退出，测试中途失败时不留下进程。
const HELPER_LIFETIME: Duration = Duration::from_secs(30);

fn pid(id: u32) -> Pid {
    #[cfg(unix)]
    return libc::pid_t::try_from(id).unwrap();
    #[cfg(windows)]
    id
}

/// libtest 里 `helper_process` 的完整名字，给 `--exact` 用。
fn helper_name() -> String {
    let (_, module) = module_path!().split_once("::").unwrap();
    format!("{module}::helper_process")
}

/// 测试自己起的子进程：按 `HELPER_MODE` 监听端口或只是等着，准备好了在标准输出写一行 `ready`。
/// 没设环境变量时什么都不做。
#[test]
#[ignore = "只给别的测试当子进程跑"]
fn helper_process() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    let _listener = (mode == "listen").then(|| TcpListener::bind("127.0.0.1:0").unwrap());
    let mut stdout = std::io::stdout();
    stdout.write_all(b"ready\n").unwrap();
    stdout.flush().unwrap();
    std::thread::sleep(HELPER_LIFETIME);
}

/// 结束时杀掉的子进程。
struct Spawned(Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 起一个 `helper_process`，等它说准备好了。
fn spawn_helper(mode: &str) -> Spawned {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &helper_name(), "--ignored", "--nocapture", "--test-threads=1"])
        .env(HELPER_MODE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let spawned = Spawned(child);
    // libtest 先在同一行写出 `test <名字> ... `，再跑测试。
    let ready = stdout.lines().map_while(Result::ok).any(|line| line.ends_with("ready"));
    assert!(ready, "helper exited before it was ready");
    spawned
}

/// 在监听端口的进程认得出来，只是等着的认不出来。
#[test]
fn listening_processes_are_found() {
    let _listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let listening = Listening::read().unwrap();
    assert!(listening.any(&[pid(std::process::id())]));

    let sleeper = spawn_helper("sleep");
    let server = spawn_helper("listen");
    let listening = Listening::read().unwrap();
    assert!(!listening.any(&[pid(sleeper.0.id())]));
    assert!(listening.any(&[pid(server.0.id())]));
    assert!(listening.any(&[pid(sleeper.0.id()), pid(server.0.id())]));
}

/// 子树里有自己的孙子进程。
#[cfg(unix)]
#[test]
fn the_subtree_reaches_grandchildren() {
    let agent = AgentShell::spawn("sh -c '/bin/sleep 30; :' & wait");
    let found = poll(|| {
        let tree = Tree::read()?;
        let shell = *tree.children(agent.pid()).first()?;
        let grandchildren = tree.children(shell);
        (!grandchildren.is_empty()).then(|| (shell, grandchildren, subtree(&tree, agent.pid())))
    });
    let (shell, grandchildren, all) = found.expect("the agent's shell never started its command");
    assert!(all.starts_with(&[agent.pid()]));
    assert!(all.contains(&shell));
    assert!(grandchildren.iter().all(|pid| all.contains(pid)));
}

/// `/proc/net/tcp` 里只取状态为 `0A` 的行的 inode。
#[test]
fn proc_net_tcp_keeps_listening_inodes() {
    let table = concat!(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
        "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1\n",
        "   1: 0100007F:1435 0100007F:A2B4 01 00000000:00000000 00:00000000 00000000  1000        0 4243 1\n",
        "   2: 0100007F:1F91 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 0 1\n",
    );
    assert_eq!(listening_inodes(table).collect::<Vec<_>>(), [4242]);
}

/// 一个当 agent 的 `sh -c`，自成进程组，测试结束时连同它起的进程整组杀掉。它的子进程只有这条测试
/// 起的，不会和并行跑的别的测试起的进程混在一起。
#[cfg(unix)]
struct AgentShell(Child);

#[cfg(unix)]
impl AgentShell {
    fn spawn(script: &str) -> Self {
        use std::os::unix::process::CommandExt as _;
        let child = Command::new("/bin/sh")
            .args(["-c", script])
            .env("RUNODE_TEST_EXE", std::env::current_exe().unwrap())
            .env("RUNODE_TEST_HELPER", helper_name())
            .env(HELPER_MODE, "listen")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }

    fn pid(&self) -> Pid {
        pid(self.0.id())
    }
}

#[cfg(unix)]
impl Drop for AgentShell {
    fn drop(&mut self) {
        // SAFETY: 只给这条测试起的进程组发信号。
        unsafe { libc::kill(-self.pid(), libc::SIGKILL) };
        let _ = self.0.wait();
    }
}

/// 反复试 `check`，直到它给出结果或者过了上限。
#[cfg(unix)]
fn poll<T>(mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(found) = check() {
            return Some(found);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 直到 `background_shells` 给出 `expected`，或者过了上限时给出最后一次的结果。
#[cfg(unix)]
fn settle(agent: Pid, foreground: &[Pid], expected: BackgroundShells) -> Option<BackgroundShells> {
    let mut last = None;
    poll(|| {
        last = background_shells(agent, foreground);
        (last == Some(expected)).then_some(())
    });
    last
}

/// agent 后台跑着命令的 shell 算一个、不在监听；不是 shell 的子进程不算。
#[cfg(unix)]
#[test]
fn a_shell_running_a_command_is_not_serving() {
    let agent = AgentShell::spawn("sh -c '/bin/sleep 30; :' & /bin/sleep 30 & wait");
    let expected = BackgroundShells { shells: 1, serving: 0 };
    assert_eq!(settle(agent.pid(), &[agent.pid()], expected), Some(expected));
}

/// shell 的子树里有进程在监听端口时算服务；shell 在前台进程组里时不算 agent 的后台 shell。
#[cfg(unix)]
#[test]
fn a_shell_whose_command_listens_is_serving() {
    let agent = AgentShell::spawn(
        r#"sh -c '"$RUNODE_TEST_EXE" --exact "$RUNODE_TEST_HELPER" --ignored --nocapture --test-threads=1; :' & wait"#,
    );
    let expected = BackgroundShells { shells: 1, serving: 1 };
    assert_eq!(settle(agent.pid(), &[agent.pid()], expected), Some(expected));

    let mut foreground = Tree::read().unwrap().children(agent.pid());
    foreground.push(agent.pid());
    assert_eq!(background_shells(agent.pid(), &foreground), Some(BackgroundShells { shells: 0, serving: 0 }));
}
