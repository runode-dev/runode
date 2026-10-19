//! 设置页的「发送测试推送」：不等 agent，马上给设备表里每条能推的登记发一条通知，看密钥和登记对不对。
//! 路线和请求跟 `Pusher` 一样（`route`、`apns`），但不重试、不改设备表：错误只报给调用方，换环境、
//! 删登记这些留给真推送时的发送线程。不经监听和宿主，所以 app 连着单独跑的宿主、监听开在那边时也能用。

use runode_paths::Dirs;
use runode_protocol::{SessionId, push::Notification};
use runode_types::session::SessionMeta;

use super::{
    PushSettings,
    apns::{self, DirectKey, Outcome, Via},
    curl::Curl,
    route,
};
use crate::devices;

/// 测试推送的内容：通知的副标题里显示的标题，和正文的几行。通知的标题是「有终端在等你回答」那句，
/// 由手机端本地化。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestPush {
    pub title: String,
    pub lines: Vec<String>,
}

/// 给每条登记发一条内容是 `content` 的通知。会阻塞到发完，调用方放到后台跑。推送开没开（`PushSettings::enabled`）不管。
///
/// 回推到了几台；没有登记时是 `Ok(0)`。登记都推不到时回原因。
pub fn send_test_push(dirs: &Dirs, settings: &PushSettings, content: &TestPush) -> Result<usize, String> {
    probe(dirs, settings, content, &Curl::default())
}

pub(super) fn probe(dirs: &Dirs, settings: &PushSettings, content: &TestPush, curl: &Curl) -> Result<usize, String> {
    let registrations =
        devices::push_registrations(dirs).map_err(|err| format!("cannot read the device table: {err}"))?;
    if registrations.is_empty() {
        return Ok(0);
    }
    curl.check()?;
    // 密钥读不了只算直连那几条推不到，经中转的照样测。
    let (mut direct, key_error) = match settings
        .direct
        .as_ref()
        .map(|key| DirectKey::load(&key.key_file, &key.key_id, &key.team_id, &key.bundle))
    {
        Some(Ok(key)) => (Some(key), None),
        Some(Err(why)) => (None, Some(why)),
        None => (None, None),
    };
    let session = SessionId::random().map_err(|err| format!("cannot pick a session id: {err}"))?;
    let meta = SessionMeta { title: Some(content.title.clone()), ..SessionMeta::default() };
    let mut errors = Vec::new();
    let mut sent = 0;
    for registration in &registrations {
        let Some(route) = route(registration, settings) else {
            errors.push(format!("nowhere to push the app {} ({:?})", registration.bundle, registration.env));
            continue;
        };
        if route.via == Via::Direct
            && let Some(why) = &key_error
        {
            errors.push(why.clone());
            continue;
        }
        let notification =
            Notification::new(registration.machine.clone(), &registration.machine_name, session, &meta, &content.lines);
        let outcome = match apns::request(
            route,
            &registration.token,
            &notification,
            direct.as_mut(),
            settings.relay.as_deref(),
            crate::now_unix(),
        ) {
            Ok(request) => apns::outcome(route.via, curl.send(&request)),
            Err(why) => Outcome::Failed(why),
        };
        let why = match outcome {
            Outcome::Done => {
                sent += 1;
                continue;
            }
            Outcome::Retry { why, .. } | Outcome::BadToken(why) | Outcome::BadKey(why) | Outcome::Failed(why) => why,
            Outcome::ExpiredJwt => "APNs says the token has expired".to_owned(),
        };
        errors.push(format!("cannot push ({:?}, {:?}): {why}", route.env, route.via));
    }
    // 几台手机常常错在同一处，一样的原因只报一次。
    errors.sort();
    errors.dedup();
    for why in &errors {
        tracing::warn!("test push: {why}");
    }
    if sent == 0 {
        return Err(errors.join("; "));
    }
    Ok(sent)
}
