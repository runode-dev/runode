//! 推送的发送线程和轮询线程，对着假的 curl（一个 shell 脚本，把收到的配置记成文件，按规则回话）
//! 和假的宿主（一对 Unix socket，按 protocol 回会话列表和屏幕文字）跑。

use std::{
    io,
    os::unix::{fs::PermissionsExt as _, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use runode_paths::Dirs;
use runode_protocol::{
    ClientMsg, Frame, FrameKind, HostMsg, PROTOCOL_VERSION, SessionId, SessionInfo,
    push::{ActivityAttributes, ActivityContent, ApnsEnv, OFFICIAL_BUNDLE},
    read_frame,
    remote::{Bytes, DeviceId},
    write_frame,
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    grid::GridSize,
    session::SessionMeta,
};

use super::{
    ApnsKey, Listening, PushSettings, Pusher,
    apns::{Route, Via, test_key},
    curl::Curl,
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

    /// 配对一台设备，登记 `bundle` 的 push-to-start token。
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

    fn channels_file(&self) -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(self.dirs.remote_access_push_channels_file().unwrap()).ok()?;
        Some(serde_json::from_str(&text).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 假的 curl：`--version` 说自己会 HTTP/2；别的时候把 stdin 上的配置存成 `req-NNN`，先找第一条匹配的
/// 规则（`rule-NNN`：第一行是要找的文字，其余是响应），没有就按 URL 回：建频道回一个新频道，别的回 200。
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
case $(cat "$req") in
  *'/v1/channels"'*) printf 'HTTP/2 200\r\n\r\n{"channel":"relay-%s"}' "$n" ;;
  *'/channels"'*'request = "POST"'*) printf 'HTTP/2 201\r\napns-channel-id: direct-%s\r\n\r\n' "$n" ;;
  *) printf 'HTTP/2 200\r\n\r\n' ;;
esac
"#;

fn content(lines: &[&str]) -> ActivityContent {
    ActivityContent {
        title: "修 bug".into(),
        agent: "Claude Code".into(),
        lines: lines.iter().map(|l| (*l).into()).collect(),
    }
}

fn target() -> Target {
    Target {
        device: DEVICE,
        token: TOKEN.into(),
        attributes: ActivityAttributes::new("6F9619FF-8B86-D011-B42D-00C04FC964FF".into(), "Mac", SESSION),
    }
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

/// 经中转起、刷新、收起：建频道 → 起 → 广播刷新 → 广播收起 → 删频道，频道文件跟着记下又删掉。
#[test]
fn a_round_through_the_relay() {
    let scratch = Scratch::new("relay");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, "https://relay.example".into());
    assert_eq!(sender.load(), []);
    sender.open(SESSION, vec![(RELAY_PROD, vec![target()])], content(&["Proceed?"]));
    let requests = scratch.wait_for(2);
    assert_eq!(url(&requests[0]), "https://relay.example/v1/channels");
    assert_eq!(body(&requests[0]), serde_json::json!({ "v": 1, "env": "production" }));
    assert_eq!(url(&requests[1]), "https://relay.example/v1/start");
    let start = body(&requests[1]);
    assert_eq!((start["token"].as_str(), start["channel"].as_str()), (Some(TOKEN), Some("relay-000")));
    assert_eq!(start["content"]["lines"], serde_json::json!(["Proceed?"]));
    let file = scratch.channels_file().unwrap();
    assert_eq!(file["sessions"][0]["channels"][0]["channel"], "relay-000");

    sender.update(SESSION, content(&["Allow edits?"]));
    let requests = scratch.wait_for(3);
    let update = body(&requests[2]);
    assert_eq!((url(&requests[2]), update["event"].as_str()), ("https://relay.example/v1/broadcast", Some("update")));
    assert_eq!(update["content"]["lines"], serde_json::json!(["Allow edits?"]));
    assert_eq!(
        scratch.channels_file().unwrap()["sessions"][0]["content"]["lines"],
        serde_json::json!(["Allow edits?"])
    );

    sender.end(SESSION, content(&[]));
    let requests = scratch.wait_for(5);
    assert_eq!(body(&requests[3])["event"], "end");
    assert_eq!(url(&requests[4]), "https://relay.example/v1/channels/delete");
    assert_eq!(body(&requests[4])["channel"], "relay-000");
    assert_eq!(scratch.channels_file().unwrap()["sessions"], serde_json::json!([]));
}

/// 直连：JWT 只在 stdin 的配置里，不在命令行参数里；建频道拿响应头里的 id。
#[test]
fn direct_pushes_sign_with_the_key() {
    let scratch = Scratch::new("direct");
    let (key_file, _) = test_key(&scratch.root);
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    let key = ApnsKey { key_file, key_id: "K".into(), team_id: "T".into(), bundle: "dev.example.app".into() };
    sender.settings(Some(key), "https://relay.example".into());
    let route = Route { env: ApnsEnv::Development, via: Via::Direct };
    sender.open(SESSION, vec![(route, vec![target()])], content(&[]));
    let requests = scratch.wait_for(2);
    assert_eq!(
        url(&requests[0]),
        "https://api-manage-broadcast.sandbox.push.apple.com:2195/1/apps/dev.example.app/channels"
    );
    assert!(requests[0].contains("header = \"authorization: bearer "));
    assert_eq!(url(&requests[1]), format!("https://api.sandbox.push.apple.com/3/device/{TOKEN}"));
    assert_eq!(body(&requests[1])["aps"]["input-push-channel"], "direct-000");

    // 密钥不对：记错误，直连的不再发，换了配置才再发。
    scratch.rule("/4/broadcasts", r#"HTTP/2 403\r\n\r\n{"reason":"InvalidProviderToken"}"#);
    sender.update(SESSION, content(&["a"]));
    scratch.wait_for(3);
    sender.update(SESSION, content(&["b"]));
    thread::sleep(Duration::from_millis(500));
    assert_eq!(scratch.requests().len(), 3);
    let (key_file, _) = test_key(&scratch.root);
    sender.settings(
        Some(ApnsKey { key_file, key_id: "K2".into(), team_id: "T".into(), bundle: "dev.example.app".into() }),
        "https://relay.example".into(),
    );
    sender.update(SESSION, content(&["c"]));
    let requests = scratch.wait_for(4);
    assert_eq!(body(&requests[3])["aps"]["content-state"]["lines"], serde_json::json!(["c"]));
}

/// token 不认时换另一个环境（另建频道）再起，成了把登记的环境改过来；两边都不认就删掉登记。
#[test]
fn bad_tokens_try_the_other_environment() {
    let scratch = Scratch::new("badtoken");
    scratch.register(ApnsEnv::Production, OFFICIAL_BUNDLE);
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, "https://relay.example".into());
    scratch.rule("/v1/start", r#"HTTP/2 400\r\n\r\n{"reason":"BadDeviceToken"}"#);
    sender.open(SESSION, vec![(RELAY_PROD, vec![target()])], content(&[]));
    let requests = scratch.wait_for(4);
    assert_eq!(body(&requests[2]), serde_json::json!({ "v": 1, "env": "development" }));
    let start = body(&requests[3]);
    assert_eq!((start["env"].as_str(), start["channel"].as_str()), (Some("development"), Some("relay-002")));
    assert_eq!(scratch.registrations()[0].env, ApnsEnv::Development);
    // 收起时两个频道都收起、都删掉。
    sender.end(SESSION, content(&[]));
    let requests = scratch.wait_for(8);
    let deleted: Vec<String> = requests[6..].iter().map(|r| body(r)["channel"].as_str().unwrap().to_owned()).collect();
    assert_eq!(deleted, ["relay-000", "relay-002"]);

    scratch.rule("/v1/start", r#"HTTP/2 410\r\n\r\n{"reason":"Unregistered"}"#);
    scratch.rule("/v1/start", r#"HTTP/2 410\r\n\r\n{"reason":"Unregistered"}"#);
    sender.open(SESSION, vec![(RELAY_PROD, vec![target()])], content(&[]));
    scratch.wait_for(12);
    assert_eq!(scratch.registrations(), []);
}

/// 连不上时退避重试；收起时还没起来的不用起了。
#[test]
fn failures_are_retried_and_ends_win() {
    let scratch = Scratch::new("retry");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, "https://relay.example".into());
    scratch.rule("/v1/channels", "FAIL");
    let started = Instant::now();
    sender.open(SESSION, vec![(RELAY_PROD, vec![target()])], content(&[]));
    let requests = scratch.wait_for(3);
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_eq!(url(&requests[2]), "https://relay.example/v1/start");

    // 起 Live Activity 一直失败时，收起会把它从队里拿掉，接着收起、删频道。
    let other = SessionId(2);
    scratch.rule("/v1/channels", "HTTP/2 503\\r\\n\\r\\n");
    sender.open(other, vec![(RELAY_PROD, vec![target()])], content(&[]));
    scratch.wait_for(4);
    sender.end(other, content(&[]));
    thread::sleep(Duration::from_secs(3));
    assert_eq!(scratch.requests().len(), 4);
}

/// 频道文件读回来的是上次没收起的会话和最后内容。
#[test]
fn open_rounds_survive_a_restart() {
    let scratch = Scratch::new("restart");
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    sender.settings(None, "https://relay.example".into());
    sender.load();
    sender.open(SESSION, vec![(RELAY_PROD, vec![target()])], content(&["q"]));
    scratch.wait_for(2);
    drop(sender);
    let sender = Sender::start(scratch.dirs.clone(), scratch.curl.clone()).unwrap();
    assert_eq!(sender.load(), [(SESSION, content(&["q"]))]);
    sender.settings(None, "https://relay.example".into());
    sender.end(SESSION, content(&["q"]));
    let requests = scratch.wait_for(4);
    assert_eq!(body(&requests[3])["channel"], "relay-000");
}

/// 假的宿主：每条连接一个线程，回 `Welcome`、会话列表和屏幕文字。
#[derive(Clone, Default)]
struct FakeHost {
    sessions: Arc<Mutex<Vec<SessionInfo>>>,
    connections: Arc<AtomicUsize>,
}

impl FakeHost {
    fn connect(&self) -> crate::Connect {
        let host = self.clone();
        Arc::new(move || -> io::Result<UnixStream> {
            let (ours, theirs) = UnixStream::pair()?;
            host.connections.fetch_add(1, Ordering::SeqCst);
            let host = host.clone();
            thread::spawn(move || host.serve(theirs));
            Ok(ours)
        })
    }

    fn serve(&self, mut stream: UnixStream) {
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
                    }
                }
                ClientMsg::ListSessions => HostMsg::SessionList { sessions: self.sessions.lock().unwrap().clone() },
                ClientMsg::ReadScreen { id, lines, .. } => {
                    assert_eq!(lines, Some(12));
                    HostMsg::ScreenText { id, text: "Do you want to proceed?\n❯ 1. Yes\n".into(), truncated: false }
                }
                other => panic!("the pusher sent {other:?}"),
            };
            let frame = Frame::control(&reply).unwrap();
            if write_frame(&mut stream, FrameKind::Control, frame.channel, &frame.payload).is_err() {
                return;
            }
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

/// 整条路：有登记才连宿主；连上时已经在等的不推，之后变成等回答的起 Live Activity（带屏幕上的问题），
/// 答了收起；关了推送就断开。
#[test]
fn the_pusher_follows_the_host() {
    let scratch = Scratch::new("pusher");
    let host = FakeHost::default();
    host.set(AgentState::Blocked);
    let pusher = Pusher::start_with(scratch.dirs.clone(), host.connect(), scratch.curl.clone()).unwrap();
    let settings =
        PushSettings { delay: Duration::ZERO, relay: "https://relay.example".into(), ..PushSettings::default() };
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
    let requests = scratch.wait_for(2);
    let start = body(&requests[1]);
    assert_eq!(start["content"]["lines"], serde_json::json!(["Do you want to proceed?", "❯ 1. Yes"]));
    assert_eq!(start["attributes"]["session"], SESSION.to_string());

    host.set(AgentState::Working);
    let requests = scratch.wait_for(4);
    assert_eq!(body(&requests[2])["event"], "end");
    assert_eq!(url(&requests[3]), "https://relay.example/v1/channels/delete");

    // 关了推送：断开，不再连。
    pusher.set_settings(PushSettings { enabled: false, ..settings });
    thread::sleep(Duration::from_millis(500));
    host.set(AgentState::Blocked);
    thread::sleep(Duration::from_secs(3));
    assert_eq!(host.connections.load(Ordering::SeqCst), 1);
    assert_eq!(scratch.requests().len(), 4);
}
