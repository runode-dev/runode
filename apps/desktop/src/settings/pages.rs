//! 设置页的各页：每页有哪些项、各用什么控件，以及主题、字体、调色板这些不止一个控件的项。

use crate::ui::icons::{
    ALERT_NOTE_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CHEVRON_UP_ICON, MINUS_ICON, SEARCH_ICON, VOLUME_ICON,
};
use crate::ui::{
    a11y::StaticText as _,
    chip::Chip,
    segmented::Segmented,
    theme::{ActiveTheme as _, Theme},
};
use gpui::{
    AnyElement, AppContext as _, Context, Div, ElementId, Entity, Role, SharedString, Window, div, prelude::*, px, svg,
};
use runode_config::StatusItem;
use runode_types::{agent::AgentKind, speech::DictationCloud};

use super::{
    Commit, SettingsView,
    controls::{Cards, Press, RowExtras, dropdown, error_text, input_box, row, row_with, stacked, swatch},
    picker::{PickItem, PickTarget},
};
use crate::{
    i18n::{action_for, os_key, tr},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        display_dir,
        font_size::{FontSize, scaled},
        hsla,
        input::{Input, InputEvent, InputState},
        status_mark::{MarkKind, status_mark},
        switch::Switch,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    General,
    Appearance,
    Colors,
    Terminal,
    Files,
    Agents,
    Dictation,
    Ssh,
    Remote,
    Keybinds,
}

/// 页面里的一项。
#[derive(Clone, Copy)]
enum Item {
    /// 小标题，文字是翻译里的 `settings.section.<名字>`。
    Section(&'static str),
    /// 一个键一行。
    Row(&'static str, Control),
    Theme,
    FontFamily,
    Palette,
    StatusBarHidden,
    /// 不提醒的 agent：自成一组，小标题后面跟着说明，卡片里是筛选框和一排小签。
    AgentExclude,
    ConfigFiles,
    ConfigFileActions,
    Pairing,
    /// 远程访问页上防火墙挡着局域网连接时的提示和放行按钮。
    Firewall,
    /// 远程访问页的测试推送按钮。
    TestPush,
    /// 登录时自启的两项（宿主、app），装没装看服务文件，不是配置里的键。
    Autostart,
    /// 语音输入选中的模型下没下，下载、删除它；文件在不在看模型目录，不是配置里的键。
    Dictation,
    /// 连过的 SSH 主机，不是配置里的键。
    SshHosts,
    /// 配对过的电脑，不是配置里的键。
    PairedMachines,
}

#[derive(Clone, Copy)]
enum Control {
    Switch,
    /// 分段选项，选项是 `runode_config::choices` 给的名字。
    Choice,
    /// 分段选项，选项写在这里；值为空的那一项表示不写，用默认。
    Options(&'static [&'static str]),
    /// 输入框，宽度。
    Text(f32),
    Color,
    Language,
    Sound,
}

/// 左边页面列表里的一组页。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Group {
    Interface,
    Work,
    Connect,
}

impl Group {
    pub const ALL: [Self; 3] = [Self::Interface, Self::Work, Self::Connect];

    pub fn id(self) -> &'static str {
        match self {
            Self::Interface => "interface",
            Self::Work => "work",
            Self::Connect => "connect",
        }
    }

    pub fn title(self) -> String {
        tr(&format!("settings.group.{}", self.id()))
    }

    /// 这一组的页，按 `Page::ALL` 的先后。
    pub fn pages(self) -> impl Iterator<Item = Page> {
        Page::ALL.into_iter().filter(move |page| page.group() == self)
    }
}

impl Page {
    pub const ALL: [Self; 10] = [
        Self::General,
        Self::Appearance,
        Self::Colors,
        Self::Keybinds,
        Self::Terminal,
        Self::Files,
        Self::Agents,
        Self::Dictation,
        Self::Ssh,
        Self::Remote,
    ];

    pub fn group(self) -> Group {
        match self {
            Self::General | Self::Appearance | Self::Colors | Self::Keybinds => Group::Interface,
            Self::Terminal | Self::Files | Self::Agents | Self::Dictation => Group::Work,
            Self::Ssh | Self::Remote => Group::Connect,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Colors => "colors",
            Self::Terminal => "terminal",
            Self::Files => "files",
            Self::Agents => "agents",
            Self::Dictation => "dictation",
            Self::Ssh => "ssh",
            Self::Remote => "remote",
            Self::Keybinds => "keybinds",
        }
    }

    pub fn title(self) -> String {
        tr(&format!("settings.page.{}", self.id()))
    }

    /// 标题下面说这一页管什么的一句话。
    pub fn description(self) -> String {
        tr(&format!("settings.page_desc.{}", self.id()))
    }

    pub fn icon(self) -> &'static str {
        use crate::ui::icons::{
            FILES_ICON, KEYBOARD_ICON, MIC_ICON, PALETTE_ICON, PHONE_ICON, PLUG_ICON, SETTINGS_ICON, SLIDERS_ICON,
            SPARKLE_ICON, TERMINAL_ICON,
        };
        match self {
            Self::General => SETTINGS_ICON,
            Self::Appearance => SLIDERS_ICON,
            Self::Colors => PALETTE_ICON,
            Self::Terminal => TERMINAL_ICON,
            Self::Files => FILES_ICON,
            Self::Agents => SPARKLE_ICON,
            Self::Dictation => MIC_ICON,
            Self::Ssh => PLUG_ICON,
            Self::Remote => PHONE_ICON,
            Self::Keybinds => KEYBOARD_ICON,
        }
    }

    fn items(self) -> &'static [Item] {
        use Control::*;
        use Item::*;
        match self {
            Self::General => &[
                Row("language", Language),
                Row("terminal-host", Switch),
                Row("auto-update", Switch),
                Row("prevent-sleep", Choice),
                Section("login"),
                Autostart,
                Section("config_file"),
                ConfigFileActions,
                ConfigFiles,
            ],
            Self::Dictation => &[
                Row("dictation-model", Choice),
                Dictation,
                Row("dictation-words", Text(300.)),
                Section("dictation_cloud"),
                Row("dictation-cloud", Choice),
                Row("dictation-openai-url", Text(300.)),
                Row("dictation-openai-key", Text(300.)),
                Row("dictation-openai-model", Text(300.)),
                Row("dictation-doubao-app-id", Text(300.)),
                Row("dictation-doubao-key", Text(300.)),
                Section("dictation_polish"),
                Row("dictation-polish-url", Text(300.)),
                Row("dictation-polish-key", Text(300.)),
                Row("dictation-polish-model", Text(300.)),
            ],
            Self::Appearance => &[
                Theme,
                Section("font"),
                FontFamily,
                Row("font-size", Text(80.)),
                Row("adjust-cell-height", Text(80.)),
                Section("window"),
                Row("window-style", Choice),
                Row("ui-font-size", Text(80.)),
                Row("window-padding-x", Text(80.)),
                Row("window-padding-y", Text(80.)),
                Row("status-bar", Switch),
                StatusBarHidden,
            ],
            Self::Colors => &[
                Row("background", Color),
                Row("foreground", Color),
                Row("cursor-color", Color),
                Row("cursor-text", Color),
                Row("selection-background", Color),
                Row("selection-foreground", Color),
                Row("search-background", Color),
                Row("search-foreground", Color),
                Row("search-selected-background", Color),
                Row("search-selected-foreground", Color),
                Row("minimum-contrast", Text(80.)),
                Palette,
            ],
            Self::Terminal => &[
                Section("cursor"),
                Row("cursor-style", Choice),
                Row("shell-integration-features", Options(&["cursor", "no-cursor"])),
                Row("cursor-style-blink", Options(&["true", "false"])),
                Row("cursor-style-blink-timeout", Text(80.)),
                Section("keyboard"),
                Row("macos-option-as-alt", Options(&["", "true", "false", "left", "right"])),
                Section("scrollback"),
                Row("scrollback-limit", Text(120.)),
                Section("clipboard"),
                Row("clipboard-write", Choice),
                Row("clipboard-read", Choice),
                Section("window"),
                Row("vt-window-resize-allowed", Switch),
                Section("tasks"),
                Row("task-placement", Choice),
                Row("task-split-fold", Switch),
            ],
            Self::Ssh => &[
                Section("ssh_hosts"),
                SshHosts,
                Section("paired_machines"),
                PairedMachines,
                Section("ssh"),
                Row("ssh-program", Text(200.)),
                Row("ssh-control-persist", Text(80.)),
                Row("ssh-restore-on-launch", Switch),
            ],
            Self::Files => &[
                Row("file-tree-preview-click", Choice),
                Row("preview-font-size", Text(80.)),
                Section("git"),
                Row("git-auto-fetch", Switch),
            ],
            Self::Agents => &[
                Section("alerts"),
                Row("agent-notifications", Switch),
                Row("agent-done-sound", Sound),
                Row("agent-blocked-sound", Sound),
                AgentExclude,
                Section("splits"),
                Row("agent-split-fold", Switch),
            ],
            Self::Remote => &[
                Row("remote-access", Switch),
                Firewall,
                Row("remote-access-port", Text(80.)),
                Row("remote-access-name", Text(200.)),
                Row("remote-access-address", Text(200.)),
                Pairing,
                Section("push"),
                Row("remote-access-push", Switch),
                Row("remote-access-push-text", Switch),
                Row("remote-access-push-delay", Text(80.)),
                Section("push_key"),
                Row("apns-key-file", Text(300.)),
                Row("apns-key-id", Text(300.)),
                Row("apns-team-id", Text(300.)),
                Row("apns-bundle-id", Text(300.)),
                TestPush,
            ],
            Self::Keybinds => &[],
        }
    }

    /// 这一页管的键。
    #[cfg(test)]
    fn keys(self) -> Vec<&'static str> {
        if self == Self::Keybinds {
            return vec!["keybind"];
        }
        self.items()
            .iter()
            .filter_map(|item| match item {
                Item::Row(key, _) => Some(*key),
                Item::Theme => Some("theme"),
                Item::FontFamily => Some("font-family"),
                Item::Palette => Some("palette"),
                Item::StatusBarHidden => Some("status-bar-hidden"),
                Item::AgentExclude => Some("agent-notifications-exclude"),
                Item::ConfigFiles => Some("config-file"),
                Item::Section(_)
                | Item::ConfigFileActions
                | Item::Pairing
                | Item::Firewall
                | Item::TestPush
                | Item::Autostart
                | Item::Dictation
                | Item::SshHosts
                | Item::PairedMachines => None,
            })
            .collect()
    }
}

/// 字节数写成「10 MiB」「2.5 MiB」。
fn mib(bytes: u64) -> String {
    let mib = bytes as f64 / (1024. * 1024.);
    if mib.fract() == 0. { format!("{mib} MiB") } else { format!("{mib:.1} MiB") }
}

/// 翻译里键名用下划线。
fn tr_key(key: &str) -> String {
    key.replace('-', "_")
}

pub(super) fn key_title(key: &str) -> String {
    tr(&format!("settings.key.{}", tr_key(key)))
}

pub(super) fn key_hint(key: &str) -> SharedString {
    tr(&hint_key(key, &crate::i18n::current())).into()
}

/// 设置项 `key` 的说明在 `locale` 的翻译里的键：各系统说法不同的写成 `os_key` 那几条，取本系统那条。
fn hint_key(key: &str, locale: &str) -> String {
    let plain = format!("settings.hint.{}", tr_key(key));
    let os = os_key(&plain);
    if crate::_rust_i18n_try_translate(locale, &os).is_some() { os } else { plain }
}

/// 分段选项列出的值。
fn control_values(key: &str, control: Control) -> Vec<&'static str> {
    match control {
        Control::Options(values) => values.to_vec(),
        _ => runode_config::choices(key).unwrap_or_default(),
    }
}

/// 配置里没写这个键时选中的那一项：没写和写了某个值一样时是那个值，没有专门的「默认」一项；别的键是
/// 「默认」那一项（空串）。
fn unset_choice(key: &str) -> &'static str {
    match key {
        "cursor-style-blink" => "false",
        _ => "",
    }
}

fn choice_label(key: &str, value: &str) -> SharedString {
    if value.is_empty() {
        return rust_i18n::t!("settings.default").into_owned().into();
    }
    tr(&format!("settings.choice.{}.{value}", tr_key(key))).into()
}

fn id(prefix: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{prefix}-{key}").into())
}

/// 列表里一项按值取的元素 id：`value` 在前面的项 `earlier` 里已经出现过时带上第几次出现，同一个值写了两遍
/// 两行的 id 才不撞；第一次出现的不带，删掉别的行它的 id 不变。
fn unique_key(earlier: &[String], value: &str) -> String {
    match earlier.iter().filter(|other| *other == value).count() {
        0 => value.to_owned(),
        seen => format!("{value}#{seen}"),
    }
}

/// 不提醒哪些 agent 的键。
const EXCLUDE_KEY: &str = "agent-notifications-exclude";

/// 没在筛选、也没展开时，没选上的 agent 显示这么多个。
const COLLAPSED_AGENTS: usize = 12;

/// 「不提醒的 agent」里的一个小标签。
struct ExcludeChoice {
    /// 写进配置的名字（`AgentKind::label`）。
    label: &'static str,
    /// 显示的名字。
    name: String,
    on: bool,
}

/// 能选的 agent，最后是「其他程序」；`excluded` 是配置里不提醒的那些。
fn exclude_choices(excluded: &[String]) -> Vec<ExcludeChoice> {
    AgentKind::ALL
        .into_iter()
        .chain([AgentKind::Other])
        .map(|kind| {
            let label = kind.label();
            let name = match kind {
                AgentKind::Other => rust_i18n::t!("settings.other_agents").into_owned(),
                kind => kind.display_name().to_owned(),
            };
            ExcludeChoice { label, name, on: excluded.iter().any(|value| value == label) }
        })
        .collect()
}

/// 显示哪些小标签、还藏着几个，选上的排在前面。`query`（已经转成小写）不空时列出名字对得上的，不藏；
/// 空着时没选上的只列前 `COLLAPSED_AGENTS` 个，`expanded` 时全列。
fn shown_agents<'a>(choices: &'a [ExcludeChoice], query: &str, expanded: bool) -> (Vec<&'a ExcludeChoice>, usize) {
    let matches = |choice: &&ExcludeChoice| {
        query.is_empty() || choice.name.to_lowercase().contains(query) || choice.label.contains(query)
    };
    let (mut shown, rest): (Vec<_>, Vec<_>) = choices.iter().filter(matches).partition(|choice| choice.on);
    let keep = if query.is_empty() && !expanded { COLLAPSED_AGENTS.min(rest.len()) } else { rest.len() };
    let hidden = rest.len() - keep;
    shown.extend(rest.into_iter().take(keep));
    (shown, hidden)
}

/// 语言的名字，用它自己的文字写。
pub(super) fn language_name(locale: &str) -> String {
    rust_i18n::t!("settings.language_name", locale = locale).into_owned()
}

impl Item {
    /// 搜索设置时拿来对的文字：`locale` 和英文的标题、说明，配置项还有键名本身，界面不是英文时照着配置
    /// 文件里的英文写法也搜得到。不是配置项的几项用画出来的标题或所在小节的标题。
    fn search_texts(self, locale: &str) -> Vec<String> {
        let both = |key: &str| {
            [rust_i18n::t!(key, locale = locale).into_owned(), rust_i18n::t!(key, locale = "en").into_owned()]
        };
        let key_texts = |key: &str| {
            let title = format!("settings.key.{}", tr_key(key));
            let mut texts = both(&title).to_vec();
            let (hint, en_hint) = (hint_key(key, locale), hint_key(key, "en"));
            texts.push(rust_i18n::t!(&hint, locale = locale).into_owned());
            texts.push(rust_i18n::t!(&en_hint, locale = "en").into_owned());
            texts.push(key.to_owned());
            texts
        };
        match self {
            Item::Section(name) => both(&format!("settings.section.{name}")).to_vec(),
            Item::Row(key, _) => key_texts(key),
            Item::Theme => [both("settings.section.color_scheme").to_vec(), key_texts("theme")].concat(),
            Item::FontFamily => key_texts("font-family"),
            Item::Palette => key_texts("palette"),
            Item::StatusBarHidden => key_texts("status-bar-hidden"),
            Item::AgentExclude => key_texts(EXCLUDE_KEY),
            Item::ConfigFiles => key_texts("config-file"),
            Item::ConfigFileActions => both("settings.config_file").to_vec(),
            Item::Pairing => [both("settings.pairing.title"), both("settings.pairing.hint")].concat(),
            Item::Firewall => [both("settings.firewall.title"), both("settings.firewall.hint")].concat(),
            Item::TestPush => [both("settings.test_push.title"), both("settings.test_push.hint")].concat(),
            Item::Autostart => both("settings.section.login").to_vec(),
            Item::Dictation => both("settings.dictation.title").to_vec(),
            Item::SshHosts => both("settings.section.ssh_hosts").to_vec(),
            Item::PairedMachines => both("settings.section.paired_machines").to_vec(),
        }
    }

    /// `query` 已经转成小写。
    fn matches(self, query: &str, locale: &str) -> bool {
        self.search_texts(locale).iter().any(|text| text.to_lowercase().contains(query))
    }

    /// 自己画小标题的项：前面不再补所在小节的标题。
    fn opens_section(self) -> bool {
        matches!(self, Item::Section(_) | Item::Theme | Item::AgentExclude)
    }
}

/// `items` 里显示着（`shown`）、对得上 `query`（已经转成小写）的项，第一项前面补上它所在的小节；小节的标题
/// 对得上时这一节显示着的项都算。
fn search_items(items: &[Item], shown: impl Fn(&Item) -> bool, query: &str, locale: &str) -> Vec<Item> {
    let mut found = Vec::new();
    // 当前小节，和它的标题对不对得上；补进结果以后清掉，不再补。
    let mut section = None;
    let mut section_hit = false;
    for item in items.iter().copied().filter(|item| shown(item)) {
        if let Item::Section(_) = item {
            section = Some(item);
            section_hit = item.matches(query, locale);
            continue;
        }
        if section_hit || item.matches(query, locale) {
            if let Some(section) = section.take().filter(|_| !item.opens_section()) {
                found.push(section);
            }
            found.push(item);
        }
    }
    found
}

impl SettingsView {
    pub(super) fn render_page(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let query = self.search.read(cx).query().trim().to_lowercase();
        if !query.is_empty() {
            return self.render_search(&query, theme, window, cx);
        }
        let title = div()
            .id("settings-title")
            .role(Role::Heading)
            .aria_level(1)
            .aria_label(self.page.title())
            .pt(px(8.))
            .pb(px(4.))
            .text_size(FontSize::Display)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(self.page.title());
        let description = self.page.description();
        let description = div()
            .id("settings-description")
            .aria_static_text(description.clone())
            .pb(px(2.))
            .text_size(FontSize::Medium)
            .text_color(theme.subtle_foreground)
            .child(description);
        let page = div().flex().flex_col().child(title).child(description);
        if self.page == Page::Keybinds {
            return page.child(self.render_keybinds(theme, window, cx));
        }
        // 小标题把一页分成几组，每组收进一张卡片。
        let mut cards = Cards::new(page, theme);
        let items: Vec<Item> = self.page.items().iter().copied().filter(|item| self.item_shown(item)).collect();
        for item in items {
            self.push_item(&mut cards, item, theme, window, cx);
        }
        cards.finish()
    }

    /// 搜索框有字时的页面：按 `Page::ALL` 的先后，每页一个标题和去那一页的按钮，下面是这一页对得上 `query`
    /// （已经转成小写）的项。快捷键页不在里面。
    fn render_search(&mut self, query: &str, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let locale = crate::i18n::current();
        let mut out = div().flex().flex_col().pt(px(8.));
        let mut found = false;
        for page in Page::ALL {
            let items = search_items(page.items(), |item| self.item_shown(item), query, &locale);
            if items.is_empty() {
                continue;
            }
            found = true;
            let title = page.title();
            let heading = div()
                .id(id("settings-search-page", page.id()))
                .role(Role::Heading)
                .aria_level(2)
                .aria_label(title.clone())
                .min_w_0()
                .truncate()
                .text_size(FontSize::Large)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.clone());
            let go = rust_i18n::t!("settings.go_to_page").into_owned();
            let go = Button::new(id("settings-search-go", page.id()), go.clone())
                .aria_label(action_for(&go, &title))
                .variant(ButtonVariant::Ghost)
                .with_size(ButtonSize::Sm)
                .gap(px(3.))
                .text_color(theme.subtle_foreground)
                .child(go)
                .child(svg().flex_none().path(CHEVRON_RIGHT_ICON).size(px(11.)).text_color(theme.muted_foreground))
                .on_press(cx, move |this, window, cx| {
                    this.clear_search(cx);
                    this.select_page(page, window, cx);
                });
            let header = div().pt(px(20.)).flex().items_center().justify_between().gap(px(8.)).child(heading).child(go);
            let mut cards = Cards::new(div().flex().flex_col().child(header), theme);
            for item in items {
                self.push_item(&mut cards, item, theme, window, cx);
            }
            out = out.child(cards.finish());
        }
        if !found {
            let text = rust_i18n::t!("settings.no_matches").into_owned();
            out = out.child(
                div()
                    .id("settings-search-empty")
                    .aria_static_text(text.clone())
                    .pt(px(20.))
                    .text_color(theme.muted_foreground)
                    .child(text),
            );
        }
        out
    }

    /// 这一项在这台机器、这份配置下显示不显示。
    fn item_shown(&self, item: &Item) -> bool {
        match item {
            // 发布的版本经中转推官方 App，用不着直连 APNs 的密钥，这一节只在自己编译的版本里显示。
            Item::Section("push_key")
            | Item::Row("apns-key-file" | "apns-key-id" | "apns-team-id" | "apns-bundle-id", _) => {
                runode_protocol::push::RELAY_URL.is_none()
            }
            Item::Section("login") | Item::Autostart => super::autostart::any_supported(),
            Item::Firewall => self.config.remote_access && self.firewall.blocked(),
            // Linux 上的 app 自己换不了新版本，更新交给装它的包管理器，后台也不查，开关没有用。
            Item::Row("auto-update", _) => !cfg!(target_os = "linux"),
            // Option 当不当 Alt、提示音（`sound`）只有 macOS 上才有。
            Item::Section("keyboard")
            | Item::Row("macos-option-as-alt" | "agent-done-sound" | "agent-blocked-sound", _) => {
                cfg!(target_os = "macos")
            }
            // 只显示选中的那家云端识别的几项。
            Item::Row(key, _) if key.starts_with("dictation-openai-") => {
                self.config.dictation_cloud == DictationCloud::OpenAi
            }
            Item::Row(key, _) if key.starts_with("dictation-doubao-") => {
                self.config.dictation_cloud == DictationCloud::Doubao
            }
            _ => true,
        }
    }

    /// 把 `item` 画进 `cards`：小标题另起一组，别的项进当前这组的卡片。
    fn push_item(&mut self, cards: &mut Cards, item: Item, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) {
        let element = match item {
            Item::Section(name) => {
                cards.section(tr(&format!("settings.section.{name}")));
                return;
            }
            Item::Row(key, control) => self.render_row(key, control, theme, window, cx).into_any_element(),
            // 预览卡不进卡片，选主题的下拉自成一组。
            Item::Theme => {
                cards.section(tr("settings.section.color_scheme"));
                cards.raw(self.render_theme_modes(theme, cx).into_any_element());
                for row in self.render_theme_rows(theme, cx) {
                    cards.push(row);
                }
                return;
            }
            Item::FontFamily => self.render_font_family(theme, cx).into_any_element(),
            Item::Palette => self.render_palette(theme, window, cx).into_any_element(),
            Item::StatusBarHidden => self.render_status_bar_hidden(theme, cx).into_any_element(),
            Item::AgentExclude => {
                cards.section_with_note(key_title(EXCLUDE_KEY), key_hint(EXCLUDE_KEY));
                cards.push(self.render_agent_exclude(theme, window, cx).into_any_element());
                return;
            }
            Item::ConfigFiles => self.render_config_files(theme, window, cx).into_any_element(),
            Item::ConfigFileActions => self.render_config_file_actions(theme, cx).into_any_element(),
            Item::Pairing => self.render_pairing(theme, cx).into_any_element(),
            Item::Firewall => self.render_firewall(theme, cx).into_any_element(),
            Item::TestPush => self.render_test_push(theme, cx).into_any_element(),
            Item::Autostart => self.render_autostart(theme, cx).into_any_element(),
            Item::Dictation => self.render_dictation(theme, cx).into_any_element(),
            Item::SshHosts => self.render_ssh_hosts(theme, cx).into_any_element(),
            Item::PairedMachines => self.render_paired_machines(theme, cx).into_any_element(),
        };
        cards.push(element);
    }

    /// runode 的配置文件里写了 `key` 时的恢复按钮。辅助工具读到的名字带上这一项的标题，一页里好几个恢复
    /// 按钮才分得清。
    pub(super) fn reset(&self, key: &'static str, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.file.has(key).then(|| {
            let explain = rust_i18n::t!("settings.reset").into_owned();
            Button::new(id("reset", key), rust_i18n::t!("settings.reset_short").into_owned())
                .aria_label(rust_i18n::t!("settings.reset_for", name = key_title(key)).into_owned())
                .variant(ButtonVariant::Ghost)
                .with_size(ButtonSize::Sm)
                .px(px(8.))
                .text_color(cx.theme().subtle_foreground)
                .tooltip(crate::ui::tooltip::tooltip(explain, None))
                .on_press(cx, move |this, _, cx| this.write_or_report(key, Vec::new(), cx))
                .into_any_element()
        })
    }

    /// 逗号隔开、各层叠加的列表键（`status-bar-hidden`、`agent-notifications-exclude`）要写回的各行，见
    /// `config::clear_first`。
    fn with_clear(&self, key: &str, values: Vec<String>) -> Vec<String> {
        crate::config::clear_first(&self.file.values(key), &self.config.values(key), values)
    }

    fn render_row(
        &mut self,
        key: &'static str,
        control: Control,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = self.config.values(key);
        let mut error = self.errors.get(key).cloned();
        let control = match control {
            Control::Switch => {
                let on = current.first().is_some_and(|value| value == "true");
                let switch = Switch::new(id("switch", key), key_title(key), on).on_press(cx, move |this, _, cx| {
                    this.write_or_report(key, this.single(key, (!on).to_string()), cx)
                });
                let notify = key == "agent-notifications" && on;
                // 开着通知、系统设置里却拒绝了：通知发不出来，提示去系统设置里打开。
                let denied = notify && crate::alerts::notifications_denied(cx);
                if denied {
                    error.get_or_insert_with(|| rust_i18n::t!("settings.notifications_denied").into_owned());
                }
                let open = denied.then(|| {
                    Button::new("open-notification-settings", rust_i18n::t!("settings.open_notification_settings"))
                        .variant(ButtonVariant::Outline)
                        .on_press(cx, |_, _, cx| {
                            cx.open_url("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
                        })
                });
                let test = notify.then(|| {
                    Button::new("test-notification", rust_i18n::t!("settings.test_notification"))
                        .variant(ButtonVariant::Outline)
                        .on_press(cx, |_, _, cx| crate::window::test_notification(cx))
                });
                div().flex().items_center().gap(px(8.)).children(open).children(test).child(switch).into_any_element()
            }
            Control::Choice | Control::Options(_) => {
                let values = control_values(key, control);
                let selected = current.first().map_or_else(|| unset_choice(key), String::as_str);
                let group = Segmented::new(key, key_title(key));
                values
                    .iter()
                    .enumerate()
                    .fold(group, |group, (ix, value)| {
                        let (label, on, value) = (choice_label(key, value), *value == selected, value.to_string());
                        group.option((key, ix), label, on, cx, move |this, _, cx| {
                            this.write_or_report(key, this.single(key, value.clone()), cx);
                        })
                    })
                    .into_any_element()
            }
            Control::Text(width) => {
                let input = self.field(key, Commit::Value(key), Some(key_placeholder(key)), window, cx);
                let field = input_box(&input, Some(width), self.errors.contains_key(key), theme);
                // 回滚历史按字节填，一长串数字看不出多大，旁边折成 MiB。
                let size = (key == "scrollback-limit")
                    .then(|| current.first().and_then(|value| value.trim().parse::<u64>().ok()))
                    .flatten()
                    .map(|bytes| {
                        let size = mib(bytes);
                        div()
                            .id("scrollback-size")
                            .aria_static_text(size.clone())
                            .text_size(FontSize::Small)
                            .text_color(theme.foreground.opacity(0.55))
                            .child(size)
                    });
                div().flex().items_center().gap(px(8.)).children(size).child(field).into_any_element()
            }
            Control::Color => {
                let color = current.first().and_then(|value| runode_config::color::parse(value)).map(hsla);
                let input = self.field(key, Commit::Value(key), Some(key_placeholder(key)), window, cx);
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(swatch(color, 20., theme))
                    .child(input_box(&input, Some(130.), self.errors.contains_key(key), theme))
                    .into_any_element()
            }
            Control::Language => {
                let label = match &self.config.language {
                    Some(tag) => language_name(&crate::i18n::configured(tag)),
                    None => rust_i18n::t!("settings.follow_system").into_owned(),
                };
                dropdown(id("dropdown", key), key_title(key), None, label, 180., theme)
                    .on_press(cx, move |this, window, cx| this.open_language_picker(window, cx))
                    .into_any_element()
            }
            Control::Sound => {
                let sound = match current.first().map(String::as_str) {
                    None | Some("none") => None,
                    Some(name) => Some(name.to_owned()),
                };
                let label = sound.clone().unwrap_or_else(|| rust_i18n::t!("settings.no_sound").into_owned());
                let name = rust_i18n::t!("settings.preview_sound_for", name = key_title(key)).into_owned();
                let preview = Button::new(id("preview-sound", key), name.clone())
                    .icon(VOLUME_ICON)
                    .with_size(ButtonSize::Icon)
                    .tooltip(crate::ui::tooltip::tooltip(name, None));
                let preview = match sound {
                    Some(sound) => preview.on_press(cx, move |_, _, _| crate::sound::play(&sound)),
                    None => preview.disabled(true),
                };
                let picker = dropdown(id("dropdown", key), key_title(key), None, label, 180., theme)
                    .on_press(cx, move |this, window, cx| this.open_sound_picker(key, window, cx));
                div().flex().items_center().gap(px(8.)).child(preview).child(picker).into_any_element()
            }
        };
        let reset = self.reset(key, cx);
        // 两种提示音前面画它提醒的那种状态的标记。
        let leading = match key {
            "agent-done-sound" => Some(status_mark("mark-agent-done-sound", MarkKind::Done, 13., cx)),
            "agent-blocked-sound" => {
                Some(status_mark("mark-agent-blocked-sound", MarkKind::Waiting { blink: false }, 13., cx))
            }
            _ => None,
        };
        let note = (key == "agent-notifications" && !crate::alerts::notifications_available()).then(|| {
            let text = rust_i18n::t!("settings.notifications_need_app").into_owned();
            div()
                .id("notifications-need-app")
                .aria_static_text(text.clone())
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(FontSize::Small)
                .text_color(theme.muted_foreground)
                .child(svg().flex_none().path(ALERT_NOTE_ICON).size(px(12.)).text_color(theme.muted_foreground))
                .child(text)
                .into_any_element()
        });
        row_with(key_title(key), Some(key_hint(key)), RowExtras { leading, note }, control, reset, error, theme)
    }

    fn open_language_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = std::iter::once(PickItem::new("", rust_i18n::t!("settings.follow_system").into_owned()))
            .chain(
                crate::i18n::available()
                    .into_iter()
                    .map(|locale| PickItem::new(locale.clone(), language_name(&locale)).with_detail(locale)),
            )
            .collect();
        let current = self.config.language.as_deref().map(crate::i18n::configured).unwrap_or_default();
        self.open_picker(key_title("language"), items, Some(current), PickTarget::Value("language"), window, cx);
    }

    fn open_sound_picker(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let items = std::iter::once(PickItem::new("none", rust_i18n::t!("settings.no_sound").into_owned()))
            .chain(crate::sound::names().into_iter().map(|name| PickItem::new(name.clone(), name)))
            .collect();
        let current = self.config.values(key).into_iter().next();
        self.open_picker(key_title(key), items, current, PickTarget::Value(key), window, cx);
    }

    fn render_font_family(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let fonts = self.config.font_family.clone();
        let count = fonts.len();
        let mut out = div().flex().flex_col();
        for (ix, name) in fonts.iter().cloned().enumerate() {
            let (title, hint, reset, error) = if ix == 0 {
                (
                    key_title("font-family"),
                    Some(key_hint("font-family")),
                    self.reset("font-family", cx),
                    self.errors.get("font-family").cloned(),
                )
            } else {
                (rust_i18n::t!("settings.fallback_font", n = ix).into_owned(), None, None, None)
            };
            let current = name.clone();
            // 元素 id 按字体名取，不按行号：删掉一行后面的行号整体前移，辅助工具的焦点会跟着跳。同一个字体
            // 写了两遍时第二个带上序号，id 才不撞。
            let key = unique_key(&fonts[..ix], &name);
            let picker = dropdown(format!("font-{key}"), title.clone(), None, name.clone(), 240., theme)
                .on_press(cx, move |this, window, cx| this.open_font_picker(ix, Some(current.clone()), window, cx));
            let remove = (count > 1).then(|| {
                let label = rust_i18n::t!("settings.remove");
                Button::new(format!("remove-font-{key}"), label.clone())
                    .aria_label(action_for(&label, &name))
                    .icon(MINUS_ICON)
                    .icon_size(12.)
                    .with_size(ButtonSize::IconSm)
                    .on_press(cx, move |this, _, cx| {
                        let mut fonts = this.config.font_family.clone();
                        // 点下去时配置可能已经变了（连按两次、外部改了文件），行号过期就不动。
                        if ix < fonts.len() {
                            fonts.remove(ix);
                            this.write_or_report("font-family", fonts, cx);
                        }
                    })
            });
            let control = div().flex().items_center().gap(px(4.)).children(remove).child(picker);
            let font = row(title, hint, control, reset, error, theme);
            // 最后一个字体和下面的「添加」按钮是一起的，线画在按钮底下。
            out = out.child(if ix + 1 == count { font.border_b_0() } else { font });
        }
        let add = Button::new("add-font", rust_i18n::t!("settings.add_fallback_font").into_owned())
            .variant(ButtonVariant::Outline)
            .on_press(cx, move |this, window, cx| this.open_font_picker(count, None, window, cx));
        out.border_b_1().border_color(theme.control_border.opacity(0.6)).child(div().pb(px(12.)).flex().child(add))
    }

    fn open_font_picker(&mut self, ix: usize, current: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let mut names = cx.text_system().all_font_names();
        // 先按原样排好去重，再按不分大小写排：只差大小写的名字夹在中间时，一样的名字不挨着，`dedup` 去不掉。
        names.sort_unstable();
        names.dedup();
        names.sort_by_key(|name| name.to_lowercase());
        let items = names.into_iter().map(|name| PickItem::new(name.clone(), name)).collect();
        self.open_picker(key_title("font-family"), items, current, PickTarget::FontFamily(ix), window, cx);
    }

    fn render_palette(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let reset = self.reset("palette", cx);
        let error = self.errors.get("palette").cloned();
        let header = row(key_title("palette"), Some(key_hint("palette")), div(), reset, error, theme);
        let cells: Vec<_> = (0..16u8)
            .map(|ix| {
                let id = format!("palette:{ix}");
                let input = self.field(&id, Commit::Palette(ix), None, window, cx);
                let color = self.config.palette.iter().rfind(|(i, _)| *i == ix).map(|(_, c)| hsla(*c));
                let error = self.errors.get(&id).cloned();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .w(px(18.))
                                    .text_size(FontSize::Small)
                                    .text_color(theme.foreground.opacity(0.55))
                                    .child(ix.to_string()),
                            )
                            .child(swatch(color, 18., theme))
                            .child(input_box(&input, Some(92.), error.is_some(), theme)),
                    )
                    .children(error.map(|err| error_text(SharedString::from(format!("error:{id}")), err, theme)))
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .child(header)
            .child(div().py(px(10.)).grid().grid_cols(4).gap_x(px(12.)).gap_y(px(8.)).children(cells))
    }

    /// 状态栏上不显示的几块，一块一个标签；写回时写成一行，逗号隔开。
    fn render_status_bar_hidden(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        const KEY: &str = "status-bar-hidden";
        let hidden = self.config.status_bar_hidden.clone();
        let chips: Vec<_> = StatusItem::ALL
            .into_iter()
            .enumerate()
            .map(|(ix, item)| {
                let on = hidden.contains(&item);
                let (icon, title) = crate::window::status_item_icon_and_title(item);
                Chip::new(("status-hidden", ix), title, on).icon(icon).on_press(cx, move |this, _, cx| {
                    let mut hidden = this.config.status_bar_hidden.clone();
                    if on {
                        hidden.retain(|other| *other != item);
                    } else {
                        hidden.push(item);
                    }
                    let names: Vec<_> = hidden.iter().map(|item| item.name()).collect();
                    let values = if names.is_empty() { Vec::new() } else { vec![names.join(", ")] };
                    this.write_or_report(KEY, this.with_clear(KEY, values), cx);
                })
            })
            .collect();
        let reset = self.reset(KEY, cx);
        let error = self.errors.get(KEY).cloned();
        // 一排开关报成以设置项命名的一组，读屏读到单个开关时知道它管什么。
        let control =
            div().id(KEY).role(gpui::Role::Group).aria_label(key_title(KEY)).flex().gap(px(6.)).children(chips);
        row(key_title(KEY), Some(key_hint(KEY)), control, reset, error, theme)
    }

    /// 「不提醒的 agent」的筛选框，第一次用到时建；在框里按 Esc 把焦点还给设置页。
    fn agent_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((field, _)) = &self.agent_filter {
            return field.clone();
        }
        // 名字和占位文字各说一件事：读屏读空框时两样都念。
        let label = rust_i18n::t!("settings.filter_for", name = key_title(EXCLUDE_KEY)).into_owned();
        let placeholder = rust_i18n::t!("settings.agent_filter", count = exclude_choices(&[]).len()).into_owned();
        let field = cx.new(|cx| InputState::new(String::new(), cx).with_label(label).with_placeholder(placeholder));
        let changed = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed(_) => cx.notify(),
            InputEvent::Dismiss => window.focus(&this.focus_handle, cx),
            InputEvent::Next | InputEvent::Previous => {}
        });
        self.agent_filter = Some((field.clone(), changed));
        field
    }

    /// 不提醒的 agent，一个一个小标签；写回时写成一行，逗号隔开。
    fn render_agent_exclude(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        const KEY: &str = EXCLUDE_KEY;
        let filter = self.agent_filter(window, cx);
        let query = filter.read(cx).query().trim().to_lowercase();
        let choices = exclude_choices(&self.config.values(KEY));
        let selected = choices.iter().filter(|choice| choice.on).count();
        let (shown, hidden) = shown_agents(&choices, &query, self.agents_expanded);
        let chips: Vec<_> = shown
            .into_iter()
            .map(|choice| {
                let (label, on) = (choice.label, choice.on);
                // id 按 agent 取：筛选、展开时显示的小标签变了，同一个 agent 的 id 不变。
                Chip::new(SharedString::from(format!("exclude-{label}")), choice.name.clone(), on)
                    .rounded_full()
                    .px(px(11.))
                    .h(scaled(28.))
                    .on_press(cx, move |this, _, cx| {
                        let mut values = this.config.values(KEY);
                        if on {
                            values.retain(|name| name != label);
                        } else {
                            values.push(label.to_owned());
                        }
                        this.write_or_report(KEY, this.with_clear(KEY, values), cx);
                    })
            })
            .collect();
        let empty = (chips.is_empty() && !query.is_empty()).then(|| {
            let text = rust_i18n::t!("settings.no_matches").into_owned();
            div()
                .id("agent-exclude-empty")
                .aria_static_text(text.clone())
                .text_size(FontSize::Body)
                .text_color(theme.muted_foreground)
                .child(text)
        });
        // 没在筛选时才能展开、收起；展开着时收起按钮一直在。
        let expanded = self.agents_expanded;
        let more = (query.is_empty() && (hidden > 0 || expanded)).then(|| {
            let (text, icon) = if expanded {
                (rust_i18n::t!("settings.show_less").into_owned(), CHEVRON_UP_ICON)
            } else {
                (rust_i18n::t!("settings.show_more", count = hidden).into_owned(), CHEVRON_DOWN_ICON)
            };
            Button::new("agent-exclude-more", text.clone())
                .variant(ButtonVariant::Ghost)
                .aria_expanded(expanded)
                .text_color(theme.subtle_foreground)
                .child(text)
                .child(svg().flex_none().path(icon).size(px(12.)).text_color(theme.subtle_foreground))
                .on_press(cx, |this, _, cx| {
                    this.agents_expanded = !this.agents_expanded;
                    cx.notify();
                })
        });
        let search_icon = svg().flex_none().path(SEARCH_ICON).size(px(13.)).text_color(theme.muted_foreground);
        let field = Input::new(&filter)
            .prefix(search_icon)
            .flex_1()
            .min_w_0()
            .h(scaled(30.))
            .px(px(10.))
            .gap(px(8.))
            .rounded(px(8.))
            .bg(theme.field)
            .text_size(FontSize::Medium);
        let count = rust_i18n::t!("settings.selected_count", count = selected).into_owned();
        let count = div()
            .id("agent-exclude-count")
            .aria_static_text(count.clone())
            .flex_none()
            .text_size(FontSize::Body)
            .text_color(theme.subtle_foreground)
            .child(count);
        let reset = self.reset(KEY, cx);
        let error = self.errors.get(KEY).cloned();
        div()
            .pt(px(14.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().flex().items_center().gap(px(8.)).child(field).child(count).children(reset))
            .child(
                div()
                    .id(KEY)
                    .role(gpui::Role::Group)
                    .aria_label(key_title(KEY))
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(chips)
                    .children(empty),
            )
            .children(more.map(|more| div().flex().child(more)))
            .children(error.map(|err| error_text("agent-exclude-error", err, theme)))
    }

    fn render_config_files(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        const KEY: &str = "config-file";
        let files = self.file.values(KEY);
        let reset = self.reset(KEY, cx);
        let error = self.errors.get(KEY).cloned();
        let mut out = stacked(row(key_title(KEY), Some(key_hint(KEY)), div(), reset, error, theme), theme);
        // 末尾总留一个空的，在里面打字就是加一行。
        let last = files.len();
        for ix in 0..=last {
            let id = format!("{KEY}:{ix}");
            let placeholder = rust_i18n::t!("settings.config_file_placeholder").into_owned();
            let input = self.field(&id, Commit::Item(KEY, ix), Some(placeholder.into()), window, cx);
            let error = self.errors.get(&id).cloned();
            let remove = files.get(ix).map(|file| {
                let key = unique_key(&files[..ix], file);
                let label = rust_i18n::t!("settings.remove");
                Button::new(format!("remove-config-file-{key}"), label.clone())
                    .aria_label(action_for(&label, file))
                    .icon(MINUS_ICON)
                    .icon_size(12.)
                    .with_size(ButtonSize::IconSm)
                    .on_press(cx, move |this, _, cx| {
                        let mut files = this.file.values(KEY);
                        if ix < files.len() {
                            files.remove(ix);
                            this.write_or_report(KEY, files, cx);
                        }
                    })
            });
            out = out.child(
                div()
                    .pt(px(4.))
                    .pb(px(if ix == last { 12. } else { 4. }))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(input_box(&input, None, error.is_some(), theme))
                            .child(div().w(px(20.)).children(remove)),
                    )
                    .children(error.map(|err| error_text(SharedString::from(format!("error:{id}")), err, theme))),
            );
        }
        out
    }

    fn render_config_file_actions(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let path = runode_config::config_path().map(|path| display_dir(&path));
        let open = Button::new("open-config", rust_i18n::t!("settings.open_config").into_owned())
            .variant(ButtonVariant::Outline)
            .on_press(cx, |_, _, cx| crate::config::open(cx));
        let reload = Button::new("reload-config", rust_i18n::t!("settings.reload_config").into_owned())
            .variant(ButtonVariant::Outline)
            .on_press(cx, |_, _, cx| crate::config::reload(cx));
        let hint = rust_i18n::t!("settings.config_file_hint", path = path.unwrap_or_default()).into_owned();
        row(
            rust_i18n::t!("settings.config_file").into_owned(),
            Some(hint.into()),
            div().flex().gap(px(6.)).child(open).child(reload),
            None,
            None,
            theme,
        )
    }
}

/// 输入框空着时的提示：默认值，没有默认值的写「默认」；APNs 几项留空就是不用，给出写法的样子。
/// 填密钥的键：输入框里遮住，录屏、共享屏幕时不露出来。
pub(super) fn is_secret(key: &str) -> bool {
    matches!(key, "dictation-openai-key" | "dictation-doubao-key" | "dictation-polish-key")
}

fn key_placeholder(key: &str) -> SharedString {
    if is_secret(key) {
        return SharedString::default();
    }
    match key {
        "apns-key-file" => return "~/AuthKey_ABC123DEFG.p8".into(),
        "apns-key-id" => return "ABC123DEFG".into(),
        "apns-team-id" => return "DEF123GHIJ".into(),
        "apns-bundle-id" => return "com.example.runode".into(),
        "dictation-words" => return "runode, Ghostty, GPUI".into(),
        "dictation-polish-url" => return "https://api.deepseek.com/v1".into(),
        "dictation-polish-model" => return "deepseek-chat".into(),
        "ssh-program" => return "ssh".into(),
        "ssh-control-persist" => return runode_ssh::DEFAULT_PERSIST_SECS.to_string().into(),
        // 空着时配对的设备上显示系统的电脑名，就把它写出来。
        "remote-access-name" => return runode_remote::host_name().into(),
        _ => {}
    }
    match runode_config::Config::default().values(key).into_iter().next() {
        Some(value) => value.into(),
        None => rust_i18n::t!("settings.default").into_owned().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只在配置文件里改、不放设置页的键。`shell-integration` 指定一种 shell 只对认不出程序名的
    /// shell 管用，runode 启动的总是 `$SHELL`，没有换程序的配置项；放在设置页上多半被选错，
    /// 关掉集成的人也少，就只留在文件里。
    const FILE_ONLY: &[&str] = &["shell-integration"];

    /// 配置的每个键都在某一页上，而且只在一页上；`FILE_ONLY` 里的除外。
    #[test]
    fn every_key_has_a_place() {
        let mut keys: Vec<&str> = Page::ALL.into_iter().flat_map(Page::keys).collect();
        keys.sort();
        let mut all: Vec<&str> = runode_config::KEYS
            .iter()
            .flat_map(|group| group.iter().copied())
            .filter(|key| !FILE_ONLY.contains(key))
            .collect();
        all.sort();
        assert_eq!(keys, all);
    }

    /// 每个键、每页、每个选项都有翻译。
    #[test]
    fn every_choice_is_a_valid_value() {
        for page in Page::ALL {
            for item in page.items() {
                let Item::Row(key, control) = item else { continue };
                if matches!(control, Control::Choice) {
                    assert!(runode_config::choices(key).is_some(), "{key} has no choices");
                }
                if !matches!(control, Control::Choice | Control::Options(_)) {
                    continue;
                }
                for value in control_values(key, *control) {
                    assert_eq!(runode_config::check_value(key, value), Ok(()), "{key} = {value}");
                }
            }
        }
    }

    /// 左边的列表按组列页：每一页在且只在一组里，各组按 `Page::ALL` 的先后接起来正好是 `Page::ALL`，
    /// 列表的顺序和 `Page::ALL` 一致；每组都有标题的翻译。
    #[test]
    fn every_page_has_a_group() {
        let listed: Vec<Page> = Group::ALL.into_iter().flat_map(Group::pages).collect();
        assert_eq!(listed, Page::ALL);
        for locale in rust_i18n::available_locales!() {
            for group in Group::ALL {
                assert!(group.pages().next().is_some(), "{group:?} is empty");
                let key = format!("settings.group.{}", group.id());
                assert!(!rust_i18n::t!(&key, locale = &locale).contains("settings."), "{locale} misses {key}");
            }
        }
    }

    /// 每一页标题下面都有一句说明。
    #[test]
    fn every_page_has_a_description() {
        for locale in rust_i18n::available_locales!() {
            for page in Page::ALL {
                let key = format!("settings.page_desc.{}", page.id());
                let text = rust_i18n::t!(&key, locale = &locale);
                assert!(!text.contains("settings.") && !text.trim().is_empty(), "{locale} misses {key}");
            }
        }
    }

    fn choices(names: &[(&'static str, bool)]) -> Vec<ExcludeChoice> {
        names.iter().map(|&(label, on)| ExcludeChoice { label, name: label.to_uppercase(), on }).collect()
    }

    /// 没在筛选时选上的排在前面，没选上的收起时只列前几个；筛选时列出对得上的，不藏。
    #[test]
    fn excluded_agents_come_first_and_the_rest_fold() {
        let mut names: Vec<(&'static str, bool)> = vec![("aider", true)];
        let rest = ["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "a9", "a10", "a11", "a12", "a13"];
        names.extend(rest.iter().map(|name| (*name, false)));
        names.push(("other", true));
        let all = choices(&names);
        let labels = |(shown, _): &(Vec<&ExcludeChoice>, usize)| shown.iter().map(|c| c.label).collect::<Vec<_>>();

        let folded = shown_agents(&all, "", false);
        assert_eq!(labels(&folded)[..3], ["aider", "other", "a0"]);
        assert_eq!(folded.0.len(), 2 + COLLAPSED_AGENTS);
        assert_eq!(folded.1, rest.len() - COLLAPSED_AGENTS);

        let expanded = shown_agents(&all, "", true);
        assert_eq!((expanded.0.len(), expanded.1), (all.len(), 0));

        let filtered = shown_agents(&all, "a1", false);
        assert_eq!(labels(&filtered), ["a1", "a10", "a11", "a12", "a13"]);
        assert_eq!(filtered.1, 0);
    }

    /// 搜索按当前语言的标题、说明，英文的标题，和键名都找得到一行。
    #[test]
    fn search_finds_rows_by_title_hint_and_english_name() {
        let row = Item::Row("scrollback-limit", Control::Text(120.));
        for query in ["回滚历史", "字节", "scrollback size", "scrollback-limit"] {
            assert!(row.matches(query, "zh-Hans"), "{query}");
        }
        assert!(!row.matches("字号", "zh-Hans"));
    }

    /// 小节的标题对得上时这一节的项都列出来，小节的标题补在它们前面。
    #[test]
    fn search_lists_a_matching_section_with_its_rows() {
        let found = search_items(Page::Terminal.items(), |_| true, "剪贴板", "zh-Hans");
        let names: Vec<String> = found
            .iter()
            .map(|item| match item {
                Item::Section(name) => format!("#{name}"),
                Item::Row(key, _) => (*key).to_owned(),
                _ => "?".to_owned(),
            })
            .collect();
        assert_eq!(names, ["#clipboard", "clipboard-write", "clipboard-read"]);
    }

    /// 每一项在各种语言下都有拿来搜的文字，没有漏翻译的。
    #[test]
    fn every_item_has_search_text() {
        for locale in rust_i18n::available_locales!() {
            for page in Page::ALL {
                for item in page.items() {
                    let texts = item.search_texts(&locale);
                    assert!(!texts.is_empty(), "{locale} {page:?}");
                    for text in texts {
                        // 漏了翻译时拿到的是不带空格的键本身。
                        let key = text.starts_with("settings.") && !text.contains(' ');
                        assert!(!text.trim().is_empty() && !key, "{locale} {page:?}: {text}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_label_is_translated() {
        for locale in rust_i18n::available_locales!() {
            let t = |key: &str| rust_i18n::t!(key, locale = &locale).into_owned();
            let missing = |key: &str| {
                let text = t(key);
                assert!(!text.contains("settings."), "{locale} misses {key}");
            };
            for page in Page::ALL {
                missing(&format!("settings.page.{}", page.id()));
                for item in page.items() {
                    match item {
                        Item::Row(key, control) => {
                            missing(&format!("settings.key.{}", tr_key(key)));
                            missing(&hint_key(key, &locale));
                            if matches!(control, Control::Choice | Control::Options(_)) {
                                for value in control_values(key, *control).iter().filter(|value| !value.is_empty()) {
                                    missing(&format!("settings.choice.{}.{value}", tr_key(key)));
                                }
                            }
                        }
                        Item::Section(name) => missing(&format!("settings.section.{name}")),
                        _ => {}
                    }
                }
                for key in page.keys() {
                    missing(&format!("settings.key.{}", tr_key(key)));
                    missing(&hint_key(key, &locale));
                }
            }
        }
    }
}
