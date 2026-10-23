//! 宿主升级时的交接，真的跨进程：旧宿主、新宿主（以及假装的新宿主）各是一个进程，由这个测试
//! 程序带上 `--exact <角色测试名>` 和环境变量 `RUNODE_HANDOFF_ROLE` 重新执行自己。没设这个变量时
//! 各个角色测试什么都不做，直接通过。角色进程的结果和日志写在测试的临时目录里。
//!
//! 只验回滚、拒绝这类不需要旧宿主退出的情况时，旧宿主就在测试进程里（`old_here`），能直接设
//! 等新宿主的期限、数自己的描述符。

#![cfg(unix)]

mod common;

use std::{
    collections::HashSet,
    fs::File,
    io::{Read as _, Write as _},
    net::Shutdown,
    os::{
        fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::{
            net::{UnixListener, UnixStream},
            process::ExitStatusExt as _,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use common::{Peer, SIZE, WAIT, script, temp_dir};
use runode_host::{
    BuildId, ClientMsg, HandoffRefusal, Host, HostMsg, SessionId, Stopped, TakeOverError, TakeOverOptions,
};
use runode_protocol::{
    AttachMode, Caps, ClientKind, Frame, FrameKind, GoodbyeReason, HANDOFF_FORMAT, HandoffPart,
    OLDEST_READABLE_HANDOFF_FORMAT, PROTOCOL_VERSION, PaneLayout, PaneRect, TabLayout, WindowLayout, WorkspaceLayout,
    decode_part, encode_part, read_frame, write_frame,
};
use runode_terminal::fd_passing;
use runode_types::{
    agent::{Agent, AgentKind, AgentState},
    clipboard::{ClipboardAccess, ClipboardRead, ClipboardWrite},
    grid::GridSize,
    settings::TermSettings,
    shell::IntegrationMode,
};

/// 角色进程演哪个角色：`old`、`successor`、`fake`。
const ROLE: &str = "RUNODE_HANDOFF_ROLE";
/// 角色进程用的临时目录：socket、锁、结果和日志都在里面。
const DIR: &str = "RUNODE_HANDOFF_DIR";
/// 新宿主的结果文件、日志用的名字，连续交接时一个一个分开。
const TAG: &str = "RUNODE_HANDOFF_TAG";
/// 新宿主当自己的快照是这个格式版本，见 `TakeOverOptions::snapshot_format`。
const SNAPSHOT_FORMAT: &str = "RUNODE_HANDOFF_SNAPSHOT_FORMAT";
/// 新宿主的构建，没设时是 `NEW_BUILD`。连续交接时每一棒要是不同的构建：同一个构建的不交接。
const SUCCESSOR_BUILD: &str = "RUNODE_HANDOFF_BUILD";
/// 假的新宿主收下所有会话后怎么办：`die` 退出，`hang` 一直不回话，`abort` 回 `HandoffAbort`，
/// `die-after-commit` 回 `HandoffReady`、收到 `Commit` 后不回 `HandoffDone` 就退出。假的旧宿主收到
/// `HandoffReady` 后怎么办：`die` 退出，`disconnect` 只断开连接、进程留着。
const FAKE: &str = "RUNODE_HANDOFF_FAKE";
/// 假的旧宿主交出的那个会话。
const FAKE_OLD_SESSION: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);

const OLD_BUILD: &str = "old-build";
const NEW_BUILD: &str = "new-build";
/// 角色进程没有会话也没有连接这么久就退出。
const ROLE_IDLE: Duration = Duration::from_secs(20);
/// 等角色进程的结果。调试构建的测试程序起来要一会儿。
const ROLE_WAIT: Duration = Duration::from_secs(30);

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
/// `err <错误>`），成功的话之前先把接手后比起动时多开着的描述符个数写进 `<tag>.fds`，然后照常跑到
/// 交接出去或者空闲，原因写进 `<tag>.stopped`。
#[test]
fn role_successor() {
    let Some(dir) = role("successor") else { return };
    // `Role::start` 用标准库拉起角色进程，不像 `launch_successor` 那样只留指定的描述符：同一个测试
    // 进程里别的测试刚收下、还没来得及设 close-on-exec 的描述符会被带过来，所以只数接手多出来的。
    let inherited = open_fds();
    let tag = std::env::var(TAG).unwrap_or_else(|_| "successor".into());
    log_to(&dir.join(format!("{tag}.log")));
    let build = std::env::var(SUCCESSOR_BUILD).unwrap_or_else(|_| NEW_BUILD.into());
    let host = Host::new(BuildId(build));
    host.mark_standalone();
    let snapshot_format = std::env::var(SNAPSHOT_FORMAT).ok().map(|format| format.parse().unwrap());
    let result = host.take_over(&socket_in(&dir), TakeOverOptions { snapshot_format, ..TakeOverOptions::default() });
    let text = match &result {
        Ok(report) => {
            // 交接用的连接、管道这些稍后才关完。
            thread::sleep(Duration::from_millis(200));
            std::fs::write(dir.join(format!("{tag}.fds")), (open_fds() - inherited).to_string()).unwrap();
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
        "die-after-commit" => {
            send(&stream, &ClientMsg::HandoffReady);
            let (data, _) = fd_passing::recv_with_fds(&stream).unwrap();
            assert!(matches!(decode_part(&data).unwrap().0, HandoffPart::Commit { .. }));
            std::process::exit(0);
        }
        other => panic!("unknown fake {other}"),
    }
}

/// 假的旧宿主：在 socket 上等新宿主，按协议交出一个会话（`cat` 跑在新开的伪终端里），收到
/// `HandoffReady` 后不发 `Commit`，按 `RUNODE_HANDOFF_FAKE` 退出或者只断开连接。只断开的接着等
/// 测试写 `go`，再往伪终端里写一行，`cat` 照样回过来就写 `fake-old.echo`：新宿主没弄坏手里那份 PTY。
#[test]
fn role_fake_old() {
    let Some(dir) = role("fake-old") else { return };
    let listener = UnixListener::bind(socket_in(&dir)).unwrap();
    let lock = File::create(dir.join("host.lock")).unwrap();
    let (master, mut cat) = cat_on_a_pty();
    std::fs::write(dir.join("fake-old.listening"), "1").unwrap();
    let (stream, _) = listener.accept().unwrap();
    assert!(matches!(receive::<ClientMsg>(&stream), ClientMsg::Hello { client: ClientKind::Successor, .. }));
    send(
        &stream,
        &HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: BuildId("fake-old".into()),
            host_pid: std::process::id(),
            snapshot_format: 0,
            standalone: true,
            handoff: HANDOFF_FORMAT,
            keep_awake: false,
            todos: false,
            desktops: 0,
        },
    );
    assert!(matches!(receive::<ClientMsg>(&stream), ClientMsg::Handoff { .. }));
    send(&stream, &HostMsg::HandoffBegin { format: HANDOFF_FORMAT, sessions: 1 });
    // 只写必填的字段，别的按默认值读：照着别的版本的旧宿主写，交接格式加了字段也不用改这里。
    let host = serde_json::json!({
        "type": "host", "format": HANDOFF_FORMAT, "build": "fake-old", "snapshot_format": 0, "sessions": 1,
        "record_history": false, "socket": socket_in(&dir),
    });
    let session = serde_json::json!({
        "type": "session", "id": FAKE_OLD_SESSION, "started": true, "pid": cat.id(), "size": SIZE,
        "settings": TermSettings::default(),
    });
    let encode = |part, blocks: &[&[u8]]| encode_part(&serde_json::from_value(part).unwrap(), blocks).unwrap();
    fd_passing::send_with_fds(&stream, &encode(host, &[]), &[listener.as_fd(), lock.as_fd()]).unwrap();
    fd_passing::send_with_fds(&stream, &encode(session, &[b"", b""]), &[master.as_fd()]).unwrap();
    assert!(matches!(receive::<ClientMsg>(&stream), ClientMsg::HandoffReady));
    match std::env::var(FAKE).unwrap().as_str() {
        "die" => std::process::exit(0),
        "disconnect" => {
            stream.shutdown(Shutdown::Both).unwrap();
            wait_file(&dir.join("go"));
            let mut master = File::from(master);
            master.write_all(b"ping\n").unwrap();
            // 伪终端回显一遍，`cat` 再写一遍。新宿主把主端设成了非阻塞（同一个打开的文件），读不到时歇一下。
            let mut seen = Vec::new();
            while seen.windows(4).filter(|window| window == b"ping").count() < 2 {
                let mut buf = [0; 256];
                match master.read(&mut buf) {
                    Ok(0) => panic!("the pty hung up: {}", String::from_utf8_lossy(&seen)),
                    Ok(read) => seen.extend_from_slice(&buf[..read]),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(err) => panic!("cannot read the pty: {err}"),
                }
            }
            std::fs::write(dir.join("fake-old.echo"), "ok").unwrap();
            let _ = cat.kill();
        }
        other => panic!("unknown fake {other}"),
    }
}

/// 新开一个伪终端，`cat` 跑在它的从端上，返回主端和 `cat`。
fn cat_on_a_pty() -> (OwnedFd, Child) {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: 输出参数指向本地变量；名字、termios、窗口尺寸都不要，传空指针。
    let opened = unsafe {
        libc::openpty(&raw mut master, &raw mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: 两个描述符是 openpty 刚开的，归这里。
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    // 主端别漏给 `cat`，不然新宿主手里那份关了伪终端也不挂断。
    // SAFETY: 描述符有效，只设 close-on-exec。
    assert_eq!(unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) }, 0);
    let cat = Command::new("/bin/cat")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .spawn()
        .unwrap();
    (master, cat)
}

/// 在这个进程里演角色时返回目录。
fn role(name: &str) -> Option<PathBuf> {
    (std::env::var(ROLE).ok()? == name).then(|| PathBuf::from(std::env::var_os(DIR).unwrap()))
}

/// 只跑 `cat` 的会话程序。不直接用 `/bin/cat`：认不出的 shell 会被加上 `-l`，GNU cat 不认这个选项，
/// 一启动就退出。
fn cat(dir: &Path) -> String {
    script(dir, "cat.sh", "exec cat")
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
            "fake-old" => "role_fake_old",
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
///
/// 计数脚本每行 fork 一次 `sleep`，CI 上测试并行跑时一行要几十毫秒，数几百行超过 `common::WAIT`，
/// 所以只在一个 `WAIT` 里数不动了才算超时，数得慢不算。
fn assert_counted(peer: &mut Peer, id: SessionId, count: u32) {
    let mut deadline = Instant::now() + common::WAIT;
    let mut last = None;
    let text = loop {
        let text = peer.screen_text(id, Some(count + 100));
        if text.lines().any(|line| line == "end") {
            break text;
        }
        let now = counted(&text).last().copied();
        if now > last {
            last = now;
            deadline = Instant::now() + common::WAIT;
        }
        assert!(Instant::now() < deadline, "the count of {id} stopped at {last:?}: {text:?}");
        thread::sleep(Duration::from_millis(20));
    };
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

/// 发一条控制消息：前端发 `ClientMsg`，假的旧宿主发 `HostMsg`。
fn send(stream: &UnixStream, message: &impl serde::Serialize) {
    let frame = Frame::control(message).unwrap();
    write_frame(&mut &*stream, frame.kind, 0, &frame.payload).unwrap();
}

/// 收一条控制消息，`HostMsg` 或 `ClientMsg` 由调用处定。
fn receive<T: serde::de::DeserializeOwned>(stream: &UnixStream) -> T {
    let frame = read_frame(&mut &*stream).unwrap().unwrap();
    assert_eq!(frame.kind, FrameKind::Control);
    frame.message().unwrap()
}

/// 起一个假的旧宿主（`role_fake_old`），等它开始监听，返回它和 socket。
fn fake_old(dir: &Path, fake: &str) -> (Role, PathBuf) {
    let old = Role::start("fake-old", dir, &[(FAKE, fake)]);
    wait_file(&dir.join("fake-old.listening"));
    (old, socket_in(dir))
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
    let later = script(&dir, "later.sh", "echo \"started-$RUNODE_SOCKET\"\nexec sleep 1000");
    let unstarted = cli.spawn_with(&later, false, None);
    let exited = cli.spawn(&script(&dir, "exit.sh", "exit 0"));

    let deadline = Instant::now() + common::WAIT;
    let agent_before = loop {
        let sessions = cli.sessions();
        let gone = sessions.iter().any(|s| s.id == exited && s.exited);
        let working = sessions.iter().find(|s| s.id == agent).and_then(|s| s.meta.agent);
        // 等到认出前台进程：在那之前只有标题里的盲文转圈，按 codex 算；认出叫 claude 的进程后
        // 才是最终的 claude（前台进程优先）。不等的话，交接前后正好落在认出之前和之后，两边对不上。
        let settled = Agent { kind: AgentKind::Claude, state: AgentState::Working };
        if gone && let Some(working) = working.filter(|agent| *agent == settled) {
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
    let started = format!("started-{}", socket.display());
    new.wait_screen(unstarted, Some(20), |text| text.replace('\n', "").contains(&started));

    // agent 交接之后过一会儿仍是工作中：交接期间没有输出，不会被判成空闲。
    thread::sleep(Duration::from_millis(1500));
    let agent_later = new.sessions().into_iter().find(|s| s.id == agent).unwrap().meta.agent;
    assert_eq!(agent_later, Some(agent_before));
}

/// 剪贴板的规矩跟着交接过去：旧宿主上设成不让写，新宿主上的会话照样不让写（DA1 的回答里没有 52），
/// 不用等桌面重新设。宿主手里的布局也跟着过去，桌面再连上时要得回来。
#[test]
fn the_clipboard_rules_and_the_layout_go_with_the_handoff() {
    let dir = temp_dir("clipboard");
    let (_old, socket) = old_process(&dir);
    let mut desktop = Peer::desktop(&socket);
    let clipboard = ClipboardAccess { write: ClipboardWrite::Deny, read: ClipboardRead::Ask };
    desktop.send(&ClientMsg::SetOptions { record_history: false, clipboard });
    let id = desktop.spawn(&script(&dir, "da.sh", r"while read line; do printf '\033[c'; done"));
    let rect = PaneRect { x: 0, y: 0, width: PaneRect::EXTENT, height: PaneRect::EXTENT };
    let tab = TabLayout { index: 1, active: true, panes: vec![PaneLayout { index: 1, id, rect, focused: true }] };
    let workspace =
        WorkspaceLayout { index: 1, name: Some("app".into()), dir: Some(dir.clone()), active: true, tabs: vec![tab] };
    let layout = vec![WindowLayout { index: 1, front: true, workspaces: vec![workspace] }];
    desktop.send(&ClientMsg::LayoutChanged { windows: Some(layout.clone()) });
    desktop.sessions();
    drop(desktop);
    // 桌面连着时旧宿主不交；断开要一会儿才登记下来，拒绝了就再试。
    let mut tries = 0;
    let _successor = loop {
        tries += 1;
        let tag = format!("successor{tries}");
        let (successor, result) = take_over(&dir, &[(TAG, &tag), (SUCCESSOR_BUILD, &tag)]);
        if result.starts_with("ok") {
            break successor;
        }
        assert!(tries < 5 && result.contains("DesktopConnected"), "{result}");
        thread::sleep(Duration::from_millis(100));
    };
    let mut desktop = Peer::desktop(&socket);
    desktop.send(&ClientMsg::Layout { req: 1 });
    assert_eq!(desktop.reply(), HostMsg::Layout { req: 1, windows: layout });
    let (channel, _) = desktop.attach(id, AttachMode::VtReplay);
    desktop.input(channel, b"x\r");
    // 行规程把 DA1 的回答回显出来。
    desktop.wait_for_output(channel, b"^[[?62;1;6;22c");
}

/// 交接时程序不读输入，写不进 PTY 的输入跟着交过去，新宿主写进去，程序读到的每一行正好一次。
#[test]
fn input_the_old_host_could_not_write_arrives_exactly_once() {
    let dir = temp_dir("pending");
    let (_old, socket) = old_process(&dir);
    let mut cli = Peer::hello(&socket, false);
    // 不回显、不按行缓冲；先睡着不读输入，交接完了才 `cat` 进文件。不 `cat` 到屏幕上：回滚历史只留
    // 一万行，调试构建的 VT 啃几百 KiB 也要好几秒。
    let out = dir.join("cat.out");
    let late_cat = format!("stty -icanon -echo\necho ready\nsleep 3\nexec cat > {}", out.display());
    let id = cli.spawn(&script(&dir, "late-cat.sh", &late_cat));
    cli.wait_screen(id, None, |text| text.contains("ready"));
    let (channel, _) = cli.attach(id, AttachMode::MetaOnly);
    // macOS 的 PTY 只收得下 1 KiB 左右；Linux 的行规程和 tty 缓冲加起来能收下 60 多 KiB。剩下的留在
    // 旧宿主的写队列里。
    let input: String = (1..=30_000).map(|n| format!("L{n:05}\n")).collect();
    cli.input(channel, input.as_bytes());
    thread::sleep(Duration::from_millis(100));

    let (_successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 1");
    let log = std::fs::read_to_string(dir.join("old.log")).unwrap();
    assert!(log.contains("bytes of input not written yet") && !log.contains(" 0 bytes of input"), "{log}");
    let deadline = Instant::now() + WAIT;
    loop {
        let got = std::fs::read_to_string(&out).unwrap_or_default();
        if got.len() >= input.len() || Instant::now() > deadline {
            assert!(got == input, "cat read {} of {} bytes, or not exactly the input", got.len(), input.len());
            break;
        }
        thread::sleep(Duration::from_millis(20));
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

/// 新宿主收到 `Commit` 后没回 `HandoffDone` 就死了，收下的 master 也早关了：旧宿主用自己留着的那份
/// 把会话接回来，shell 没收到 SIGHUP，数数的程序一行不丢一行不重地数完，之后照样能交接。
#[test]
fn a_successor_that_dies_after_the_commit_gives_the_sessions_back() {
    let dir = temp_dir("diescommit");
    let (_old, socket) = old_here(&dir);
    let mut cli = Peer::hello(&socket, false);
    let count = 200;
    let id = cli.spawn(&counter(&dir, "count.sh", count, 1, "0.01", 0));
    wait_counted(&mut cli, id, 10);

    let mut fake = Role::start("fake", &dir, &[(FAKE, "die-after-commit")]);
    assert_eq!(wait_file(&dir.join("fake.received")), "1");
    assert!(fake.wait().success());
    assert_handoff_goodbye(&cli);

    let mut again = Peer::hello(&socket, false);
    assert_counted(&mut again, id, count);
    drop(again);
    let (_successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 1");
    let (mut new, _) = hello_build(&socket);
    new.wait_screen(id, Some(10), |text| text.contains("end"));
}

/// 旧宿主收到 `HandoffReady` 后没发 `Commit` 就死了：新宿主当作它提交了，照样接手，会话接着能用。
#[test]
fn an_old_host_that_dies_before_committing_is_taken_over() {
    let dir = temp_dir("olddies");
    let (mut old, socket) = fake_old(&dir, "die");
    let host = Host::new(BuildId(NEW_BUILD.into()));
    host.mark_standalone();
    let report = host.take_over(&socket, TakeOverOptions::default()).unwrap();
    assert_eq!(report.sessions, 1);
    assert!(old.wait().success());
    let mut peer = Peer::pair(&host);
    let (channel, _) = peer.attach(FAKE_OLD_SESSION, AttachMode::Snapshot);
    peer.input(channel, b"hi\r");
    peer.wait_for_output(channel, b"hi\r\nhi");
    peer.send(&ClientMsg::Kill { id: FAKE_OLD_SESSION });
}

/// 旧宿主收到 `HandoffReady` 后没发 `Commit` 就断开、进程还在：它回滚了，新宿主接手失败，收下的 PTY
/// 原样交回、不留会话，旧宿主手里那份照常能用。
#[test]
fn an_old_host_that_only_disconnects_keeps_its_sessions() {
    let dir = temp_dir("olddisconnects");
    let (mut old, socket) = fake_old(&dir, "disconnect");
    let host = Host::new(BuildId(NEW_BUILD.into()));
    host.mark_standalone();
    let result = host.take_over(&socket, TakeOverOptions::default());
    assert!(matches!(&result, Err(TakeOverError::Failed(reason)) if reason.contains("rolled back")), "{result:?}");
    assert!(Peer::pair(&host).sessions().is_empty());
    std::fs::write(dir.join("go"), "1").unwrap();
    assert_eq!(wait_file(&dir.join("fake-old.echo")), "ok");
    assert!(old.wait().success());
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
    let id = desktop.spawn(&cat(&dir));
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

/// 要接手的和旧宿主是同一个构建（另一个新 app 抢先让这个构建的新宿主接手了，后来的 app 探到的
/// 已经过时）：回 `Busy`，什么都不动，连着的命令行也不受影响。
#[test]
fn a_successor_of_the_same_build_is_told_the_host_is_busy() {
    let dir = temp_dir("same");
    let (_old, socket) = old_here(&dir);
    let mut cli = Peer::hello(&socket, false);
    let id = cli.spawn(&cat(&dir));
    match Host::new(BuildId(OLD_BUILD.into())).take_over(&socket, TakeOverOptions::default()) {
        Err(TakeOverError::Refused(HandoffRefusal::Busy)) => {}
        other => panic!("expected busy, got {other:?}"),
    }
    assert_eq!(cli.sessions().into_iter().map(|s| s.id).collect::<Vec<_>>(), [id]);
    // 别的构建照样能接手。
    drop(cli);
    let (_successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 1");
}

/// 旧宿主正在交接时连上来的新宿主收到的是 `Goodbye { Handoff }`（交接开始前连上、还没要交接的，
/// 和交接期间被接受的连接都这样）：当作 `Busy`，不当作失败。旧宿主要交接之后才说也一样。
#[test]
fn a_goodbye_for_a_handoff_means_busy() {
    let dir = temp_dir("bye");
    let socket = socket_in(&dir);
    let listener = UnixListener::bind(&socket).unwrap();
    let welcome = HostMsg::Welcome {
        protocol: PROTOCOL_VERSION,
        build: BuildId(OLD_BUILD.into()),
        host_pid: std::process::id(),
        snapshot_format: 1,
        standalone: true,
        handoff: HANDOFF_FORMAT,
        keep_awake: false,
        todos: false,
        desktops: 0,
    };
    let goodbye = HostMsg::Goodbye { reason: GoodbyeReason::Handoff };
    let scripts = [vec![goodbye.clone()], vec![welcome, goodbye]];
    let fake = thread::spawn(move || {
        for script in scripts {
            let (mut stream, _) = listener.accept().unwrap();
            for answer in script {
                // 先读对面的一条（`Hello`，然后是 `Handoff`），再回话。
                read_frame(&mut stream).unwrap().unwrap();
                let frame = Frame::control(&answer).unwrap();
                write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
            }
        }
    });
    for _ in 0..2 {
        let new = Host::new(BuildId(NEW_BUILD.into()));
        match new.take_over(&socket, TakeOverOptions::default()) {
            Err(TakeOverError::Refused(HandoffRefusal::Busy)) => {}
            other => panic!("expected busy, got {other:?}"),
        }
    }
    fake.join().unwrap();
}

/// 两个新 app 同时升级：两个新宿主一起来接手，一个接手成了，另一个得到 `Busy`（旧宿主正在交接、
/// 交接期间被接受的连接收到 `Goodbye`，或者连到了刚接手的同构建新宿主），不会被当成失败。
#[test]
fn of_two_successors_at_once_one_is_told_the_host_is_busy() {
    let dir = temp_dir("two");
    let (_old, socket) = old_process(&dir);
    let mut cli = Peer::hello(&socket, false);
    let id = cli.spawn(&counter(&dir, "count.sh", 100, 1, "0.01", 0));
    drop(cli);
    let first = Role::start("successor", &dir, &[]);
    let here = Host::new(BuildId(NEW_BUILD.into()));
    here.mark_standalone();
    let second = here.take_over(&socket, TakeOverOptions::default());
    let first_result = wait_file(&dir.join("successor.result"));
    match (&second, first_result.as_str()) {
        (Ok(report), _) => {
            assert_eq!(report.sessions, 1);
            assert_eq!(first_result, format!("err {:?}", TakeOverError::Refused(HandoffRefusal::Busy)));
        }
        (Err(TakeOverError::Refused(HandoffRefusal::Busy)), "ok 1") => {}
        (second, first) => panic!("expected one handoff and one busy, got {first} and {second:?}"),
    }
    let (mut new, build) = hello_build(&socket);
    assert_eq!(build.0, NEW_BUILD);
    assert_counted(&mut new, id, 100);
    drop(first);
}

/// 一个宿主接一个宿主连续交接 20 次（每一棒是不同的构建），每个新宿主开着的描述符一样多。
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
        let (successor, result) = take_over(&dir, &[(TAG, &tag), (SUCCESSOR_BUILD, &tag)]);
        assert_eq!(result, "ok 3", "handoff {n}");
        fds.push(wait_file(&dir.join(format!("{tag}.fds"))).parse::<usize>().unwrap());
        hosts.push(successor);
        if n > 0 {
            let previous = &mut hosts[n - 1];
            assert!(previous.wait().success());
        }
    }
    assert!(fds.iter().all(|&count| count == fds[0]), "descriptors per host: {fds:?}");
    // 每个新宿主收到 `Welcome` 后读到的连接对端都是旧宿主自报的进程号，见 `peer_pid_after_an_answer`。只有
    // macOS 这样：Linux 上（`SO_PEERCRED`）读到的总是最早调 `listen` 的那个宿主，第二棒起新宿主都会记一笔对不上。
    #[cfg(target_os = "macos")]
    for n in 0..20 {
        let log = std::fs::read_to_string(dir.join(format!("host{n}.log"))).unwrap();
        assert!(!log.contains("the connection says"), "host{n}: {log}");
    }
    let (mut last, build) = hello_build(&socket);
    assert_eq!(build.0, "host19");
    let mut listed: Vec<SessionId> = last.sessions().into_iter().map(|s| s.id).collect();
    listed.sort();
    let mut ids = ids;
    ids.sort();
    assert_eq!(listed, ids);
}

/// 监听的 socket 交接过以后，连上去的一方要等对方回过话再读对端进程号（`LOCAL_PEERPID`）：那时
/// 读到的才是接手的新宿主，和它在 `Welcome` 里自报的一样。新宿主认旧宿主、桌面结束旧宿主都靠这条。
#[cfg(target_os = "macos")]
#[test]
fn peer_pid_after_an_answer() {
    use std::os::fd::AsRawFd as _;
    let peer_pid = |stream: &UnixStream| {
        let mut pid: libc::pid_t = 0;
        let mut len = libc::socklen_t::try_from(size_of::<libc::pid_t>()).unwrap();
        // SAFETY: 描述符来自 `stream`；值指向本地变量，长度是它的大小。
        let result = unsafe {
            libc::getsockopt(stream.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len)
        };
        assert_eq!(result, 0);
        pid
    };
    let dir = temp_dir("peerpid");
    let (mut old, socket) = old_process(&dir);
    let (successor, result) = take_over(&dir, &[]);
    assert_eq!(result, "ok 0");
    assert!(old.wait().success());
    let mut peer = Peer::connect(&socket);
    peer.send(&ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: BuildId(common::BUILD.into()),
        client: ClientKind::Cli,
        caps: Caps::default(),
        session: None,
        device: None,
    });
    let HostMsg::Welcome { host_pid, .. } = peer.message() else { panic!("expected welcome") };
    assert_eq!(host_pid, successor.child.id());
    assert_eq!(u32::try_from(peer_pid(&peer.stream)).unwrap(), host_pid);
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
    let id = cli.spawn(&cat(&dir));
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
