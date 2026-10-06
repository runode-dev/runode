//! 宿主升级时的交接，真的跨进程：旧宿主、新宿主（以及假装的新宿主）各是一个进程，由这个测试
//! 程序带上 `--exact <角色测试名>` 和环境变量 `RUNODE_HANDOFF_ROLE` 重新执行自己。没设这个变量时
//! 各个角色测试什么都不做，直接通过。角色进程的结果和日志写在测试的临时目录里。
//!
//! 只验回滚、拒绝这类不需要旧宿主退出的情况时，旧宿主就在测试进程里（`old_here`），能直接设
//! 等新宿主的期限、数自己的描述符。

mod common;

use std::{
    collections::HashSet,
    fs::File,
    io::Write as _,
    os::unix::{
        net::{UnixListener, UnixStream},
        process::ExitStatusExt as _,
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use common::{Peer, SIZE, script, temp_dir};
use runode_host::{
    BuildId, ClientMsg, HandoffRefusal, Host, HostMsg, SessionId, Stopped, TakeOverError, TakeOverOptions,
};
use runode_protocol::{
    AttachMode, Caps, ClientKind, Frame, FrameKind, GoodbyeReason, HANDOFF_FORMAT, HandoffPart,
    OLDEST_READABLE_HANDOFF_FORMAT, PROTOCOL_VERSION, decode_part, read_frame, write_frame,
};
use runode_shared_types::{agent::AgentState, grid::GridSize, shell::IntegrationMode};
use runode_terminal::fd_passing;

/// 角色进程演哪个角色：`old`、`successor`、`fake`。
const ROLE: &str = "RUNODE_HANDOFF_ROLE";
/// 角色进程用的临时目录：socket、锁、结果和日志都在里面。
const DIR: &str = "RUNODE_HANDOFF_DIR";
/// 新宿主的结果文件、日志用的名字，连续交接时一个一个分开。
const TAG: &str = "RUNODE_HANDOFF_TAG";
/// 新宿主当自己的快照是这个格式版本，见 `TakeOverOptions::snapshot_format`。
const SNAPSHOT_FORMAT: &str = "RUNODE_HANDOFF_SNAPSHOT_FORMAT";
/// 假的新宿主收下所有会话后怎么办：`die` 退出，`hang` 一直不回话，`abort` 回 `HandoffAbort`。
const FAKE: &str = "RUNODE_HANDOFF_FAKE";

const OLD_BUILD: &str = "old-build";
const NEW_BUILD: &str = "new-build";
/// 角色进程没有会话也没有连接这么久就退出。
const ROLE_IDLE: Duration = Duration::from_secs(20);
/// 等角色进程的结果。调试构建的测试程序起来要一会儿。
const ROLE_WAIT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------------------------
// 角色

/// 旧宿主：单独一个进程跑着，直到交接出去（或者空闲）。退出的原因写进 `old.stopped`。
#[test]
fn role_old() {
    let Some(dir) = role("old") else { return };
    log_to(&dir.join("old.log"));
    let host = Host::new(BuildId(OLD_BUILD.into()));
    host.mark_standalone();
    host.listen(&socket_in(&dir), &dir.join("host.lock")).unwrap();
    let stopped = host.run_until_idle(ROLE_IDLE);
    std::fs::write(dir.join("old.stopped"), format!("{stopped:?}")).unwrap();
}

/// 新宿主：接手 socket 上的旧宿主，结果写进 `<tag>.result`（`ok <会话数> <退成重放的会话>` 或者
/// `err <错误>`），成功的话之前先把自己开着的描述符个数写进 `<tag>.fds`，然后照常跑到交接出去
/// 或者空闲，原因写进 `<tag>.stopped`。
#[test]
fn role_successor() {
    let Some(dir) = role("successor") else { return };
    let tag = std::env::var(TAG).unwrap_or_else(|_| "successor".into());
    log_to(&dir.join(format!("{tag}.log")));
    let host = Host::new(BuildId(NEW_BUILD.into()));
    host.mark_standalone();
    let snapshot_format = std::env::var(SNAPSHOT_FORMAT).ok().map(|format| format.parse().unwrap());
    let result = host.take_over(&socket_in(&dir), TakeOverOptions { snapshot_format });
    let text = match &result {
        Ok(report) => {
            // 交接用的连接、管道这些稍后才关完。
            thread::sleep(Duration::from_millis(200));
            std::fs::write(dir.join(format!("{tag}.fds")), open_fds().to_string()).unwrap();
            let replayed: Vec<String> = report.replayed.iter().map(ToString::to_string).collect();
            format!("ok {} {}", report.sessions, replayed.join(","))
        }
        Err(err) => format!("err {err:?}"),
    };
    std::fs::write(dir.join(format!("{tag}.result")), text.trim_end()).unwrap();
    if result.is_ok() {
        let stopped = host.run_until_idle(ROLE_IDLE);
        std::fs::write(dir.join(format!("{tag}.stopped")), format!("{stopped:?}")).unwrap();
    }
}

/// 假的新宿主：按协议要交接、把会话都收下（描述符随即关掉），收完写 `fake.received`，然后按
/// `RUNODE_HANDOFF_FAKE` 退出、卡住或者放弃。
#[test]
fn role_fake_successor() {
    let Some(dir) = role("fake") else { return };
    let stream = UnixStream::connect(socket_in(&dir)).unwrap();
    let sessions = receive_everything(&stream);
    std::fs::write(dir.join("fake.received"), sessions.to_string()).unwrap();
    match std::env::var(FAKE).unwrap().as_str() {
        "die" => std::process::exit(0),
        "hang" => loop {
            thread::sleep(Duration::from_secs(3600));
        },
        "abort" => send(&stream, &ClientMsg::HandoffAbort { reason: "testing".into() }),
        other => panic!("unknown fake {other}"),
    }
}

/// 在这个进程里演角色时返回目录。
fn role(name: &str) -> Option<PathBuf> {
    (std::env::var(ROLE).ok()? == name).then(|| PathBuf::from(std::env::var_os(DIR).unwrap()))
}

fn socket_in(dir: &Path) -> PathBuf {
    dir.join("host.sock")
}

/// 角色进程的日志：INFO 及以上写进 `path`，交接的用时、降级的会话和失败的原因都在里面。
fn log_to(path: &Path) {
    struct FileLog(Mutex<File>);
    struct Message(String);
    impl tracing::field::Visit for Message {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
    impl tracing::Subscriber for FileLog {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::INFO
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut message = Message(String::new());
            event.record(&mut message);
            let mut file = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = writeln!(file, "{}{}", event.metadata().level(), message.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    let file = File::create(path).unwrap();
    tracing::subscriber::set_global_default(FileLog(Mutex::new(file))).unwrap();
}

/// 这个进程开着的描述符个数。
fn open_fds() -> usize {
    // `read_dir` 自己开着的那一个每次都算在里面，不影响比较。
    std::fs::read_dir("/dev/fd").unwrap().count()
}

// ---------------------------------------------------------------------------------------------
// 测试这边的工具

/// 一个角色进程，丢掉时杀掉它。
struct Role {
    child: std::process::Child,
}

impl Drop for Role {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Role {
    fn start(name: &str, dir: &Path, env: &[(&str, &str)]) -> Self {
        let test = match name {
            "old" => "role_old",
            "successor" => "role_successor",
            "fake" => "role_fake_successor",
            "rollbacks" => "role_rollbacks",
            other => panic!("unknown role {other}"),
        };
        let tag = env.iter().find(|(key, _)| *key == TAG).map_or(name, |(_, tag)| tag);
        let stderr = File::create(dir.join(format!("{tag}.stderr"))).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(ROLE, name)
            .env(DIR, dir)
            // shell 从家目录开始；别碰用户真实的家目录。
            .env("HOME", dir)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        Self { child }
    }

    /// 等它退出，最多 `ROLE_WAIT`。
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + ROLE_WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the role process did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// 等 `path` 出现、写完，返回内容。
fn wait_file(path: &Path) -> String {
    let deadline = Instant::now() + ROLE_WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {}", path.display());
        thread::sleep(Duration::from_millis(10));
    }
}

/// 起一个单独进程的旧宿主，等它开始监听，返回它和 socket。
fn old_process(dir: &Path) -> (Role, PathBuf) {
    let old = Role::start("old", dir, &[]);
    let socket = socket_in(dir);
    let deadline = Instant::now() + ROLE_WAIT;
    while UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "the old host did not start listening");
        thread::sleep(Duration::from_millis(10));
    }
    (old, socket)
}

/// 在这个进程里开一个单独跑的旧宿主（没进 `run_until_idle`，交接出去后也不退出）。
fn old_here(dir: &Path) -> (Host, PathBuf) {
    let host = Host::new(BuildId(OLD_BUILD.into()));
    host.mark_standalone();
    let socket = socket_in(dir);
    host.listen(&socket, &dir.join("host.lock")).unwrap();
    (host, socket)
}

/// 起新宿主接手，返回它和它的结果。
fn take_over(dir: &Path, env: &[(&str, &str)]) -> (Role, String) {
    let successor = Role::start("successor", dir, env);
    let tag = env.iter().find(|(key, _)| *key == TAG).map_or("successor", |(_, tag)| tag);
    let result = wait_file(&dir.join(format!("{tag}.result")));
    (successor, result)
}

/// 连上（新）宿主，返回连接和它的构建。
fn hello_build(socket: &Path) -> (Peer, BuildId) {
    let mut peer = Peer::connect(socket);
    peer.send(&ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: BuildId(common::BUILD.into()),
        client: ClientKind::Cli,
        caps: Caps::default(),
        session: None,
        device: None,
    });
    match peer.message() {
        HostMsg::Welcome { build, handoff, .. } => {
            assert_eq!(handoff, HANDOFF_FORMAT);
            (peer, build)
        }
        other => panic!("expected welcome, got {other:?}"),
    }
}

/// 一个从 1 数到 `count` 的会话：每行是补零到 6 位的数加上 `pad` 个 `x`，每数 `batch` 个歇
/// `pause` 秒，数完打 `end`。
fn counter(dir: &Path, name: &str, count: u32, batch: u32, pause: &str, pad: usize) -> String {
    let pad = "x".repeat(pad);
    script(
        dir,
        name,
        &format!(
            "i=0\nwhile [ $i -lt {count} ]; do\n  j=0\n  while [ $j -lt {batch} ] && [ $i -lt {count} ]; do\n    \
             i=$((i+1)); printf '%06d{pad}\\n' $i; j=$((j+1))\n  done\n  sleep {pause}\ndone\necho end\nexec sleep 1000"
        ),
    )
}

/// 屏幕上（含回滚历史）数到的数，按行的先后。
fn counted(text: &str) -> Vec<u32> {
    text.lines().filter_map(|line| line.get(..6)?.parse().ok()).collect()
}

/// 数完了的计数会话：回滚历史里从 1 到 `count` 一个不少、一个不多。
fn assert_counted(peer: &mut Peer, id: SessionId, count: u32) {
    let text = peer.wait_screen(id, Some(count + 100), |text| text.lines().any(|line| line == "end"));
    let numbers = counted(&text);
    let expected: Vec<u32> = (1..=count).collect();
    if numbers != expected {
        let missing: Vec<_> = expected.iter().filter(|n| !numbers.contains(n)).take(10).collect();
        let mut seen = HashSet::new();
        let repeated: Vec<_> = numbers.iter().filter(|n| !seen.insert(**n)).take(10).collect();
        panic!("counted {} lines; missing {missing:?}, repeated {repeated:?}", numbers.len());
    }
}

/// 等计数会话数到至少 `at`。
fn wait_counted(peer: &mut Peer, id: SessionId, at: u32) {
    peer.wait_screen(id, Some(10), |text| counted(text).last().is_some_and(|&n| n >= at));
}

/// 收到的连接被交接断开：断开前最后一条是 `Goodbye { Handoff }`。
fn assert_handoff_goodbye(peer: &Peer) {
    let messages = peer.until_closed();
    assert!(
        messages.iter().any(|message| matches!(message, HostMsg::Goodbye { reason: GoodbyeReason::Handoff })),
        "{messages:?}"
    );
}

fn send(stream: &UnixStream, message: &ClientMsg) {
    let frame = Frame::control(message).unwrap();
    write_frame(&mut &*stream, frame.kind, 0, &frame.payload).unwrap();
}

fn receive(stream: &UnixStream) -> HostMsg {
    let frame = read_frame(&mut &*stream).unwrap().unwrap();
    assert_eq!(frame.kind, FrameKind::Control);
    frame.message().unwrap()
}

/// 假的新宿主：握手、要交接、收下所有会话（描述符都关掉），返回会话数。
fn receive_everything(stream: &UnixStream) -> u32 {
    send(
        stream,
        &ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId("fake".into()),
            client: ClientKind::Successor,
            caps: Caps::default(),
            session: None,
            device: None,
        },
    );
    assert!(matches!(receive(stream), HostMsg::Welcome { handoff: HANDOFF_FORMAT, .. }));
    send(stream, &ClientMsg::Handoff { min_format: OLDEST_READABLE_HANDOFF_FORMAT, max_format: HANDOFF_FORMAT });
    let HostMsg::HandoffBegin { sessions, .. } = receive(stream) else { panic!("expected the handoff to begin") };
    let (data, fds) = fd_passing::recv_with_fds(stream).unwrap();
    assert!(matches!(decode_part(&data).unwrap().0, HandoffPart::Host { .. }));
    assert_eq!(fds.len(), 2);
    for _ in 0..sessions {
        let (data, _fds) = fd_passing::recv_with_fds(stream).unwrap();
        assert!(matches!(decode_part(&data).unwrap().0, HandoffPart::Session { .. }));
    }
    sessions
}

// ---------------------------------------------------------------------------------------------
// 测试

/// 新宿主接过所有会话：数数的程序数到头一个不少一个不多，vim 接着能编辑、写文件，agent 仍是
/// 工作中，没启动的会话照常 `Start`（开会话时设的环境变量还在），shell 已经退出的会话不交；
/// 已有的连接收到 `Goodbye { Handoff }`；旧宿主以 0 退出、socket 文件还在，连上去的是新构建。
#[test]
fn a_new_host_takes_every_session_over() {
    let dir = temp_dir("main");
    let (mut old, socket) = old_process(&dir);
    let mut cli = Peer::hello(&socket, false);
    let count = 300;
    let counting = cli.spawn(&counter(&dir, "count.sh", count, 1, "0.01", 0));
    // 前台是个叫 claude 的程序（在自己的进程组里），标题是工作中的转圈。
    std::os::unix::fs::symlink("/bin/sleep", dir.join("claude")).unwrap();
    let agent_body = format!("printf '\\033]0;\u{280b} fixing\\007'\nset -m\n{}/claude 1000", dir.display());
    let agent = cli.spawn(&script(&dir, "agent.sh", &agent_body));
    let file = dir.join("edited.txt");
    // 没有 vim 的机器上跳过这一项。
    let vim = Path::new("/usr/bin/vim")
        .exists()
        .then(|| cli.spawn(&script(&dir, "vim.sh", &format!("exec /usr/bin/vim -u NONE -N -n {}", file.display()))));
    let later = script(&dir, "later.sh", "echo \"started-$RUNODE_TEST_EXTRA-$RUNODE_SOCKET\"\nexec sleep 1000");
    let unstarted = cli.spawn_with(&later, false, vec![("RUNODE_TEST_EXTRA".into(), "extra".into())], None);
    let exited = cli.spawn(&script(&dir, "exit.sh", "exit 0"));

    let deadline = Instant::now() + common::WAIT;
    let agent_before = loop {
        let sessions = cli.sessions();
        let gone = sessions.iter().any(|s| s.id == exited && s.exited);
        let working = sessions.iter().find(|s| s.id == agent).and_then(|s| s.meta.agent);
        if gone && let Some(working) = working.filter(|agent| agent.state == AgentState::Working) {
            break working;
        }
        assert!(Instant::now() < deadline, "the sessions did not settle: {sessions:?}");
        thread::sleep(Duration::from_millis(20));
    };
    if let Some(vim) = vim {
        cli.wait_screen(vim, None, |text| text.contains('~'));
    }
    wait_counted(&mut cli, counting, 20);
    let mut watcher = Peer::hello(&socket, false);
    watcher.attach(counting, AttachMode::MetaOnly);

    let (_successor, result) = take_over(&dir, &[]);
    let expected = 3 + usize::from(vim.is_some());
    assert_eq!(result, format!("ok {expected}"));
    assert_handoff_goodbye(&watcher);
    assert_handoff_goodbye(&cli);
    assert!(old.wait().success());
    assert_eq!(std::fs::read_to_string(dir.join("old.stopped")).unwrap(), format!("{:?}", Stopped::Handoff));
    assert!(socket.exists());

    let (mut new, build) = hello_build(&socket);
    assert_eq!(build.0, NEW_BUILD);
    let sessions = new.sessions();
    let mut ids: Vec<SessionId> = sessions.iter().map(|s| s.id).collect();
    ids.sort();
    let mut expected: Vec<SessionId> =
        [Some(counting), Some(agent), vim, Some(unstarted)].into_iter().flatten().collect();
    expected.sort();
    assert_eq!(ids, expected, "the exited session is not handed over");
    let agent_now = || sessions.iter().find(|s| s.id == agent).unwrap().meta.agent;
    assert_eq!(agent_now(), Some(agent_before));

    assert_counted(&mut new, counting, count);

    if let Some(vim) = vim {
        let (channel, _) = new.attach(vim, AttachMode::VtReplay);
        new.input(channel, b"ihello");
        thread::sleep(Duration::from_millis(300));
        new.input(channel, b"\x1b");
        thread::sleep(Duration::from_millis(300));
        new.input(channel, b":wq\r");
        new.wait(channel, |message, _| matches!(message, Some(HostMsg::Exited { id, .. }) if *id == vim));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");
    }

    new.send(&ClientMsg::Start { id: unstarted, integration: IntegrationMode::Off });
    let started = format!("started-extra-{}", socket.display());
    new.wait_screen(unstarted, Some(20), |text| text.replace('\n', "").contains(&started));

    // agent 交接之后过一会儿仍是工作中：交接期间没有输出，不会被判成空闲。
    thread::sleep(Duration::from_millis(1500));
    let agent_later = new.sessions().into_iter().find(|s| s.id == agent).unwrap().meta.agent;
    assert_eq!(agent_later, Some(agent_before));
}

/// 交接时程序不读输入，写不进 PTY 的输入跟着交过去，新宿主写进去，程序读到的每一行正好一次。
#[test]
fn input_the_old_host_could_not_write_arrives_exactly_once() {
    let dir = temp_dir("pending");
    let (_old, socket) = old_process(&dir);
    let mut cli = Peer::hello(&socket, false);
    // 不回显、不按行缓冲；先睡着不读输入，交接完了才 `cat` 出来。
    let id = cli.spawn(&script(&dir, "late-cat.sh", "stty -icanon -echo\necho ready\nsleep 3\nexec cat"));
    cli.wait_screen(id, None, |text| text.contains("ready"));
    let (channel, _) = cli.attach(id, AttachMode::MetaOnly);
    let lines = 800;
    let input: String = (1..=lines).map(|n| format!("L{n:04}\n")).collect();
    // PTY 只收得下 1 KiB 左右，剩下的留在旧宿主的写队列里。
    cli.input(channel, input.as_bytes());
    thread::sleep(Duration::from_millis(100));

    let (_successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 1");
    let log = std::fs::read_to_string(dir.join("old.log")).unwrap();
    assert!(log.contains("bytes of input not written yet") && !log.contains(" 0 bytes of input"), "{log}");
    let (mut new, _) = hello_build(&socket);
    let last = format!("L{lines:04}");
    let text = new.wait_screen(id, Some(lines + 100), |text| text.contains(&last));
    for n in 1..=lines {
        let line = format!("L{n:04}");
        assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{line} in {text}");
    }
}

/// 新宿主收下会话后、回 `HandoffReady` 之前死掉：旧宿主回滚，什么都不丢，之后照样能交接。
#[test]
fn a_successor_that_dies_before_it_is_ready_changes_nothing() {
    let dir = temp_dir("dies");
    let (_old, socket) = old_here(&dir);
    let mut cli = Peer::hello(&socket, false);
    let count = 200;
    let id = cli.spawn(&counter(&dir, "count.sh", count, 1, "0.01", 0));
    wait_counted(&mut cli, id, 10);

    let mut fake = Role::start("fake", &dir, &[(FAKE, "die")]);
    assert_eq!(wait_file(&dir.join("fake.received")), "1");
    assert!(fake.wait().success());
    assert_handoff_goodbye(&cli);

    // 又接受连接了，会话接着数。
    let mut again = Peer::hello(&socket, false);
    assert_counted(&mut again, id, count);
    // 再交接一次照样成。
    drop(again);
    let (_successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 1");
    let (mut new, _) = hello_build(&socket);
    new.wait_screen(id, Some(10), |text| text.contains("end"));
}

/// 新宿主卡住不回话：期限一到旧宿主杀掉它、回滚；放弃的（`HandoffAbort`）也回滚。会话都照常。
#[test]
fn a_stuck_or_giving_up_successor_is_rolled_back() {
    let dir = temp_dir("stuck");
    let (old, socket) = old_here(&dir);
    old.set_handoff_deadline(Duration::from_millis(500));
    let mut cli = Peer::hello(&socket, false);
    let count = 200;
    let id = cli.spawn(&counter(&dir, "count.sh", count, 1, "0.01", 0));
    wait_counted(&mut cli, id, 10);

    let mut fake = Role::start("fake", &dir, &[(FAKE, "hang")]);
    assert_eq!(wait_file(&dir.join("fake.received")), "1");
    assert_eq!(fake.wait().signal(), Some(libc::SIGKILL), "the stuck successor must be killed");
    assert_handoff_goodbye(&cli);

    std::fs::remove_file(dir.join("fake.received")).unwrap();
    let mut fake = Role::start("fake", &dir, &[(FAKE, "abort")]);
    assert_eq!(wait_file(&dir.join("fake.received")), "1");
    assert!(fake.wait().success());

    let mut again = Peer::hello(&socket, false);
    assert_counted(&mut again, id, count);
}

/// 程序正在大量输出时交接：输出一行不丢、一行不重。程序一批一批地写满 PTY，交接完了测试才
/// 叫它停（`stop` 文件），所以交接时它一定还在写；数到的行都留在回滚历史里（上限一万行）。
#[test]
fn heavy_output_survives_the_handoff() {
    let dir = temp_dir("heavy");
    let (_old, socket) = old_here(&dir);
    let mut cli = Peer::hello(&socket, false);
    let limit = 9000;
    let stop = dir.join("stop");
    let flood = script(
        &dir,
        "flood.sh",
        &format!(
            "i=0\nwhile [ $i -lt {limit} ] && [ ! -e {} ]; do\n  j=0\n  while [ $j -lt 100 ]; do \
             i=$((i+1)); printf '%06d\\n' $i; j=$((j+1)); done\n  sleep 0.02\ndone\necho end\nexec sleep 1000",
            stop.display()
        ),
    );
    let id = cli.spawn(&flood);
    wait_counted(&mut cli, id, 300);
    let (_successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 1");
    File::create(&stop).unwrap();
    let (mut new, _) = hello_build(&socket);
    let text = new.wait_screen(id, Some(limit + 100), |text| text.lines().any(|line| line == "end"));
    let numbers = counted(&text);
    let last = *numbers.last().unwrap();
    assert!(last < limit, "the output finished before the handoff; nothing was tested");
    assert_eq!(numbers, (1..=last).collect::<Vec<_>>(), "lost or repeated lines");
}

/// 快照格式不同时退成 VT 重放：会话在报告里列为退化的，屏幕上的内容还在。
#[test]
fn a_different_snapshot_format_falls_back_to_a_replay() {
    let dir = temp_dir("format");
    let (_old, socket) = old_here(&dir);
    let mut cli = Peer::hello(&socket, false);
    let id = cli.spawn(&script(&dir, "marker.sh", "echo marker-before\nexec sleep 1000"));
    cli.wait_screen(id, None, |text| text.contains("marker-before"));
    let (_successor, result) = take_over(&dir, &[(SNAPSHOT_FORMAT, "999")]);
    assert_eq!(result, format!("ok 1 {id}"));
    let (mut new, _) = hello_build(&socket);
    assert!(new.screen_text(id, None).contains("marker-before"));
    let log = std::fs::read_to_string(dir.join("successor.log")).unwrap();
    assert!(log.contains("1 rebuilt from a VT replay"), "{log}");
}

/// 有桌面的界面连着、或者宿主跑在 app 里时不交接；被拒绝的新宿主什么状态都不留。
#[test]
fn a_connected_desktop_or_an_in_app_host_refuses() {
    let dir = temp_dir("refuse");
    let (_old, socket) = old_here(&dir);
    let mut desktop = Peer::desktop(&socket);
    let id = desktop.spawn("/bin/cat");
    let new = Host::new(BuildId(NEW_BUILD.into()));
    new.mark_standalone();
    for _ in 0..2 {
        match new.take_over(&socket, TakeOverOptions::default()) {
            Err(TakeOverError::Refused(HandoffRefusal::DesktopConnected)) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
    assert_eq!(desktop.sessions().len(), 1);
    // 什么都没留下：还能自己监听。
    let other = temp_dir("refuse-new");
    new.listen(&socket_in(&other), &other.join("host.lock")).unwrap();
    desktop.send(&ClientMsg::Kill { id });

    let dir = temp_dir("in-app");
    let in_app = Host::new(BuildId(OLD_BUILD.into()));
    let socket = socket_in(&dir);
    in_app.listen(&socket, &dir.join("host.lock")).unwrap();
    match Host::new(BuildId(NEW_BUILD.into())).take_over(&socket, TakeOverOptions::default()) {
        Err(TakeOverError::Refused(HandoffRefusal::NotStandalone)) => {}
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// 不会交接的老宿主：协议 3 的回 `Incompatible`，`Welcome::handoff` 为 0 的也算。
#[test]
fn an_old_host_without_handoffs_is_pre_handoff() {
    let dir = temp_dir("pre");
    let socket = socket_in(&dir);
    let listener = UnixListener::bind(&socket).unwrap();
    let answers = [
        HostMsg::Incompatible { protocol: 3, build: BuildId("v3".into()), reason: "old".into() },
        HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: BuildId("no-handoff".into()),
            host_pid: 1,
            snapshot_format: 1,
            standalone: true,
            handoff: 0,
        },
    ];
    let fake = thread::spawn(move || {
        for answer in answers {
            let (mut stream, _) = listener.accept().unwrap();
            let hello = read_frame(&mut stream).unwrap().unwrap().message::<ClientMsg>().unwrap();
            assert!(matches!(hello, ClientMsg::Hello { client: ClientKind::Successor, .. }));
            let frame = Frame::control(&answer).unwrap();
            write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        }
    });
    for _ in 0..2 {
        let new = Host::new(BuildId(NEW_BUILD.into()));
        assert!(matches!(new.take_over(&socket, TakeOverOptions::default()), Err(TakeOverError::PreHandoff)));
    }
    fake.join().unwrap();
}

/// 一个宿主接一个宿主连续交接 20 次，每个新宿主开着的描述符一样多。
#[test]
fn twenty_handoffs_in_a_row_do_not_leak_descriptors() {
    let dir = temp_dir("chain");
    let (_old, socket) = old_process(&dir);
    let mut cli = Peer::hello(&socket, false);
    let ids: Vec<SessionId> =
        (0..3).map(|n| cli.spawn(&script(&dir, &format!("s{n}.sh"), "exec sleep 1000"))).collect();
    drop(cli);
    let mut hosts = Vec::new();
    let mut fds = Vec::new();
    for n in 0..20 {
        let tag = format!("host{n}");
        let (successor, result) = take_over(&dir, &[(TAG, &tag)]);
        assert_eq!(result, "ok 3", "handoff {n}");
        fds.push(wait_file(&dir.join(format!("{tag}.fds"))).parse::<usize>().unwrap());
        hosts.push(successor);
        if n > 0 {
            let previous = &mut hosts[n - 1];
            assert!(previous.wait().success());
        }
    }
    assert!(fds.iter().all(|&count| count == fds[0]), "descriptors per host: {fds:?}");
    let (mut last, build) = hello_build(&socket);
    assert_eq!(build.0, NEW_BUILD);
    let mut listed: Vec<SessionId> = last.sessions().into_iter().map(|s| s.id).collect();
    listed.sort();
    let mut ids = ids;
    ids.sort();
    assert_eq!(listed, ids);
}

/// 新宿主一次次放弃，旧宿主一次次回滚，描述符不增长。数描述符要整个进程里只有这一件事，所以
/// 放在单独的进程里做（`role_rollbacks`）。
#[test]
fn rolled_back_handoffs_do_not_leak_descriptors() {
    let dir = temp_dir("rollbacks");
    let mut rollbacks = Role::start("rollbacks", &dir, &[]);
    let counts = wait_file(&dir.join("rollbacks.result"));
    assert!(rollbacks.wait().success());
    let (before, after) = counts.split_once(' ').unwrap();
    assert_eq!(after, before, "descriptors before and after 20 rollbacks");
}

/// `rolled_back_handoffs_do_not_leak_descriptors` 的进程：开着一个会话的宿主被假的新宿主要了
/// 20 次又放弃，前后各数一次描述符，写进 `rollbacks.result`；之后会话照常。
#[test]
fn role_rollbacks() {
    let Some(dir) = role("rollbacks") else { return };
    let (_old, socket) = old_here(&dir);
    let mut cli = Peer::hello(&socket, false);
    let id = cli.spawn("/bin/cat");
    drop(cli);
    let settle = || thread::sleep(Duration::from_millis(200));
    let roll_back = || {
        let stream = UnixStream::connect(&socket).unwrap();
        assert_eq!(receive_everything(&stream), 1);
        send(&stream, &ClientMsg::HandoffAbort { reason: "testing".into() });
        // 旧宿主回滚后关掉连接。
        while read_frame(&mut &stream).is_ok_and(|frame| frame.is_some()) {}
    };
    roll_back();
    settle();
    let before = open_fds();
    for _ in 0..20 {
        roll_back();
    }
    settle();
    let after = open_fds();
    let mut cli = Peer::hello(&socket, false);
    let (channel, _) = cli.attach(id, AttachMode::VtReplay);
    cli.input(channel, b"still here\r");
    cli.wait_for_output(channel, b"still here");
    std::fs::write(dir.join("rollbacks.result"), format!("{before} {after}")).unwrap();
}

/// 交接各步用时（不默认跑）：`RUNODE_HANDOFF_TIMING` 指定结果写到哪个文件，按会话数 1、10、50
/// 各交接一次，每个会话的回滚历史写满一万行；结果是新旧宿主日志里的用时那一行。
#[test]
#[ignore = "measures how long handoffs take"]
fn handoff_timing() {
    let out = std::env::var("RUNODE_HANDOFF_TIMING").unwrap_or_else(|_| "handoff-timing.txt".into());
    let mut report = String::new();
    for sessions in [1, 10, 50] {
        let dir = temp_dir(&format!("timing{sessions}"));
        let (mut old, socket) = old_process(&dir);
        let mut cli = Peer::hello(&socket, false);
        let size = GridSize { cols: 120, rows: 40, ..SIZE };
        // 回滚历史写满一万行，每行几乎占满 120 列。
        let line = format!("line $i {}", "-".repeat(100));
        let fill = script(
            &dir,
            "fill.sh",
            &format!("i=0\nwhile [ $i -lt 10000 ]; do i=$((i+1)); echo \"{line}\"; done\necho filled\nexec sleep 1000"),
        );
        let ids: Vec<SessionId> = (0..sessions).map(|_| cli.spawn_sized(&fill, size, true)).collect();
        // 几十个会话同时写满回滚历史时会话线程忙，读屏幕可能等不到回话，等一会儿再读。
        let deadline = Instant::now() + Duration::from_secs(300);
        for &id in &ids {
            loop {
                cli.send(&ClientMsg::ReadScreen { id, lines: Some(2), command: None });
                match cli.reply() {
                    HostMsg::ScreenText { text, .. } if text.contains("filled") => break,
                    _ => thread::sleep(Duration::from_millis(100)),
                }
                assert!(Instant::now() < deadline, "the sessions did not fill their scrollback");
            }
        }
        drop(cli);
        let (_successor, result) = take_over(&dir, &[]);
        assert_eq!(result, format!("ok {sessions}"));
        assert!(old.wait().success());
        let pick = |file: &str, needle: &str| {
            std::fs::read_to_string(dir.join(file))
                .unwrap()
                .lines()
                .find(|line| line.contains(needle))
                .unwrap_or_default()
                .to_owned()
        };
        report += &format!(
            "{sessions} sessions:\n  old: {}\n  new: {}\n",
            pick("old.log", "handed"),
            pick("successor.log", "took")
        );
    }
    std::fs::write(out, report).unwrap();
}
