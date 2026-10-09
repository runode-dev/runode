//! 自动更新：在后台查 GitHub 上的新版本、下载好，退出时换上，见 `runode_update`。
//!
//! 配置项 `auto-update` 开着时，启动后过 `FIRST_CHECK` 查一次，之后每隔 `CHECK_INTERVAL` 再查，
//! 查到新版本直接在后台下载；这份 app 不能自己更新时（自己打包的、不在 .app 里、放在写不了的位置）
//! 不查。菜单里的「检查更新…」随时查，结果弹框告诉用户：有新版本时问要不要下载更新，选了才下；
//! 有新版本却不能自己更新时，给一个打开下载页的按钮。
//!
//! 查和下载期间菜单里那一项显示「正在检查更新…」和「正在下载 Runode X… 42%」，进度由下载线程写进
//! `Progress`，前台每隔 `PROGRESS_REFRESH` 看一眼、百分比变了才重画菜单。
//!
//! 下好以后菜单里那一项换成「重启以更新到 Runode X」，后台查到的还发一条系统通知，点了弹框问要不要
//! 重启。不重启也行：app 退出时装上（`on_app_quit`），下次打开就是新版本。重启
//! （`window::quit_to_update`）按退出走，会话留得下就留在后台，不看配置项 `terminal-host`；退出时
//! 装上新版本，GPUI 的 `restart` 等 app 退出后把它重新打开。新版本打开时发现宿主是旧的构建，照
//! 升级的规矩让新宿主接手会话，见 `host_client`。

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use gpui::{App, Global, PromptLevel, SystemNotification};
use runode_update::{Error, Installation, Release, Staged};

use crate::config::AppConfig;

/// 启动后过这么久第一次查：不和启动抢资源。
const FIRST_CHECK: Duration = Duration::from_secs(30);
/// 之后每隔这么久查一次。
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// 下载时多久看一次进度。
const PROGRESS_REFRESH: Duration = Duration::from_millis(250);
/// 「下好了」的通知的标识。
pub const NOTIFICATION_TAG: &str = "runode-update";
/// 这份 app 的版本。
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新走到哪一步了。`S` 是下好的新版本，测试里换成别的，好不经下载走状态转移。
#[derive(Default)]
enum Phase<S = Staged> {
    #[default]
    Idle,
    /// 正在查有没有新版本；`manual` 是用户在菜单里点的，有结果时弹框说。
    Checking { manual: bool },
    /// 正在下载 `version`；`manual` 是用户选了下载的，下好了弹框说。
    Downloading { version: String, progress: Arc<Progress>, manual: bool },
    /// 下好了，等着装上。
    Ready(S),
}

/// 要查一次时接着做什么，见 `Phase::start_check`。
#[derive(Debug, PartialEq, Eq)]
enum Start {
    /// 在后台查。
    Check,
    /// 已经下好了，用户点的：问要不要重启。
    OfferRestart,
    /// 什么都不做。
    Nothing,
}

impl<S> Phase<S> {
    /// 要查一次（`manual` 是用户点的）：空闲时转到 `Checking`、去查；正在查或下载时不再查，只记下
    /// 用户点过、有结果时弹框说；已经下好了时不再查，用户点的就问要不要重启。
    fn start_check(&mut self, manual: bool) -> Start {
        match self {
            Self::Ready(_) if manual => Start::OfferRestart,
            Self::Ready(_) => Start::Nothing,
            Self::Checking { manual: asked } | Self::Downloading { manual: asked, .. } => {
                *asked |= manual;
                Start::Nothing
            }
            Self::Idle => {
                *self = Self::Checking { manual };
                Start::Check
            }
        }
    }

    /// 查完或者下载完了：回到空闲，返回是不是用户点的（要弹框说结果）。下载失败就停在空闲，等下一轮
    /// 定时（`CHECK_INTERVAL`）再查、再下。
    fn finish(&mut self) -> bool {
        let manual = matches!(self, Self::Checking { manual: true } | Self::Downloading { manual: true, .. });
        *self = Self::Idle;
        manual
    }

    /// 退出时：下好了的交出来去装上，回到空闲。
    fn take_ready(&mut self) -> Option<S> {
        match std::mem::take(self) {
            Self::Ready(staged) => Some(staged),
            _ => None,
        }
    }
}

/// 下载的进度，下载线程写、前台读。
#[derive(Default)]
struct Progress {
    received: AtomicU64,
    /// 服务器没说总大小时是 0。
    total: AtomicU64,
}

impl Progress {
    fn percent(&self) -> Option<u64> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.received.load(Ordering::Relaxed) * 100 / total).min(100))
    }
}

#[derive(Default)]
struct Updater {
    phase: Phase,
}

impl Global for Updater {}

/// 查了一次的结果。
enum Found {
    /// 没有更新的版本；带着最新的版本号。
    UpToDate(String),
    /// 有新版本，这份 app 也能自己装：等着下载。
    Available { release: Release, installation: Installation },
    /// 有新版本，但这份 app 不能自己装：只能去下载页。
    Manual { release: Release, reason: String },
}

/// 开始定时查，退出时装上下好的新版本。要在配置装上之后调用。
pub fn install(cx: &mut App) {
    cx.set_global(Updater::default());
    cx.on_app_quit(|cx| {
        on_quit(cx);
        async {}
    })
    .detach();
    cx.spawn(async move |cx| {
        // 这份 app 不能自己更新时后台不查，菜单里点了照样查、给下载页。
        let supported = cx
            .background_executor()
            .spawn(async {
                Installation::current().inspect_err(|reason| tracing::debug!("no updates: {reason}")).is_ok()
            })
            .await;
        if !supported {
            return;
        }
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            cx.update(|cx| {
                if cx.global::<AppConfig>().0.auto_update {
                    check(false, cx);
                }
            });
            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

/// 菜单里那一项的文字，跟着更新走到哪一步变。
pub fn menu_label(cx: &App) -> String {
    match cx.try_global::<Updater>().map(|updater| &updater.phase) {
        Some(Phase::Ready(staged)) => rust_i18n::t!("update.menu_restart", version = staged.version()).into_owned(),
        Some(Phase::Checking { .. }) => rust_i18n::t!("update.menu_checking").into_owned(),
        Some(Phase::Downloading { version, progress, .. }) => match progress.percent() {
            Some(percent) => rust_i18n::t!("update.menu_downloading", version = version, percent = percent),
            None => rust_i18n::t!("update.menu_downloading_unknown", version = version),
        }
        .into_owned(),
        _ => rust_i18n::t!("update.menu_check").into_owned(),
    }
}

/// 菜单里点了那一项：下好了就重启以更新，否则马上查一次、弹框说结果；正在查时等它查完再说。
pub fn menu_clicked(cx: &mut App) {
    if matches!(cx.global::<Updater>().phase, Phase::Ready(_)) {
        crate::window::quit_to_update(cx);
    } else {
        check(true, cx);
    }
}

/// 在后台查一次，没在查、没在下载、也还没下好时才查；`manual` 时把结果弹框告诉用户。
fn check(manual: bool, cx: &mut App) {
    match cx.global_mut::<Updater>().phase.start_check(manual) {
        Start::Check => {}
        Start::OfferRestart => return offer_restart(cx),
        Start::Nothing => return,
    }
    crate::menus::set_menus(cx);
    let found = cx.background_executor().spawn(async { look() });
    cx.spawn(async move |cx| {
        let found = found.await;
        cx.update(|cx| finish_check(found, cx));
    })
    .detach();
}

/// 查最新的版本，比这份新就看这份能不能自己装。
fn look() -> Result<Found, Error> {
    let release = runode_update::latest()?;
    if !runode_update::is_newer(&release.version, VERSION) {
        return Ok(Found::UpToDate(release.version));
    }
    tracing::info!("Runode {} is out, this is {VERSION}", release.version);
    let installation = match Installation::current() {
        Ok(installation) => installation,
        Err(reason) => return Ok(Found::Manual { release, reason }),
    };
    if release.archive().is_none() {
        return Ok(Found::Manual { reason: Error::NoArchive.to_string(), release });
    }
    Ok(Found::Available { release, installation })
}

fn finish_check(found: Result<Found, Error>, cx: &mut App) {
    let manual = cx.global_mut::<Updater>().phase.finish();
    match found {
        // 后台查到的直接下；手动查的先问，选了才下。
        Ok(Found::Available { release, installation }) if !manual => download(release, installation, false, cx),
        Ok(Found::Available { release, installation }) => {
            let title = rust_i18n::t!("update.available_title", version = release.version);
            let detail = rust_i18n::t!("update.available_detail", current = VERSION);
            let answers = [&*rust_i18n::t!("update.download"), &*rust_i18n::t!("update.later")];
            prompt(PromptLevel::Info, &title, &detail, &answers, cx, move |answer, cx| {
                if answer == 0 {
                    download(release, installation, true, cx);
                }
            });
        }
        Ok(Found::UpToDate(latest)) => {
            tracing::info!("Runode is up to date: {VERSION}, the latest is {latest}");
            if manual {
                let title = rust_i18n::t!("update.up_to_date_title");
                let detail = rust_i18n::t!("update.up_to_date_detail", version = VERSION);
                prompt(PromptLevel::Info, &title, &detail, &[&rust_i18n::t!("update.ok")], cx, |_, _| {});
            }
        }
        Ok(Found::Manual { release, reason }) => {
            tracing::info!("Runode {} cannot update itself: {reason}", release.version);
            if manual {
                let title = rust_i18n::t!("update.manual_title", version = release.version);
                let detail = rust_i18n::t!("update.manual_detail", reason = reason);
                let answers = [&*rust_i18n::t!("update.open_page"), &*rust_i18n::t!("update.later")];
                prompt(PromptLevel::Info, &title, &detail, &answers, cx, move |answer, cx| {
                    if answer == 0 {
                        cx.open_url(&release.page);
                    }
                });
            }
        }
        Err(err) => {
            tracing::warn!("failed to check for updates: {err}");
            if manual {
                let title = rust_i18n::t!("update.failed_title");
                prompt(PromptLevel::Warning, &title, &err.to_string(), &[&rust_i18n::t!("update.ok")], cx, |_, _| {});
            }
        }
    }
    crate::menus::set_menus(cx);
}

/// 在后台下载 `release`、核对好，菜单里显示进度；`manual` 时下好了弹框说，否则发通知。
fn download(release: Release, installation: Installation, manual: bool, cx: &mut App) {
    let progress = Arc::new(Progress::default());
    cx.global_mut::<Updater>().phase =
        Phase::Downloading { version: release.version.clone(), progress: progress.clone(), manual };
    crate::menus::set_menus(cx);
    let staged = cx.background_executor().spawn(async move {
        installation.stage(&release, &|received, total| {
            progress.received.store(received, Ordering::Relaxed);
            progress.total.store(total, Ordering::Relaxed);
        })
    });
    cx.spawn(async move |cx| {
        let staged = staged.await;
        cx.update(|cx| finish_download(staged, cx));
    })
    .detach();
    cx.spawn(async move |cx| {
        let mut shown = None;
        loop {
            cx.background_executor().timer(PROGRESS_REFRESH).await;
            let downloading = cx.update(|cx| {
                let Phase::Downloading { progress, .. } = &cx.global::<Updater>().phase else { return false };
                let percent = progress.percent();
                if percent != shown {
                    shown = percent;
                    crate::menus::set_menus(cx);
                }
                true
            });
            if !downloading {
                break;
            }
        }
    })
    .detach();
}

fn finish_download(staged: Result<Staged, Error>, cx: &mut App) {
    let manual = cx.global_mut::<Updater>().phase.finish();
    match staged {
        Ok(staged) => {
            cx.global_mut::<Updater>().phase = Phase::Ready(staged);
            if manual {
                offer_restart(cx);
            } else {
                post_ready(cx);
            }
        }
        Err(err) => {
            tracing::warn!("failed to download the update: {err}");
            if manual {
                let title = rust_i18n::t!("update.download_failed_title");
                prompt(PromptLevel::Warning, &title, &err.to_string(), &[&rust_i18n::t!("update.ok")], cx, |_, _| {});
            }
        }
    }
    crate::menus::set_menus(cx);
}

/// 弹框说新版本下好了，问现在重启还是退出时再装；点了「下好了」的通知时也是它。
pub fn offer_restart(cx: &mut App) {
    let Phase::Ready(staged) = &cx.global::<Updater>().phase else { return };
    let title = rust_i18n::t!("update.ready_title", version = staged.version());
    let detail = rust_i18n::t!("update.ready_detail");
    let answers = [&*rust_i18n::t!("update.restart"), &*rust_i18n::t!("update.later")];
    prompt(PromptLevel::Info, &title, &detail, &answers, cx, |answer, cx| {
        if answer == 0 {
            crate::window::quit_to_update(cx);
        }
    });
}

/// 发一条「下好了」的系统通知。
fn post_ready(cx: &mut App) {
    let Phase::Ready(staged) = &cx.global::<Updater>().phase else { return };
    let notification = SystemNotification {
        tag: NOTIFICATION_TAG.into(),
        title: rust_i18n::t!("update.ready_title", version = staged.version()).into_owned().into(),
        body: rust_i18n::t!("update.ready_notification").into_owned().into(),
        actions: Vec::new(),
    };
    crate::window::listen_notifications(cx);
    cx.show_system_notification(notification);
}

/// 在最前面的窗口上弹框，选了第几个按钮交给 `then`；没有窗口时不弹。从菜单派发时窗口正在处理
/// 动作，弹不了框，推迟到这一轮更新结束。
fn prompt(
    level: PromptLevel,
    title: &str,
    detail: &str,
    answers: &[&str],
    cx: &mut App,
    then: impl FnOnce(usize, &mut App) + 'static,
) {
    let (title, detail) = (title.to_owned(), detail.to_owned());
    let answers: Vec<String> = answers.iter().map(|answer| (*answer).to_owned()).collect();
    cx.defer(move |cx| {
        let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
        let labels: Vec<&str> = answers.iter().map(String::as_str).collect();
        let Ok(answer) = window.update(cx, |_, window, cx| window.prompt(level, &title, Some(&detail), &labels, cx))
        else {
            return;
        };
        cx.spawn(async move |cx| {
            if let Ok(answer) = answer.await {
                cx.update(|cx| then(answer, cx));
            }
        })
        .detach();
    });
}

/// app 退出时：下好了就装上。重启以更新时，GPUI 的 `restart` 随后把装上的新版本打开。
fn on_quit(cx: &mut App) {
    let Some(staged) = cx.global_mut::<Updater>().phase.take_ready() else { return };
    cx.dismiss_system_notification(NOTIFICATION_TAG);
    if let Err(err) = staged.install() {
        tracing::error!("failed to install the update: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 下好的新版本，测试里用版本号代替。
    type TestPhase = Phase<&'static str>;

    fn downloading(manual: bool) -> TestPhase {
        Phase::Downloading { version: "2.0.0".into(), progress: Arc::default(), manual }
    }

    /// 定时查到新版本：空闲时去查，查完回到空闲、不弹框，接着下载；下好了等着装上。
    #[test]
    fn a_background_check_downloads_and_waits_to_install() {
        let mut phase = TestPhase::default();
        assert_eq!(phase.start_check(false), Start::Check);
        assert!(matches!(phase, Phase::Checking { manual: false }));
        assert!(!phase.finish());
        assert!(matches!(phase, Phase::Idle));
        phase = downloading(false);
        assert!(!phase.finish());
        phase = Phase::Ready("2.0.0");
        // 下好了以后定时的不再查，用户点的问要不要重启。
        assert_eq!(phase.start_check(false), Start::Nothing);
        assert_eq!(phase.start_check(true), Start::OfferRestart);
        assert!(matches!(phase, Phase::Ready("2.0.0")));
    }

    /// 正在查或下载时用户点了：不再查一次，记下来，有结果时弹框说。
    #[test]
    fn a_click_while_busy_asks_for_the_result() {
        let mut phase = TestPhase::Checking { manual: false };
        assert_eq!(phase.start_check(true), Start::Nothing);
        assert!(phase.finish());
        let mut phase = downloading(false);
        assert_eq!(phase.start_check(true), Start::Nothing);
        assert!(phase.finish());
        // 定时的那一轮撞上用户点的，不把它改回不弹框。
        let mut phase = TestPhase::Checking { manual: true };
        assert_eq!(phase.start_check(false), Start::Nothing);
        assert!(phase.finish());
    }

    /// 下载失败后回到空闲，下一轮定时（每隔 `CHECK_INTERVAL`，6 小时）照常再查。
    #[test]
    fn a_failed_download_is_retried_on_the_next_check() {
        assert_eq!(CHECK_INTERVAL, Duration::from_secs(6 * 60 * 60));
        let mut phase = downloading(false);
        assert!(!phase.finish());
        assert!(matches!(phase, Phase::Idle));
        assert_eq!(phase.start_check(false), Start::Check);
    }

    /// 退出时下好了就交出来装上，只交一次；没下好时什么都不装。
    #[test]
    fn quitting_installs_only_what_is_ready() {
        let mut phase = TestPhase::Ready("2.0.0");
        assert_eq!(phase.take_ready(), Some("2.0.0"));
        assert_eq!(phase.take_ready(), None);
        assert_eq!(downloading(false).take_ready(), None);
        assert_eq!(TestPhase::Checking { manual: true }.take_ready(), None);
    }

    /// 更新包的签名要求里的 bundle id 和这个 app 的一样，不然新版本一律被拒、只在日志里看得到。
    #[test]
    fn the_update_checks_this_apps_bundle_id() {
        let plist = include_str!("../Info.plist");
        let (_, rest) = plist.split_once("<key>CFBundleIdentifier</key>").unwrap();
        let id = rest.split_once("<string>").unwrap().1.split_once("</string>").unwrap().0;
        assert_eq!(id, runode_update::BUNDLE_ID);
    }
}
