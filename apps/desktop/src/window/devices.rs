//! 经远程访问配对过的设备，连着的标出来，手机端引导页列出它们，撤销前问过用户。
//! 设备表和连着哪些设备（监听方写在状态文件里的 `ListenerStatus::connected`）都是数据目录里的文件，
//! 监听不一定开在这个进程里，所以每隔 `DEVICES_POLL` 读一次。

use std::time::Duration;

use gpui::{App, Context, Global, PromptLevel, SharedString, Window};
use runode_protocol::remote::DeviceId;

use super::WindowView;

/// 隔多久读一次配对过的设备和连着哪些。
const DEVICES_POLL: Duration = Duration::from_secs(2);

/// 配对过的设备，按配对的先后。
#[derive(PartialEq)]
struct Devices(Vec<Paired>);

impl Global for Devices {}

#[derive(Clone, PartialEq)]
pub(super) struct Paired {
    pub(super) id: DeviceId,
    pub(super) name: SharedString,
    /// 配对的时刻，Unix 秒。
    pub(super) paired_at: u64,
    /// 现在连着。
    pub(super) live: bool,
}

/// 在后台一直跟着配对过的设备和连着哪些，变了就重画各窗口。
pub fn watch(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            let devices = cx.background_executor().spawn(async { read_devices() }).await;
            cx.update(|cx| {
                if cx.try_global::<Devices>() != Some(&devices) {
                    cx.set_global(devices);
                    cx.refresh_windows();
                }
            });
            cx.background_executor().timer(DEVICES_POLL).await;
        }
    })
    .detach();
}

/// 读设备表和监听方的状态。读不了设备表时当没有设备；监听没开时都没连着。
fn read_devices() -> Devices {
    let dirs = runode_paths::Dirs::from_env();
    let connected = runode_remote_access::listener_status(&dirs).ok().flatten().map(|status| status.connected);
    let devices = runode_remote_access::list_devices(&dirs).unwrap_or_default();
    Devices(
        devices
            .into_iter()
            .map(|device| Paired {
                id: device.device_id,
                live: connected.as_ref().is_some_and(|ids| ids.contains(&device.device_id)),
                name: device.name.into(),
                paired_at: device.paired_at,
            })
            .collect(),
    )
}

/// 配对过的设备，按配对的先后；启动后第一次读到之前是空的。
pub(super) fn devices(cx: &App) -> &[Paired] {
    cx.try_global::<Devices>().map_or(&[], |devices| &devices.0)
}

impl WindowView {
    /// 问用户要不要撤销设备，撤销了马上重读设备表；它连着的连接几秒内断开。
    pub(super) fn revoke_device(&mut self, id: DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(device) = devices(cx).iter().find(|device| device.id == id) else { return };
        let title = rust_i18n::t!("machine.revoke_title", device = device.name);
        let detail = rust_i18n::t!("machine.revoke_detail");
        let answers = [rust_i18n::t!("machine.revoke"), rust_i18n::t!("machine.cancel")];
        let answers: Vec<&str> = answers.iter().map(AsRef::as_ref).collect();
        let answer = window.prompt(PromptLevel::Warning, &title, Some(&detail), &answers, cx);
        cx.spawn(async move |_, cx| {
            if answer.await != Ok(0) {
                return;
            }
            cx.update(|cx| {
                if let Err(err) = runode_remote_access::revoke_device(&runode_paths::Dirs::from_env(), id) {
                    tracing::warn!("cannot revoke remote device {id}: {err}");
                }
                cx.set_global(read_devices());
                cx.refresh_windows();
            });
        })
        .detach();
    }
}
