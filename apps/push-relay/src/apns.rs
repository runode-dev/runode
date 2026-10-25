//! 发给 APNs：用 Apple 开发者后台下载的 .p8 密钥签 JWT，`POST /3/device/<token>` 走 HTTP/2，开发环境
//! 的主机名多一段 `.sandbox`。

use std::{
    future::Future,
    path::Path,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ring::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair},
};
use runode_protocol::{
    push::{ApnsEnv, OFFICIAL_BUNDLE, PushError},
    remote::{decode_base64url, encode_base64url},
};

use crate::handle::encode_hex;

/// 签好的 JWT 用这么久就换新的（APNs 要求 20 分钟到 1 小时之间换）。
const JWT_LIFETIME: Duration = Duration::from_secs(40 * 60);
/// 一次请求最多等这么久。
const TIMEOUT: Duration = Duration::from_secs(5);

/// 要发的一条推送。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Push {
    pub env: ApnsEnv,
    pub token: Vec<u8>,
    pub payload: Vec<u8>,
    /// `apns-expiration`，Unix 秒。
    pub expiration: u64,
    pub collapse_id: String,
}

/// APNs 的回话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    /// 响应体里的 `reason`；成功时为空。
    pub reason: String,
    pub retry_after: Option<String>,
}

impl Reply {
    /// APNs 不认这个 device token：可能是封进句柄的环境不对。
    pub fn bad_token(&self) -> bool {
        self.status == 410 || (self.status == 400 && self.reason == "BadDeviceToken")
    }
}

/// 发推送的一方，测试换成假的。`Err` 是没拿到回话。
pub trait Apns: Send + Sync + 'static {
    fn send(&self, push: &Push) -> impl Future<Output = Result<Reply, String>> + Send;
}

impl<T: Apns> Apns for std::sync::Arc<T> {
    fn send(&self, push: &Push) -> impl Future<Output = Result<Reply, String>> + Send {
        T::send(self, push)
    }
}

/// 真的 APNs。
pub struct Client {
    http: reqwest::Client,
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    jwt: Mutex<Option<(String, Instant)>>,
}

impl Client {
    /// 读 .p8 文件（PEM 包着的 PKCS#8 P-256 私钥）。
    pub fn new(key_file: &Path, key_id: &str, team_id: &str) -> Result<Self, String> {
        let pem = std::fs::read_to_string(key_file)
            .map_err(|err| format!("cannot read the APNs key {}: {err}", key_file.display()))?;
        let der = pem_body(&pem).ok_or_else(|| format!("{} is not a PEM file", key_file.display()))?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &SystemRandom::new())
            .map_err(|err| format!("{} is not a P-256 private key: {err}", key_file.display()))?;
        // reqwest 用进程默认的 TLS 后端，装过了就不再装。
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| format!("cannot set up the HTTP client: {err}"))?;
        Ok(Self { http, key, key_id: key_id.to_owned(), team_id: team_id.to_owned(), jwt: Mutex::new(None) })
    }

    /// 签好的 JWT（ES256），缓存着的过了 `JWT_LIFETIME` 或者正是 `expired` 时重签。同时被拒的几条只重签
    /// 一次：APNs 换 token 太勤会回 `TooManyProviderTokenUpdates`。
    fn jwt(&self, expired: Option<&str>) -> Result<String, String> {
        let mut cached = self.jwt.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((jwt, at)) = cached.as_ref()
            && expired != Some(jwt.as_str())
            && at.elapsed() < JWT_LIFETIME
        {
            return Ok(jwt.clone());
        }
        let header = serde_json::json!({ "alg": "ES256", "kid": self.key_id });
        let claims = serde_json::json!({ "iss": self.team_id, "iat": now_unix() });
        let input = format!(
            "{}.{}",
            encode_base64url(header.to_string().as_bytes()),
            encode_base64url(claims.to_string().as_bytes())
        );
        let signature =
            self.key.sign(&SystemRandom::new(), input.as_bytes()).map_err(|_| "cannot sign the APNs token")?;
        let jwt = format!("{input}.{}", encode_base64url(signature.as_ref()));
        *cached = Some((jwt.clone(), Instant::now()));
        Ok(jwt)
    }

    async fn post(&self, push: &Push, jwt: &str) -> Result<Reply, String> {
        let sandbox = if push.env == ApnsEnv::Production { "" } else { ".sandbox" };
        let url = format!("https://api{sandbox}.push.apple.com/3/device/{}", encode_hex(&push.token));
        let response = self
            .http
            .post(url)
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", OFFICIAL_BUNDLE)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", push.expiration.to_string())
            .header("apns-collapse-id", &push.collapse_id)
            .body(push.payload.clone())
            .send()
            .await
            .map_err(|err| format!("cannot reach APNs: {}", err.without_url()))?;
        let status = response.status().as_u16();
        let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let body = response.bytes().await.unwrap_or_default();
        let reason = serde_json::from_slice::<PushError>(&body).map(|error| error.reason).unwrap_or_default();
        Ok(Reply { status, reason, retry_after })
    }
}

impl Apns for Client {
    /// JWT 过期（403 `ExpiredProviderToken`）时重签再发一次。
    async fn send(&self, push: &Push) -> Result<Reply, String> {
        let jwt = self.jwt(None)?;
        let reply = self.post(push, &jwt).await?;
        if reply.status == 403 && reply.reason == "ExpiredProviderToken" {
            return self.post(push, &self.jwt(Some(&jwt))?).await;
        }
        Ok(reply)
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
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

#[cfg(test)]
mod tests {
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair as _, UnparsedPublicKey};

    use super::*;

    /// 现生成一把 P-256 私钥，写成 Apple 给的 .p8 那样的 PEM。
    fn key_file(dir: &Path) -> (std::path::PathBuf, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let base64: String = encode_base64url(pkcs8.as_ref())
            .chars()
            .map(|c| {
                if c == '-' {
                    '+'
                } else if c == '_' {
                    '/'
                } else {
                    c
                }
            })
            .collect();
        let padded = format!("{base64}{}", "=".repeat((4 - base64.len() % 4) % 4));
        let lines: Vec<&str> = padded.as_bytes().chunks(64).map(|line| std::str::from_utf8(line).unwrap()).collect();
        let path = dir.join("AuthKey_TEST.p8");
        std::fs::write(
            &path,
            format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n")),
        )
        .unwrap();
        (path, key.public_key().as_ref().to_vec())
    }

    /// JWT 是 ES256 签的 `header.claims`，公钥验得过；缓存着的原样再给，被拒的那个才重签，已经换过的不再换。
    #[test]
    fn jwts_are_signed_and_cached() {
        let dir = std::env::temp_dir().join(format!("runode-push-apns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, public) = key_file(&dir);
        let client = Client::new(&path, "KEYID12345", "TEAMID1234").unwrap();
        let jwt = client.jwt(None).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let header: serde_json::Value = serde_json::from_slice(&decode_base64url(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({ "alg": "ES256", "kid": "KEYID12345" }));
        let claims: serde_json::Value = serde_json::from_slice(&decode_base64url(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAMID1234");
        let signature = decode_base64url(parts[2]).unwrap();
        let input = format!("{}.{}", parts[0], parts[1]);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &public).verify(input.as_bytes(), &signature).unwrap();
        assert_eq!(client.jwt(None).unwrap(), jwt);
        let renewed = client.jwt(Some(&jwt)).unwrap();
        assert_ne!(renewed, jwt);
        assert_eq!(client.jwt(Some(&jwt)).unwrap(), renewed);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_tokens_are_410_and_bad_device_token() {
        let reply = |status, reason: &str| Reply { status, reason: reason.into(), retry_after: None };
        assert!(reply(410, "Unregistered").bad_token());
        assert!(reply(400, "BadDeviceToken").bad_token());
        assert!(!reply(400, "BadTopic").bad_token());
        assert!(!reply(200, "").bad_token());
    }
}
