//! 发给 FCM：用 Firebase 项目的服务账号签一个 JWT（RS256），向 Google 换 OAuth 的 access token，再拿它
//! `POST` 到 HTTP v1 的 `messages:send`。请求体由 `runode_protocol::push::fcm_message` 拼好交进来；FCM 的
//! 回话换成电脑认得的 APNs 状态（`reply`）。

use std::{
    future::Future,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use runode_protocol::push::{
    FcmServiceAccount, fcm_access_token, fcm_apns_status, fcm_assertion, fcm_token_request, is_bad_token, pem_der,
};
use tokio::time::Instant;

use crate::apns::{Reply, now_unix};

/// access token 离过期还有这么久就换新的，不拿快过期的去发。
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);
/// 一次请求最多等这么久。
const TIMEOUT: Duration = Duration::from_secs(5);
/// 换不到 access token 后头一回隔这么久才再换；接着还换不到就翻倍，最多到 `FETCH_BACKOFF_MAX`。上回的
/// 退避期过了 `FETCH_BACKOFF_MAX` 以上才又换不到时从头算。
const FETCH_BACKOFF: Duration = Duration::from_secs(2);
const FETCH_BACKOFF_MAX: Duration = Duration::from_secs(30);

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
    access: Arc<AccessCache>,
}

/// 换来的 access token 和它过期的时刻；同时只换一次，换不到时退避一阵再换。
#[derive(Default)]
struct AccessCache {
    token: Mutex<Option<(String, Instant)>>,
    /// 上回没换到；成功换到一个就清掉。
    failure: Mutex<Option<FetchFailure>>,
    /// 换 token 的拿着它直到结果记下，后到的等它换完再看缓存和 `failure`。
    refresh: Arc<tokio::sync::Mutex<()>>,
}

/// 换不到 access token 的那次：错在哪、到什么时候才再换、这回退避了多久。
struct FetchFailure {
    why: String,
    retry_at: Instant,
    backoff: Duration,
}

impl AccessCache {
    /// 缓存着的离过期还远于 `RENEW_BEFORE`、也不是 `rejected` 时原样给；不然在退避期里交回上回的错，过了
    /// 退避期用 `fetch()` 换一个新的记下。换不到（含在退避期里）时缓存着的还没过期就拿它兜底。`fetch()`
    /// 交回 token 和它过期的时刻，放在单独的任务里跑，调用方半路被丢掉也照样把结果记下。
    async fn get<F>(self: &Arc<Self>, rejected: Option<&str>, fetch: impl FnOnce() -> F) -> Result<String, String>
    where
        F: Future<Output = Result<(String, Instant), String>> + Send + 'static,
    {
        self.renewed(rejected, fetch).await.or_else(|why| self.unexpired().ok_or(why))
    }

    async fn renewed<F>(self: &Arc<Self>, rejected: Option<&str>, fetch: impl FnOnce() -> F) -> Result<String, String>
    where
        F: Future<Output = Result<(String, Instant), String>> + Send + 'static,
    {
        if let Some(token) = self.cached(rejected) {
            return Ok(token);
        }
        self.backing_off()?;
        let refresh = self.refresh.clone().lock_owned().await;
        if let Some(token) = self.cached(rejected) {
            return Ok(token);
        }
        self.backing_off()?;
        let (cache, fetch) = (self.clone(), fetch());
        tokio::spawn(async move {
            let _refresh = refresh;
            cache.record(fetch.await)
        })
        .await
        .unwrap_or_else(|err| Err(format!("fetching an FCM access token failed: {err}")))
    }

    fn record(&self, fetched: Result<(String, Instant), String>) -> Result<String, String> {
        match fetched {
            Ok((token, expires_at)) => {
                *self.token.lock().unwrap_or_else(PoisonError::into_inner) = Some((token.clone(), expires_at));
                *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
                Ok(token)
            }
            Err(why) => {
                let mut failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
                let now = Instant::now();
                let backoff = match failure.as_ref() {
                    Some(last) if now < last.retry_at + FETCH_BACKOFF_MAX => (last.backoff * 2).min(FETCH_BACKOFF_MAX),
                    _ => FETCH_BACKOFF,
                };
                *failure = Some(FetchFailure { why: why.clone(), retry_at: now + backoff, backoff });
                Err(why)
            }
        }
    }

    /// FCM 不认的 `rejected` 从缓存里丢掉，换不到新的时后来的请求也不再拿它去发。
    fn cached(&self, rejected: Option<&str>) -> Option<String> {
        let mut cached = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        let (token, expires_at) = cached.as_ref()?;
        if rejected == Some(token.as_str()) {
            *cached = None;
            return None;
        }
        (Instant::now() + RENEW_BEFORE < *expires_at).then(|| token.clone())
    }

    /// 缓存着的还没过期时给它。
    fn unexpired(&self) -> Option<String> {
        let cached = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        let (token, expires_at) = cached.as_ref()?;
        (Instant::now() < *expires_at).then(|| token.clone())
    }

    /// 还在上回没换到之后的退避期里时交回那次的错。
    fn backing_off(&self) -> Result<(), String> {
        let failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        match failure.as_ref() {
            Some(failure) if now < failure.retry_at => {
                Err(format!("{} (not retrying for another {:?})", failure.why, failure.retry_at - now))
            }
            _ => Ok(()),
        }
    }
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
            access: Arc::default(),
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

    /// access token：`rejected` 是 FCM 刚不认的那个。
    async fn access_token(&self, rejected: Option<&str>) -> Result<String, String> {
        self.access.get(rejected, || self.fetch_access_token()).await
    }

    /// 向 Google 换一个 access token，交回它和过期的时刻；过期时刻从发出请求时算起。JWT 当场签好，交回的
    /// future 不借着 `self`。
    fn fetch_access_token(&self) -> impl Future<Output = Result<(String, Instant), String>> + Send + 'static {
        let (assertion, http, token_uri) = (self.assertion(), self.http.clone(), self.token_uri.clone());
        async move {
            let body = fcm_token_request(&assertion?);
            let sent = Instant::now();
            let response = http
                .post(&token_uri)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .await
                .map_err(|err| format!("cannot reach Google for an FCM access token: {}", err.without_url()))?;
            let status = response.status().as_u16();
            let (token, lifetime) = fcm_access_token(&response.bytes().await.unwrap_or_default())
                .map_err(|why| format!("Google gave no FCM access token: HTTP {status} {why}"))?;
            let expires_at = sent
                .checked_add(Duration::from_secs(lifetime))
                .ok_or_else(|| format!("Google gave an FCM access token lasting {lifetime}s"))?;
            Ok((token, expires_at))
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// 起 `count` 个并发的 `get(rejected, …)`，每次换 token 记一次数、等一会儿才回，`fail` 时换不到；交回各自的结果。
    async fn gets(
        cache: &Arc<AccessCache>,
        fetches: &Arc<AtomicUsize>,
        rejected: Option<&'static str>,
        fail: bool,
        count: usize,
    ) -> Vec<Result<String, String>> {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..count {
            let (cache, fetches) = (cache.clone(), fetches.clone());
            tasks.spawn(async move { cache.get(rejected, || fetch(fetches, fail)).await });
        }
        tasks.join_all().await
    }

    /// 换一次 token：记一次数、等一会儿才回，`fail` 时换不到；换到的一小时后过期。
    async fn fetch(fetches: Arc<AtomicUsize>, fail: bool) -> Result<(String, Instant), String> {
        let n = fetches.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        if fail {
            return Err(format!("down{n}"));
        }
        Ok((format!("token{n}"), Instant::now() + Duration::from_secs(3600)))
    }

    /// `gets` 都换得到时：交回换了几次和各自拿到的 token。
    async fn concurrent_gets(cache: AccessCache, rejected: Option<&'static str>, count: usize) -> (usize, Vec<String>) {
        let (cache, fetches) = (Arc::new(cache), Arc::new(AtomicUsize::new(0)));
        let tokens = gets(&cache, &fetches, rejected, false, count).await.into_iter().map(Result::unwrap).collect();
        (fetches.load(Ordering::SeqCst), tokens)
    }

    /// 缓存里没有时同时来的只换一次，都拿到那一个。
    #[tokio::test(start_paused = true)]
    async fn concurrent_misses_fetch_once() {
        let (fetches, tokens) = concurrent_gets(AccessCache::default(), None, 8).await;
        assert_eq!(fetches, 1);
        assert!(tokens.iter().all(|token| token == "token0"), "{tokens:?}");
    }

    /// 同一个 token 被不认时同时来的也只换一次。
    #[tokio::test(start_paused = true)]
    async fn concurrent_rejections_fetch_once() {
        let cache = AccessCache::default();
        *cache.token.lock().unwrap() = Some(("old".into(), Instant::now() + Duration::from_secs(3600)));
        let (fetches, tokens) = concurrent_gets(cache, Some("old"), 2).await;
        assert_eq!(fetches, 1);
        assert!(tokens.iter().all(|token| token == "token0"), "{tokens:?}");
    }

    /// 被不认的 token 换不到新的时，后来不带 `rejected` 的请求也不再拿到它。
    #[tokio::test(start_paused = true)]
    async fn a_rejected_token_is_not_handed_out_again() {
        let cache = Arc::new(AccessCache::default());
        *cache.token.lock().unwrap() = Some(("old".into(), Instant::now() + Duration::from_secs(3600)));
        let fetches = Arc::new(AtomicUsize::new(0));
        assert!(gets(&cache, &fetches, Some("old"), true, 1).await[0].is_err());
        let later = gets(&cache, &fetches, None, true, 1).await;
        assert!(later[0].as_ref().is_err_and(|err| err.contains("down0")), "{later:?}");
    }

    /// 换不到 token 时同时来的只换一次，退避期里来的不再换，都拿到那次的错；退避期过了同时来的也只换
    /// 一次，接着还换不到就退避得更久；换到了之后再换不到，退避从头算。
    #[tokio::test(start_paused = true)]
    async fn failed_fetches_back_off() {
        let cache = Arc::new(AccessCache::default());
        let fetches = Arc::new(AtomicUsize::new(0));
        let fetched = || fetches.load(Ordering::SeqCst);
        let all_fail_with = |results: &[Result<String, String>], why: &str| {
            assert!(results.iter().all(|result| result.as_ref().is_err_and(|err| err.contains(why))), "{results:?}");
        };

        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down0");
        assert_eq!(fetched(), 1);
        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down0");
        assert_eq!(fetched(), 1);

        tokio::time::advance(FETCH_BACKOFF - Duration::from_millis(1)).await;
        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down0");
        assert_eq!(fetched(), 1);
        tokio::time::advance(Duration::from_millis(1)).await;
        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down1");
        assert_eq!(fetched(), 2);

        tokio::time::advance(FETCH_BACKOFF).await;
        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down1");
        assert_eq!(fetched(), 2);
        tokio::time::advance(FETCH_BACKOFF).await;
        let tokens = gets(&cache, &fetches, None, false, 8).await;
        assert_eq!(fetched(), 3);
        assert!(tokens.iter().all(|token| token.as_deref() == Ok("token2")), "{tokens:?}");

        *cache.token.lock().unwrap() = None;
        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down3");
        assert_eq!(fetched(), 4);
        tokio::time::advance(FETCH_BACKOFF).await;
        all_fail_with(&gets(&cache, &fetches, None, true, 8).await, "down4");
        assert_eq!(fetched(), 5);
    }

    /// 上回的退避期过了很久才又换不到时，退避从头算。
    #[tokio::test(start_paused = true)]
    async fn backoff_starts_over_after_a_quiet_spell() {
        let cache = Arc::new(AccessCache::default());
        let fetches = Arc::new(AtomicUsize::new(0));
        gets(&cache, &fetches, None, true, 1).await;
        tokio::time::advance(FETCH_BACKOFF).await;
        gets(&cache, &fetches, None, true, 1).await;
        tokio::time::advance(FETCH_BACKOFF_MAX * 2).await;
        gets(&cache, &fetches, None, true, 1).await;
        assert_eq!(fetches.load(Ordering::SeqCst), 3);
        tokio::time::advance(FETCH_BACKOFF).await;
        let later = gets(&cache, &fetches, None, true, 1).await;
        assert!(later[0].as_ref().is_err_and(|err| err.contains("down3")), "{later:?}");
    }

    /// 过了该换新的时刻换不到新的时，没过期的照样给，退避期里也给；过期了就交回错。
    #[tokio::test(start_paused = true)]
    async fn an_unexpired_token_covers_failed_fetches() {
        let cache = Arc::new(AccessCache::default());
        let lifetime = RENEW_BEFORE + Duration::from_secs(60);
        *cache.token.lock().unwrap() = Some(("old".into(), Instant::now() + lifetime));
        let fetches = Arc::new(AtomicUsize::new(0));
        tokio::time::advance(Duration::from_secs(61)).await;
        let tokens = gets(&cache, &fetches, None, true, 8).await;
        assert!(tokens.iter().all(|token| token.as_deref() == Ok("old")), "{tokens:?}");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        let tokens = gets(&cache, &fetches, None, true, 8).await;
        assert!(tokens.iter().all(|token| token.as_deref() == Ok("old")), "{tokens:?}");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);

        tokio::time::advance(lifetime).await;
        let later = gets(&cache, &fetches, None, true, 1).await;
        assert!(later[0].as_ref().is_err_and(|err| err.contains("down1")), "{later:?}");
    }

    /// 换 token 的请求半路被丢掉时，换到的和换不到的都照样记下，后来的不再换。
    #[tokio::test(start_paused = true)]
    async fn a_dropped_request_still_records_the_fetch() {
        for fail in [true, false] {
            let cache = Arc::new(AccessCache::default());
            let fetches = Arc::new(AtomicUsize::new(0));
            let request = {
                let (cache, fetches) = (cache.clone(), fetches.clone());
                tokio::spawn(async move { cache.get(None, || fetch(fetches, fail)).await })
            };
            for _ in 0..100 {
                if fetches.load(Ordering::SeqCst) > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            request.abort();
            assert!(request.await.is_err_and(|err| err.is_cancelled()));
            let later = gets(&cache, &fetches, None, true, 1).await;
            assert_eq!(fetches.load(Ordering::SeqCst), 1, "{later:?}");
            let later = later[0].as_deref();
            assert!(
                if fail { later.is_err_and(|err| err.contains("down0")) } else { later == Ok("token0") },
                "{later:?}"
            );
        }
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
