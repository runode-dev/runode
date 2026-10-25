//! HTTP 接口：`RELAY_REGISTER` 换句柄，`RELAY_NOTIFY` 发通知。出错时回 `PushError`，限频时回 429 带
//! `Retry-After`；APNs 的回话原样带回，电脑按状态决定重试、删登记还是放弃。

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
    routing::post,
};
use runode_protocol::push::{
    ApnsEnv, EXPIRES_AFTER, PushError, RELAY_NOTIFY, RELAY_REGISTER, RELAY_VERSION, RelayHandle, RelayNotify,
    RelayRegister, notify_payload,
};

use crate::{
    apns::{Apns, Push, Reply, now_unix},
    handle::{Keys, Sealed, decode_hex, encode_hex, parse_uuid},
    limit::{Limiter, ip_key},
};

/// 请求体最多这么多字节。
const MAX_BODY: usize = 8 * 1024;
/// device token 解出来的字节数要在这个范围里（APNs 现在给 32 字节）。
const TOKEN_BYTES: std::ops::RangeInclusive<usize> = 16..=100;
/// 同一个地址每分钟最多换这么多次句柄。
const REGISTER_PER_IP: u32 = 30;
/// 同一个地址每分钟最多发这么多条通知（一台电脑给好几部手机推，每部算一条）。
const NOTIFY_PER_IP: u32 = 120;
/// 同一部手机在同一台电脑上每分钟最多收这么多条。
const NOTIFY_PER_HANDLE: u32 = 10;
const MINUTE: Duration = Duration::from_secs(60);
/// 一次通知连同换环境、重签 JWT 在内，发给 APNs 最多花这么久。要短于电脑等中转回话的时限，不然电脑判超时
/// 重发，手机会响两次。
const DELIVER_DEADLINE: Duration = Duration::from_secs(12);

pub struct Relay<A> {
    keys: Keys,
    apns: A,
    /// 在反向代理后面：客户端地址取 `X-Forwarded-For` 最右边那个，它是代理自己加上的。
    trust_proxy: bool,
    register_ip: Limiter,
    notify_ip: Limiter,
    notify_handle: Limiter,
}

impl<A: Apns> Relay<A> {
    pub fn new(keys: Keys, apns: A, trust_proxy: bool) -> Self {
        Self {
            keys,
            apns,
            trust_proxy,
            register_ip: Limiter::new(REGISTER_PER_IP, MINUTE),
            notify_ip: Limiter::new(NOTIFY_PER_IP, MINUTE),
            notify_handle: Limiter::new(NOTIFY_PER_HANDLE, MINUTE),
        }
    }

    fn client(&self, peer: SocketAddr, headers: &HeaderMap) -> String {
        let forwarded = || {
            let value = headers.get_all("x-forwarded-for").iter().next_back()?.to_str().ok()?;
            value.rsplit(',').next()?.trim().parse::<IpAddr>().ok()
        };
        let ip = if self.trust_proxy { forwarded() } else { None };
        ip_key(ip.unwrap_or(peer.ip()))
    }
}

/// 服务的路由；连接要带 `ConnectInfo<SocketAddr>`（`into_make_service_with_connect_info`）。
pub fn router<A: Apns>(relay: Arc<Relay<A>>) -> Router {
    Router::new()
        .route(RELAY_REGISTER, post(register::<A>))
        .route(RELAY_NOTIFY, post(notify::<A>))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(relay)
}

async fn register<A: Apns>(
    State(relay): State<Arc<Relay<A>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(wait) = relay.register_ip.take(&relay.client(peer, &headers), Instant::now()) {
        return too_many(wait);
    }
    let Ok(request) = serde_json::from_slice::<RelayRegister>(&body) else {
        return error(StatusCode::BAD_REQUEST, "BadRequest");
    };
    if request.v != RELAY_VERSION {
        return error(StatusCode::BAD_REQUEST, "UnsupportedVersion");
    }
    if request.env == ApnsEnv::Unknown {
        return error(StatusCode::BAD_REQUEST, "BadEnvironment");
    }
    let Some(token) = decode_hex(&request.token).filter(|token| TOKEN_BYTES.contains(&token.len())) else {
        return error(StatusCode::BAD_REQUEST, "BadDeviceToken");
    };
    let Some(machine) = parse_uuid(&request.machine) else {
        return error(StatusCode::BAD_REQUEST, "BadMachine");
    };
    match relay.keys.seal(&Sealed { env: request.env, machine, token }) {
        Ok(handle) => Json(RelayHandle { handle }).into_response(),
        Err(_) => {
            tracing::error!("cannot seal a handle: the system random number generator failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "InternalServerError")
        }
    }
}

async fn notify<A: Apns>(
    State(relay): State<Arc<Relay<A>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(wait) = relay.notify_ip.take(&relay.client(peer, &headers), Instant::now()) {
        return too_many(wait);
    }
    let Ok(request) = serde_json::from_slice::<RelayNotify>(&body) else {
        return error(StatusCode::BAD_REQUEST, "BadRequest");
    };
    if request.v != RELAY_VERSION {
        return error(StatusCode::BAD_REQUEST, "UnsupportedVersion");
    }
    let Some(sealed) = relay.keys.open(&request.handle) else {
        return error(StatusCode::BAD_REQUEST, "BadDeviceToken");
    };
    if parse_uuid(&request.notification.machine) != Some(sealed.machine) {
        return error(StatusCode::FORBIDDEN, "MachineMismatch");
    }
    let phone = format!("{}{}", encode_hex(&sealed.token), encode_hex(&sealed.machine));
    if let Err(wait) = relay.notify_handle.take(&phone, Instant::now()) {
        return too_many(wait);
    }
    let Some(payload) = notify_payload(&request.notification) else {
        return error(StatusCode::BAD_REQUEST, "PayloadTooLarge");
    };
    let now = now_unix();
    let push = Push {
        env: sealed.env,
        token: sealed.token,
        payload,
        expiration: request.expiration.clamp(now, now + EXPIRES_AFTER),
        collapse_id: request.notification.collapse_id(),
    };
    let started = Instant::now();
    let reply = match tokio::time::timeout(DELIVER_DEADLINE, deliver(&relay.apns, push)).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(why)) => {
            tracing::warn!("push failed after {:?}: {why}", started.elapsed());
            return error(StatusCode::BAD_GATEWAY, "ApnsUnreachable");
        }
        Err(_) => {
            tracing::warn!("push gave up after {:?}", started.elapsed());
            return error(StatusCode::GATEWAY_TIMEOUT, "ApnsUnreachable");
        }
    };
    tracing::info!("push answered {} {} after {:?}", reply.status, reply.reason, started.elapsed());
    relay_reply(reply)
}

/// 发给 APNs；不认 device token 时换另一个环境再发一次，两边都不认才算不认。另一个环境那次连不上时
/// 照第一次的回话算，电脑据此删掉登记，手机下次连上会重新登记。
async fn deliver(apns: &impl Apns, push: Push) -> Result<Reply, String> {
    let reply = apns.send(&push).await?;
    if !reply.bad_token() {
        return Ok(reply);
    }
    let env = if push.env == ApnsEnv::Production { ApnsEnv::Development } else { ApnsEnv::Production };
    Ok(apns.send(&Push { env, ..push }).await.unwrap_or(reply))
}

fn relay_reply(reply: Reply) -> Response {
    let Ok(status) = StatusCode::from_u16(reply.status) else {
        return error(StatusCode::BAD_GATEWAY, "BadApnsResponse");
    };
    if status.is_success() {
        return StatusCode::OK.into_response();
    }
    let mut response = error(status, &reply.reason);
    if let Some(value) = reply.retry_after.and_then(|after| HeaderValue::from_str(&after).ok()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

fn error(status: StatusCode, reason: &str) -> Response {
    (status, Json(PushError { reason: reason.to_owned() })).into_response()
}

fn too_many(wait: Duration) -> Response {
    let mut response = error(StatusCode::TOO_MANY_REQUESTS, "TooManyRequests");
    let seconds = wait.as_secs_f64().ceil().max(1.0) as u64;
    response.headers_mut().insert(RETRY_AFTER, HeaderValue::from(seconds));
    response
}
