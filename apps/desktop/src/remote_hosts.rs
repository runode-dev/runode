//! 远端主机（SSH 主机和配对的电脑）在界面这边要办的事：连上一台主机后开始办那台机器上的命令行转来的请求，把
//! 那台主机上断开着的终端接回来；ssh 要问用户时交给最前面的窗口弹框；连接状态一变就更新全局的 `HostsChanged`。
//! 连上的宿主会跟着 ssh 的登录被结束时，这次运行里每台主机问一次要不要把它装成登录服务（`install_service`）。
//! 用户可以给主机另起一个显示名（`set_display_name`），连接仍按 `HostId` 认。主机上次连上时报的系统记下来
//! （`system`），没连着时界面也认得出是什么系统。

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs, io,
    path::PathBuf,
};

use futures::StreamExt as _;
use gpui::{App, AppContext as _, Global, PromptLevel};

use crate::{
    host_client::{
        hosts::{self, HostId, Phase, RemoteEvent, Status},
        paired, ssh,
    },
    terminal_view, window,
};

/// 远端主机的连接状态变了、有主机被忘掉时换一个新值，要列主机的界面（设置页）观察它。
#[derive(Default)]
pub struct HostsChanged(u64);

impl Global for HostsChanged {}

fn hosts_changed(cx: &mut App) {
    let next = cx.default_global::<HostsChanged>().0 + 1;
    cx.set_global(HostsChanged(next));
}

/// 配对了一台电脑：列配对的电脑的界面跟着重画，画在各窗口缓存的视图里的名字要整个刷新。
pub fn machines_changed(cx: &mut App) {
    cx.refresh_windows();
    hosts_changed(cx);
}

/// 用户给远端主机起的显示名，按 `HostId::key` 记，各窗口共用；存在 `Dirs::host_names_file` 里，忘掉主机时留着。
#[derive(Default)]
struct HostNames(BTreeMap<String, String>);

impl Global for HostNames {}

/// 各主机上次连上时报的系统（`Connected::os` 的写法），按 `HostId::key` 记；存在 `Dirs::host_systems_file` 里，
/// 忘掉主机时留着。
#[derive(Default)]
struct HostSystems(BTreeMap<String, String>);

impl Global for HostSystems {}

/// 读出按 `HostId::key` 记的一张表。读不懂的文件挪到一边再从空的开始，下次保存时不把记过的盖掉。
fn load_map(path: Option<PathBuf>) -> BTreeMap<String, String> {
    let Some(path) = path else {
        return BTreeMap::new();
    };
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            let bad = path.with_extension("json.bad");
            tracing::warn!("cannot parse {}, moving it to {}: {err}", path.display(), bad.display());
            if let Err(err) = fs::rename(&path, &bad) {
                tracing::warn!("cannot move {} aside: {err}", path.display());
            }
            BTreeMap::new()
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
        Err(err) => {
            tracing::warn!("cannot read {}: {err}", path.display());
            BTreeMap::new()
        }
    }
}

fn save_map(path: Option<PathBuf>, map: &BTreeMap<String, String>) -> io::Result<()> {
    let path = path.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    runode_paths::replace_file(&path, &serde_json::to_vec_pretty(map)?)
}

/// 远端主机 `host` 没另起名字时显示的名字：SSH 主机是连接目标。本机为空。
pub fn default_name(host: &HostId) -> String {
    match host {
        HostId::Local => String::new(),
        HostId::Ssh(name) => name.clone(),
        HostId::Paired(id) => paired::machine(id).map_or_else(|| id.clone(), |machine| machine.host_name),
    }
}

/// 远端主机 `host` 在界面上显示的名字：用户起过名字时是那个名字，否则是 `default_name`。本机是「本机」。
/// 配对的电脑没起过名字、主机名又和某台 SSH 主机的显示名一样时（多半就是同一台经两条路连着），注明是配对的
/// 那台，侧栏和主机菜单里才分得清。
pub fn display_name(host: &HostId, cx: &App) -> String {
    let Some(key) = host.key() else {
        return rust_i18n::t!("workspace.host_local").into_owned();
    };
    if let Some(name) = cx.try_global::<HostNames>().and_then(|names| names.0.get(&key)) {
        return name.clone();
    }
    let name = default_name(host);
    let clashes =
        || hosts::remotes().iter().any(|other| matches!(other, HostId::Ssh(_)) && display_name(other, cx) == name);
    if matches!(host, HostId::Paired(_)) && clashes() {
        return rust_i18n::t!("workspace.host_paired_name", name = name).into_owned();
    }
    name
}

/// 用户给远端主机 `host` 起过名字。
pub fn has_display_name(host: &HostId, cx: &App) -> bool {
    host.key().is_some_and(|key| cx.try_global::<HostNames>().is_some_and(|names| names.0.contains_key(&key)))
}

/// 把远端主机 `host` 的显示名改成 `display`；空的或者和 `default_name` 一样时不再另起名字。
pub fn set_display_name(host: &HostId, display: &str, cx: &mut App) {
    let Some(key) = host.key() else {
        return;
    };
    let display = display.trim();
    let default = default_name(host);
    let names = &mut cx.default_global::<HostNames>().0;
    let changed = if display.is_empty() || display == default {
        names.remove(&key).is_some()
    } else {
        names.insert(key, display.to_owned()).is_none_or(|old| old != display)
    };
    if !changed {
        return;
    }
    if let Err(err) = save_map(runode_paths::Dirs::from_env().host_names_file(), names) {
        tracing::warn!("cannot save the host names: {err}");
    }
    // 显示名画在各窗口缓存的视图里，只通知窗口的根视图换不全，要整个刷新。
    cx.refresh_windows();
    hosts_changed(cx);
}

/// 远端主机 `host` 上次连上时报的系统，`Connected::os` 的写法；没连上过时为 `None`。本机是这台机器的系统。
pub fn system<'a>(host: &HostId, cx: &'a App) -> Option<&'a str> {
    let Some(key) = host.key() else {
        return Some(std::env::consts::OS);
    };
    cx.try_global::<HostSystems>().and_then(|systems| systems.0.get(&key)).map(String::as_str)
}

/// 远端主机 `host` 上的文件和 git 读不了时给人看的原因：配对的电脑装的是别的构建，读写项目的那套协议不管兼容，
/// 开不了（`ChannelRefusal::BuildMismatch`）。读得了、还没连上时为空。
pub fn project_unavailable(host: &HostId) -> Option<String> {
    if !matches!(host, HostId::Paired(_)) {
        return None;
    }
    match hosts::status_of(host) {
        Status::Connected(connected) if connected.mismatch => {
            Some(rust_i18n::t!("paired.build_mismatch", build = connected.build).into_owned())
        }
        _ => None,
    }
}

/// 系统 `os`（`Connected::os` 的写法）给人看的名字，认不出时为 `None`。
pub fn system_name(os: &str) -> Option<&'static str> {
    match os {
        "macos" => Some("macOS"),
        "linux" => Some("Linux"),
        "windows" => Some("Windows"),
        _ => None,
    }
}

/// 远端主机 `host` 的远程桌面能不能看：系统是 macOS、Windows 或 Linux 就算能，Linux 屏幕上有没有人登录桌面
/// 等打开时由那边的采集进程报；没连上过、系统不明时算不能，本机不算。
pub fn screen_capturable(host: &HostId, cx: &App) -> bool {
    !host.is_local() && system(host, cx).and_then(system_name).is_some()
}

/// 记下远端主机 `host` 报的系统 `os`；变了才存，并刷新画着系统图标、远程桌面按钮的各窗口。
fn remember_system(host: &HostId, os: &str, cx: &mut App) {
    let Some(key) = host.key() else {
        return;
    };
    if os.is_empty() || system(host, cx) == Some(os) {
        return;
    }
    let systems = &mut cx.default_global::<HostSystems>().0;
    systems.insert(key, os.to_owned());
    if let Err(err) = save_map(runode_paths::Dirs::from_env().host_systems_file(), systems) {
        tracing::warn!("cannot save the host systems: {err}");
    }
    cx.refresh_windows();
}

/// 把 SSH 主机上的宿主装成登录服务办到了哪一步，按连接目标记。
#[derive(Clone, Debug)]
pub enum ServiceJob {
    Running,
    /// 装好了；`moved` 为假时原来的宿主还有会话、接着跑，它退出后才由服务起。
    Done {
        moved: bool,
    },
    /// 没装上；`linger` 为真时是这个用户开不了 linger，要管理员开。
    Failed {
        linger: bool,
        message: String,
    },
}

#[derive(Default)]
struct ServiceJobs(HashMap<String, ServiceJob>);

impl Global for ServiceJobs {}

pub fn service_job(name: &str, cx: &App) -> Option<ServiceJob> {
    cx.try_global::<ServiceJobs>()?.0.get(name).cloned()
}

fn set_service_job(name: &str, job: ServiceJob, cx: &mut App) {
    cx.default_global::<ServiceJobs>().0.insert(name.to_owned(), job);
    hosts_changed(cx);
}

/// 在后台把 SSH 主机 `name` 上的宿主装成登录服务；`report` 时没办成（或还要等）在最前面的窗口上说。
pub fn install_service(name: &str, report: bool, cx: &mut App) {
    let Some(host) = ssh::find(name) else {
        return;
    };
    if matches!(service_job(name, cx), Some(ServiceJob::Running)) {
        return;
    }
    set_service_job(name, ServiceJob::Running, cx);
    let name = name.to_owned();
    let task = cx.background_spawn(async move { host.install_service() });
    cx.spawn(async move |cx| {
        let job = match task.await {
            Ok(service) => ServiceJob::Done { moved: service.moved },
            Err(failure) => {
                tracing::warn!("cannot make the host on {name} a login service: {failure:?}");
                ServiceJob::Failed { linger: failure.code == runode_ssh::bridge::LINGER, message: failure.message }
            }
        };
        cx.update(|cx| {
            if report && !matches!(job, ServiceJob::Done { moved: true }) {
                tell(&rust_i18n::t!("ssh.service_title"), &service_job_text(&job), cx);
            }
            set_service_job(&name, job, cx);
        });
    })
    .detach();
}

/// 装登录服务的结果在界面上的说法。
pub fn service_job_text(job: &ServiceJob) -> String {
    match job {
        ServiceJob::Running => rust_i18n::t!("ssh.service_running").into_owned(),
        ServiceJob::Done { moved: true } => rust_i18n::t!("ssh.service_done").into_owned(),
        ServiceJob::Done { moved: false } => rust_i18n::t!("ssh.service_later").into_owned(),
        ServiceJob::Failed { linger: true, .. } => rust_i18n::t!("ssh.service_linger").into_owned(),
        ServiceJob::Failed { message, .. } => {
            rust_i18n::t!("ssh.service_failed", error = message.as_str()).into_owned()
        }
    }
}

/// 清理 SSH 主机上的旧构建办到了哪一步，按连接目标记。
#[derive(Clone, Debug)]
pub enum PruneJob {
    Running,
    Done { count: usize, kib: u64 },
    Failed(String),
}

#[derive(Default)]
struct PruneJobs(HashMap<String, PruneJob>);

impl Global for PruneJobs {}

pub fn prune_job(name: &str, cx: &App) -> Option<PruneJob> {
    cx.try_global::<PruneJobs>()?.0.get(name).cloned()
}

fn set_prune_job(name: &str, job: PruneJob, cx: &mut App) {
    cx.default_global::<PruneJobs>().0.insert(name.to_owned(), job);
    hosts_changed(cx);
}

/// 在后台删掉 SSH 主机 `name` 上用不着的 runode 构建（`RemoteHost::prune_builds`）。
pub fn prune_builds(name: &str, cx: &mut App) {
    let Some(host) = ssh::find(name) else {
        return;
    };
    if matches!(prune_job(name, cx), Some(PruneJob::Running)) {
        return;
    }
    set_prune_job(name, PruneJob::Running, cx);
    let name = name.to_owned();
    let task = cx.background_spawn(async move { host.prune_builds() });
    cx.spawn(async move |cx| {
        let job = match task.await {
            Ok((count, kib)) => PruneJob::Done { count, kib },
            Err(error) => {
                tracing::warn!("cannot remove old builds on {name}: {error}");
                PruneJob::Failed(error)
            }
        };
        cx.update(|cx| set_prune_job(&name, job, cx));
    })
    .detach();
}

/// 清理旧构建的结果在界面上的说法。
pub fn prune_job_text(job: &PruneJob) -> String {
    match job {
        PruneJob::Running => rust_i18n::t!("ssh.prune_running").into_owned(),
        PruneJob::Done { count: 0, .. } => rust_i18n::t!("ssh.prune_none").into_owned(),
        PruneJob::Done { count, kib } => {
            let size = crate::dictation::format_size(kib * 1024);
            rust_i18n::t!("ssh.prune_done", count = count, size = size.as_str()).into_owned()
        }
        PruneJob::Failed(error) => rust_i18n::t!("ssh.prune_failed", error = error.as_str()).into_owned(),
    }
}

/// 在最前面的窗口上说一句，只有「好」一个按钮。
fn tell(title: &str, detail: &str, cx: &mut App) {
    let Some(front) = window::front_window(cx) else {
        return;
    };
    let ok = rust_i18n::t!("ssh.ok");
    let _ = front.update(cx, |_, window, cx| {
        crate::prompt::show(window, PromptLevel::Info, title, Some(detail), &[ok.as_ref()], cx)
    });
}

/// SSH 主机 `name` 的宿主会跟着 ssh 的登录被结束：问用户要不要装成登录服务。
fn offer_service(name: &str, cx: &mut App) {
    let Some(front) = window::front_window(cx) else {
        return;
    };
    let host = display_name(&HostId::Ssh(name.to_owned()), cx);
    let title = rust_i18n::t!("ssh.bound_title", host = host).into_owned();
    let detail = rust_i18n::t!("ssh.bound_detail");
    let (install, later) = (rust_i18n::t!("ssh.bound_install"), rust_i18n::t!("ssh.bound_later"));
    let answers: [&str; 2] = [&install, &later];
    let Ok(answer) = front.update(cx, |_, window, cx| {
        crate::prompt::show(window, PromptLevel::Warning, &title, Some(&detail), &answers, cx)
    }) else {
        return;
    };
    let name = name.to_owned();
    cx.spawn(async move |cx| {
        if answer.await == Ok(0) {
            cx.update(|cx| install_service(&name, true, cx));
        }
    })
    .detach();
}

/// 在后台断开远端主机 `host`。
pub fn disconnect(host: &HostId, cx: &mut App) {
    let host = host.clone();
    cx.background_spawn(async move { hosts::disconnect(&host) }).detach();
}

/// 在后台忘掉远端主机 `host`；只该在没有工作区、终端用着它时调。
/// 配对的电脑是解除配对（`paired::unpair`）：那边没撤销成时在最前面的窗口上说一声，请用户到那台电脑上撤销。
pub fn forget(host: &HostId, cx: &mut App) {
    crate::project_host::forget(host);
    let HostId::Paired(id) = host else {
        let host = host.clone();
        cx.background_spawn(async move { hosts::forget(&host) }).detach();
        return;
    };
    let name = display_name(host, cx);
    let id = id.clone();
    let task = cx.background_spawn(async move { paired::unpair(&id) });
    cx.spawn(async move |cx| {
        let unpaired = task.await;
        cx.update(|cx| {
            let detail = match unpaired {
                Ok(()) => None,
                Err(paired::UnpairError::NotRevoked(error)) => {
                    tracing::warn!("{name} did not revoke this computer: {error}");
                    Some(rust_i18n::t!("paired.not_revoked", host = name.as_str(), error = error).into_owned())
                }
                Err(paired::UnpairError::Local(error)) => {
                    tracing::warn!("cannot forget {name}: {error}");
                    Some(rust_i18n::t!("paired.forget_failed", host = name.as_str(), error = error).into_owned())
                }
            };
            if let Some(detail) = detail {
                tell(&rust_i18n::t!("paired.forget_title"), &detail, cx);
            }
            hosts_changed(cx);
        });
    })
    .detach();
}

/// 开始收远端主机的事件，app 退出时让各台主机的主连接退出、取消正在拨的配对的电脑。要在恢复窗口之前调。
pub fn install(cx: &mut App) {
    let dirs = runode_paths::Dirs::from_env();
    cx.set_global(HostNames(load_map(dirs.host_names_file())));
    cx.set_global(HostSystems(load_map(dirs.host_systems_file())));
    cx.on_app_quit(|_| {
        ssh::close_masters();
        paired::stop_dialing();
        async {}
    })
    .detach();
    let Some(mut events) = hosts::events() else {
        return;
    };
    cx.spawn(async move |cx| {
        let mut seen = Seen::default();
        while let Some(event) = events.next().await {
            cx.update(|cx| handle(event, &mut seen, cx));
        }
    })
    .detach();
}

/// 收事件时记着的主机。
#[derive(Default)]
struct Seen {
    /// 在办它的 `Link` 转来的请求。
    served: HashSet<HostId>,
    /// 问过要不要装成登录服务。
    offered: HashSet<HostId>,
}

fn handle(event: RemoteEvent, seen: &mut Seen, cx: &mut App) {
    match event {
        RemoteEvent::Connected { host, alive } => {
            // 连上前刚被忘掉的主机不再建出来（`hosts::link_of` 会新建）。
            if !hosts::remotes().contains(&host) {
                return;
            }
            let status = hosts::status_of(&host);
            if let Status::Connected(connected) = &status {
                remember_system(&host, &connected.os, cx);
            }
            let bound = matches!(status, Status::Connected(connected) if connected.session_bound);
            if let HostId::Ssh(name) = &host
                && bound
                && service_job(name, cx).is_none()
                && seen.offered.insert(host.clone())
            {
                offer_service(name, cx);
            }
            if seen.served.insert(host.clone()) {
                window::serve(host.clone(), hosts::link_of(&host), cx);
            }
            terminal_view::remote_reconnected(host, alive, cx);
        }
        RemoteEvent::Status(host) => {
            for front in window::windows(cx) {
                front.update(cx, |view, _, cx| view.remote_status_changed(&host, cx)).ok();
            }
            window::status_bar::redraw_status_bars(cx);
            hosts_changed(cx);
        }
        // 再用这台主机时是新的一条 `Link`，连上时要重新办它转来的请求。
        RemoteEvent::Forgotten(host) => {
            seen.served.remove(&host);
            if let HostId::Ssh(name) = &host {
                if cx.has_global::<ServiceJobs>() {
                    cx.global_mut::<ServiceJobs>().0.remove(name);
                }
                if cx.has_global::<PruneJobs>() {
                    cx.global_mut::<PruneJobs>().0.remove(name);
                }
            }
            window::status_bar::redraw_status_bars(cx);
            hosts_changed(cx);
        }
        // 问在最前面的窗口上；一个窗口都没有时丢掉，算取消。
        RemoteEvent::Ask(ask) => match window::front_window(cx) {
            Some(front) => {
                front.update(cx, |view, window, cx| view.show_ask(ask, window, cx)).ok();
            }
            None => tracing::info!("no window to ask {:?} for ssh to {}", ask.prompt, ask.host),
        },
    }
}

/// 连接状态在界面上的说法。
pub fn status_text(status: &Status) -> String {
    let percent = |done: u64, total: u64| (done * 100).checked_div(total).map(|p| format!("{p}%")).unwrap_or_default();
    match status {
        Status::Idle => rust_i18n::t!("ssh.status_idle").into_owned(),
        Status::Connecting(Phase::Authenticating) => rust_i18n::t!("ssh.status_authenticating").into_owned(),
        Status::Connecting(Phase::Probing) => rust_i18n::t!("ssh.status_probing").into_owned(),
        Status::Connecting(Phase::Downloading { done, total }) => {
            rust_i18n::t!("ssh.status_downloading", percent = percent(*done, *total)).into_owned()
        }
        Status::Connecting(Phase::Uploading { done, total }) => {
            rust_i18n::t!("ssh.status_uploading", percent = percent(*done, *total)).into_owned()
        }
        Status::Connecting(Phase::Starting) => rust_i18n::t!("ssh.status_starting").into_owned(),
        Status::Connected(_) => rust_i18n::t!("ssh.status_connected").into_owned(),
        Status::Waiting { attempt, error, .. } => {
            rust_i18n::t!("ssh.status_waiting", attempt = attempt, error = error.as_str()).into_owned()
        }
        Status::Stopped { error, .. } => rust_i18n::t!("ssh.status_stopped", error = error.as_str()).into_owned(),
    }
}
