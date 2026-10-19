//! 推送的发送线程和轮询线程，对着假的 curl（一个 shell 脚本，把收到的配置记成文件，按规则回话）
//! 和假的宿主（一对 Unix socket，按 protocol 回会话列表和屏幕文字）跑。

use std::{
    io,
    os::unix::{fs::PermissionsExt as _, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use runode_paths::Dirs;
use runode_protocol::{
    ClientMsg, Frame, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION, SessionId, SessionInfo,
    push::{ApnsEnv, EXPIRES_AFTER, Notification, NotificationKind, OFFICIAL_BUNDLE},
    read_frame,
    remote::{Bytes, DeviceId},
    todo::{Todo, TodoSpec},
    write_frame,
};
use runode_types::{
    agent::{Agent, AgentKind, AgentState},
    grid::GridSize,
    session::SessionMeta,
};

use super::{
    ApnsKey, Listening, PushSettings, Pusher, TestPush,
    apns::{Route, Via, test_key},
    curl::Curl,
    probe::probe,
    route,
    send::{Sender, Target},
};
use crate::devices::{self, Device, PushRegistration};

/// 等异步的结果最多这么久。
const PATIENCE: Duration = Duration::from_secs(15);
const TOKEN: &str = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011";
const DEVICE: DeviceId = DeviceId([7; 16]);
const SESSION: SessionId = SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
const RELAY_PROD: Route = Route { env: ApnsEnv::Production, via: Via::Relay };

/// 测试用的目录，结束时删掉。
struct Scratch {
    root: PathBuf,
    dirs: Dirs,
    log: PathBuf,
    curl: Curl,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("rra-push-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let log = root.join("curl");
        std::fs::create_dir_all(&log).unwrap();
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        let program = root.join("curl.sh");
        std::fs::write(&program, FAKE_CURL.replace("@LOG@", &log.display().to_string())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { root, dirs, log, curl: Curl { program } }
    }

    /// 下一个配置里含 `pattern` 的请求回 `response`（`\r\n` 照写，`FAIL` 是连不上），只用一次。
    fn rule(&self, pattern: &str, response: &str) {
        let n = std::fs::read_dir(&self.log).unwrap().count();
        std::fs::write(self.log.join(format!("rule-{n:03}")), format!("{pattern}\n{response}")).unwrap();
    }

    /// 收到过的请求的配置，按先后。
    fn requests(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.log)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("req-"))
            .collect();
        names.sort();
        names.iter().map(|name| std::fs::read_to_string(self.log.join(name)).unwrap()).collect()
    }

    /// 等到收到 `n` 个请求，返回它们；再多等一会儿，确认没有多出来的。
    fn wait_for(&self, n: usize) -> Vec<String> {
        let deadline = Instant::now() + PATIENCE;
        while self.requests().len() < n {
            assert!(Instant::now() < deadline, "timed out waiting for {n} requests: {:#?}", self.requests());
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(300));
        let requests = self.requests();
        assert_eq!(requests.len(), n, "{requests:#?}");
        requests
    }

    /// 配对一台设备，登记 `bundle` 的 device token。
    fn register(&self, env: ApnsEnv, bundle: &str) {
        let device =
            Device { device_id: DEVICE, name: "手机".into(), public_key: Bytes(vec![4]), paired_at: 1, last_seen: 1 };
        devices::add(&self.dirs, device).unwrap();
        let registration = PushRegistration {
            device_id: DEVICE,
            token: TOKEN.into(),
            env,
            bundle: bundle.into(),
            machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF".into(),
            machine_name: "Mac".into(),
            updated_at: 1,
        };
        assert!(devices::set_push(&self.dirs, registration).unwrap());
    }

    fn registrations(&self) -> Vec<PushRegistration> {
        devices::push_registrations(&self.dirs).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 假的 curl：`--version` 说自己会 HTTP/2；别的时候把 stdin 上的配置存成 `req-NNN`，先找第一条匹配的
/// 规则（`rule-NNN`：第一行是要找的文字，其余是响应），没有就回 200。
const FAKE_CURL: &str = r#"#!/bin/sh
dir='@LOG@'
if [ "$1" = "--version" ]; then
  printf 'curl 8.7.1 (x86_64-apple-darwin24.0) libcurl/8.7.1\nFeatures: alt-svc AsynchDNS HTTP2 IPv6\n'
  exit 0
fi
n=$(ls "$dir" | grep -c '^req-')
n=$(printf '%03d' "$n")
req="$dir/req-$n"
cat > "$req"
for rule in "$dir"/rule-*; do
  [ -f "$rule" ] || continue
  pattern=$(head -n 1 "$rule")
  if grep -qF -- "$pattern" "$req"; then
    body=$(tail -n +2 "$rule")
    rm -f "$rule"
    if [ "$body" = "FAIL" ]; then echo "curl: (28) Operation timed out" >&2; exit 28; fi
    printf '%b' "$body"
    exit 0
  fi
done
printf 'HTTP/2 200\r\n\r\n'
"#;

const MACHINE: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";

fn notification(lines: &[&str]) -> Notification {
    Notification {
        machine: MACHINE.into(),
        machine_name: "Mac".into(),
        session: SESSION,
        title: "修 bug".into(),
        agent: "Claude Code".into(),
        lines: lines.iter().map(|l| (*l).into()).collect(),
        kind: NotificationKind::Blocked,
    }
}

fn target(route: Route, lines: &[&str]) -> Target {
    Target { route, device: DEVICE, token: TOKEN.into(), notification: notification(lines) }
}

/// 请求体里的 JSON。
fn body(config: &str) -> serde_json::Value {
    let line = config.lines().find_map(|line| line.strip_prefix("data-raw = ")).expect("a body");
    let unquoted = line[1..line.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\");
    serde_json::from_str(&unquoted).unwrap()
}

fn url(config: &str) -> &str {
    let line = config.lines().find_map(|line| line.strip_prefix("url = ")).expect("a url");
    &line[1..line.len() - 1]
}

/// 请求头 `name` 的值。
fn header<'a>(config: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("header = \"{name}: ");
    config.lines().find_map(|line| line.strip_prefix(prefix.as_str())).map(|value| &value[..value.len() - 1])
}

/// 经中转：一条通知一个请求，体是 `RelayNotify`。
#[test]
fn notifications_through_the_relay() {
    let scratch = Scratch::new("relay");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    let before = crate::now_unix();
    sender.notify(vec![target(RELAY_PROD, &["Proceed?"])]);
    let requests = scratch.wait_for(1);
    assert_eq!(url(&requests[0]), "https://relay.example/v2/notify");
    let notify = body(&requests[0]);
    assert_eq!((notify["v"].as_u64(), notify["env"].as_str()), (Some(2), Some("production")));
    assert_eq!(notify["token"], TOKEN);
    assert_eq!(notify["notification"]["lines"], serde_json::json!(["Proceed?"]));
    assert_eq!(notify["notification"]["session"], SESSION.to_string());
    let expiration = notify["expiration"].as_u64().unwrap();
    assert!((before + EXPIRES_AFTER..=crate::now_unix() + EXPIRES_AFTER).contains(&expiration));
}

/// 直连：JWT 只在 stdin 的配置里，不在命令行参数里；请求头带上 topic、提醒类型、过期时刻和
/// collapse-id。
#[test]
fn direct_pushes_sign_with_the_key() {
    let scratch = Scratch::new("direct");
    let (key_file, _) = test_key(&scratch.root);
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    let key = ApnsKey { key_file, key_id: "K".into(), team_id: "T".into(), bundle: "dev.example.app".into() };
    sender.settings(Some(key), Some("https://relay.example".into()));
    let route = Route { env: ApnsEnv::Development, via: Via::Direct };
    let before = crate::now_unix();
    sender.notify(vec![target(route, &["Proceed?"])]);
    let requests = scratch.wait_for(1);
    let request = &requests[0];
    assert_eq!(url(request), format!("https://api.sandbox.push.apple.com/3/device/{TOKEN}"));
    assert!(header(request, "authorization").unwrap().starts_with("bearer "));
    assert_eq!(header(request, "apns-topic"), Some("dev.example.app"));
    assert_eq!(header(request, "apns-push-type"), Some("alert"));
    assert_eq!(header(request, "apns-priority"), Some("10"));
    assert_eq!(header(request, "apns-collapse-id"), Some(SESSION.to_string().as_str()));
    let expiration: u64 = header(request, "apns-expiration").unwrap().parse().unwrap();
    assert!((before + EXPIRES_AFTER..=crate::now_unix() + EXPIRES_AFTER).contains(&expiration));
    assert_eq!(body(request)["aps"]["alert"]["body"], "Proceed?");
    assert_eq!(body(request)["session"], SESSION.to_string());

    // 密钥不对：记错误，直连的不再发，换了配置才再发。
    scratch.rule("/3/device", r#"HTTP/2 403\r\n\r\n{"reason":"InvalidProviderToken"}"#);
    sender.notify(vec![target(route, &["a"])]);
    scratch.wait_for(2);
    sender.notify(vec![target(route, &["b"])]);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(scratch.requests().len(), 2);
    let (key_file, _) = test_key(&scratch.root);
    sender.settings(
        Some(ApnsKey { key_file, key_id: "K2".into(), team_id: "T".into(), bundle: "dev.example.app".into() }),
        Some("https://relay.example".into()),
    );
    sender.notify(vec![target(route, &["c"])]);
    let requests = scratch.wait_for(3);
    assert_eq!(body(&requests[2])["aps"]["alert"]["body"], "c");
}

/// 配置的密钥文件一时读不了：直连的通知先不发；文件补好了，配置不变，下一条通知照样直连发出去。
#[test]
fn a_missing_key_file_is_read_again_later() {
    let scratch = Scratch::new("direct-missing");
    let key_file = scratch.root.join("AuthKey_TEST.p8");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    let key = ApnsKey {
        key_file: key_file.clone(),
        key_id: "K".into(),
        team_id: "T".into(),
        bundle: "dev.example.app".into(),
    };
    sender.settings(Some(key), Some("https://relay.example".into()));
    let route = Route { env: ApnsEnv::Development, via: Via::Direct };
    sender.notify(vec![target(route, &["a"])]);
    // 发送线程按先后办命令、按先后发排着的通知：等到这条中转的发出去，`a` 就已经因为读不到密钥
    // 丢掉了，这时才补文件。换个会话，免得它顶掉排着的 `a`。
    let mut marker = target(RELAY_PROD, &["marker"]);
    marker.notification.session = SessionId(1);
    sender.notify(vec![marker]);
    scratch.wait_for(1);
    assert_eq!(test_key(&scratch.root).0, key_file);
    sender.notify(vec![target(route, &["b"])]);
    let requests = scratch.wait_for(2);
    assert_eq!(body(&requests[1])["aps"]["alert"]["body"], "b");
}

/// token 不认时换另一个环境再发，成了把登记的环境改过来；两边都不认就删掉登记。
#[test]
fn bad_tokens_try_the_other_environment() {
    let scratch = Scratch::new("badtoken");
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    scratch.rule("/v2/notify", r#"HTTP/2 400\r\n\r\n{"reason":"BadDeviceToken"}"#);
    sender.notify(vec![target(RELAY_PROD, &[])]);
    let requests = scratch.wait_for(2);
    assert_eq!(body(&requests[0])["env"], "production");
    assert_eq!(body(&requests[1])["env"], "development");
    // 发送线程拿到响应之后才改登记，晚于假 curl 记下请求。
    wait_until("the registration switches to development", || scratch.registrations()[0].env == ApnsEnv::Development);

    scratch.rule("/v2/notify", r#"HTTP/2 410\r\n\r\n{"reason":"Unregistered"}"#);
    scratch.rule("/v2/notify", r#"HTTP/2 410\r\n\r\n{"reason":"Unregistered"}"#);
    sender.notify(vec![target(RELAY_PROD, &[])]);
    scratch.wait_for(4);
    wait_until("the registration is dropped", || scratch.registrations().is_empty());
}

/// 连不上时退避重试。
#[test]
fn failures_are_retried() {
    let scratch = Scratch::new("retry");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    scratch.rule("/v2/notify", "FAIL");
    let started = Instant::now();
    sender.notify(vec![target(RELAY_PROD, &[])]);
    let requests = scratch.wait_for(2);
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_eq!(body(&requests[0])["notification"], body(&requests[1])["notification"]);
}

/// 429 要求等的时间大得离谱（加上去会溢出）：这条放弃，发送线程照常办下一条。
#[test]
fn an_absurd_retry_after_gives_up_without_stopping_the_sender() {
    let scratch = Scratch::new("retryafter");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    scratch
        .rule("/v2/notify", r#"HTTP/2 429\r\nretry-after: 18446744073709551615\r\n\r\n{"reason":"TooManyRequests"}"#);
    sender.notify(vec![target(RELAY_PROD, &["first"])]);
    scratch.wait_for(1);
    sender.notify(vec![target(RELAY_PROD, &["second"])]);
    let requests = scratch.wait_for(2);
    assert_eq!(body(&requests[1])["notification"]["lines"], serde_json::json!(["second"]));
}

/// 在退避的通知：会话不再等了就不发；同一个会话又等了，只发新的，旧的不会晚到把新问题替掉。
#[test]
fn backing_off_notifications_are_cancelled_or_replaced() {
    let scratch = Scratch::new("cancel");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    scratch.rule("/v2/notify", "FAIL");
    sender.notify(vec![target(RELAY_PROD, &["old"])]);
    scratch.wait_for(1);
    sender.cancel(SESSION);
    thread::sleep(Duration::from_secs(3));
    scratch.wait_for(1);

    scratch.rule("/v2/notify", "FAIL");
    sender.notify(vec![target(RELAY_PROD, &["old"])]);
    scratch.wait_for(2);
    sender.notify(vec![target(RELAY_PROD, &["new"])]);
    let requests = scratch.wait_for(3);
    assert_eq!(body(&requests[2])["notification"]["lines"], serde_json::json!(["new"]));
    thread::sleep(Duration::from_secs(3));
    scratch.wait_for(3);
}

/// 任务做完的通知发到中转的 v3 接口；中转服务没有这个接口、回 404 时算推不了，不重试。
#[test]
fn todo_done_needs_the_v3_relay() {
    let scratch = Scratch::new("v3");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    scratch.rule("/v3/notify", r#"HTTP/2 404\r\n\r\n"#);
    let done = Notification::todo_done(MACHINE.into(), "Mac", SESSION, "加暗色模式");
    sender.notify(vec![Target { route: RELAY_PROD, device: DEVICE, token: TOKEN.into(), notification: done }]);
    let requests = scratch.wait_for(1);
    assert_eq!(url(&requests[0]), "https://relay.example/v3/notify");
    thread::sleep(Duration::from_secs(3));
    scratch.wait_for(1);
}

/// 在退避的通知：那台手机注销了、换了 token 或者关了推送，就不发了。
#[test]
fn backing_off_notifications_follow_the_registrations() {
    let scratch = Scratch::new("keep");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, Some("https://relay.example".into()));
    scratch.rule("/v2/notify", "FAIL");
    sender.notify(vec![target(RELAY_PROD, &[])]);
    scratch.wait_for(1);
    sender.keep(vec![(DEVICE, "0123".into())]);
    thread::sleep(Duration::from_secs(3));
    scratch.wait_for(1);
}

/// 假的宿主：每条连接一个线程，回 `Welcome`、会话列表和屏幕文字；有任务表时说认得看板，回任务表，
/// 回完紧跟一条 `TodosChanged`。
#[derive(Clone, Default)]
struct FakeHost {
    /// 设了时会话列表像真的宿主一样在另一个线程里回，过这么久才回（会话线程忙着）。
    list_delay: Arc<Mutex<Option<Duration>>>,
    /// 收到了还没回的会话列表请求有几个，和最多时有几个。
    lists_waiting: Arc<AtomicUsize>,
    most_lists_waiting: Arc<AtomicUsize>,
    /// 收到过几条 `ListSessions`。
    list_sessions: Arc<AtomicUsize>,
    sessions: Arc<Mutex<Vec<SessionInfo>>>,
    /// 任务表；为 `None` 时是不认得看板的旧宿主。
    todos: Arc<Mutex<Option<Vec<Todo>>>>,
    /// 收到过几条 `ListTodos`。
    list_todos: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    /// 设了时下一条 `ListSessions` 回 `Goodbye`（交接开始了）并断开，回完自己清掉。
    goodbye: Arc<AtomicBool>,
    /// 宿主这头的各条连接，`hang_up` 用。
    streams: Arc<Mutex<Vec<UnixStream>>>,
}

impl FakeHost {
    fn connect(&self) -> crate::Connect {
        let host = self.clone();
        Arc::new(move || -> io::Result<UnixStream> {
            let (ours, theirs) = UnixStream::pair()?;
            host.connections.fetch_add(1, Ordering::SeqCst);
            host.streams.lock().unwrap().push(theirs.try_clone()?);
            let host = host.clone();
            thread::spawn(move || host.serve(theirs));
            Ok(ours)
        })
    }

    fn serve(&self, mut stream: UnixStream) {
        // 晚回的会话列表从别的线程写，和这里的回话一帧一帧地轮着写。
        let writer = Arc::new(Mutex::new(stream.try_clone().unwrap()));
        let write = |writer: &Mutex<UnixStream>, message: &HostMsg| {
            let frame = Frame::control(message).unwrap();
            write_frame(&mut *writer.lock().unwrap(), FrameKind::Control, frame.channel, &frame.payload).is_ok()
        };
        while let Ok(Some(frame)) = read_frame(&mut stream) {
            let Ok(message) = frame.message::<ClientMsg>() else { continue };
            let reply = match message {
                ClientMsg::Hello { client, .. } => {
                    assert_eq!(client, runode_protocol::ClientKind::Cli);
                    HostMsg::Welcome {
                        protocol: PROTOCOL_VERSION,
                        build: runode_protocol::BuildId::default(),
                        host_pid: 1,
                        snapshot_format: 0,
                        standalone: false,
                        handoff: 0,
                        keep_awake: false,
                        todos: self.todos.lock().unwrap().is_some(),
                        desktops: 0,
                    }
                }
                ClientMsg::ListSessions if self.goodbye.swap(false, Ordering::SeqCst) => {
                    write(&writer, &HostMsg::Goodbye { reason: GoodbyeReason::Handoff });
                    return;
                }
                ClientMsg::ListSessions => {
                    self.list_sessions.fetch_add(1, Ordering::SeqCst);
                    let list = HostMsg::SessionList { sessions: self.sessions.lock().unwrap().clone() };
                    let Some(delay) = *self.list_delay.lock().unwrap() else {
                        if !write(&writer, &list) {
                            return;
                        }
                        continue;
                    };
                    let waiting = self.lists_waiting.fetch_add(1, Ordering::SeqCst) + 1;
                    self.most_lists_waiting.fetch_max(waiting, Ordering::SeqCst);
                    let (host, writer) = (self.clone(), writer.clone());
                    thread::spawn(move || {
                        thread::sleep(delay);
                        host.lists_waiting.fetch_sub(1, Ordering::SeqCst);
                        write(&writer, &list);
                    });
                    continue;
                }
                ClientMsg::ListTodos { req } => {
                    self.list_todos.fetch_add(1, Ordering::SeqCst);
                    let todos = self.todos.lock().unwrap().clone().expect("an old host got ListTodos");
                    if !write(&writer, &HostMsg::Todos { req, todos, schedules: Vec::new() }) {
                        return;
                    }
                    HostMsg::TodosChanged
                }
                ClientMsg::ReadScreen { id, lines, .. } => {
                    assert_eq!(lines, Some(30));
                    HostMsg::ScreenText { id, text: "Do you want to proceed?\n❯ 1. Yes\n".into(), truncated: false }
                }
                other => panic!("the pusher sent {other:?}"),
            };
            if !write(&writer, &reply) {
                return;
            }
        }
    }

    /// 断开所有连接。
    fn hang_up(&self) {
        for stream in self.streams.lock().unwrap().drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn set(&self, state: AgentState) {
        let meta = SessionMeta {
            title: Some("修 bug".into()),
            agent: Some(Agent { kind: AgentKind::Claude, state }),
            ..SessionMeta::default()
        };
        let size = GridSize { cols: 80, rows: 24, cell_width_px: 0, cell_height_px: 0 };
        let info = SessionInfo { id: SESSION, size, meta, clients: 0, claimed: true, exited: false, size_owner: None };
        *self.sessions.lock().unwrap() = vec![info];
    }
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// 整条路：有登记才连宿主；连上时已经在等的不推，之后变成等回答的推一条通知（带屏幕上的问题），
/// 一直等着也只推这一条；关了推送就断开。
#[test]
fn the_pusher_follows_the_host() {
    let scratch = Scratch::new("pusher");
    let host = FakeHost::default();
    host.set(AgentState::Blocked);
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    let settings =
        PushSettings { delay: Duration::ZERO, relay: Some("https://relay.example".into()), ..PushSettings::default() };
    pusher.set_settings(settings.clone());
    pusher.set_listening(Listening::On);
    // 没有登记时不连。
    thread::sleep(Duration::from_millis(500));
    assert_eq!(host.connections.load(Ordering::SeqCst), 0);

    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    wait_until("the pusher connects", || host.connections.load(Ordering::SeqCst) == 1);
    // 第一份列表里已经在等的不推。
    thread::sleep(Duration::from_secs(3));
    assert_eq!(scratch.requests(), Vec::<String>::new());

    host.set(AgentState::Working);
    thread::sleep(Duration::from_millis(2500));
    host.set(AgentState::Blocked);
    let requests = scratch.wait_for(1);
    let notification = &body(&requests[0])["notification"];
    assert_eq!(notification["lines"], serde_json::json!(["Do you want to proceed?"]));
    assert_eq!(notification["session"], SESSION.to_string());
    assert_eq!((notification["machine"].as_str(), notification["machine_name"].as_str()), (Some(MACHINE), Some("Mac")));
    // 一直等着不再推，答了也不推。
    thread::sleep(Duration::from_secs(3));
    host.set(AgentState::Working);
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(scratch.requests().len(), 1);

    // 关了推送：断开，不再连。
    pusher.set_settings(PushSettings { enabled: false, ..settings });
    thread::sleep(Duration::from_millis(500));
    host.set(AgentState::Blocked);
    thread::sleep(Duration::from_secs(3));
    assert_eq!(host.connections.load(Ordering::SeqCst), 1);
    assert_eq!(scratch.requests().len(), 1);
    // 宿主不认得看板：不要任务表。
    assert_eq!(host.list_todos.load(Ordering::SeqCst), 0);
}

/// 宿主开始交接时说了 `Goodbye`，交接却回滚了：照常重连，接着推。
#[test]
fn a_rolled_back_handoff_keeps_pushing() {
    let scratch = Scratch::new("pusher-rollback");
    let host = FakeHost::default();
    host.set(AgentState::Blocked);
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    let settings =
        PushSettings { delay: Duration::ZERO, relay: Some("https://relay.example".into()), ..PushSettings::default() };
    pusher.set_settings(settings);
    pusher.set_listening(Listening::On);
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    wait_until("the pusher connects", || host.connections.load(Ordering::SeqCst) == 1);
    host.goodbye.store(true, Ordering::SeqCst);
    wait_until("the pusher reconnects", || host.connections.load(Ordering::SeqCst) >= 2);

    host.set(AgentState::Working);
    thread::sleep(Duration::from_millis(2500));
    host.set(AgentState::Blocked);
    scratch.wait_for(1);
}

/// 任务从进行中变成做完：经中转的 v3 接口推一条任务做完的通知，不推等回答的。
#[test]
fn finished_todos_are_pushed() {
    let scratch = Scratch::new("pusher-todo");
    let host = FakeHost::default();
    host.set(AgentState::Working);
    let now = crate::now_unix();
    let todo = Todo {
        id: "0000002a".into(),
        number: 0,
        spec: TodoSpec {
            title: "加暗色模式".into(),
            prompt: String::new(),
            dir: "/tmp".into(),
            agent: "claude".into(),
            worktree: false,
            ..TodoSpec::default()
        },
        created: now - 200,
        started: Some(now - 100),
        session: Some(SESSION),
        worktree_dir: None,
        branch: None,
        done_at: None,
        schedule: None,
        rank: 0,
    };
    *host.todos.lock().unwrap() = Some(vec![todo]);
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    pusher.set_settings(PushSettings {
        delay: Duration::ZERO,
        relay: Some("https://relay.example".into()),
        ..PushSettings::default()
    });
    pusher.set_listening(Listening::On);
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    wait_until("the pusher reads the todos", || host.list_todos.load(Ordering::SeqCst) >= 2);

    host.set(AgentState::Idle);
    let requests = scratch.wait_for(1);
    assert_eq!(url(&requests[0]), "https://relay.example/v3/notify");
    let notify = body(&requests[0]);
    assert_eq!(notify["v"], 3);
    let notification = &notify["notification"];
    assert_eq!(
        (notification["kind"].as_str(), notification["title"].as_str()),
        (Some("todo_done"), Some("加暗色模式"))
    );
    assert_eq!(notification["session"], SESSION.to_string());
    // 一直停着不再推。
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(scratch.requests().len(), 1);
}

/// 宿主回会话列表比两轮之间还慢：上一份没回之前不要下一份，晚到的那份不会和下一轮的任务表配成一对。
#[test]
fn a_slow_session_list_holds_back_the_next_round() {
    let scratch = Scratch::new("pusher-slow");
    let host = FakeHost::default();
    host.set(AgentState::Working);
    *host.todos.lock().unwrap() = Some(Vec::new());
    *host.list_delay.lock().unwrap() = Some(Duration::from_millis(2500));
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    pusher.set_settings(PushSettings {
        delay: Duration::ZERO,
        relay: Some("https://relay.example".into()),
        ..PushSettings::default()
    });
    pusher.set_listening(Listening::On);
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    wait_until("the pusher asks for a second list", || host.list_sessions.load(Ordering::SeqCst) >= 2);
    assert_eq!(host.most_lists_waiting.load(Ordering::SeqCst), 1);
    // 任务表跟着会话列表一轮一轮地要，不多要。
    wait_until("the todos follow the lists", || {
        host.list_todos.load(Ordering::SeqCst) == host.list_sessions.load(Ordering::SeqCst)
    });
}

/// 关了推送时，在退避的通知马上不发了：不到下一轮读设备表就被叫醒（这里是宿主断开）时也一样。
#[test]
fn turning_pushes_off_drops_backing_off_notifications() {
    let scratch = Scratch::new("pusher-off");
    let host = FakeHost::default();
    host.set(AgentState::Working);
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    let settings =
        PushSettings { delay: Duration::ZERO, relay: Some("https://relay.example".into()), ..PushSettings::default() };
    pusher.set_settings(settings.clone());
    pusher.set_listening(Listening::On);
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    wait_until("the pusher connects", || host.connections.load(Ordering::SeqCst) == 1);
    thread::sleep(Duration::from_millis(2500));

    scratch.rule("/v2/notify", "FAIL");
    host.set(AgentState::Blocked);
    wait_until("the first attempt", || scratch.requests().len() == 1);
    pusher.set_settings(PushSettings { enabled: false, ..settings });
    host.hang_up();
    // 退避两秒后本该重试。
    thread::sleep(Duration::from_secs(3));
    assert_eq!(scratch.requests().len(), 1);
}

/// 设备表一时读不了（这里是文件坏了）不当成手机都注销了：照旧连着宿主，之后要等回答的照样推。
#[test]
fn an_unreadable_device_table_keeps_the_registrations() {
    let scratch = Scratch::new("pusher-unreadable");
    let host = FakeHost::default();
    host.set(AgentState::Working);
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    pusher.set_settings(PushSettings {
        delay: Duration::ZERO,
        relay: Some("https://relay.example".into()),
        ..PushSettings::default()
    });
    pusher.set_listening(Listening::On);
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    wait_until("the first session list", || host.list_sessions.load(Ordering::SeqCst) >= 1);

    std::fs::write(scratch.dirs.remote_access_devices_file().unwrap(), "{ not json").unwrap();
    // 每轮先读设备表再要会话列表：又要了两份列表时，坏了的设备表至少读过一次。
    let lists = host.list_sessions.load(Ordering::SeqCst);
    wait_until("two more rounds", || host.list_sessions.load(Ordering::SeqCst) >= lists + 2);
    host.set(AgentState::Blocked);
    scratch.wait_for(1);
    assert_eq!(host.connections.load(Ordering::SeqCst), 1);
}

/// 官方 App 只在这个构建带着中转时经中转推，没带时跳过。
#[test]
fn the_official_app_needs_a_relay() {
    let registration = PushRegistration {
        device_id: DEVICE,
        token: TOKEN.into(),
        env: ApnsEnv::Production,
        bundle: OFFICIAL_BUNDLE.into(),
        machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF".into(),
        machine_name: "Mac".into(),
        updated_at: 1,
    };
    let relay = PushSettings { relay: Some("https://relay.example".into()), ..PushSettings::default() };
    assert_eq!(route(&registration, &relay), Some(RELAY_PROD));
    assert_eq!(route(&registration, &PushSettings { relay: None, ..relay }), None);
}

/// 登记都推不出去（没带中转的构建里只有官方 App 的登记）时不连宿主，单独跑的宿主照常能因空闲退出。
#[test]
fn unroutable_registrations_leave_the_host_alone() {
    let scratch = Scratch::new("unroutable");
    let host = FakeHost::default();
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    pusher.set_settings(PushSettings { delay: Duration::ZERO, relay: None, ..PushSettings::default() });
    pusher.set_listening(Listening::On);
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    thread::sleep(Duration::from_secs(3));
    assert_eq!(host.connections.load(Ordering::SeqCst), 0);
}

/// 测试推送：给每条登记直接发一条通知，回推到了几台；没有登记时回 0，推不到的登记回原因，不发请求。
#[test]
fn a_test_push_notifies_each_phone() {
    let scratch = Scratch::new("probe");
    let (key_file, _) = test_key(&scratch.root);
    let key = ApnsKey { key_file, key_id: "K".into(), team_id: "T".into(), bundle: "dev.example.app".into() };
    let settings = PushSettings { enabled: false, direct: Some(key), relay: None, ..PushSettings::default() };
    let content = TestPush { title: "测试推送".into(), lines: vec!["推送没问题。".into()] };
    let probe = |settings: &PushSettings| probe(&scratch.dirs, settings, &content, &scratch.curl);
    assert_eq!(probe(&settings), Ok(0));
    scratch.register(ApnsEnv::Development, "dev.example.app");
    assert_eq!(probe(&settings), Ok(1));
    let requests = scratch.wait_for(1);
    assert_eq!(url(&requests[0]), format!("https://api.sandbox.push.apple.com/3/device/{TOKEN}"));
    let alert = &body(&requests[0])["aps"]["alert"];
    assert_eq!(alert["body"], "推送没问题。");
    assert_eq!(alert["subtitle-loc-args"], serde_json::json!(["测试推送", "Mac"]));
    assert!(alert.get("title-loc-args").is_none());

    let err = probe(&PushSettings { direct: None, ..settings.clone() }).unwrap_err();
    assert!(err.contains("dev.example.app"), "{err}");
    scratch.wait_for(1);
    scratch.rule("/3/device", r#"HTTP/2 400\r\n\r\n{"reason":"BadDeviceToken"}"#);
    let err = probe(&settings).unwrap_err();
    assert!(err.contains("BadDeviceToken"), "{err}");
    // 不改设备表。
    assert_eq!(scratch.registrations()[0].env, ApnsEnv::Development);
}
