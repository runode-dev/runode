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
use runode_protocol::push::{
    FcmServiceAccount, fcm_access_token, fcm_apns_status, fcm_assertion, fcm_token_request, is_bad_token, pem_der,
};

use crate::apns::{Reply, now_unix};

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
    /// 读 Firebase 控制台下载的服务账号 JSON 文件。
    pub fn new(account_file: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(account_file)
            .map_err(|err| format!("cannot read the FCM service account {}: {err}", account_file.display()))?;
        let account = FcmServiceAccount::parse(&text).map_err(|err| format!("{} {err}", account_file.display()))?;
        let der = pem_der(&account.private_key)
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
            send_url: account.send_url(),
            client_email: account.client_email,
            token_uri: account.token_uri,
            access: Mutex::new(None),
        })
    }

    /// 签好的 JWT，拿去换 access token。
    fn assertion(&self) -> Result<String, String> {
        fcm_assertion(&self.client_email, &self.token_uri, now_unix(), |input| {
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
        let body = fcm_token_request(&self.assertion()?);
        let response = self
            .http
            .post(&self.token_uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|err| format!("cannot reach Google for an FCM access token: {}", err.without_url()))?;
        let status = response.status().as_u16();
        let (token, lifetime) = fcm_access_token(&response.bytes().await.unwrap_or_default())
            .map_err(|why| format!("Google gave no FCM access token: HTTP {status} {why}"))?;
        let renew_at = Instant::now() + Duration::from_secs(lifetime).saturating_sub(RENEW_BEFORE);
        *self.access.lock().unwrap_or_else(PoisonError::into_inner) = Some((token.clone(), renew_at));
        Ok(token)
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

/// FCM 的回话（HTTP 状态、响应体和 `Retry-After`）换成电脑认得的 APNs 回话（`fcm_apns_status`）。成功的
/// 和电脑见了会删掉登记的不带 `Retry-After`，别的带上 FCM 给的。
pub fn reply(status: u16, body: &[u8], retry_after: Option<String>) -> Reply {
    let (status, reason) = fcm_apns_status(status, body);
    let retry_after = if status == 200 || is_bad_token(status, &reason) { None } else { retry_after };
    Reply { status, reason, retry_after }
}

#[cfg(test)]
mod tests {
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
    use runode_protocol::remote::decode_base64url;
    use serde_json::Value;

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
        assert_eq!(client.send_url, "https://fcm.googleapis.com/v1/projects/runode-1234/messages:send");
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
