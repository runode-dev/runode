//! 外观页的配色：三张预览卡选不用主题、一个主题，还是浅色、深色外观各一个（跟着系统换），下面一组
//! 是选主题的下拉，左边的色块是那个主题的背景、前景和强调色。预览和色块按主题文件里的颜色画
//! （`runode_config::theme_config`），读过的主题记在 `SettingsView::theme_looks` 里。

use gpui::{AnyElement, Context, Div, FontWeight, Role, Window, div, prelude::*, px, svg};
use runode_config::Config;
use runode_shared_types::color::Rgb;

use super::{
    SettingsView,
    controls::{Colors, Press, dropdown, row},
    picker::{PickItem, PickTarget, ThemeSlot},
};
use crate::{
    assets::{BAN_ICON, MONITOR_ICON, PALETTE_ICON},
    i18n::tr,
    ui::hsla,
};

/// `theme` 的值拆开：不用主题、一个主题，或者浅色、深色外观各一个。
#[derive(Debug, PartialEq)]
pub(super) enum ThemeChoice {
    None,
    Single(String),
    Pair { light: String, dark: String },
}

impl ThemeChoice {
    pub fn parse(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::None;
        };
        let side = |want: &str| {
            value.split(',').map(str::trim).find_map(|part| part.strip_prefix(want)).map(|name| name.trim().to_owned())
        };
        match (side("light:"), side("dark:")) {
            (None, None) => Self::Single(value.to_owned()),
            (light, dark) => {
                let light = light.or_else(|| dark.clone()).unwrap_or_default();
                let dark = dark.unwrap_or_else(|| light.clone());
                Self::Pair { light, dark }
            }
        }
    }

    pub fn value(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Single(name) => Some(name.clone()),
            Self::Pair { light, dark } => Some(format!("light:{light},dark:{dark}")),
        }
    }

    /// 选了 `slot` 用 `name` 之后的样子。
    pub fn with(&self, slot: ThemeSlot, name: String) -> Self {
        let (light, dark) = match self {
            Self::None => (name.clone(), name.clone()),
            Self::Single(current) => (current.clone(), current.clone()),
            Self::Pair { light, dark } => (light.clone(), dark.clone()),
        };
        match slot {
            ThemeSlot::Single => Self::Single(name),
            ThemeSlot::Light => Self::Pair { light: name, dark },
            ThemeSlot::Dark => Self::Pair { light, dark: name },
        }
    }

    fn mode(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single(_) => "single",
            Self::Pair { .. } => "pair",
        }
    }
}

/// 画预览和色块用的三个颜色。
#[derive(Clone, Copy)]
pub(super) struct Look {
    bg: Rgb,
    fg: Rgb,
    /// 调色板里的蓝色，和设置页的强调色一样取。
    accent: Rgb,
}

impl Look {
    fn of(config: &Config) -> Self {
        let accent = config.palette.iter().rfind(|(i, _)| *i == 4).map_or(Rgb(0x3b, 0x82, 0xf6), |(_, c)| *c);
        Self { bg: config.background, fg: config.foreground, accent }
    }
}

/// 预览卡上没有具体主题可画时（比如还没选浅色、深色主题）用的浅色和深色。
const PLAIN_LIGHT: Look = Look { bg: Rgb(0xff, 0xff, 0xff), fg: Rgb(0x1d, 0x1d, 0x1f), accent: Rgb(0x3b, 0x82, 0xf6) };
const PLAIN_DARK: Look = Look { bg: Rgb(0x16, 0x16, 0x18), fg: Rgb(0xe5, 0xe5, 0xe7), accent: Rgb(0x60, 0xa5, 0xfa) };

impl SettingsView {
    fn open_theme_picker(
        &mut self,
        slot: ThemeSlot,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = runode_config::theme_names().into_iter().map(|name| PickItem::new(name.clone(), name)).collect();
        let title = rust_i18n::t!(match slot {
            ThemeSlot::Single => "settings.key.theme",
            ThemeSlot::Light => "settings.theme.light",
            ThemeSlot::Dark => "settings.theme.dark",
        })
        .into_owned();
        self.open_picker(title, items, current, PickTarget::Theme(slot), window, cx);
    }

    /// 选好了主题。
    pub(super) fn set_theme(&mut self, slot: ThemeSlot, name: String, cx: &mut Context<Self>) {
        let theme = ThemeChoice::parse(self.config.theme.as_deref()).with(slot, name);
        self.write_or_report("theme", theme.value().into_iter().collect(), cx);
    }

    /// 点了一张预览卡。从不用主题换到要主题时还不知道用哪个，先打开挑主题的列表。
    fn pick_theme_mode(&mut self, mode: &str, window: &mut Window, cx: &mut Context<Self>) {
        match (mode, ThemeChoice::parse(self.config.theme.as_deref())) {
            ("none", _) => self.write_or_report("theme", Vec::new(), cx),
            ("single", ThemeChoice::Pair { dark, .. }) => self.write_or_report("theme", vec![dark], cx),
            ("single", ThemeChoice::None) => self.open_theme_picker(ThemeSlot::Single, None, window, cx),
            ("pair", ThemeChoice::Single(name)) => {
                self.write_or_report("theme", vec![format!("light:{name},dark:{name}")], cx)
            }
            ("pair", ThemeChoice::None) => self.open_theme_picker(ThemeSlot::Dark, None, window, cx),
            _ => {}
        }
    }

    /// 主题 `name` 的颜色；找不到这个主题时为 `None`。
    fn theme_look(&mut self, name: &str) -> Option<Look> {
        *self
            .theme_looks
            .entry(name.to_owned())
            .or_insert_with(|| runode_config::theme_config(name).as_ref().map(Look::of))
    }

    /// 三张预览卡，下面是说明和上次写回失败的原因。
    pub(super) fn render_theme_modes(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let theme = ThemeChoice::parse(self.config.theme.as_deref());
        let current = Look::of(&self.config);
        let (pair, single, none) = match &theme {
            ThemeChoice::None => (vec![PLAIN_LIGHT, PLAIN_DARK], current, current),
            ThemeChoice::Single(name) => {
                let look = self.theme_look(name).unwrap_or(current);
                (vec![look, look], look, Look::of(&Config::default()))
            }
            ThemeChoice::Pair { light, dark } => {
                let light = self.theme_look(light).unwrap_or(PLAIN_LIGHT);
                let dark = self.theme_look(dark).unwrap_or(PLAIN_DARK);
                (vec![light, dark], dark, Look::of(&Config::default()))
            }
        };
        let tiles =
            [("pair", MONITOR_ICON, pair), ("single", PALETTE_ICON, vec![single]), ("none", BAN_ICON, vec![none])]
                .into_iter()
                .enumerate()
                .map(|(ix, (mode, icon, looks))| {
                    let on = mode == theme.mode();
                    let tint = if on { colors.accent } else { colors.fg.opacity(0.75) };
                    let label = tr(&format!("settings.theme.{mode}"));
                    div()
                        .id(("theme-mode", ix))
                        .role(Role::RadioButton)
                        .aria_label(label.clone())
                        .aria_toggled(on.into())
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .h(px(104.))
                                .flex()
                                .rounded(px(10.))
                                .overflow_hidden()
                                .border_2()
                                .border_color(if on { colors.accent } else { colors.border })
                                .children(looks.into_iter().map(preview)),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_center()
                                .items_center()
                                .gap(px(6.))
                                .text_color(tint)
                                .when(on, |label| label.font_weight(FontWeight::MEDIUM))
                                .child(svg().path(icon).size(px(14.)).text_color(tint))
                                .child(label),
                        )
                        .on_press(cx, move |this, window, cx| this.pick_theme_mode(mode, window, cx))
                });
        let note = |text: String, color| div().pt(px(10.)).pl(px(4.)).text_size(px(12.)).text_color(color).child(text);
        div()
            .flex()
            .flex_col()
            .pb(px(12.))
            .child(
                div()
                    .id("theme-modes")
                    .role(Role::RadioGroup)
                    .aria_label(tr("settings.section.color_scheme"))
                    .flex()
                    .gap(px(12.))
                    .children(tiles),
            )
            .child(note(tr("settings.hint.theme"), colors.fg.opacity(0.55)))
            .children(self.errors.get("theme").cloned().map(|err| note(err, colors.error)))
    }

    /// 选主题的下拉，一个主题时一行，浅色、深色各一个时两行，不用主题时没有。
    pub(super) fn render_theme_rows(&mut self, colors: Colors, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let slots = match ThemeChoice::parse(self.config.theme.as_deref()) {
            ThemeChoice::None => Vec::new(),
            ThemeChoice::Single(name) => vec![(ThemeSlot::Single, name)],
            ThemeChoice::Pair { light, dark } => vec![(ThemeSlot::Light, light), (ThemeSlot::Dark, dark)],
        };
        slots
            .into_iter()
            .map(|(slot, name)| {
                let (title, element) = match slot {
                    ThemeSlot::Single => ("settings.theme.name", "theme-single"),
                    ThemeSlot::Light => ("settings.theme.light", "theme-light"),
                    ThemeSlot::Dark => ("settings.theme.dark", "theme-dark"),
                };
                let chips = self.theme_look(&name).map(|look| chips(look, colors).into_any_element());
                let current = name.clone();
                let title = rust_i18n::t!(title).into_owned();
                let picker = dropdown(element, title.clone(), chips, name, 240., colors)
                    .on_press(cx, move |this, window, cx| {
                        this.open_theme_picker(slot, Some(current.clone()), window, cx)
                    });
                row(title, None, picker, None, None, colors).into_any_element()
            })
            .collect()
    }
}

/// 一个缩小的窗口：左边侧栏几道横线，右边一块内容，第一道线是强调色。几个并排时各占一份。
fn preview(look: Look) -> Div {
    let (bg, fg) = (hsla(look.bg), hsla(look.fg));
    let line = |width: f32, color: gpui::Hsla| div().flex_none().h(px(4.)).w(px(width)).rounded_full().bg(color);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .bg(hsla(look.bg.mix(look.fg, 0.06)))
        .child(
            div()
                .flex_none()
                .w(px(34.))
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .children([18., 14., 16., 12.].map(|width| line(width, fg.opacity(0.3)))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .my(px(8.))
                .mr(px(8.))
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .overflow_hidden()
                .rounded(px(6.))
                .bg(bg)
                .border_1()
                .border_color(fg.opacity(0.1))
                .child(line(44., hsla(look.accent)))
                .children([64., 52., 58., 36.].map(|width| line(width, fg.opacity(0.3)))),
        )
}

/// 下拉里主题名左边的色块：背景、前景、强调色三竖条。
fn chips(look: Look, colors: Colors) -> Div {
    div()
        .flex_none()
        .h(px(14.))
        .flex()
        .rounded(px(3.))
        .overflow_hidden()
        .border_1()
        .border_color(colors.border)
        .children([look.bg, look.fg, look.accent].map(|color| div().w(px(7.)).h_full().bg(hsla(color))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_value_splits_and_joins() {
        assert_eq!(ThemeChoice::parse(None), ThemeChoice::None);
        assert_eq!(ThemeChoice::parse(Some("Dracula")), ThemeChoice::Single("Dracula".into()));
        let pair = ThemeChoice::parse(Some("light:Day, dark:Night"));
        assert_eq!(pair, ThemeChoice::Pair { light: "Day".into(), dark: "Night".into() });
        assert_eq!(pair.value().as_deref(), Some("light:Day,dark:Night"));
        assert_eq!(pair.with(ThemeSlot::Light, "Dawn".into()).value().as_deref(), Some("light:Dawn,dark:Night"));
        assert_eq!(
            ThemeChoice::Single("A".into()).with(ThemeSlot::Dark, "B".into()).value().as_deref(),
            Some("light:A,dark:B")
        );
        assert_eq!(ThemeChoice::None.with(ThemeSlot::Single, "C".into()).value().as_deref(), Some("C"));
        // 只写了一边的，另一边用同一个。
        assert_eq!(ThemeChoice::parse(Some("dark:X")), ThemeChoice::Pair { light: "X".into(), dark: "X".into() });
    }
}
