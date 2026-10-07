//! 自动更新：在后台查 GitHub 上的新版本、下载好，退出时换上，见 `runode_update`。
//!
//! 配置项 `auto-update` 开着时，启动后过 `FIRST_CHECK` 查一次，之后每隔 `CHECK_INTERVAL` 再查；
//! 这份 app 不能自己更新时（自己打包的、不在 .app 里、放在写不了的位置）不查。菜单里的「检查
//! 更新…」随时查，结果弹框告诉用户；有新版本却不能自己更新时，给一个打开下载页的按钮。
//!
//! 下好以后菜单里那一项换成「重启以更新到 Runode X」，后台查到的还发一条带「现在重启」按钮的
//! 系统通知。不重启也行：app 退出时装上（`on_app_quit`），下次打开就是新版本。重启
//! （`window::quit_to_update`）按退出走，会话留得下就留在后台，不看配置项 `terminal-host`；退出时
//! 装上新版本，再由一个等 app 退出的 shell 进程把它重新打开。新版本打开时发现宿主是旧的构建，照
//! 升级的规矩让新宿主接手会话，见 `host_client`。

use std::{path::PathBuf, process::Command, time::Duration};

use gpui::{App, Global, PromptLevel, SystemNotification, SystemNotificationAction};
use runode_update::{Error, Installation, Release, Staged};

use crate::config::AppConfig;

/// 启动后过这么久第一次查：不和启动抢资源。
const FIRST_CHECK: Duration = Duration::from_secs(30);
/// 之后每隔这么久查一次。
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// 「下好了」的通知的标识。
pub const NOTIFICATION_TAG: &str = "runode-update";
/// 通知上「现在重启」按钮的标识。
const RESTART_ACTION: &str = "restart";
/// 这份 app 的版本。
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 更新走到哪一步了。
#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    /// 正在查或者下载；`manual` 是用户在菜单里点的，有结果时弹框说。
    Busy { manual: bool },
    /// 下好了，等着装上。
    Ready(Staged),
}

#[derive(Default)]
struct Updater {
    phase: Phase,
    /// 退出时装上新版本后重新打开它（重启以更新），见 `relaunch_on_quit`。
    relaunch: bool,
}

impl Global for Updater {}

/// 查了一次的结果。
enum Found {
    /// 没有更新的版本；带着最新的版本号。
    UpToDate(String),
    /// 新版本下好了。
    Staged(Staged),
    /// 有新版本，但这份 app 不能自己装：只能去下载页。
    Manual { release: Release, reason: String },
    /// 这份 app 不能自己更新，后台查时不查。
    Skipped,
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
        // 上次下好却没装上的暂存目录先删掉，查到新版本时重新下。
        cx.background_executor()
            .spawn(async {
                if let Ok(installation) = Installation::current() {
                    installation.clean();
                }
            })
            .await;
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
        Some(Phase::Busy { .. }) => rust_i18n::t!("update.menu_checking").into_owned(),
        _ => rust_i18n::t!("update.menu_check").into_owned(),
    }
}

/// 菜单里点了那一项：下好了就重启以更新，否则马上查一次、弹框说结果；正在查时等它查完再说。
pub fn menu_clicked(cx: &mut App) {
    if matches!(cx.global::<Updater>().phase, Phase::Ready(_)) {
        restart(cx);
    } else {
        check(true, cx);
    }
}

/// 点了「下好了」的通知：点的是「现在重启」就重启，点的是通知本身时弹框问。
pub fn on_notification(action: Option<&str>, cx: &mut App) {
    if !matches!(cx.global::<Updater>().phase, Phase::Ready(_)) {
        return;
    }
    if action == Some(RESTART_ACTION) {
        restart(cx);
    } else {
        offer_restart(cx);
    }
}

/// 重启以更新时、退出前由 `window::quit_to_update` 调：退出时装上新版本后重新打开它。
pub fn relaunch_on_quit(cx: &mut App) {
    cx.global_mut::<Updater>().relaunch = true;
}

fn restart(cx: &mut App) {
    crate::window::quit_to_update(cx);
}

/// 在后台查一次，没在查、也还没下好时才查；`manual` 时把结果弹框告诉用户。
fn check(manual: bool, cx: &mut App) {
    let updater = cx.global_mut::<Updater>();
    match &mut updater.phase {
        Phase::Ready(_) => {
            if manual {
                offer_restart(cx);
            }
            return;
        }
        Phase::Busy { manual: asked } => {
            *asked |= manual;
            return;
        }
        Phase::Idle => updater.phase = Phase::Busy { manual },
    }
    crate::menus::set_menus(cx);
    let found = cx.background_executor().spawn(async move { look(manual) });
    cx.spawn(async move |cx| {
        let found = found.await;
        cx.update(|cx| finish(found, cx));
    })
    .detach();
}

/// 查最新的版本，比这份新就下载、核对好。不是 `manual` 时，这份 app 不能自己更新就不查。阻塞到
/// 下完。
fn look(manual: bool) -> Result<Found, Error> {
    let installation = Installation::current();
    if let Err(reason) = &installation
        && !manual
    {
        tracing::debug!("not checking for updates: {reason}");
        return Ok(Found::Skipped);
    }
    let release = runode_update::latest()?;
    if !runode_update::is_newer(&release.version, VERSION) {
        return Ok(Found::UpToDate(release.version));
    }
    tracing::info!("Runode {} is out, this is {VERSION}", release.version);
    let installation = match installation {
        Ok(installation) => installation,
        Err(reason) => return Ok(Found::Manual { release, reason: reason.to_string() }),
    };
    match installation.stage(&release) {
        Ok(staged) => Ok(Found::Staged(staged)),
        Err(Error::NoArchive) => Ok(Found::Manual { reason: Error::NoArchive.to_string(), release }),
        Err(err) => Err(err),
    }
}

fn finish(found: Result<Found, Error>, cx: &mut App) {
    let manual = matches!(cx.global::<Updater>().phase, Phase::Busy { manual: true });
    cx.global_mut::<Updater>().phase = Phase::Idle;
    match found {
        Ok(Found::Staged(staged)) => {
            cx.global_mut::<Updater>().phase = Phase::Ready(staged);
            if manual {
                offer_restart(cx);
            } else {
                post_ready(cx);
            }
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
        // 后台查的、这份 app 不能自己更新，跳过了；用户在这期间点了菜单时，按菜单里点的重新查一次。
        Ok(Found::Skipped) => {
            if manual {
                check(true, cx);
                return;
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

/// 弹框说新版本下好了，问现在重启还是退出时再装。
fn offer_restart(cx: &mut App) {
    let Phase::Ready(staged) = &cx.global::<Updater>().phase else { return };
    let title = rust_i18n::t!("update.ready_title", version = staged.version());
    let detail = rust_i18n::t!("update.ready_detail");
    let answers = [&*rust_i18n::t!("update.restart"), &*rust_i18n::t!("update.later")];
    prompt(PromptLevel::Info, &title, &detail, &answers, cx, |answer, cx| {
        if answer == 0 {
            restart(cx);
        }
    });
}

/// 发一条「下好了」的系统通知，带「现在重启」按钮。
fn post_ready(cx: &mut App) {
    let Phase::Ready(staged) = &cx.global::<Updater>().phase else { return };
    let notification = SystemNotification {
        tag: NOTIFICATION_TAG.into(),
        title: rust_i18n::t!("update.ready_title", version = staged.version()).into_owned().into(),
        body: rust_i18n::t!("update.ready_notification").into_owned().into(),
        actions: vec![SystemNotificationAction {
            id: RESTART_ACTION.into(),
            label: rust_i18n::t!("update.restart").into_owned().into(),
        }],
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

/// app 退出时：下好了就装上；是重启以更新时，再重新打开它（装没装上都开，用户要的是重启）。
fn on_quit(cx: &mut App) {
    let updater = cx.global_mut::<Updater>();
    let relaunch = updater.relaunch;
    let Phase::Ready(staged) = std::mem::take(&mut updater.phase) else { return };
    let app = staged.app().to_owned();
    cx.dismiss_system_notification(NOTIFICATION_TAG);
    if let Err(err) = staged.install() {
        tracing::error!("failed to install the update: {err}");
    }
    if relaunch {
        reopen_after_exit(app);
    }
}

/// 拉起一个 shell，等这个进程退出后用 `open` 打开 `app`。它在自己的进程组里，app 退出时不跟着走。
fn reopen_after_exit(app: PathBuf) {
    let script = r#"while /bin/kill -0 "$1" 2>/dev/null; do /bin/sleep 0.2; done; exec /usr/bin/open "$2""#;
    let spawned = {
        use std::os::unix::process::CommandExt as _;
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .arg("sh")
            .arg(std::process::id().to_string())
            .arg(&app)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
    };
    if let Err(err) = spawned {
        tracing::error!("failed to reopen {} after quitting: {err}", app.display());
    }
}

#[cfg(test)]
mod tests {
    /// 更新包的签名要求里的 bundle id 和这个 app 的一样，不然新版本一律被拒、只在日志里看得到。
    #[test]
    fn the_update_checks_this_apps_bundle_id() {
        let plist = include_str!("../Info.plist");
        let (_, rest) = plist.split_once("<key>CFBundleIdentifier</key>").unwrap();
        let id = rest.split_once("<string>").unwrap().1.split_once("</string>").unwrap().0;
        assert_eq!(id, runode_update::BUNDLE_ID);
    }
}
