//! 引导页的第 3 步：把编进这一版的 skill 装给 agent，macOS 上另有一张系统权限的卡片，点进管理权限那一屏。
//! 家目录可能在 NFS 上，读写都放在后台，不在界面线程上等。

use std::io;

use gpui::{AnyElement, Context, Div, FontWeight, Role, SharedString, Stateful, div, prelude::*, px, svg};
use runode_agent_skill::{Env, State};

#[cfg(target_os = "macos")]
use super::Step;
use super::{Onboarding, text_label};
#[cfg(not(target_os = "macos"))]
use crate::ui::font_size::scaled;
#[cfg(target_os = "macos")]
use crate::ui::icons::MONITOR_ICON;
use crate::ui::{
    a11y::{Press as _, StaticText as _},
    badge::{Badge, BadgeVariant},
    button::{Button, ButtonVariant},
    font_size::FontSize,
    icons::{CHECK_ICON, SPARKLE_ICON},
    theme::Theme,
};

/// skill 卡读状态时看的那个 skill。
const SKILL: &str = "runode";

/// skill 卡的状态。
#[derive(Clone, Default, PartialEq, Eq)]
enum SkillStatus {
    #[default]
    NotInstalled,
    Installing,
    /// 正本已经在了。刚由这一页装上时带着装上的结果，读出来的状态是空的。
    Installed(Installed),
    Failed(String),
}

/// 这一页装上 skill 的结果。
#[derive(Clone, Default, PartialEq, Eq)]
struct Installed {
    /// 另外给装好的 agent 的显示名。
    agents: Vec<&'static str>,
    /// 是用户自己管的链接、没动的正本，写成给人看的路径。
    left_as_is: Vec<String>,
}

/// 第 3 步的状态，跟着 `Onboarding` 活。
#[derive(Default)]
pub(super) struct Setup {
    skill: SkillStatus,
}

fn skill_env() -> Env {
    Env::from_dirs(&runode_paths::Dirs::from_env(), |key| std::env::var_os(key))
}

impl Onboarding {
    /// 后台读 skill 装没装，翻到这一步时读。正在装时不理读到的结果，刚装上时记下的结果也留着。
    pub(super) fn read_skill(&mut self, cx: &mut Context<Self>) {
        let job = cx.background_spawn(async { runode_agent_skill::state(&skill_env(), SKILL) });
        cx.spawn(async move |this, cx| {
            let state = job.await;
            this.update(cx, |this, cx| {
                let next = match (&this.setup.skill, state) {
                    (SkillStatus::Installing, _) => return,
                    (SkillStatus::Installed(_), State::Installed { .. } | State::Elsewhere) => return,
                    (_, State::Installed { .. } | State::Elsewhere) => SkillStatus::Installed(Installed::default()),
                    (_, State::Missing) => SkillStatus::NotInstalled,
                };
                if this.setup.skill != next {
                    this.setup.skill = next;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 后台装上编进这一版的 skill，装完离开了这一步也照样记下结果。
    fn install_skill(&mut self, cx: &mut Context<Self>) {
        self.setup.skill = SkillStatus::Installing;
        cx.notify();
        let job = cx.background_spawn(async {
            let env = skill_env();
            let mut installed = Installed::default();
            for report in runode_agent_skill::install_all(&env)? {
                if report.left_as_is {
                    let home = env.home.as_deref();
                    let path = home
                        .and_then(|home| {
                            runode_paths::home_relative(&report.canonical, home, std::path::MAIN_SEPARATOR)
                        })
                        .unwrap_or_else(|| report.canonical.display().to_string());
                    installed.left_as_is.push(path);
                }
                for (agent, _) in report.linked {
                    if !installed.agents.contains(&agent) {
                        installed.agents.push(agent);
                    }
                }
            }
            io::Result::Ok(installed)
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.setup.skill = match result {
                    Ok(installed) => SkillStatus::Installed(installed),
                    Err(err) => SkillStatus::Failed(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 标题下面的内容：一张卡片，左边 skill，macOS 上右边隔一条竖线是权限。
    pub(super) fn render_setup(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let card = div()
            .w_full()
            .rounded(px(12.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .flex()
            .child(self.render_skill(theme, cx));
        #[cfg(target_os = "macos")]
        let card =
            card.child(div().flex_none().w(px(1.)).bg(theme.border)).child(self.render_permissions_card(theme, cx));
        #[cfg(not(target_os = "macos"))]
        let card = div().flex().justify_center().child(card.max_w(scaled(380.)));
        card.into_any_element()
    }

    fn render_skill(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let installed = matches!(self.setup.skill, SkillStatus::Installed(_));
        let badge = if installed {
            status_badge("onboarding-skill-status", rust_i18n::t!("onboarding.skill_installed").into(), true)
        } else {
            status_badge("onboarding-skill-status", rust_i18n::t!("onboarding.skill_recommended").into(), false)
        };
        let (agents, left_as_is) = match &self.setup.skill {
            SkillStatus::Installed(installed) => (&installed.agents[..], &installed.left_as_is[..]),
            _ => (&[][..], &[][..]),
        };
        let linked = (!agents.is_empty()).then(|| {
            let agents = agents.join(&rust_i18n::t!("onboarding.separator"));
            let text = rust_i18n::t!("onboarding.skill_linked", agents = agents).into_owned();
            note("onboarding-skill-linked", text, body_color(theme))
        });
        let left_as_is = (!left_as_is.is_empty()).then(|| {
            let lines: Vec<String> = left_as_is
                .iter()
                .map(|path| rust_i18n::t!("skill_install.left_as_is", path = path).into_owned())
                .collect();
            note("onboarding-skill-left-as-is", lines.join("\n"), body_color(theme))
        });
        let error = match &self.setup.skill {
            SkillStatus::Failed(err) => {
                let text = rust_i18n::t!("onboarding.skill_failed", err = err).into_owned();
                Some(note("onboarding-skill-error", text, theme.destructive))
            }
            _ => None,
        };
        let busy = self.setup.skill == SkillStatus::Installing;
        let button = (!installed).then(|| {
            let label = if busy {
                rust_i18n::t!("onboarding.skill_installing")
            } else {
                rust_i18n::t!("onboarding.skill_install")
            };
            Button::new("onboarding-skill-install", label.into_owned())
                .variant(ButtonVariant::Secondary)
                .disabled(busy)
                .when(!busy, |button| button.cursor_pointer().on_press(cx, |this, _, cx| this.install_skill(cx)))
        });
        column()
            .child(header(
                "onboarding-skill-title",
                SPARKLE_ICON,
                rust_i18n::t!("onboarding.skill_title").into_owned(),
                badge,
                theme,
            ))
            .child(note(
                "onboarding-skill-body",
                rust_i18n::t!("onboarding.skill_body").into_owned(),
                body_color(theme),
            ))
            .children(left_as_is)
            .children(linked)
            .children(error)
            .children(button.map(footer))
    }

    /// 系统权限的卡片：开了几项，没全开时按钮点进管理权限那一屏。
    #[cfg(target_os = "macos")]
    fn render_permissions_card(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let (granted, total) = self.permissions.read(cx).count();
        let id = "onboarding-permissions-status";
        let badge = match granted {
            0 => status_badge(id, rust_i18n::t!("onboarding.permissions_none").into(), false),
            n if n == total => status_badge(id, rust_i18n::t!("onboarding.permissions_all").into(), true),
            n => {
                let text = rust_i18n::t!("onboarding.permissions_some", n = n.to_string(), total = total.to_string());
                status_badge(id, text.into(), false)
            }
        };
        let button = (granted < total).then(|| {
            Button::new("onboarding-grant", rust_i18n::t!("onboarding.grant").into_owned())
                .variant(ButtonVariant::Secondary)
                .cursor_pointer()
                .on_press(cx, |this, _, cx| this.go(Step::Permissions, cx))
        });
        column()
            .child(header(
                "onboarding-permissions-title",
                MONITOR_ICON,
                rust_i18n::t!("onboarding.permissions_title").into_owned(),
                badge,
                theme,
            ))
            .child(note(
                "onboarding-permissions-body",
                rust_i18n::t!("onboarding.permissions_body").into_owned(),
                body_color(theme),
            ))
            .children(button.map(footer))
    }
}

/// 卡片里的一栏。
fn column() -> Div {
    div().flex_1().min_w_0().p(px(20.)).flex().flex_col().gap(px(12.))
}

/// 一栏顶上一行：图标、标题，右边徽标。
fn header(id: &'static str, icon: &'static str, title: String, badge: Badge, theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(svg().path(icon).flex_none().size(px(18.)).text_color(theme.foreground))
        .child(
            div()
                .id(id)
                .role(Role::Heading)
                .aria_level(2)
                .aria_label(title.clone())
                .flex_1()
                .min_w_0()
                .text_size(FontSize::Large)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(badge)
}

/// 标题右边的状态徽标，文字报给辅助工具；`done` 时是带对勾的成功色。
fn status_badge(id: &'static str, text: SharedString, done: bool) -> Badge {
    let badge = Badge::new(text.clone()).id(id).aria_static_text(text);
    if done { badge.variant(BadgeVariant::Success).icon(CHECK_ICON) } else { badge.variant(BadgeVariant::Secondary) }
}

fn body_color(theme: &Theme) -> gpui::Hsla {
    theme.foreground.opacity(0.6)
}

/// 栏里的一段字。
fn note(id: &'static str, text: String, color: gpui::Hsla) -> Stateful<Div> {
    text_label(id, text, color).text_size(FontSize::Body)
}

/// 栏底的按钮，排在最下面，几栏的按钮对齐。
fn footer(button: Button) -> Div {
    div().mt_auto().pt(px(4.)).flex().child(button)
}
