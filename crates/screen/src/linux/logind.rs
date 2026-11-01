//! 经 D-Bus 问 logind 和桌面：锁没锁屏（`locked`），连着时不让电脑睡、不让屏幕熄（`Inhibitor`）。

use std::{
    os::fd::OwnedFd,
    sync::{Mutex, PoisonError},
};

use zbus::{blocking::Connection, zvariant::OwnedObjectPath};

const LOGIN1: &str = "org.freedesktop.login1";

/// 图形会话在 logind 里的对象，查一次以后记着。
static SESSION: Mutex<Option<(Connection, OwnedObjectPath)>> = Mutex::new(None);

/// 图形会话锁着屏（logind 的 `LockedHint`，GNOME、KDE 这些锁屏时会设）。查不到时当成没锁。
pub(crate) fn locked() -> bool {
    let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
    if session.is_none() {
        *session = find_session();
    }
    let Some((conn, path)) = session.as_ref() else { return false };
    let locked = zbus::blocking::Proxy::new(conn, LOGIN1, path.as_ref(), "org.freedesktop.login1.Session")
        .and_then(|proxy| proxy.get_property::<bool>("LockedHint"));
    match locked {
        Ok(locked) => locked,
        Err(err) => {
            tracing::debug!("cannot read LockedHint: {err}");
            *session = None;
            false
        }
    }
}

fn find_session() -> Option<(Connection, OwnedObjectPath)> {
    let id = std::env::var("XDG_SESSION_ID").ok()?;
    let conn = Connection::system().ok()?;
    let reply = conn
        .call_method(
            Some(LOGIN1),
            "/org/freedesktop/login1",
            Some("org.freedesktop.login1.Manager"),
            "GetSession",
            &(id,),
        )
        .ok()?;
    let path: OwnedObjectPath = reply.body().deserialize().ok()?;
    Some((conn, path))
}

/// 拦着电脑睡眠和屏幕变暗：logind 的 sleep、idle 抑制锁，加上桌面的 `org.freedesktop.ScreenSaver`；丢掉时放开。
/// 哪项拿不到就少那项。
pub(crate) struct Inhibitor {
    _logind: Option<OwnedFd>,
    /// 拿着屏保抑制的会话总线连接：连接断了桌面就放开。
    _screensaver: Option<Connection>,
}

impl Inhibitor {
    pub(crate) fn acquire() -> Self {
        let logind = Connection::system()
            .and_then(|conn| {
                let reply = conn.call_method(
                    Some(LOGIN1),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    "Inhibit",
                    &("sleep:idle", "runode", "Someone is viewing this screen remotely", "block"),
                )?;
                let fd: zbus::zvariant::OwnedFd = reply.body().deserialize()?;
                Ok(OwnedFd::from(fd))
            })
            .inspect_err(|err| tracing::debug!("cannot inhibit sleep through logind: {err}"))
            .ok();
        let screensaver = Connection::session()
            .and_then(|conn| {
                conn.call_method(
                    Some("org.freedesktop.ScreenSaver"),
                    "/org/freedesktop/ScreenSaver",
                    Some("org.freedesktop.ScreenSaver"),
                    "Inhibit",
                    &("runode", "Someone is viewing this screen remotely"),
                )?;
                Ok(conn)
            })
            .inspect_err(|err| tracing::debug!("cannot inhibit the screen saver: {err}"))
            .ok();
        Self { _logind: logind, _screensaver: screensaver }
    }
}
