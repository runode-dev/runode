//! 新建工作区对话框里的「Runode 配对」：粘贴另一台电脑「手机端」页上复制来的配对链接，认出是配对链接就配对，
//! 配上了选中这台电脑、接着浏览它的文件夹；下面列出局域网里找到的、开着远程访问还没配对过的电脑。

use std::time::{SystemTime, UNIX_EPOCH};

use futures::{StreamExt as _, channel::mpsc::unbounded};
use gpui::{
    Action, AnyElement, App, Context, Div, Entity, FocusHandle, Focusable as _, Role, SharedString, Subscription,
    Window, div, prelude::*, px, svg,
};
use runode_protocol::remote::{PAIRING_URI_PREFIX, PairingUri, RejectReason};
use runode_remote::client::{self, Browser, DialError, Found};

use super::Source;
use crate::window::WindowView;
use crate::{
    host_client::{hosts::HostId, paired},
    ui::{
        a11y::Press,
        button::{Button, ButtonVariant},
        font_size::{FontSize, scaled},
        icons::MONITOR_ICON,
        input::{Input, InputEvent, InputState},
        theme::ActiveTheme as _,
    },
};

/// 主机菜单里「添加远程主机」下的「Runode 配对」。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct AddPairedMachine;

/// 点了「Runode 配对」后展开的一栏。
pub(super) struct Pairing {
    field: Entity<InputState>,
    _events: Subscription,
    state: PairState,
    /// 局域网里找到的、开着远程访问的电脑，按名字排好。
    found: Vec<Found>,
    /// 开着的查找，这一栏收起时丢掉就停。
    _browser: Option<Browser>,
    /// 点了找到的哪台电脑，提示去那台上复制链接。
    picked: Option<String>,
    /// 第几次配对：收起再展开、换了链接以后，先前那次的结果不再算数。
    attempt: u64,
}

impl Pairing {
    pub(super) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

/// 配对办到了哪一步。
#[derive(Clone, Debug, PartialEq, Eq)]
enum PairState {
    Idle,
    /// 在和 `host` 配对。
    Pairing {
        host: String,
    },
    /// 没配上，原因写给人看。
    Failed(String),
}

impl WindowView {
    /// 展开「Runode 配对」一栏，焦点给链接框，开始在局域网里找开着远程访问的电脑。
    pub(super) fn add_paired_machine(&mut self, _: &AddPairedMachine, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        dialog.adding = None;
        if dialog.pairing.is_none() {
            let field = cx.new(|cx| {
                InputState::new(String::new(), cx)
                    .with_placeholder(rust_i18n::t!("workspace.pair_link_placeholder").into_owned())
                    .with_label(rust_i18n::t!("workspace.pair_link").into_owned())
            });
            let events = cx.subscribe_in(&field, window, |this, field, event: &InputEvent, window, cx| match event {
                // 粘贴进来就认，不用再按回车。
                InputEvent::Changed(text) => {
                    if text.trim().starts_with(PAIRING_URI_PREFIX) {
                        this.pair_with(text.trim().to_owned(), window, cx);
                    } else if let Some(pairing) = this.pairing_mut() {
                        pairing.state = PairState::Idle;
                        cx.notify();
                    }
                }
                InputEvent::Next => {
                    let text = field.read(cx).query().trim().to_owned();
                    this.pair_with(text, window, cx);
                }
                InputEvent::Dismiss => this.close_new_workspace(window, cx),
                InputEvent::Previous => {}
            });
            let browser = self.browse_machines(cx);
            let dialog = self.new_workspace.as_mut().expect("the dialog is open");
            dialog.pairing = Some(Pairing {
                field,
                _events: events,
                state: PairState::Idle,
                found: Vec::new(),
                _browser: browser,
                picked: None,
                attempt: 0,
            });
        }
        if let Some(pairing) = self.new_workspace.as_ref().and_then(|dialog| dialog.pairing.as_ref()) {
            window.focus(&pairing.field.focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn pairing_mut(&mut self) -> Option<&mut Pairing> {
        self.new_workspace.as_mut().and_then(|dialog| dialog.pairing.as_mut())
    }

    /// 在局域网里找开着远程访问的电脑，找到的列表变了就换上；找不了时不列。
    fn browse_machines(&mut self, cx: &mut Context<Self>) -> Option<Browser> {
        let (sender, mut found) = unbounded::<Vec<Found>>();
        let browser = match client::browse(move |list| {
            let _ = sender.unbounded_send(list);
        }) {
            Ok(browser) => browser,
            Err(err) => {
                tracing::info!("cannot look for computers on the local network: {err}");
                return None;
            }
        };
        cx.spawn(async move |this, cx| {
            while let Some(mut list) = found.next().await {
                list.sort_by_key(|found| found.name.to_lowercase());
                let alive = this.update(cx, |this, cx| {
                    if let Some(pairing) = this.pairing_mut() {
                        pairing.found = list;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        })
        .detach();
        Some(browser)
    }

    /// 收起「Runode 配对」一栏，焦点还给对话框。
    fn cancel_pairing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.new_workspace
            && dialog.pairing.take().is_some()
        {
            window.focus(&dialog.focus_handle(), cx);
            cx.notify();
        }
    }

    /// 用链接 `text` 配对；认不出、过期了的当场说明，不去连。配上了选中这台电脑，接着浏览它的文件夹。
    fn pair_with(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pairing) = self.pairing_mut() else {
            return;
        };
        if matches!(pairing.state, PairState::Pairing { .. }) || text.is_empty() {
            return;
        }
        let uri = match text.parse::<PairingUri>() {
            Ok(uri) => uri,
            Err(err) => {
                tracing::info!("not a pairing link: {err}");
                let key =
                    if text.starts_with(PAIRING_URI_PREFIX) { "paired.link_broken" } else { "paired.link_invalid" };
                pairing.state = PairState::Failed(rust_i18n::t!(key).into_owned());
                cx.notify();
                return;
            }
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        if uri.expires_at <= now {
            pairing.state = PairState::Failed(rust_i18n::t!("paired.link_expired").into_owned());
            cx.notify();
            return;
        }
        pairing.attempt += 1;
        let attempt = pairing.attempt;
        let host = uri.host_name.clone();
        pairing.state = PairState::Pairing { host: host.clone() };
        cx.notify();
        let device = runode_remote::host_name();
        let job = cx.background_spawn(async move { paired::pair(&uri, &device) });
        cx.spawn_in(window, async move |this, cx| {
            let paired = job.await;
            this.update_in(cx, |this, window, cx| {
                let Some(pairing) = this.pairing_mut().filter(|pairing| pairing.attempt == attempt) else {
                    return;
                };
                match paired {
                    Ok(id) => {
                        tracing::info!("paired with {host} ({id})");
                        crate::remote_hosts::machines_changed(cx);
                        this.set_workspace_host(HostId::Paired(id), window, cx);
                        this.show_source(Source::Browse, window, cx);
                    }
                    Err(err) => {
                        tracing::warn!("cannot pair with {host}: {err:?}");
                        pairing.state = PairState::Failed(pair_error_text(&err, &host));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// 「Runode 配对」展开后的一栏：链接框、从哪里拿链接的说明、配对的进度或没配上的原因，下面是局域网里找到的
    /// 还没配对过的电脑。
    pub(super) fn render_pairing(&self, pairing: &Pairing, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let field_bg = cx.theme().mix(0.03);
        let hover_bg = cx.theme().accent;
        let busy = matches!(pairing.state, PairState::Pairing { .. });
        let pair = Button::new("pair-connect", rust_i18n::t!("workspace.pair_connect"))
            .variant(ButtonVariant::Default)
            .h_auto()
            .py(px(5.))
            .cursor_pointer()
            .disabled(busy)
            .on_press(cx, |this, window, cx| {
                let text = this
                    .new_workspace
                    .as_ref()
                    .and_then(|dialog| dialog.pairing.as_ref())
                    .map(|pairing| pairing.field.read(cx).query().trim().to_owned())
                    .unwrap_or_default();
                this.pair_with(text, window, cx);
            });
        let cancel = Button::new("pair-cancel", rust_i18n::t!("workspace.host_add_cancel"))
            .variant(ButtonVariant::Ghost)
            .h_auto()
            .py(px(5.))
            .cursor_pointer()
            .on_press(cx, |this, window, cx| this.cancel_pairing(window, cx));
        let note = |text: String| div().text_size(FontSize::Small).text_color(fg.opacity(0.55)).child(text);
        // 进度和没配上的原因读给辅助工具：状态一变就念出来。
        let status = match &pairing.state {
            PairState::Idle => None,
            PairState::Pairing { host } => Some(rust_i18n::t!("workspace.pairing", host = host.as_str()).into_owned()),
            PairState::Failed(reason) => Some(reason.clone()),
        };
        let status = status.map(|text| {
            div()
                .id("pair-status")
                .role(Role::Status)
                .aria_label(text.clone())
                .text_color(fg.opacity(if matches!(pairing.state, PairState::Failed(_)) { 0.85 } else { 0.6 }))
                .child(text)
        });
        let known: Vec<[u8; 32]> = paired::machines().iter().map(|machine| machine.fingerprint).collect();
        let found: Vec<AnyElement> = pairing
            .found
            .iter()
            .filter(|found| !known.contains(&found.fingerprint))
            .map(|found| {
                let name = found.name.clone();
                let id = SharedString::from(format!(
                    "pair-found-{}",
                    runode_protocol::remote::encode_base64url(&found.fingerprint)
                ));
                let picked = pairing.picked.as_deref() == Some(name.as_str());
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(name.clone())
                    .aria_description(rust_i18n::t!("workspace.pair_found_hint").into_owned())
                    .aria_selected(picked)
                    .flex_none()
                    .h(scaled(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(picked, |row| row.bg(hover_bg))
                    .hover(move |row| row.bg(hover_bg))
                    .child(svg().flex_none().path(MONITOR_ICON).size(px(14.)).text_color(fg.opacity(0.6)))
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .on_press(cx, move |this, _, cx| {
                        if let Some(pairing) = this.pairing_mut() {
                            pairing.picked = Some(name.clone());
                            cx.notify();
                        }
                    })
                    .into_any_element()
            })
            .collect();
        let picked = pairing.picked.as_ref().map(|name| {
            let text = rust_i18n::t!("workspace.pair_found_picked", host = name.as_str()).into_owned();
            div().id("pair-picked").role(Role::Status).aria_label(text.clone()).text_color(fg.opacity(0.7)).child(text)
        });
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Input::new(&pairing.field).flex_1().min_w_0().bg(field_bg))
                    .child(pair)
                    .child(cancel),
            )
            .child(note(rust_i18n::t!("workspace.pair_detail").into_owned()))
            .children(status)
            .when(!found.is_empty(), |body| {
                body.child(note(rust_i18n::t!("workspace.pair_found").into_owned()))
                    .child(div().id("pair-found").role(Role::List).flex().flex_col().children(found))
                    .children(picked)
            })
    }
}

/// 没配上的原因写给人看；`host` 是链接里那台电脑的名字。
fn pair_error_text(err: &DialError, host: &str) -> String {
    match err {
        DialError::Rejected(RejectReason::PairingInvalid) => rust_i18n::t!("paired.link_used").into_owned(),
        DialError::Expired => rust_i18n::t!("paired.link_expired").into_owned(),
        DialError::Unreachable { last } => {
            rust_i18n::t!("paired.pair_unreachable", host = host, reason = last.as_str()).into_owned()
        }
        DialError::FingerprintMismatch => rust_i18n::t!("paired.pair_fingerprint", host = host).into_owned(),
        DialError::Rejected(RejectReason::RateLimited) => rust_i18n::t!("paired.rate_limited").into_owned(),
        DialError::Rejected(RejectReason::Disabled) => rust_i18n::t!("paired.pair_disabled", host = host).into_owned(),
        DialError::Rejected(_) => rust_i18n::t!("paired.rejected").into_owned(),
        DialError::Protocol(reason) => rust_i18n::t!("paired.protocol", reason = reason).into_owned(),
        DialError::Io(err) => rust_i18n::t!("paired.pair_failed", reason = err.to_string()).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use runode_protocol::remote::RejectReason;
    use runode_remote::client::DialError;

    use super::pair_error_text;

    /// 用过的、过期的链接和指纹对不上、连不上各有各的说法，都带上那台电脑的名字或原因。
    #[test]
    fn pairing_failures_say_what_went_wrong() {
        let text = |err: DialError| pair_error_text(&err, "Mac mini");
        let used = text(DialError::Rejected(RejectReason::PairingInvalid));
        let expired = text(DialError::Expired);
        let fingerprint = text(DialError::FingerprintMismatch);
        let unreachable = text(DialError::Unreachable { last: "timed out".into() });
        assert!(fingerprint.contains("Mac mini"));
        assert!(unreachable.contains("Mac mini") && unreachable.contains("timed out"));
        let all = [&used, &expired, &fingerprint, &unreachable];
        for (ix, a) in all.iter().enumerate() {
            assert!(all[ix + 1..].iter().all(|b| a != b), "{a} is said twice");
        }
    }
}
