//! 推送请求怎么拼、响应怎么读：直连 APNs（用配置里的密钥签 JWT）或者经中转服务
//! （`runode_protocol::push` 里的 `RelayStart` 这些请求体）。
//!
//! 直连时用到 APNs 的四个接口：建频道、删频道（`api-manage-broadcast` 上的 `/1/apps/<bundle>/channels`），
//! 用 push-to-start token 起 Live Activity（`/3/device/<token>`），往频道里广播
//! （`/4/broadcasts/apps/<bundle>`）。开发环境的主机名多一段 `.sandbox`。

use std::{
    path::Path,
    time::{Duration, Instant},
};

use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use runode_protocol::{
    push::{
        ActivityAttributes, ActivityContent, ActivityEvent, ApnsEnv, PushError, RELAY_BROADCAST, RELAY_CREATE_CHANNEL,
        RELAY_DELETE_CHANNEL, RELAY_START, RELAY_VERSION, RelayBroadcast, RelayChannel, RelayCreateChannel,
        RelayDeleteChannel, RelayStart, end_payload, start_payload, update_payload,
    },
    remote::{decode_base64url, encode_base64url},
};
use serde::{Deserialize, Serialize};

use super::curl::{Request, Response};

/// 签好的 JWT 用这么久就换新的（APNs 要求 20 分钟到 1 小时之间换）。
const JWT_LIFETIME: Duration = Duration::from_secs(40 * 60);

/// 推送走哪条路：APNs 环境，直连还是经中转。一个会话在每条路上各建一个频道。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct Route {
    pub(crate) env: ApnsEnv,
    pub(crate) via: Via,
}

impl Route {
    /// 同一种发法、另一个 APNs 环境。
    pub(crate) fn other_env(self) -> Self {
        let env = if self.env == ApnsEnv::Production { ApnsEnv::Development } else { ApnsEnv::Production };
        Self { env, ..self }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Via {
    /// 用配置里的密钥直连 APNs。
    Direct,
    /// 经中转服务。
    Relay,
}

/// 直连 APNs 用的密钥：从 .p8 文件读出来的 P-256 私钥，和签 JWT 要的 key id、team id，以及 App 的
/// bundle id。签好的 JWT 缓存 `JWT_LIFETIME`。
pub(crate) struct DirectKey {
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    pub(crate) bundle: String,
    jwt: Option<(String, Instant)>,
}

impl DirectKey {
    /// 读 .p8 文件（PEM 包着的 PKCS#8）。
    pub(crate) fn load(key_file: &Path, key_id: &str, team_id: &str, bundle: &str) -> Result<Self, String> {
        let pem = std::fs::read_to_string(key_file)
            .map_err(|err| format!("cannot read the APNs key {}: {err}", key_file.display()))?;
        let der = pem_body(&pem).ok_or_else(|| format!("{} is not a PEM file", key_file.display()))?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &SystemRandom::new())
            .map_err(|err| format!("{} is not a P-256 private key: {err}", key_file.display()))?;
        Ok(Self { key, key_id: key_id.to_owned(), team_id: team_id.to_owned(), bundle: bundle.to_owned(), jwt: None })
    }

    /// 签好的 JWT（ES256），缓存着的过了 `JWT_LIFETIME` 才重签。
    fn jwt(&mut self) -> Result<String, String> {
        if let Some((jwt, at)) = &self.jwt
            && at.elapsed() < JWT_LIFETIME
        {
            return Ok(jwt.clone());
        }
        let header = serde_json::json!({ "alg": "ES256", "kid": self.key_id });
        let claims = serde_json::json!({ "iss": self.team_id, "iat": crate::now_unix() });
        let signing_input = format!(
            "{}.{}",
            encode_base64url(header.to_string().as_bytes()),
            encode_base64url(claims.to_string().as_bytes())
        );
        let signature = self
            .key
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| "cannot sign the APNs token".to_owned())?;
        let jwt = format!("{signing_input}.{}", encode_base64url(signature.as_ref()));
        self.jwt = Some((jwt.clone(), Instant::now()));
        Ok(jwt)
    }

    /// 下次要 JWT 时重签：APNs 说它过期了。
    pub(crate) fn forget_jwt(&mut self) {
        self.jwt = None;
    }
}

/// PEM 里 `-----` 行之间的 base64（标准字母表，带填充）解出来的字节。
fn pem_body(pem: &str) -> Option<Vec<u8>> {
    let text: String = pem.lines().map(str::trim).filter(|line| !line.starts_with("-----")).collect();
    // 换成 base64url 的字母表、去掉填充，借 `decode_base64url` 解。
    let url: String = text
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    decode_base64url(&url).ok().filter(|der| !der.is_empty())
}

/// 要办的一件事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Call<'a> {
    CreateChannel,
    DeleteChannel { channel: &'a str },
    Start { token: &'a str, channel: &'a str, attributes: &'a ActivityAttributes, content: &'a ActivityContent },
    Broadcast { channel: &'a str, event: ActivityEvent, content: &'a ActivityContent },
}

/// 拼出 `call` 在路线 `route` 上的请求。直连时要 `direct`；`relay` 是中转服务的基址。
pub(crate) fn request(
    route: Route,
    call: &Call<'_>,
    direct: Option<&mut DirectKey>,
    relay: &str,
    now: u64,
) -> Result<Request, String> {
    match route.via {
        Via::Direct => {
            let key = direct.ok_or_else(|| "no APNs key is configured".to_owned())?;
            direct_request(route.env, call, key, now)
        }
        Via::Relay => relay_request(route.env, call, relay, now),
    }
}

fn direct_request(env: ApnsEnv, call: &Call<'_>, key: &mut DirectKey, now: u64) -> Result<Request, String> {
    // 频道管理的端口：沙盒 2195，生产 2196。
    let (sandbox, port) = if env == ApnsEnv::Production { ("", 2196) } else { (".sandbox", 2195) };
    let manage = format!("https://api-manage-broadcast{sandbox}.push.apple.com:{port}/1/apps/{}/channels", key.bundle);
    let mut headers = vec![("authorization", format!("bearer {}", key.jwt()?))];
    let too_big = || "the push does not fit in an APNs payload".to_owned();
    let (method, url, body) = match *call {
        Call::CreateChannel => {
            ("POST", manage, Some(br#"{"message-storage-policy":0,"push-type":"LiveActivity"}"#.to_vec()))
        }
        Call::DeleteChannel { channel } => {
            headers.push(("apns-channel-id", channel.to_owned()));
            ("DELETE", manage, None)
        }
        Call::Start { token, channel, attributes, content } => {
            headers.extend([
                ("apns-topic", format!("{}.push-type.liveactivity", key.bundle)),
                ("apns-push-type", "liveactivity".to_owned()),
                ("apns-priority", "10".to_owned()),
            ]);
            let body = start_payload(attributes, content, channel, now).ok_or_else(too_big)?;
            ("POST", format!("https://api{sandbox}.push.apple.com/3/device/{token}"), Some(body))
        }
        Call::Broadcast { channel, event, content } => {
            headers.extend([
                ("apns-channel-id", channel.to_owned()),
                ("apns-push-type", "LiveActivity".to_owned()),
                ("apns-priority", "10".to_owned()),
            ]);
            let body = match event {
                ActivityEvent::End => end_payload(content, now),
                _ => update_payload(content, now),
            };
            ("POST", format!("https://api{sandbox}.push.apple.com/4/broadcasts/apps/{}", key.bundle), body)
        }
    };
    if method == "POST" && body.is_none() {
        return Err(too_big());
    }
    Ok(Request { method, url, headers, body })
}

fn relay_request(env: ApnsEnv, call: &Call<'_>, relay: &str, now: u64) -> Result<Request, String> {
    let relay = relay.trim_end_matches('/');
    let too_big = || "the push does not fit in an APNs payload".to_owned();
    let (path, body) = match *call {
        Call::CreateChannel => (RELAY_CREATE_CHANNEL, json(&RelayCreateChannel { v: RELAY_VERSION, env })),
        Call::DeleteChannel { channel } => {
            (RELAY_DELETE_CHANNEL, json(&RelayDeleteChannel { v: RELAY_VERSION, env, channel: channel.to_owned() }))
        }
        Call::Start { token, channel, attributes, content } => {
            let start = RelayStart::new(env, token.to_owned(), channel.to_owned(), attributes.clone(), content, now)
                .ok_or_else(too_big)?;
            (RELAY_START, json(&start))
        }
        Call::Broadcast { channel, event, content } => {
            let broadcast = match event {
                ActivityEvent::End => RelayBroadcast::end(env, channel.to_owned(), content, now),
                _ => RelayBroadcast::update(env, channel.to_owned(), content, now),
            }
            .ok_or_else(too_big)?;
            (RELAY_BROADCAST, json(&broadcast))
        }
    };
    Ok(Request { method: "POST", url: format!("{relay}{path}"), headers: Vec::new(), body: Some(body) })
}

fn json(value: &impl Serialize) -> Vec<u8> {
    // 只有结构体、字符串和数，编不出来的情况实际不会有。
    serde_json::to_vec(value).unwrap_or_default()
}

/// 一个请求的结果怎么办。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// 办成了；建频道时带着频道的 id。
    Done(Option<String>),
    /// 网络出错、APNs 或中转服务忙不过来（5xx、429）：过一会儿再试，`after` 是对面要求至少等多久。
    Retry { after: Option<Duration>, why: String },
    /// push-to-start token 不认（410 或 400 `BadDeviceToken`），可能是登记的环境不对。
    BadToken(String),
    /// 直连用的密钥不对（403 `InvalidProviderToken`）。
    BadKey(String),
    /// JWT 过期（403 `ExpiredProviderToken`）：重签了再试。
    ExpiredJwt,
    /// 别的错误，不再试。
    Failed(String),
}

/// 看 `call` 在 `via` 上的响应（`Err` 是没拿到响应）该怎么办。
pub(crate) fn outcome(via: Via, call: &Call<'_>, response: Result<Response, String>) -> Outcome {
    let response = match response {
        Ok(response) => response,
        Err(why) => return Outcome::Retry { after: None, why },
    };
    let status = response.status;
    let reason = serde_json::from_slice::<PushError>(&response.body).map(|error| error.reason).unwrap_or_default();
    let why = if reason.is_empty() { format!("HTTP {status}") } else { format!("HTTP {status} {reason}") };
    match status {
        200..=299 => {
            if *call != Call::CreateChannel {
                return Outcome::Done(None);
            }
            let channel = match via {
                Via::Direct => response.header("apns-channel-id").map(str::to_owned),
                Via::Relay => serde_json::from_slice::<RelayChannel>(&response.body).ok().map(|body| body.channel),
            };
            match channel.filter(|channel| !channel.is_empty()) {
                Some(channel) => Outcome::Done(Some(channel)),
                None => Outcome::Failed(format!("{why} without a channel id")),
            }
        }
        429 => {
            let after = response.header("retry-after").and_then(|after| after.parse().ok()).map(Duration::from_secs);
            Outcome::Retry { after, why }
        }
        500..=599 => Outcome::Retry { after: None, why },
        410 if matches!(call, Call::Start { .. }) => Outcome::BadToken(why),
        400 if reason == "BadDeviceToken" && matches!(call, Call::Start { .. }) => Outcome::BadToken(why),
        403 if via == Via::Direct && reason == "InvalidProviderToken" => Outcome::BadKey(why),
        403 if via == Via::Direct && reason == "ExpiredProviderToken" => Outcome::ExpiredJwt,
        _ => Outcome::Failed(why),
    }
}

/// 测试用：现生成一把 P-256 私钥，写成 Apple 给的 .p8 那样的 PEM，返回文件和公钥。
#[cfg(test)]
pub(super) fn test_key(dir: &Path) -> (std::path::PathBuf, Vec<u8>) {
    use ring::signature::KeyPair as _;

    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let base64: String = encode_base64url(pkcs8.as_ref())
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    let padding = "=".repeat((4 - base64.len() % 4) % 4);
    let body = format!("{base64}{padding}");
    let lines: Vec<&str> = body.as_bytes().chunks(64).map(|line| std::str::from_utf8(line).unwrap()).collect();
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n"));
    let path = dir.join("AuthKey_TEST.p8");
    std::fs::write(&path, pem).unwrap();
    (path, key.public_key().as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};
    use runode_protocol::SessionId;

    use super::*;

    const CHANNEL: &str = "mLiGCQf1Ee+wAAAAAAAAAA==";

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rra-apns-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// JWT 是 ES256 签的 `header.claims`，签名 64 字节，用公钥验得过；缓存着的原样再给。
    #[test]
    fn jwts_are_signed_with_the_key() {
        let dir = temp_dir("jwt");
        let (path, public) = test_key(&dir);
        let mut key = DirectKey::load(&path, "KEYID12345", "TEAM123456", "dev.runode.mobile").unwrap();
        let jwt = key.jwt().unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value = serde_json::from_slice(&decode_base64url(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({ "alg": "ES256", "kid": "KEYID12345" }));
        let claims: serde_json::Value = serde_json::from_slice(&decode_base64url(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAM123456");
        assert!(claims["iat"].as_u64().unwrap() > 1_700_000_000);
        let signature = decode_base64url(parts[2]).unwrap();
        assert_eq!(signature.len(), 64);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &public)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
        assert_eq!(key.jwt().unwrap(), jwt);
        key.forget_jwt();
        assert!(key.jwt().is_ok());

        std::fs::write(&path, "not a key").unwrap();
        assert!(DirectKey::load(&path, "k", "t", "b").is_err());
        assert!(DirectKey::load(&dir.join("missing.p8"), "k", "t", "b").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn attributes() -> ActivityAttributes {
        ActivityAttributes::new("M".into(), "Mac", SessionId(1))
    }

    fn content() -> ActivityContent {
        ActivityContent { title: "t".into(), agent: "Claude Code".into(), lines: vec!["Proceed?".into()] }
    }

    #[test]
    fn direct_requests_follow_apns() {
        let dir = temp_dir("direct");
        let (path, _) = test_key(&dir);
        let mut key = DirectKey::load(&path, "K", "T", "dev.runode.mobile").unwrap();
        let dev = Route { env: ApnsEnv::Development, via: Via::Direct };
        let prod = Route { env: ApnsEnv::Production, via: Via::Direct };

        let create = request(dev, &Call::CreateChannel, Some(&mut key), "", 5).unwrap();
        assert_eq!(create.method, "POST");
        assert_eq!(
            create.url,
            "https://api-manage-broadcast.sandbox.push.apple.com:2195/1/apps/dev.runode.mobile/channels"
        );
        assert!(create.headers[0].1.starts_with("bearer "));
        assert_eq!(
            create.body.as_deref(),
            Some(br#"{"message-storage-policy":0,"push-type":"LiveActivity"}"#.as_slice())
        );

        let delete = request(prod, &Call::DeleteChannel { channel: CHANNEL }, Some(&mut key), "", 5).unwrap();
        assert_eq!(delete.method, "DELETE");
        assert_eq!(delete.url, "https://api-manage-broadcast.push.apple.com:2196/1/apps/dev.runode.mobile/channels");
        assert!(delete.headers.contains(&("apns-channel-id", CHANNEL.into())));
        assert_eq!(delete.body, None);

        let (attributes, content) = (attributes(), content());
        let start = Call::Start { token: "00ff", channel: CHANNEL, attributes: &attributes, content: &content };
        let start = request(prod, &start, Some(&mut key), "", 5).unwrap();
        assert_eq!(start.url, "https://api.push.apple.com/3/device/00ff");
        for header in [
            ("apns-topic", "dev.runode.mobile.push-type.liveactivity".to_owned()),
            ("apns-push-type", "liveactivity".into()),
            ("apns-priority", "10".into()),
        ] {
            assert!(start.headers.contains(&header), "{header:?}");
        }
        assert_eq!(start.body, start_payload(&attributes, &content, CHANNEL, 5));

        let end = Call::Broadcast { channel: CHANNEL, event: ActivityEvent::End, content: &content };
        let end = request(dev, &end, Some(&mut key), "", 7).unwrap();
        assert_eq!(end.url, "https://api.sandbox.push.apple.com/4/broadcasts/apps/dev.runode.mobile");
        assert!(end.headers.contains(&("apns-push-type", "LiveActivity".into())));
        assert!(end.headers.contains(&("apns-channel-id", CHANNEL.into())));
        assert_eq!(end.body, end_payload(&content, 7));

        assert!(request(dev, &Call::CreateChannel, None, "", 5).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relay_requests_use_the_relay_bodies() {
        let route = Route { env: ApnsEnv::Production, via: Via::Relay };
        let relay = "https://relay.example/";
        let create = request(route, &Call::CreateChannel, None, relay, 5).unwrap();
        assert_eq!((create.method, create.url.as_str()), ("POST", "https://relay.example/v1/channels"));
        assert_eq!(create.body.as_deref(), Some(br#"{"v":1,"env":"production"}"#.as_slice()));
        assert!(create.headers.is_empty());

        let delete = request(route, &Call::DeleteChannel { channel: "c" }, None, relay, 5).unwrap();
        assert_eq!(delete.url, "https://relay.example/v1/channels/delete");

        let content = content();
        let update = Call::Broadcast { channel: "c", event: ActivityEvent::Update, content: &content };
        let update = request(route, &update, None, relay, 5).unwrap();
        assert_eq!(update.url, "https://relay.example/v1/broadcast");
        let body: RelayBroadcast = serde_json::from_slice(update.body.as_deref().unwrap()).unwrap();
        assert_eq!(body, RelayBroadcast::update(ApnsEnv::Production, "c".into(), &content, 5).unwrap());
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Result<Response, String> {
        Ok(Response {
            status,
            headers: headers.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            body: body.as_bytes().to_vec(),
        })
    }

    #[test]
    fn responses_decide_what_happens_next() {
        let (attributes, content) = (attributes(), content());
        let start = Call::Start { token: "00", channel: "c", attributes: &attributes, content: &content };
        let create = Call::CreateChannel;
        assert_eq!(
            outcome(Via::Direct, &create, response(201, &[("apns-channel-id", "abc=")], "")),
            Outcome::Done(Some("abc=".into()))
        );
        assert_eq!(
            outcome(Via::Relay, &create, response(200, &[], r#"{"channel":"x"}"#)),
            Outcome::Done(Some("x".into()))
        );
        assert!(matches!(outcome(Via::Direct, &create, response(201, &[], "")), Outcome::Failed(_)));
        assert_eq!(outcome(Via::Direct, &start, response(200, &[], "")), Outcome::Done(None));
        assert!(matches!(outcome(Via::Relay, &start, Err("timeout".into())), Outcome::Retry { after: None, .. }));
        assert!(matches!(outcome(Via::Relay, &start, response(503, &[], "")), Outcome::Retry { after: None, .. }));
        assert_eq!(
            outcome(Via::Relay, &start, response(429, &[("retry-after", "7")], r#"{"reason":"TooManyRequests"}"#)),
            Outcome::Retry { after: Some(Duration::from_secs(7)), why: "HTTP 429 TooManyRequests".into() }
        );
        assert!(matches!(
            outcome(Via::Relay, &start, response(410, &[], r#"{"reason":"Unregistered"}"#)),
            Outcome::BadToken(_)
        ));
        assert!(matches!(
            outcome(Via::Direct, &start, response(400, &[], r#"{"reason":"BadDeviceToken"}"#)),
            Outcome::BadToken(_)
        ));
        assert!(matches!(
            outcome(Via::Direct, &create, response(400, &[], r#"{"reason":"BadDeviceToken"}"#)),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            outcome(Via::Direct, &start, response(403, &[], r#"{"reason":"InvalidProviderToken"}"#)),
            Outcome::BadKey(_)
        ));
        // 中转服务自己的密钥不对不是这边能改的。
        assert!(matches!(
            outcome(Via::Relay, &start, response(403, &[], r#"{"reason":"InvalidProviderToken"}"#)),
            Outcome::Failed(_)
        ));
        assert_eq!(
            outcome(Via::Direct, &create, response(403, &[], r#"{"reason":"ExpiredProviderToken"}"#)),
            Outcome::ExpiredJwt
        );
        assert!(matches!(
            outcome(Via::Direct, &start, response(400, &[], r#"{"reason":"BadTopic"}"#)),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn routes_switch_environments() {
        let route = Route { env: ApnsEnv::Production, via: Via::Relay };
        assert_eq!(route.other_env(), Route { env: ApnsEnv::Development, via: Via::Relay });
        assert_eq!(route.other_env().other_env(), route);
    }
}
