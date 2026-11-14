//! 发给 FCM：用 Firebase 项目的服务账号签一个 JWT（RS256），向 Google 换 OAuth 的 access token，再拿它
//! `POST` 到 HTTP v1 的 `messages:send`。请求体由 `runode_protocol::push::fcm_message` 拼好交进来；FCM 的
//! 回话换成电脑认得的 APNs 状态（`reply`）。

use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use runode_protocol::{push::pem_der, remote::encode_base64url};
use serde_json::Value;

use crate::apns::{Reply, now_unix};

/// 服务账号要的权限：只能发 FCM 消息。
pub const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
/// 换 access token 时的 `grant_type`，已经按表单的规矩转义过。
const GRANT_TYPE: &str = "urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer";
/// 签给 Google 的 JWT 的有效期，Google 收的最长就是一小时。
const JWT_LIFETIME: u64 = 60 * 60;
/// access token 离过期还有这么久就换新的，不拿快过期的去发。
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
/// 一次请求最多等这么久。
const TIMEOUT: Duration = Duration::from_secs(5);

/// 发 FCM 消息的一方，测试换成假的。`message` 是 `fcm_message` 拼的请求体；回话已经换成电脑认得的
/// 样子（`reply`），`Err` 是没拿到回话。
pub trait Fcm: Send + Sync + 'static {
    fn send(&self, message: &[u8]) -> impl Future<Output = Result<Reply, String>> + Send;
}

impl<T: Fcm> Fcm for Arc<T> {
    fn send(&self, message: &[u8]) -> impl Future<Output = Result<Reply, String>> + Send {
        T::send(self, message)
    }
}

/// 真的 FCM。
pub struct Client {
    http: reqwest::Client,
    key: RsaKeyPair,
    client_email: String,
    token_uri: String,
    send_url: String,
    /// 换来的 access token 和该换新的时刻。
    access: Mutex<Option<(String, Instant)>>,
}

impl Client {
    /// 读 Firebase 控制台下载的服务账号 JSON：`client_email`、`private_key`（PEM 包着的 PKCS#8 RSA 私钥）、
    /// `project_id`、`token_uri`。
    pub fn new(account_file: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(account_file)
            .map_err(|err| format!("cannot read the FCM service account {}: {err}", account_file.display()))?;
        let account: Value = serde_json::from_str(&text)
            .map_err(|err| format!("{} is not a service account JSON: {err}", account_file.display()))?;
        let field = |name: &str| {
            account[name]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{} has no `{name}`", account_file.display()))
        };
        let der = pem_der(&field("private_key")?)
            .ok_or_else(|| format!("the private key in {} is not PEM", account_file.display()))?;
        let key = RsaKeyPair::from_pkcs8(&der)
            .map_err(|err| format!("the private key in {} is not an RSA key: {err}", account_file.display()))?;
        // reqwest 用进程默认的 TLS 后端，装过了就不再装。
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| format!("cannot set up the HTTP client: {err}"))?;
        Ok(Self {
            http,
            key,
            client_email: field("client_email")?,
            token_uri: field("token_uri")?,
            send_url: send_url(&field("project_id")?),
            access: Mutex::new(None),
        })
    }

    /// 签好的 JWT，拿去换 access token。
    fn assertion(&self) -> Result<String, String> {
        assertion(&self.client_email, &self.token_uri, now_unix(), |input| {
            let mut signature = vec![0; self.key.public().modulus_len()];
            self.key
                .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), input, &mut signature)
                .map_err(|_| "cannot sign the FCM token request".to_owned())?;
            Ok(signature)
        })
    }

    /// access token：缓存着的没到该换的时候、也不是 `rejected` 时原样给，不然向 Google 换一个新的。
    async fn access_token(&self, rejected: Option<&str>) -> Result<String, String> {
        {
            let cached = self.access.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((token, renew_at)) = cached.as_ref()
                && rejected != Some(token.as_str())
                && Instant::now() < *renew_at
            {
                return Ok(token.clone());
            }
        }
        let body = format!("grant_type={GRANT_TYPE}&assertion={}", self.assertion()?);
        let response = self
            .http
            .post(&self.token_uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|err| format!("cannot reach Google for an FCM access token: {}", err.without_url()))?;
        let status = response.status().as_u16();
        let body: Value = serde_json::from_slice(&response.bytes().await.unwrap_or_default()).unwrap_or_default();
        let (Some(token), Some(lifetime)) = (body["access_token"].as_str(), body["expires_in"].as_u64()) else {
            let why = body["error"].as_str().unwrap_or_default();
            return Err(format!("Google gave no FCM access token: HTTP {status} {why}"));
        };
        let renew_at = Instant::now() + Duration::from_secs(lifetime).saturating_sub(RENEW_BEFORE);
        *self.access.lock().unwrap_or_else(PoisonError::into_inner) = Some((token.to_owned(), renew_at));
        Ok(token.to_owned())
    }

    async fn post(&self, message: &[u8], access_token: &str) -> Result<Reply, String> {
        let response = self
            .http
            .post(&self.send_url)
            .header("authorization", format!("Bearer {access_token}"))
            .header("content-type", "application/json")
            .body(message.to_vec())
            .send()
            .await
            .map_err(|err| format!("cannot reach FCM: {}", err.without_url()))?;
        let status = response.status().as_u16();
        let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let body = response.bytes().await.unwrap_or_default();
        Ok(reply(status, &body, retry_after))
    }
}

impl Fcm for Client {
    /// access token 不认（401）时换一个再发一次。
    async fn send(&self, message: &[u8]) -> Result<Reply, String> {
        let access_token = self.access_token(None).await?;
        let reply = self.post(message, &access_token).await?;
        if reply.status == 401 {
            return self.post(message, &self.access_token(Some(&access_token)).await?).await;
        }
        Ok(reply)
    }
}

/// Firebase 项目 `project` 发消息的网址。
fn send_url(project: &str) -> String {
    format!("https://fcm.googleapis.com/v1/projects/{project}/messages:send")
}

/// 向 `token_uri` 换 access token 用的 JWT（RS256）：`header.claims` 交给 `sign` 签（RSASSA-PKCS1-v1_5、
/// SHA-256），再拼上签名。`iat` 是签的时刻，Unix 秒。
fn assertion<E>(
    client_email: &str,
    token_uri: &str,
    iat: u64,
    sign: impl FnOnce(&[u8]) -> Result<Vec<u8>, E>,
) -> Result<String, E> {
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": client_email,
        "scope": SCOPE,
        "aud": token_uri,
        "iat": iat,
        "exp": iat + JWT_LIFETIME,
    });
    let input = format!(
        "{}.{}",
        encode_base64url(header.to_string().as_bytes()),
        encode_base64url(claims.to_string().as_bytes())
    );
    let signature = sign(input.as_bytes())?;
    Ok(format!("{input}.{}", encode_base64url(&signature)))
}

/// FCM 的回话（HTTP 状态、响应体和 `Retry-After`）换成电脑认得的 APNs 状态：token 作废了
/// （`UNREGISTERED`）是 410 `Unregistered`，token 不对（针对 token 的 `INVALID_ARGUMENT`、
/// `SENDER_ID_MISMATCH`）是 400 `BadDeviceToken`，电脑见了都删掉登记；限额（`QUOTA_EXCEEDED`）是 429
/// `TooManyRequests`，带上 FCM 给的 `Retry-After`。别的照 FCM 的状态带回，原因是它的错误码。
pub fn reply(status: u16, body: &[u8], retry_after: Option<String>) -> Reply {
    let answer = |status, reason: &str, retry_after| Reply { status, reason: reason.to_owned(), retry_after };
    if (200..300).contains(&status) {
        return answer(200, "", None);
    }
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let error = &body["error"];
    let details = error["details"].as_array().map(Vec::as_slice).unwrap_or_default();
    let code = details_of(details, "google.firebase.fcm.v1.FcmError")
        .find_map(|detail| detail["errorCode"].as_str())
        .or_else(|| error["status"].as_str())
        .unwrap_or_default();
    let about_token = || {
        let field = details_of(details, "google.rpc.BadRequest")
            .flat_map(|detail| detail["fieldViolations"].as_array().map(Vec::as_slice).unwrap_or_default())
            .any(|violation| violation["field"] == "message.token");
        let message = error["message"].as_str().unwrap_or_default().to_ascii_lowercase();
        field || message.contains("registration token")
    };
    match code {
        "UNREGISTERED" => answer(410, "Unregistered", None),
        "SENDER_ID_MISMATCH" => answer(400, "BadDeviceToken", None),
        "INVALID_ARGUMENT" if about_token() => answer(400, "BadDeviceToken", None),
        "QUOTA_EXCEEDED" => answer(429, "TooManyRequests", retry_after),
        code => answer(status, code, retry_after),
    }
}

/// 错误详情里 `@type` 是 `type_name` 的那几项。
fn details_of<'a>(details: &'a [Value], type_name: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    details.iter().filter(move |detail| {
        detail["@type"].as_str().is_some_and(|t| t.strip_prefix("type.googleapis.com/") == Some(type_name))
    })
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
    use runode_protocol::remote::decode_base64url;

    use super::*;

    fn fcm_error(status: u16, code: &str, message: &str) -> Vec<u8> {
        let body = serde_json::json!({
            "error": {
                "code": status,
                "message": message,
                "status": if status == 404 { "NOT_FOUND" } else { "INVALID_ARGUMENT" },
                "details": [{ "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", "errorCode": code }],
            }
        });
        body.to_string().into_bytes()
    }

    fn status_and_reason(reply: &Reply) -> (u16, &str) {
        (reply.status, reply.reason.as_str())
    }

    /// 电脑据此删登记、过会儿再试还是放弃。
    #[test]
    fn replies_map_to_what_the_computer_knows() {
        assert_eq!(
            reply(200, br#"{"name":"projects/p/messages/1"}"#, None),
            Reply { status: 200, reason: String::new(), retry_after: None }
        );
        let unregistered = reply(404, &fcm_error(404, "UNREGISTERED", "Requested entity was not found."), None);
        assert_eq!(status_and_reason(&unregistered), (410, "Unregistered"));
        assert!(unregistered.bad_token());
        let mismatch = reply(403, &fcm_error(403, "SENDER_ID_MISMATCH", "SenderId mismatch"), None);
        assert_eq!(status_and_reason(&mismatch), (400, "BadDeviceToken"));
        let bad_token = reply(
            400,
            &fcm_error(400, "INVALID_ARGUMENT", "The registration token is not a valid FCM registration token"),
            None,
        );
        assert_eq!(status_and_reason(&bad_token), (400, "BadDeviceToken"));
        let field = serde_json::json!({ "error": { "code": 400, "status": "INVALID_ARGUMENT", "message": "Invalid value",
            "details": [{ "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [{ "field": "message.token", "description": "Invalid registration token" }] }] } });
        assert_eq!(status_and_reason(&reply(400, field.to_string().as_bytes(), None)), (400, "BadDeviceToken"));
        // 不是 token 的毛病不删登记。
        let other = reply(400, &fcm_error(400, "INVALID_ARGUMENT", "Invalid value at 'message.android.ttl'"), None);
        assert_eq!(status_and_reason(&other), (400, "INVALID_ARGUMENT"));
        assert!(!other.bad_token());

        let quota = reply(429, &fcm_error(429, "QUOTA_EXCEEDED", "quota"), Some("60".into()));
        assert_eq!(
            (quota.status, quota.reason.as_str(), quota.retry_after.as_deref()),
            (429, "TooManyRequests", Some("60"))
        );
        let busy = reply(503, &fcm_error(503, "UNAVAILABLE", "busy"), Some("10".into()));
        assert_eq!((busy.status, busy.reason.as_str(), busy.retry_after.as_deref()), (503, "UNAVAILABLE", Some("10")));
        assert_eq!(status_and_reason(&reply(502, b"<html>bad gateway</html>", None)), (502, ""));
        // 项目不对之类的 404 没有 `UNREGISTERED`，不删登记。
        let project = serde_json::json!({ "error": { "code": 404, "status": "NOT_FOUND", "message": "not found" } });
        assert_eq!(status_and_reason(&reply(404, project.to_string().as_bytes(), None)), (404, "NOT_FOUND"));
    }

    #[test]
    fn assertions_carry_the_scope_and_audience() {
        let jwt = assertion("relay@p.iam.gserviceaccount.com", "https://oauth2.googleapis.com/token", 1_000, |input| {
            Ok::<_, ()>(input.len().to_be_bytes().to_vec())
        })
        .unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let header: Value = serde_json::from_slice(&decode_base64url(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({ "alg": "RS256", "typ": "JWT" }));
        let claims: Value = serde_json::from_slice(&decode_base64url(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims,
            serde_json::json!({
                "iss": "relay@p.iam.gserviceaccount.com",
                "scope": SCOPE,
                "aud": "https://oauth2.googleapis.com/token",
                "iat": 1_000,
                "exp": 1_000 + JWT_LIFETIME,
            })
        );
        let signed = format!("{}.{}", parts[0], parts[1]);
        assert_eq!(decode_base64url(parts[2]).unwrap(), signed.len().to_be_bytes());
        assert_eq!(send_url("runode-1234"), "https://fcm.googleapis.com/v1/projects/runode-1234/messages:send");
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("runode-push-fcm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// ring 生成不了 RSA 私钥，借系统的 openssl 现生成一把（PKCS#8 的 PEM，和服务账号里的一样）。
    fn rsa_key_pem(dir: &Path) -> String {
        let path = dir.join("key.pem");
        let status = std::process::Command::new("openssl")
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out"])
            .arg(&path)
            .stderr(std::process::Stdio::null())
            .status()
            .expect("the FCM signing test needs openssl");
        assert!(status.success());
        std::fs::read_to_string(&path).unwrap()
    }

    /// 服务账号读得进来，签出的 JWT 用它的公钥验得过；缺项、不是 RSA 私钥时读不进来。
    #[test]
    fn service_accounts_load_and_sign() {
        let dir = temp_dir("account");
        let pem = rsa_key_pem(&dir);
        let account = serde_json::json!({
            "type": "service_account",
            "project_id": "runode-1234",
            "private_key": pem,
            "client_email": "relay@runode-1234.iam.gserviceaccount.com",
            "token_uri": "https://oauth2.googleapis.com/token",
        });
        let path = dir.join("account.json");
        std::fs::write(&path, account.to_string()).unwrap();
        let client = Client::new(&path).unwrap();
        assert_eq!(client.send_url, send_url("runode-1234"));
        let jwt = client.assertion().unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let public = client.key.public().as_ref().to_vec();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &decode_base64url(parts[2]).unwrap())
            .unwrap();

        let mut missing = account.clone();
        missing["token_uri"] = Value::Null;
        std::fs::write(&path, missing.to_string()).unwrap();
        assert!(Client::new(&path).err().unwrap().contains("token_uri"));
        let mut not_rsa = account;
        not_rsa["private_key"] = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".into();
        std::fs::write(&path, not_rsa.to_string()).unwrap();
        assert!(Client::new(&path).is_err());
        assert!(Client::new(&dir.join("missing.json")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
