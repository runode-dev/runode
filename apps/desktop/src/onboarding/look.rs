//! 引导页第二步「外观」：上面选配色（自动跟着系统、浅色、深色），下面选主题，点了马上写进配置。
//! 主题卡按主题文件里的颜色画（`runode_config::theme_config`），建页时在后台读一遍记在
//! `Onboarding::themes` 里，界面线程上不读主题文件。

use std::collections::HashMap;

use gpui::{AnyElement, Context, Div, FontWeight, Role, SharedString, Stateful, div, prelude::*, px};
use runode_config::{DEFAULT_LIGHT_THEME, DEFAULT_THEME};
use runode_types::color::{Rgb, TerminalColor};

use super::{Onboarding, text_label};
use crate::{
    config::AppConfig,
    settings::{Look, PLAIN_DARK, PLAIN_LIGHT, ThemeChoice, ThemeSlot, preview},
    ui::{
        self,
        a11y::Press as _,
        font_size::{FontSize, scaled},
        hsla,
        theme::Theme,
    },
};

/// 下面一排列出的主题，先深色后浅色；都随 app 带着。
const BUILT_IN_THEMES: [&str; 14] = [
    "Catppuccin Mocha",
    "Dracula",
    "TokyoNight",
    "Nord",
    "Gruvbox Dark",
    "Rose Pine",
    "Kanagawa Wave",
    "GitHub Dark",
    "Catppuccin Latte",
    "GitHub Light Default",
    "Rose Pine Dawn",
    "Gruvbox Light",
    "TokyoNight Day",
    "Nord Light",
];

/// 配色的三档：浅色、深色外观各一个主题跟着系统换，还是只用一个浅色或深色的主题。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Auto,
    Light,
    Dark,
}

/// 一张主题卡要画的颜色。
struct ThemeCard {
    name: &'static str,
    bg: Rgb,
    fg: Rgb,
    cursor: Rgb,
    /// 调色板的前八个颜色。
    ansi: [Rgb; 8],
    light: bool,
}

impl ThemeCard {
    fn load(name: &'static str) -> Option<Self> {
        let config = runode_config::theme_config(name)?;
        let cursor = match config.cursor_color {
            Some(TerminalColor::Rgb(color)) => color,
            _ => config.foreground,
        };
        let palette = ui::theme::ansi_colors(&config.palette);
        let mut ansi = [Rgb(0, 0, 0); 8];
        ansi.copy_from_slice(&palette[..8]);
        Some(Self {
            name,
            bg: config.background,
            fg: config.foreground,
            cursor,
            ansi,
            light: ui::theme::is_light(config.foreground, config.background),
        })
    }
}

/// 读过的主题：卡片，配色缩略图用的颜色，和别的主题是不是浅色。
pub(super) struct Themes {
    default_light: Option<ThemeCard>,
    default_dark: Option<ThemeCard>,
    built_in: Vec<ThemeCard>,
    light_look: Look,
    dark_look: Look,
    /// 不在卡片里的主题是不是浅色；找不到的主题算深色。
    other: HashMap<String, bool>,
}

impl Themes {
    /// 读卡片上的主题，连同 `others` 里不在卡片上的主题的深浅。读主题文件，不在界面线程上调。
    fn load(others: &[String]) -> Self {
        let look = |name: &str, plain: Look| runode_config::theme_config(name).as_ref().map_or(plain, Look::of);
        let mut themes = Self {
            default_light: ThemeCard::load(DEFAULT_LIGHT_THEME),
            default_dark: ThemeCard::load(DEFAULT_THEME),
            built_in: BUILT_IN_THEMES.into_iter().filter_map(ThemeCard::load).collect(),
            light_look: look(DEFAULT_LIGHT_THEME, PLAIN_LIGHT),
            dark_look: look(DEFAULT_THEME, PLAIN_DARK),
            other: HashMap::new(),
        };
        for name in others {
            if !themes.knows(name) {
                themes.other.insert(name.clone(), light_of(name));
            }
        }
        themes
    }

    fn card_light(&self, name: &str) -> Option<bool> {
        let known = self.default_light.iter().chain(&self.default_dark).chain(&self.built_in);
        known.into_iter().find(|card| card.name == name).map(|card| card.light)
    }

    fn knows(&self, name: &str) -> bool {
        self.card_light(name).is_some() || self.other.contains_key(name)
    }

    /// 只查读过的，不读文件；还没读过的主题先当作深色，后台读完重画时改过来（`Onboarding::follow_theme`）。
    fn is_light(&self, name: &str) -> bool {
        self.card_light(name).or_else(|| self.other.get(name).copied()).unwrap_or(false)
    }
}

/// 主题 `name` 是不是浅色，找不到的算深色。读主题文件。
fn light_of(name: &str) -> bool {
    runode_config::theme_config(name).is_some_and(|config| ui::theme::is_light(config.foreground, config.background))
}

/// 认 `choice` 是哪一档、换档时要知道深浅的主题：只有一个主题时是它。
fn judged(choice: &ThemeChoice) -> Vec<String> {
    match choice {
        ThemeChoice::Single(name) => vec![name.clone()],
        ThemeChoice::Pair { .. } => Vec::new(),
    }
}

/// `choice` 算哪一档：浅色、深色各一个是自动，一个主题按它的深浅。
fn scheme_of(choice: &ThemeChoice, mut is_light: impl FnMut(&str) -> bool) -> Scheme {
    match choice {
        ThemeChoice::Pair { .. } => Scheme::Auto,
        ThemeChoice::Single(name) if is_light(name) => Scheme::Light,
        ThemeChoice::Single(_) => Scheme::Dark,
    }
}

/// 换到 `scheme` 这一档后的主题。换成自动时原来那个主题按深浅留在它那一边，另一边用 runode 自己的；
/// 换成浅色或深色时留下那一边的主题，原来那个深浅不对就换成 runode 自己的。
fn with_scheme(choice: &ThemeChoice, scheme: Scheme, mut is_light: impl FnMut(&str) -> bool) -> ThemeChoice {
    match (scheme, choice) {
        (Scheme::Auto, ThemeChoice::Single(name)) if is_light(name) => {
            ThemeChoice::Pair { light: name.clone(), dark: DEFAULT_THEME.to_owned() }
        }
        (Scheme::Auto, ThemeChoice::Single(name)) => {
            ThemeChoice::Pair { light: DEFAULT_LIGHT_THEME.to_owned(), dark: name.clone() }
        }
        (Scheme::Auto, ThemeChoice::Pair { light, dark }) => {
            ThemeChoice::Pair { light: light.clone(), dark: dark.clone() }
        }
        (Scheme::Light, ThemeChoice::Pair { light, .. }) => ThemeChoice::Single(light.clone()),
        (Scheme::Dark, ThemeChoice::Pair { dark, .. }) => ThemeChoice::Single(dark.clone()),
        (Scheme::Light, ThemeChoice::Single(name)) if is_light(name) => ThemeChoice::Single(name.clone()),
        (Scheme::Light, ThemeChoice::Single(_)) => ThemeChoice::Single(DEFAULT_LIGHT_THEME.to_owned()),
        (Scheme::Dark, ThemeChoice::Single(name)) if !is_light(name) => ThemeChoice::Single(name.clone()),
        (Scheme::Dark, ThemeChoice::Single(_)) => ThemeChoice::Single(DEFAULT_THEME.to_owned()),
    }
}

/// 在 `scheme` 这一档点了主题 `name`（`light` 是它的深浅）后的主题：自动时换掉深浅相同的那一边。
fn with_theme(choice: &ThemeChoice, scheme: Scheme, name: &str, light: bool) -> ThemeChoice {
    match scheme {
        Scheme::Auto => choice.with(if light { ThemeSlot::Light } else { ThemeSlot::Dark }, name.to_owned()),
        Scheme::Light | Scheme::Dark => ThemeChoice::Single(name.to_owned()),
    }
}

/// 深浅为 `light` 的主题 `name` 是不是正用着的那个（自动时看深浅相同的那一边）。
fn chosen(choice: &ThemeChoice, name: &str, light: bool) -> bool {
    match choice {
        ThemeChoice::Single(current) => current == name,
        ThemeChoice::Pair { light: current, .. } if light => current == name,
        ThemeChoice::Pair { dark: current, .. } => current == name,
    }
}

fn current_choice(cx: &gpui::App) -> ThemeChoice {
    ThemeChoice::parse(cx.global::<AppConfig>().0.theme.as_deref())
}

impl Onboarding {
    /// 在后台读主题，读完记进 `themes` 再重画。
    pub(super) fn load_themes(cx: &mut Context<Self>) {
        let others = judged(&current_choice(cx));
        let job = cx.background_spawn(async move { Themes::load(&others) });
        cx.spawn(async move |this, cx| {
            let themes = job.await;
            this.update(cx, |this, cx| {
                this.themes = Some(themes);
                // 读的时候配置可能又换了主题。
                this.follow_theme(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 配置里换上的主题还不知道深浅时，在后台补读，读完重画。
    pub(super) fn follow_theme(&mut self, cx: &mut Context<Self>) {
        let Some(themes) = &self.themes else { return };
        let missing: Vec<String> = judged(&current_choice(cx)).into_iter().filter(|name| !themes.knows(name)).collect();
        if missing.is_empty() {
            return;
        }
        let job = cx.background_spawn(async move {
            missing
                .into_iter()
                .map(|name| {
                    let light = light_of(&name);
                    (name, light)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let found = job.await;
            this.update(cx, |this, cx| {
                if let Some(themes) = &mut this.themes {
                    themes.other.extend(found);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn pick_scheme(&mut self, scheme: Scheme, cx: &mut Context<Self>) {
        let choice = current_choice(cx);
        let Some(themes) = &self.themes else { return };
        let next = with_scheme(&choice, scheme, |name| themes.is_light(name));
        self.write_theme(&choice, next, cx);
    }

    fn pick_theme(&mut self, name: &'static str, light: bool, cx: &mut Context<Self>) {
        let choice = current_choice(cx);
        let Some(themes) = &self.themes else { return };
        let scheme = scheme_of(&choice, |name| themes.is_light(name));
        self.write_theme(&choice, with_theme(&choice, scheme, name, light), cx);
    }

    /// 点了「默认主题」：自动时两边都换成 runode 自己的。
    fn pick_default(&mut self, cx: &mut Context<Self>) {
        let choice = current_choice(cx);
        let Some(themes) = &self.themes else { return };
        let next = match scheme_of(&choice, |name| themes.is_light(name)) {
            Scheme::Auto => ThemeChoice::Pair { light: DEFAULT_LIGHT_THEME.to_owned(), dark: DEFAULT_THEME.to_owned() },
            Scheme::Light => ThemeChoice::Single(DEFAULT_LIGHT_THEME.to_owned()),
            Scheme::Dark => ThemeChoice::Single(DEFAULT_THEME.to_owned()),
        };
        self.write_theme(&choice, next, cx);
    }

    /// 写进配置，配置重载后这一页跟着重画；写不进去时记下原因报在页上。
    fn write_theme(&mut self, current: &ThemeChoice, next: ThemeChoice, cx: &mut Context<Self>) {
        if &next == current {
            return;
        }
        self.theme_error = crate::config::set("theme", &next.value(), cx)
            .err()
            .map(|err| rust_i18n::t!("onboarding.write_failed", err = err.to_string()).into_owned());
        cx.notify();
    }

    /// 标题下面的内容：配色、主题的卡片，下面是提示和写不进去的原因。
    /// 主题还没读完时只写一句正在读。
    pub(super) fn render_look(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(themes) = &self.themes else {
            return div()
                .w_full()
                .py(px(40.))
                .flex()
                .justify_center()
                .child(
                    text_label(
                        "onboarding-themes-loading",
                        rust_i18n::t!("onboarding.themes_loading").into_owned(),
                        theme.foreground.opacity(0.55),
                    )
                    .text_size(FontSize::Body),
                )
                .into_any_element();
        };
        let choice = current_choice(cx);
        let mono = ui::font::mono(cx);
        let scheme = scheme_of(&choice, |name| themes.is_light(name));

        let tiles = [
            (
                Scheme::Auto,
                "onboarding-scheme-auto",
                "onboarding.scheme_auto",
                vec![themes.light_look, themes.dark_look],
            ),
            (Scheme::Light, "onboarding-scheme-light", "onboarding.scheme_light", vec![themes.light_look]),
            (Scheme::Dark, "onboarding-scheme-dark", "onboarding.scheme_dark", vec![themes.dark_look]),
        ]
        .into_iter()
        .map(|(tile, id, label, looks)| {
            let on = tile == scheme;
            let label = rust_i18n::t!(label).into_owned();
            div()
                .id(id)
                .role(Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(on.into())
                .flex_1()
                .min_w_0()
                .max_w(scaled(128.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .cursor_pointer()
                .child(
                    div()
                        .h(scaled(52.))
                        .flex()
                        .rounded(px(10.))
                        .overflow_hidden()
                        .border_2()
                        .border_color(if on { theme.ring } else { theme.control_border })
                        .children(looks.into_iter().map(preview)),
                )
                .child(choice_name(label, on, theme))
                .on_press(cx, move |this, _, cx| this.pick_scheme(tile, cx))
        });
        let schemes = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(section_title(rust_i18n::t!("onboarding.scheme").into_owned(), theme))
            .child(
                div()
                    .id("onboarding-schemes")
                    .role(Role::RadioGroup)
                    .aria_label(rust_i18n::t!("onboarding.scheme").into_owned())
                    .flex()
                    .gap(px(16.))
                    .children(tiles),
            );

        let defaults: Vec<&ThemeCard> = match scheme {
            Scheme::Auto => themes.default_light.iter().chain(&themes.default_dark).collect(),
            Scheme::Light => themes.default_light.iter().collect(),
            Scheme::Dark => themes.default_dark.iter().collect(),
        };
        let default_name = match scheme {
            Scheme::Auto => SharedString::from("Runode"),
            Scheme::Light => DEFAULT_LIGHT_THEME.into(),
            Scheme::Dark => DEFAULT_THEME.into(),
        };
        let default_on = match scheme {
            Scheme::Auto => {
                choice == ThemeChoice::Pair { light: DEFAULT_LIGHT_THEME.to_owned(), dark: DEFAULT_THEME.to_owned() }
            }
            Scheme::Light => chosen(&choice, DEFAULT_LIGHT_THEME, true),
            Scheme::Dark => chosen(&choice, DEFAULT_THEME, false),
        };
        let default_card =
            theme_card("onboarding-theme-default".into(), default_name, &defaults, default_on, &mono, theme)
                .on_press(cx, |this, _, cx| this.pick_default(cx));
        let cards = themes
            .built_in
            .iter()
            .filter(|card| match scheme {
                Scheme::Auto => true,
                Scheme::Light => card.light,
                Scheme::Dark => !card.light,
            })
            .map(|card| {
                let (name, light) = (card.name, card.light);
                let on = chosen(&choice, name, light);
                theme_card(format!("onboarding-theme-{name}").into(), name.into(), &[card], on, &mono, theme)
                    .on_press(cx, move |this, _, cx| this.pick_theme(name, light, cx))
            })
            .collect::<Vec<_>>();
        let built_in = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(section_title(rust_i18n::t!("onboarding.builtin_themes").into_owned(), theme))
            .child(
                div().id("onboarding-themes-scroll").overflow_x_scroll().flex().gap(px(14.)).pb(px(4.)).children(cards),
            );
        let picks = div()
            .id("onboarding-themes")
            .role(Role::RadioGroup)
            .aria_label(rust_i18n::t!("onboarding.themes").into_owned())
            .flex()
            .gap(px(20.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(section_title(rust_i18n::t!("onboarding.default_theme").into_owned(), theme))
                    .child(default_card),
            )
            .child(div().flex_none().w(px(1.)).bg(theme.border))
            .child(built_in);

        let card = div()
            .w_full()
            .rounded(px(12.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .child(div().px(px(20.)).py(px(16.)).child(schemes))
            .child(div().h(px(1.)).bg(theme.border))
            .child(div().px(px(20.)).py(px(16.)).child(picks));
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(card)
            .child(
                text_label(
                    "onboarding-themes-hint",
                    rust_i18n::t!("onboarding.themes_hint").into_owned(),
                    theme.foreground.opacity(0.55),
                )
                .px(px(4.))
                .text_size(FontSize::Body),
            )
            .children(self.theme_error.clone().map(|err| {
                text_label("onboarding-theme-error", err, theme.destructive).px(px(4.)).text_size(FontSize::Body)
            }))
            .into_any_element()
    }
}

/// 卡片里一组的小标题；组本身带着名字报给辅助工具，这里不另报。
fn section_title(text: String, theme: &Theme) -> Div {
    div().text_size(FontSize::Body).font_weight(FontWeight::MEDIUM).text_color(theme.subtle_foreground).child(text)
}

/// 选项下面的名字，选中时加粗。
fn choice_name(text: impl Into<SharedString>, on: bool, theme: &Theme) -> Div {
    div()
        .w_full()
        .truncate()
        .text_center()
        .text_size(FontSize::Body)
        .text_color(if on { theme.foreground } else { theme.foreground.opacity(0.75) })
        .when(on, |name| name.font_weight(FontWeight::SEMIBOLD))
        .child(text.into())
}

/// 一张主题卡：主题的背景上画「Aa」、提示符、光标和一排调色板；`looks` 有两个时左右各半。
fn theme_card(
    id: SharedString,
    name: SharedString,
    looks: &[&ThemeCard],
    on: bool,
    mono: &Option<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let half = looks.len() > 1;
    let face = div()
        .w(scaled(120.))
        .h(scaled(72.))
        .flex()
        .rounded(px(10.))
        .overflow_hidden()
        .border_2()
        .border_color(if on { theme.ring } else { theme.control_border })
        .children(looks.iter().map(|card| swatch(card, half, mono)));
    div()
        .id(id)
        .role(Role::RadioButton)
        .aria_label(name.clone())
        .aria_toggled(on.into())
        .flex_none()
        .w(scaled(120.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .cursor_pointer()
        .child(face)
        .child(choice_name(name, on, theme))
}

/// 卡片上一个主题的那一块；`half` 时只占一半宽，调色板排成两行。
fn swatch(card: &ThemeCard, half: bool, mono: &Option<SharedString>) -> Div {
    let fg = hsla(card.fg);
    let dot = |color: Rgb| div().flex_none().size(px(6.)).rounded_full().bg(hsla(color));
    let dots =
        if half {
            div().flex().flex_col().gap(px(3.)).children(
                card.ansi.chunks(4).map(|row| div().flex().gap(px(3.)).children(row.iter().copied().map(dot))),
            )
        } else {
            div().flex().gap(px(4.)).children(card.ansi.iter().copied().map(dot))
        };
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .p(px(10.))
        .flex()
        .flex_col()
        .justify_between()
        .bg(hsla(card.bg))
        .child(
            div()
                .when_some(mono.clone(), |text, family| text.font_family(family))
                .text_size(FontSize::Title)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(fg)
                .child("Aa"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .child(div().flex_none().w(px(14.)).h(px(6.)).rounded(px(2.)).bg(hsla(card.ansi[2])))
                .child(div().flex_none().w(px(6.)).h(px(10.)).bg(hsla(card.cursor))),
        )
        .child(dots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light(name: &str) -> bool {
        name.contains("Light") || name == "Day"
    }

    fn single(name: &str) -> ThemeChoice {
        ThemeChoice::Single(name.to_owned())
    }

    fn pair(light: &str, dark: &str) -> ThemeChoice {
        ThemeChoice::Pair { light: light.to_owned(), dark: dark.to_owned() }
    }

    #[test]
    fn scheme_follows_choice() {
        assert_eq!(scheme_of(&pair("Day", "Night"), light), Scheme::Auto);
        assert_eq!(scheme_of(&single("Day"), light), Scheme::Light);
        assert_eq!(scheme_of(&single("Night"), light), Scheme::Dark);
    }

    #[test]
    fn switching_scheme_keeps_what_fits() {
        // 换成自动：原来的主题按深浅留在它那一边。
        assert_eq!(with_scheme(&single("Day"), Scheme::Auto, light), pair("Day", DEFAULT_THEME));
        assert_eq!(with_scheme(&single("Night"), Scheme::Auto, light), pair(DEFAULT_LIGHT_THEME, "Night"));
        assert_eq!(with_scheme(&pair("Day", "Night"), Scheme::Auto, light), pair("Day", "Night"));
        // 换成浅色或深色：留下那一边，深浅不对的换成 runode 自己的。
        assert_eq!(with_scheme(&pair("Day", "Night"), Scheme::Light, light), single("Day"));
        assert_eq!(with_scheme(&pair("Day", "Night"), Scheme::Dark, light), single("Night"));
        assert_eq!(with_scheme(&single("Night"), Scheme::Light, light), single(DEFAULT_LIGHT_THEME));
        assert_eq!(with_scheme(&single("Day"), Scheme::Light, light), single("Day"));
        assert_eq!(with_scheme(&single("Day"), Scheme::Dark, light), single(DEFAULT_THEME));
        assert_eq!(with_scheme(&single("Night"), Scheme::Dark, light), single("Night"));
    }

    #[test]
    fn picking_a_theme_replaces_the_matching_side() {
        assert_eq!(with_theme(&single("Night"), Scheme::Dark, "Dracula", false), single("Dracula"));
        assert_eq!(with_theme(&single("Day"), Scheme::Light, "Dawn", true), single("Dawn"));
        assert_eq!(with_theme(&pair("Day", "Night"), Scheme::Auto, "Dawn", true), pair("Dawn", "Night"));
        assert_eq!(with_theme(&pair("Day", "Night"), Scheme::Auto, "Dracula", false), pair("Day", "Dracula"));
    }

    #[test]
    fn chosen_looks_at_the_matching_side() {
        assert!(chosen(&single("Nord"), "Nord", false));
        assert!(!chosen(&single("Nord"), "Dracula", false));
        assert!(chosen(&pair("Day", "Night"), "Day", true));
        assert!(chosen(&pair("Day", "Night"), "Night", false));
        assert!(!chosen(&pair("Day", "Night"), "Night", true));
    }

    #[test]
    fn built_in_themes_are_bundled() {
        for name in BUILT_IN_THEMES {
            assert!(runode_config::theme_config(name).is_some(), "{name} is missing");
        }
        assert!(ThemeCard::load(DEFAULT_LIGHT_THEME).is_some_and(|card| card.light));
        assert!(ThemeCard::load(DEFAULT_THEME).is_some_and(|card| !card.light));
        assert!(BUILT_IN_THEMES[..8].iter().all(|name| ThemeCard::load(name).is_some_and(|card| !card.light)));
        assert!(BUILT_IN_THEMES[8..].iter().all(|name| ThemeCard::load(name).is_some_and(|card| card.light)));
    }
}
