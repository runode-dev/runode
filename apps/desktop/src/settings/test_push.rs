//! 远程访问那一页的测试推送：按下去马上给登记过的手机推一条通知
//! （`runode_remote::send_test_push`），看推送的设置和密钥对不对。用当前配置里的推送设置，
//! 不管推送开没开，也不管监听开在哪个进程里。

use crate::ui::theme::Theme;
use gpui::{AppContext as _, Context, Div, Task};

use super::{
    SettingsView,
    controls::{Press, row},
};
use crate::ui::a11y::Disable as _;
use crate::ui::button::{Button, ButtonVariant};

const ERROR_ID: &str = "test-push";
/// 测试推送用到的配置项，按下时都要已经写回。
const APNS_KEYS: [&str; 4] = ["apns-key-file", "apns-key-id", "apns-team-id", "apns-bundle-id"];

#[derive(Default)]
pub(super) enum State {
    #[default]
    Idle,
    Sending {
        _task: Task<()>,
    },
    /// 推到了几台。
    Sent(usize),
}

impl SettingsView {
    pub(super) fn render_test_push(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let sending = matches!(self.test_push, State::Sending { .. });
        let label = if sending { "settings.test_push.sending" } else { "settings.test_push.send" };
        let send = Button::new("test-push", rust_i18n::t!(label).into_owned())
            .variant(ButtonVariant::Outline)
            .aria_disabled(sending)
            .on_press(cx, |this, _, cx| this.send_test_push(cx));
        let hint = match self.test_push {
            State::Sent(count) => rust_i18n::t!("settings.test_push.sent", count = count),
            _ => rust_i18n::t!("settings.test_push.hint"),
        };
        let title = rust_i18n::t!("settings.test_push.title").into_owned();
        row(title, Some(hint.into_owned().into()), send, None, self.errors.get(ERROR_ID).cloned(), theme)
    }

    fn send_test_push(&mut self, cx: &mut Context<Self>) {
        if matches!(self.test_push, State::Sending { .. }) {
            return;
        }
        self.errors.remove(ERROR_ID);
        // 刚改的密钥几项可能还没写回（停手计时没到、按下时焦点还在输入框里）：先写回，写不进去的不测。
        self.commit_all(cx);
        if APNS_KEYS.iter().any(|key| self.errors.contains_key(*key)) {
            cx.notify();
            return;
        }
        let dirs = runode_paths::Dirs::from_env();
        // 写回时配置马上重载了，`self.config` 要等观察者回调才换，所以读全局的。
        let settings = crate::remote_access::push_settings(&cx.global::<crate::config::AppConfig>().0);
        let content = runode_remote::TestPush {
            title: rust_i18n::t!("settings.test_push.push_title").into_owned(),
            lines: vec![rust_i18n::t!("settings.test_push.push_line").into_owned()],
        };
        let job = cx.background_spawn(async move { runode_remote::send_test_push(&dirs, &settings, &content) });
        self.test_push = State::Sending {
            _task: cx.spawn(async move |this, cx| {
                let result = job.await;
                this.update(cx, |this, cx| {
                    this.test_push = match result {
                        Ok(0) => {
                            this.errors
                                .insert(ERROR_ID.into(), rust_i18n::t!("settings.test_push.no_phone").into_owned());
                            State::Idle
                        }
                        Ok(count) => State::Sent(count),
                        Err(err) => {
                            this.errors.insert(
                                ERROR_ID.into(),
                                rust_i18n::t!("settings.test_push.failed", err = err).into_owned(),
                            );
                            State::Idle
                        }
                    };
                    cx.notify();
                })
                .ok();
            }),
        };
        cx.notify();
    }
}
