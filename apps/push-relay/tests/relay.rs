//! 两个接口对着假的 APNs 跑：请求体用 `runode_protocol::push` 的类型拼，线上格式由那边对着样例锁住。

use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header::RETRY_AFTER},
};
use runode_protocol::{
    SessionId,
    push::{ApnsEnv, EXPIRES_AFTER, Notification, RELAY_VERSION, RelayNotify, RelayRegister, notify_payload},
};
use runode_push::{
    apns::{Apns, Push, Reply, now_unix},
    handle::{Keys, generate_key},
    server::{Relay, router},
};
use serde_json::Value;
use tower::ServiceExt as _;

const TOKEN: &str = "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011";
const MACHINE: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";

/// 假的 APNs：记下发来的推送，按排好的回话答，没排的回 200；`delay` 不为空时每次先等这么久。
#[derive(Default)]
struct FakeApns {
    sent: Mutex<Vec<Push>>,
    replies: Mutex<VecDeque<Result<Reply, String>>>,
    delay: Mutex<Option<Duration>>,
}

impl Apns for FakeApns {
    async fn send(&self, push: &Push) -> Result<Reply, String> {
        self.sent.lock().unwrap().push(push.clone());
        let delay = *self.delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let ok = Reply { status: 200, reason: String::new(), retry_after: None };
        self.replies.lock().unwrap().pop_front().unwrap_or(Ok(ok))
    }
}

struct Harness {
    app: Router,
    apns: Arc<FakeApns>,
}

impl Harness {
    fn new() -> Self {
        let apns = Arc::new(FakeApns::default());
        let keys = Keys::parse(&format!("1={}", generate_key().unwrap())).unwrap();
        Self { app: router(Arc::new(Relay::new(keys, apns.clone(), false))), apns }
    }

    fn reply(&self, status: u16, reason: &str, retry_after: Option<&str>) {
        let reply = Reply { status, reason: reason.into(), retry_after: retry_after.map(str::to_owned) };
        self.apns.replies.lock().unwrap().push_back(Ok(reply));
    }

    fn sent(&self) -> Vec<Push> {
        self.apns.sent.lock().unwrap().clone()
    }

    /// 从 `ip` 发 `body` 到 `path`，回状态、`Retry-After` 和 JSON 体（没有体时是 `Null`，不是 JSON 时是字符串）。
    async fn post(&self, path: &str, body: Vec<u8>, ip: [u8; 4]) -> (StatusCode, Option<String>, Value) {
        let mut request =
            Request::post(path).header("content-type", "application/json").body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ConnectInfo(SocketAddr::from((ip, 40000))));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let retry = response.headers().get(RETRY_AFTER).map(|v| v.to_str().unwrap().to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| if bytes.is_empty() { Value::Null } else { String::from_utf8_lossy(&bytes).into() });
        (status, retry, body)
    }

    async fn register(&self, env: ApnsEnv, token: &str, machine: &str) -> (StatusCode, Value) {
        let request = RelayRegister { v: RELAY_VERSION, env, token: token.into(), machine: machine.into() };
        let (status, _, body) = self.post("/v1/register", serde_json::to_vec(&request).unwrap(), [10, 0, 0, 1]).await;
        (status, body)
    }

    async fn handle(&self) -> String {
        let (status, body) = self.register(ApnsEnv::Production, TOKEN, MACHINE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["handle"].as_str().unwrap().to_owned()
    }

    async fn notify(&self, handle: &str, notification: &Notification) -> (StatusCode, Option<String>, Value) {
        let request = RelayNotify::new(handle.into(), notification, now_unix()).unwrap();
        self.post("/v1/notify", serde_json::to_vec(&request).unwrap(), [10, 0, 0, 2]).await
    }
}

fn notification() -> Notification {
    Notification::todo_done(MACHINE.into(), "Mac", SessionId(0x0123_4567_89ab_cdef_0011_2233_4455_6677), "加暗色模式")
}

/// 换来的句柄发得出通知：APNs 收到的是封进去的 token 和环境、电脑那边一样拼出来的 payload。
#[tokio::test]
async fn handles_deliver_to_the_sealed_token() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    assert!(!handle.contains(TOKEN));
    let (status, _, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, body), (StatusCode::OK, Value::Null));
    let sent = relay.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].env, ApnsEnv::Production);
    assert_eq!(sent[0].token, runode_push::handle::decode_hex(TOKEN).unwrap());
    assert_eq!(sent[0].payload, notify_payload(&notification()).unwrap());
    assert_eq!(sent[0].collapse_id, "0123456789abcdef0011223344556677");
    let now = now_unix();
    assert!((now..=now + EXPIRES_AFTER).contains(&sent[0].expiration));
}

/// 句柄只替换它时报的那台电脑推；解不开的句柄按 APNs 不认 token 回，电脑会删掉这条登记。
#[tokio::test]
async fn handles_are_bound_to_their_machine() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    let mut other = notification();
    other.machine = "00000000-0000-0000-0000-000000000000".into();
    let (status, _, body) = relay.notify(&handle, &other).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::FORBIDDEN, Some("MachineMismatch")));
    // 大小写不同的同一个 UUID 认得。
    let mut lower = notification();
    lower.machine = MACHINE.to_lowercase();
    assert_eq!(relay.notify(&handle, &lower).await.0, StatusCode::OK);

    let foreign = Harness::new().handle().await;
    let (status, _, body) = relay.notify(&foreign, &notification()).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::BAD_REQUEST, Some("BadDeviceToken")));
    assert_eq!(relay.sent().len(), 1);
}

/// APNs 不认 token 时换另一个环境再发；成了回 200，两边都不认才把 APNs 的错带回去。
#[tokio::test]
async fn bad_tokens_try_the_other_environment() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    relay.reply(400, "BadDeviceToken", None);
    assert_eq!(relay.notify(&handle, &notification()).await.0, StatusCode::OK);
    let envs: Vec<ApnsEnv> = relay.sent().iter().map(|push| push.env).collect();
    assert_eq!(envs, [ApnsEnv::Production, ApnsEnv::Development]);

    relay.reply(410, "Unregistered", None);
    relay.reply(400, "BadDeviceToken", None);
    let (status, _, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::BAD_REQUEST, Some("BadDeviceToken")));
    assert_eq!(relay.sent().len(), 4);
}

/// 换到另一个环境那次连不上时照第一次的回话算：APNs 说过不认这个 token，电脑该删登记，不该一直重试。
#[tokio::test]
async fn an_unreachable_second_environment_keeps_the_first_answer() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    relay.reply(410, "Unregistered", None);
    relay.apns.replies.lock().unwrap().push_back(Err("connection reset".into()));
    let (status, _, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::GONE, Some("Unregistered")));
    assert_eq!(relay.sent().len(), 2);
}

/// APNs 一直不回话时按总时限放弃、回 504，不让电脑那头先等到超时再重发。
#[tokio::test(start_paused = true)]
async fn slow_apns_gives_up_within_the_deadline() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    *relay.apns.delay.lock().unwrap() = Some(Duration::from_secs(60));
    let started = tokio::time::Instant::now();
    let (status, _, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::GATEWAY_TIMEOUT, Some("ApnsUnreachable")));
    assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
}

/// APNs 的别的错误原样带回，`Retry-After` 也带上；连不上 APNs 时回 502，电脑过一会儿再试。
#[tokio::test]
async fn apns_errors_pass_through() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    relay.reply(429, "TooManyRequests", Some("30"));
    let (status, retry, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, retry.as_deref()), (StatusCode::TOO_MANY_REQUESTS, Some("30")));
    assert_eq!(body["reason"], "TooManyRequests");

    relay.apns.replies.lock().unwrap().push_back(Err("connection reset".into()));
    let (status, _, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::BAD_GATEWAY, Some("ApnsUnreachable")));
    assert_eq!(relay.sent().len(), 2);
}

/// 同一部手机在同一台电脑上推得太勤时回 429 带 `Retry-After`，不发给 APNs。
#[tokio::test]
async fn notifications_are_limited_per_handle() {
    let relay = Harness::new();
    let handle = relay.handle().await;
    for _ in 0..10 {
        assert_eq!(relay.notify(&handle, &notification()).await.0, StatusCode::OK);
    }
    let (status, retry, body) = relay.notify(&handle, &notification()).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("TooManyRequests")));
    assert!(retry.unwrap().parse::<u64>().unwrap() >= 1);
    assert_eq!(relay.sent().len(), 10);
    // 同一部手机为别的电脑换的句柄另算。
    let other = "00000000-0000-0000-0000-000000000001";
    let (_, body) = relay.register(ApnsEnv::Production, TOKEN, other).await;
    let mut elsewhere = notification();
    elsewhere.machine = other.into();
    assert_eq!(relay.notify(body["handle"].as_str().unwrap(), &elsewhere).await.0, StatusCode::OK);
}

/// 同一个地址换句柄太勤时回 429。
#[tokio::test]
async fn registrations_are_limited_per_address() {
    let relay = Harness::new();
    for _ in 0..30 {
        assert_eq!(relay.register(ApnsEnv::Production, TOKEN, MACHINE).await.0, StatusCode::OK);
    }
    let (status, body) = relay.register(ApnsEnv::Production, TOKEN, MACHINE).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("TooManyRequests")));
}

#[tokio::test]
async fn bad_registrations_are_refused() {
    let relay = Harness::new();
    for (env, token, machine, reason) in [
        (ApnsEnv::Unknown, TOKEN, MACHINE, "BadEnvironment"),
        (ApnsEnv::Production, "80f0", MACHINE, "BadDeviceToken"),
        (ApnsEnv::Production, &"zz".repeat(32), MACHINE, "BadDeviceToken"),
        (ApnsEnv::Production, TOKEN, "not-a-uuid", "BadMachine"),
    ] {
        let (status, body) = relay.register(env, token, machine).await;
        assert_eq!((status, body["reason"].as_str()), (StatusCode::BAD_REQUEST, Some(reason)), "{token} {machine}");
    }
    let wrong_version = serde_json::json!({ "v": 2, "env": "production", "token": TOKEN, "machine": MACHINE });
    let (status, _, body) = relay.post("/v1/register", wrong_version.to_string().into_bytes(), [10, 0, 0, 1]).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::BAD_REQUEST, Some("UnsupportedVersion")));
    let (status, _, body) = relay.post("/v1/register", b"{".to_vec(), [10, 0, 0, 1]).await;
    assert_eq!((status, body["reason"].as_str()), (StatusCode::BAD_REQUEST, Some("BadRequest")));
    let (status, _, _) = relay.post("/v1/notify", vec![b' '; 9 * 1024], [10, 0, 0, 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}
