//! 直连 FCM：用配置里的 Firebase 服务账号签一个 JWT（RS256），向它的 `token_uri` 换 OAuth 的 access
//! token，再拿它 `POST` 到 HTTP v1 的 `messages:send`，请求体是 `runode_protocol::push::fcm_message`。两种
//! 请求都经 curl 发，HTTP/1.1 就行。

use std::{
    path::Path,
    time::{Duration, Instant},
};

use ring::{
    rand::SystemRandom,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use runode_protocol::push::{
    EXPIRES_AFTER, FcmServiceAccount, Notification, fcm_access_token, fcm_apns_status, fcm_assertion, fcm_message,
    fcm_token_request, is_bad_token, pem_der,
};

use super::{
    apns::Outcome,
    curl::{Curl, Request, Response},
};

/// access token 离过期还有这么久就换新的，不拿快过期的去发。
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);

/// 直连 FCM 用的服务账号：读出来的 RSA 私钥和账号里的几项，以及换来的 access token。
pub(crate) struct FcmKey {
    key: RsaKeyPair,
    account: FcmServiceAccount,
    send_url: String,
    /// 换来的 access token 和该换新的时刻。
    access: Option<(String, Instant)>,
}

impl FcmKey {
    /// 读 Firebase 控制台下载的服务账号 JSON 文件。
    pub(crate) fn load(file: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(file)
            .map_err(|err| format!("cannot read the FCM service account {}: {err}", file.display()))?;
        let account = FcmServiceAccount::parse(&text).map_err(|err| format!("{} {err}", file.display()))?;
        let der =
            pem_der(&account.private_key).ok_or_else(|| format!("the private key in {} is not PEM", file.display()))?;
        let key = RsaKeyPair::from_pkcs8(&der)
            .map_err(|err| format!("the private key in {} is not an RSA key: {err}", file.display()))?;
        Ok(Self { key, send_url: account.send_url(), account, access: None })
    }

    /// 签好的 JWT，拿去换 access token。
    fn assertion(&self) -> Result<String, String> {
        fcm_assertion(&self.account.client_email, &self.account.token_uri, crate::now_unix(), |input| {
            let mut signature = vec![0; self.key.public().modulus_len()];
            self.key
                .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), input, &mut signature)
                .map_err(|_| "cannot sign the FCM token request".to_owned())?;
            Ok(signature)
        })
    }

    /// access token：缓存着的没到该换的时候原样给，不然向 `token_uri` 换一个新的。换不成时 `Err` 是这条
    /// 推送该怎么办：Google 不认服务账号（400、401）是 `Outcome::BadKey`。
    fn access_token(&mut self, curl: &Curl) -> Result<String, Outcome> {
        if let Some((token, renew_at)) = &self.access
            && Instant::now() < *renew_at
        {
            return Ok(token.clone());
        }
        let assertion = self.assertion().map_err(Outcome::Failed)?;
        let request = Request {
            method: "POST",
            url: self.account.token_uri.clone(),
            headers: vec![("content-type", "application/x-www-form-urlencoded".to_owned())],
            body: Some(fcm_token_request(&assertion).into_bytes()),
            http2: false,
        };
        let response = curl.send(&request).map_err(|why| Outcome::Retry {
            after: None,
            why: format!("cannot reach Google for an FCM access token: {why}"),
        })?;
        let status = response.status;
        let parsed = fcm_access_token(&response.body);
        if (200..300).contains(&status)
            && let Ok((token, lifetime)) = parsed
        {
            let renew_at = Instant::now() + Duration::from_secs(lifetime).saturating_sub(RENEW_BEFORE);
            self.access = Some((token.clone(), renew_at));
            return Ok(token);
        }
        let error = parsed.err().unwrap_or_default();
        let why = format!("Google gave no FCM access token: HTTP {status} {error}").trim_end().to_owned();
        Err(match status {
            429 | 500..=599 => Outcome::Retry { after: retry_after(&response), why },
            400 | 401 => Outcome::BadKey(why),
            _ => Outcome::Failed(why),
        })
    }

    /// 下次发时重换 access token：FCM 不认现在这个。
    pub(crate) fn forget_access_token(&mut self) {
        self.access = None;
    }
}

/// 用 `key` 给 FCM registration token 原文 `token` 发通知 `notification`，过期时刻按 `now` 算。
pub(crate) fn push(key: &mut FcmKey, curl: &Curl, token: &str, notification: &Notification, now: u64) -> Outcome {
    let access_token = match key.access_token(curl) {
        Ok(access_token) => access_token,
        Err(outcome) => return outcome,
    };
    let Some(body) = fcm_message(token, notification, now + EXPIRES_AFTER, now) else {
        return Outcome::Failed("the push does not fit in an FCM message".to_owned());
    };
    let request = Request {
        method: "POST",
        url: key.send_url.clone(),
        headers: vec![("authorization", format!("Bearer {access_token}"))],
        body: Some(body),
        http2: false,
    };
    outcome(curl.send(&request))
}

/// FCM 的响应（`Err` 是没拿到响应）该怎么办，先按 `fcm_apns_status` 换成 APNs 的状态和原因再分。
fn outcome(response: Result<Response, String>) -> Outcome {
    let response = match response {
        Ok(response) => response,
        Err(why) => return Outcome::Retry { after: None, why },
    };
    let (status, reason) = fcm_apns_status(response.status, &response.body);
    // 日志里写 FCM 原本的状态。
    let why = if reason.is_empty() {
        format!("HTTP {}", response.status)
    } else {
        format!("HTTP {} {reason}", response.status)
    };
    match status {
        200..=299 => Outcome::Done,
        429 | 500..=599 => Outcome::Retry { after: retry_after(&response), why },
        status if is_bad_token(status, &reason) => Outcome::BadToken(why),
        // access token 不认：重换了再试。
        401 => Outcome::ExpiredJwt,
        // 余下的 403、404：服务账号没有发消息的权限、项目不对。
        403 | 404 => Outcome::BadKey(why),
        _ => Outcome::Failed(why),
    }
}

/// 响应的 `Retry-After`（秒）。
fn retry_after(response: &Response) -> Option<Duration> {
    response.header("retry-after").and_then(|after| after.parse().ok()).map(Duration::from_secs)
}

/// 测试用：借系统的 openssl 现生成一把 RSA 私钥（ring 生成不了），写成 Firebase 给的那样的服务账号 JSON，
/// 项目是 `runode-test`，换 access token 的端点是 `token_uri`。
#[cfg(all(test, unix))]
pub(super) fn test_account(dir: &Path, token_uri: &str) -> std::path::PathBuf {
    let pem_file = dir.join("fcm-key.pem");
    let status = std::process::Command::new("openssl")
        .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out"])
        .arg(&pem_file)
        .stderr(std::process::Stdio::null())
        .status()
        .expect("the FCM tests need openssl");
    assert!(status.success());
    let account = serde_json::json!({
        "type": "service_account",
        "project_id": "runode-test",
        "private_key": std::fs::read_to_string(&pem_file).unwrap(),
        "client_email": "push@runode-test.iam.gserviceaccount.com",
        "token_uri": token_uri,
    });
    let path = dir.join("fcm-account.json");
    std::fs::write(&path, account.to_string()).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Result<Response, String> {
        Ok(Response {
            status,
            headers: headers.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            body: body.as_bytes().to_vec(),
        })
    }

    fn fcm_error(status: u16, code: &str, message: &str) -> String {
        serde_json::json!({
            "error": {
                "code": status,
                "message": message,
                "details": [{ "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", "errorCode": code }],
            }
        })
        .to_string()
    }

    #[test]
    fn responses_decide_what_happens_next() {
        assert_eq!(outcome(response(200, &[], r#"{"name":"projects/p/messages/1"}"#)), Outcome::Done);
        assert!(matches!(outcome(Err("timeout".into())), Outcome::Retry { after: None, .. }));
        assert!(matches!(outcome(response(503, &[], "")), Outcome::Retry { after: None, .. }));
        assert_eq!(
            outcome(response(429, &[("retry-after", "7")], &fcm_error(429, "QUOTA_EXCEEDED", "quota"))),
            Outcome::Retry { after: Some(Duration::from_secs(7)), why: "HTTP 429 TooManyRequests".into() }
        );
        // token 作废了、不对、不是这个项目的：删登记。
        let unregistered = fcm_error(404, "UNREGISTERED", "Requested entity was not found.");
        assert_eq!(outcome(response(404, &[], &unregistered)), Outcome::BadToken("HTTP 404 Unregistered".into()));
        let invalid =
            fcm_error(400, "INVALID_ARGUMENT", "The registration token is not a valid FCM registration token");
        assert!(matches!(outcome(response(400, &[], &invalid)), Outcome::BadToken(_)));
        let mismatch = fcm_error(403, "SENDER_ID_MISMATCH", "SenderId mismatch");
        assert!(matches!(outcome(response(403, &[], &mismatch)), Outcome::BadToken(_)));
        // access token 不认：重换。
        assert_eq!(outcome(response(401, &[], r#"{"error":{"status":"UNAUTHENTICATED"}}"#)), Outcome::ExpiredJwt);
        // 服务账号的毛病。
        let denied = r#"{"error":{"code":403,"status":"PERMISSION_DENIED","message":"denied"}}"#;
        assert_eq!(outcome(response(403, &[], denied)), Outcome::BadKey("HTTP 403 PERMISSION_DENIED".into()));
        let project = r#"{"error":{"code":404,"status":"NOT_FOUND","message":"not found"}}"#;
        assert!(matches!(outcome(response(404, &[], project)), Outcome::BadKey(_)));
        // 别的错误不重试，也不删登记。
        let ttl = fcm_error(400, "INVALID_ARGUMENT", "Invalid value at 'message.android.ttl'");
        assert_eq!(outcome(response(400, &[], &ttl)), Outcome::Failed("HTTP 400 INVALID_ARGUMENT".into()));
    }

    /// 假的 curl：请求的配置存成 `request`，每次在 `count` 里记一行，回 `response` 文件的内容。
    #[cfg(unix)]
    const FAKE_CURL: &str =
        "#!/bin/sh\ndir='@DIR@'\ncat > \"$dir/request\"\necho x >> \"$dir/count\"\ncat \"$dir/response\"\n";

    /// access token 换来后缓存着，到该换的时候、或者丢掉后才重换；换不成时按 Google 的回话分。
    #[cfg(unix)]
    #[test]
    fn access_tokens_are_cached() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("rra-fcm-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("curl.sh");
        std::fs::write(&program, FAKE_CURL.replace("@DIR@", &dir.display().to_string())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let curl = Curl::new(program);
        let answer = |text: &str| std::fs::write(dir.join("response"), text).unwrap();
        let count = || std::fs::read_to_string(dir.join("count")).unwrap_or_default().lines().count();

        let mut key = FcmKey::load(&test_account(&dir, "https://oauth.example/token")).unwrap();
        assert_eq!(key.send_url, "https://fcm.googleapis.com/v1/projects/runode-test/messages:send");
        answer("HTTP/1.1 200 OK\r\n\r\n{\"access_token\":\"ya29.a\",\"expires_in\":3599}");
        assert_eq!(key.access_token(&curl), Ok("ya29.a".into()));
        let request = std::fs::read_to_string(dir.join("request")).unwrap();
        assert!(request.contains("url = \"https://oauth.example/token\""), "{request}");
        assert!(request.contains("header = \"content-type: application/x-www-form-urlencoded\""), "{request}");
        assert!(request.contains("data-raw = \"grant_type="), "{request}");
        answer("HTTP/1.1 200 OK\r\n\r\n{\"access_token\":\"ya29.b\",\"expires_in\":3599}");
        assert_eq!(key.access_token(&curl), Ok("ya29.a".into()));
        assert_eq!(count(), 1);
        key.forget_access_token();
        assert_eq!(key.access_token(&curl), Ok("ya29.b".into()));
        assert_eq!(count(), 2);
        // 到了该换的时候。
        key.access = Some(("ya29.old".into(), Instant::now()));
        assert_eq!(key.access_token(&curl), Ok("ya29.b".into()));
        assert_eq!(count(), 3);

        key.forget_access_token();
        answer("HTTP/1.1 400 Bad Request\r\n\r\n{\"error\":\"invalid_grant\",\"error_description\":\"Invalid JWT\"}");
        assert_eq!(
            key.access_token(&curl),
            Err(Outcome::BadKey("Google gave no FCM access token: HTTP 400 invalid_grant: Invalid JWT".into()))
        );
        answer("HTTP/1.1 401 Unauthorized\r\n\r\n{\"error\":\"invalid_client\"}");
        assert!(matches!(key.access_token(&curl), Err(Outcome::BadKey(_))));
        answer("HTTP/1.1 503 Service Unavailable\r\nretry-after: 9\r\n\r\n");
        assert!(matches!(
            key.access_token(&curl),
            Err(Outcome::Retry { after: Some(after), .. }) if after == Duration::from_secs(9)
        ));
        answer("HTTP/1.1 200 OK\r\n\r\n<html></html>");
        assert!(matches!(key.access_token(&curl), Err(Outcome::Failed(_))));
        assert!(matches!(key.access_token(&Curl::new(dir.join("missing"))), Err(Outcome::Retry { after: None, .. })));

        std::fs::write(dir.join("broken.json"), "{}").unwrap();
        assert!(FcmKey::load(&dir.join("broken.json")).err().unwrap().contains("client_email"));
        assert!(FcmKey::load(&dir.join("missing.json")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
