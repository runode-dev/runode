//! 经 SSH 连的远端主机在界面这边要办的事：连上一台主机后开始办那台机器上的命令行转来的请求，把那台
//! 主机上断开着的终端接回来；ssh 要问用户时交给最前面的窗口弹框；连接状态一变就更新全局的 `HostsChanged`。
//! 连上的宿主会跟着 ssh 的登录被结束时，这次运行里每台主机问一次要不要把它装成登录服务（`install_service`）。

use std::collections::{HashMap, HashSet};

use futures::StreamExt as _;
use gpui::{App, AppContext as _, Global, PromptLevel};

use crate::{
    host_client::remote::{self, HostId, Phase, RemoteEvent, Status},
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

/// 把远端宿主装成登录服务办到了哪一步，按主机名记。
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

/// 在后台把主机 `name` 上的宿主装成登录服务；`report` 时没办成（或还要等）在最前面的窗口上说。
pub fn install_service(name: &str, report: bool, cx: &mut App) {
    let Some(host) = remote::find(name) else {
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
                ServiceJob::Failed { linger: failure.code == "linger", message: failure.message }
            }
        };
        cx.update(|cx| {
            if report && !matches!(job, ServiceJob::Done { moved: true }) {
                tell(&service_job_text(&job), cx);
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

/// 清理远端旧构建办到了哪一步，按主机名记。
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

/// 在后台删掉主机 `name` 上用不着的 runode 构建（`RemoteHost::prune_builds`）。
pub fn prune_builds(name: &str, cx: &mut App) {
    let Some(host) = remote::find(name) else {
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
fn tell(detail: &str, cx: &mut App) {
    let Some(front) = window::front_window(cx) else {
        return;
    };
    let (title, ok) = (rust_i18n::t!("ssh.service_title"), rust_i18n::t!("ssh.ok"));
    let _ =
        front.update(cx, |_, window, cx| window.prompt(PromptLevel::Info, &title, Some(detail), &[ok.as_ref()], cx));
}

/// 主机 `name` 的宿主会跟着 ssh 的登录被结束：问用户要不要装成登录服务。
fn offer_service(name: &str, cx: &mut App) {
    let Some(front) = window::front_window(cx) else {
        return;
    };
    let title = rust_i18n::t!("ssh.bound_title", host = name);
    let detail = rust_i18n::t!("ssh.bound_detail");
    let (install, later) = (rust_i18n::t!("ssh.bound_install"), rust_i18n::t!("ssh.bound_later"));
    let answers: [&str; 2] = [&install, &later];
    let Ok(answer) =
        front.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(&detail), &answers, cx))
    else {
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

/// 在后台断开主机 `name`。
pub fn disconnect(name: &str, cx: &mut App) {
    let Some(host) = remote::find(name) else {
        return;
    };
    cx.background_spawn(async move { host.disconnect() }).detach();
}

/// 在后台忘掉主机 `name`；只该在没有工作区、终端用着它时调。
pub fn forget(name: &str, cx: &mut App) {
    let name = name.to_owned();
    crate::project_host::forget(&name);
    cx.background_spawn(async move { remote::forget(&name) }).detach();
}

/// 开始收远端主机的事件，app 退出时让各台主机的主连接退出。要在恢复窗口之前调。
pub fn install(cx: &mut App) {
    cx.on_app_quit(|_| {
        remote::close_masters();
        async {}
    })
    .detach();
    let Some(mut events) = remote::events() else {
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

/// 收事件时记着的主机名。
#[derive(Default)]
struct Seen {
    /// 在办它的 `Link` 转来的请求。
    served: HashSet<String>,
    /// 问过要不要装成登录服务。
    offered: HashSet<String>,
}

fn handle(event: RemoteEvent, seen: &mut Seen, cx: &mut App) {
    match event {
        RemoteEvent::Connected { host, alive } => {
            let bound = remote::find(&host).is_some_and(
                |remote| matches!(remote.status(), Status::Connected(connected) if connected.session_bound),
            );
            if bound && service_job(&host, cx).is_none() && seen.offered.insert(host.clone()) {
                offer_service(&host, cx);
            }
            if seen.served.insert(host.clone())
                && let Some(remote) = remote::remote(&host)
            {
                window::serve(HostId::Ssh(host.clone()), remote.link.clone(), cx);
            }
            terminal_view::remote_reconnected(HostId::Ssh(host), alive, cx);
        }
        RemoteEvent::Status(host) => {
            for front in window::windows(cx) {
                front.update(cx, |view, _, cx| view.remote_status_changed(&host, cx)).ok();
            }
            hosts_changed(cx);
        }
        // 再用这个名字时是新的一条 `Link`，连上时要重新办它转来的请求。
        RemoteEvent::Forgotten(host) => {
            seen.served.remove(&host);
            if cx.has_global::<ServiceJobs>() {
                cx.global_mut::<ServiceJobs>().0.remove(&host);
            }
            if cx.has_global::<PruneJobs>() {
                cx.global_mut::<PruneJobs>().0.remove(&host);
            }
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
