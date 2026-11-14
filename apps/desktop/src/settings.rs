//! 设置页：配置文件里的每一项做成开关、选项、输入框或列表，改了就写回 runode 自己的配置文件
//! （`runode_config::ConfigFile`，文件里别的行原样留着），再重载配置让各个窗口跟着变。铺在终端窗口
//! 里（`window::show_settings`），盖住侧栏和终端；点右上的关闭或按 Esc 发 `Close`，回到终端。
//!
//! 显示的是合起来生效的值：Ghostty 的配置、主题和 runode 的配置文件叠在一起之后的结果。runode 的
//! 配置文件里写了的项旁边有恢复按钮，点了删掉那几行，回到 Ghostty、主题或内置的值。输入框停手
//! 一会儿、按回车或者失去焦点时写回，写之前按读配置时的规矩检查，不对就不写、在那一项下面说原因。
//!
//! 哪一页有哪些项在 `pages`，开关、选项这些控件在 `controls`，从长列表里挑一项的浮层在 `picker`，
//! 快捷键那一页在 `keybinds`，远程访问那一页的配对手机在 `pairing`、测试推送在 `test_push`，外观页的配色在 `theme`，
//! 远程主机页的 SSH 主机列表在 `ssh_hosts`、配对的电脑在 `paired_machines`。

mod autostart;
mod controls;
mod dictation;
mod firewall;
mod keybinds;
mod pages;
mod paired_machines;
mod pairing;
mod picker;
mod ssh_hosts;
mod test_push;
mod theme;

use crate::ui::theme::ActiveTheme as _;
use crate::ui::theme::Theme;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, Render,
    Role, ScrollHandle, SharedString, Subscription, Task, Window, div, point, prelude::*, px, svg,
};
use runode_config::{Config, ConfigFile};

use crate::{
    config::AppConfig,
    i18n::tr,
    ui::icons::{CLOSE_ICON, CODE_ICON, SEARCH_ICON},
    ui::{
        a11y::{Hide, Press},
        button::{Button, ButtonVariant},
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
        kbd::Kbd,
    },
};
use pages::{Group, Page};
use picker::Picker;

/// 左边页面列表的宽度，按基准字号的点数。
const NAV_WIDTH: f32 = 220.;
/// 输入框停手这么久之后写回。
const COMMIT_DELAY: Duration = Duration::from_millis(700);

/// 问还有工作区、终端用着哪些远端主机。
type HostsInUse = Box<dyn Fn(&App) -> HashSet<crate::host_client::hosts::HostId>>;

/// 点了关闭、按了 Esc 或关分屏的键：窗口收起设置页。
pub struct Close;

// 关分屏的键在设置页里收起设置页，见 `keybinds::bind`。
gpui::actions!(runode, [CloseSettings]);

/// 输入框里的文字改的是什么。
#[derive(Clone, Debug, PartialEq)]
enum Commit {
    /// 只有一个值的键；空着就删掉，回到默认。
    Value(&'static str),
    /// 可以写多行的键里的第几行；空着就删掉这一行。
    Item(&'static str, usize),
    /// 调色板里的一个序号。
    Palette(u8),
}

struct Field {
    input: Entity<InputState>,
    commit: Commit,
    /// 停手一会儿再写回的计时。
    pending: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    page: Page,
    config: Arc<Config>,
    /// runode 自己的配置文件，写回时在它上面改。
    file: ConfigFile,
    /// 各个输入框，按 `field_id`；换页时清空。
    fields: HashMap<String, Field>,
    /// 写回失败的原因，按输入框或键名。
    errors: HashMap<String, String>,
    picker: Option<Picker>,
    keybinds: keybinds::State,
    /// 读过的主题的颜色，按主题名；找不到的主题记为 `None`，不再去找。
    theme_looks: HashMap<String, Option<theme::Look>>,
    scroll: ScrollHandle,
    test_push: test_push::State,
    /// 正在后台装上或去掉的自启，按 `Kind::name`。
    autostart_jobs: HashMap<&'static str, Task<()>>,
    /// 各自启装没装，按 `Kind::name`，在后台查（`refresh_autostart`）；还没查到的不在里面。
    autostart: HashMap<&'static str, bool>,
    firewall: firewall::State,
    _observe: Subscription,
    /// 语音输入的模型下载有了进度。
    _downloads: Subscription,
    /// 还有工作区、终端用着的远端主机，由开设置页的窗口给。
    hosts_in_use: HostsInUse,
    /// 远端主机的连接状态变了。
    _hosts: Subscription,
    /// 正在改显示名的配对的电脑。
    renaming_machine: Option<paired_machines::Renaming>,
    /// 「不提醒的 agent」的筛选框，第一次画那一组时建。
    agent_filter: Option<(Entity<InputState>, Subscription)>,
    /// 「不提醒的 agent」没选上的全列出来，不只列前几个。
    agents_expanded: bool,
    /// 左边的搜索框；有字时页面换成各页对得上的项（`render_search`）。
    search: Entity<InputState>,
    _search: Subscription,
}

impl EventEmitter<Close> for SettingsView {}

impl SettingsView {
    pub fn new(
        hosts_in_use: impl Fn(&App) -> HashSet<crate::host_client::hosts::HostId> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe_global_in::<AppConfig>(window, |this, window, cx| this.config_changed(window, cx));
        let label = rust_i18n::t!("settings.search").into_owned();
        let placeholder = rust_i18n::t!("settings.search_placeholder").into_owned();
        let search = cx.new(|cx| InputState::new(String::new(), cx).with_label(label).with_placeholder(placeholder));
        // Esc 清掉搜索，焦点还给页面，再按一次才收起设置页。
        let search_events = cx.subscribe_in(&search, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed(_) => cx.notify(),
            InputEvent::Dismiss => {
                this.clear_search(cx);
                window.focus(&this.focus_handle, cx);
            }
            InputEvent::Next | InputEvent::Previous => {}
        });
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            page: Page::General,
            config: cx.global::<AppConfig>().0.clone(),
            file: read_file(),
            fields: HashMap::new(),
            errors: HashMap::new(),
            picker: None,
            keybinds: keybinds::State::default(),
            theme_looks: HashMap::new(),
            scroll: ScrollHandle::new(),
            test_push: test_push::State::default(),
            autostart_jobs: HashMap::new(),
            autostart: HashMap::new(),
            firewall: firewall::State::default(),
            _observe: observe,
            _downloads: cx.observe_global::<crate::dictation::Downloads>(|_, cx| cx.notify()),
            hosts_in_use: Box::new(hosts_in_use),
            _hosts: cx.observe_global::<crate::remote_hosts::HostsChanged>(|_, cx| cx.notify()),
            renaming_machine: None,
            agent_filter: None,
            agents_expanded: false,
            search,
            _search: search_events,
        };
        view.refresh_autostart(cx);
        view.refresh_firewall(cx);
        view
    }

    /// 配置重载了：换上新值，没在编辑的输入框跟着更新。
    fn config_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (on, port) = (self.config.remote_access, self.config.remote_access_port);
        self.config = cx.global::<AppConfig>().0.clone();
        if (self.config.remote_access, self.config.remote_access_port) != (on, port) {
            self.refresh_firewall(cx);
        }
        self.file = read_file();
        for field in self.fields.values() {
            if field.input.focus_handle(cx).is_focused(window) {
                continue;
            }
            let text = self.field_text(&field.commit);
            field.input.update(cx, |input, cx| {
                if input.query() != text {
                    input.set_query(text, cx);
                }
            });
        }
        cx.notify();
    }

    /// 换到远程主机页的主机列表。
    pub fn show_ssh_hosts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.select_page(Page::Ssh, window, cx);
    }

    fn select_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.commit_all(cx);
        self.page = page;
        self.fields.clear();
        self.errors.clear();
        self.picker = None;
        self.keybinds.stop_recording();
        self.scroll.set_offset(point(px(0.), px(0.)));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_query(String::new(), cx));
        cx.notify();
    }

    /// 输入框里该显示的文字：当前生效的值。
    fn field_text(&self, commit: &Commit) -> String {
        match commit {
            Commit::Value(key) => self.config.values(key).into_iter().next().unwrap_or_default(),
            Commit::Item(key, ix) => self.list_values(key).get(*ix).cloned().unwrap_or_default(),
            Commit::Palette(ix) => self
                .config
                .palette
                .iter()
                .rfind(|(i, _)| i == ix)
                .map(|(_, c)| runode_config::hex(*c))
                .unwrap_or_default(),
        }
    }

    /// 可以写多行的键当前的各行：生效的值；`config-file` 读进来不留原文，看配置文件本身。
    fn list_values(&self, key: &str) -> Vec<String> {
        match key {
            "config-file" => self.file.values(key),
            _ => self.config.values(key),
        }
    }

    /// `id` 这个输入框，没有就建一个，显示 `commit` 对应的值。
    fn field(
        &mut self,
        id: &str,
        commit: Commit,
        placeholder: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(field) = self.fields.get(id) {
            return field.input.clone();
        }
        let text = self.field_text(&commit);
        let label = match &commit {
            Commit::Value(key) | Commit::Item(key, _) => pages::key_title(key),
            Commit::Palette(ix) => format!("{} {ix}", pages::key_title("palette")),
        };
        let input = cx.new(|cx| {
            let input = InputState::editing(text, 0, cx).with_label(label);
            let input = if pages::is_secret(id) { input.with_masked() } else { input };
            match placeholder {
                Some(placeholder) => input.with_placeholder(placeholder),
                None => input,
            }
        });
        let key = id.to_owned();
        let changed = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed(_) => this.schedule_commit(&key, cx),
            InputEvent::Next | InputEvent::Previous => this.commit(&key, cx),
            InputEvent::Dismiss => {
                this.revert(&key, cx);
                window.focus(&this.focus_handle, cx);
            }
        });
        let key = id.to_owned();
        let blurred = cx.on_focus_out(&input.focus_handle(cx), window, move |this, _, _, cx| this.commit(&key, cx));
        self.fields.insert(
            id.to_owned(),
            Field { input: input.clone(), commit, pending: None, _subscriptions: [changed, blurred] },
        );
        input
    }

    fn schedule_commit(&mut self, id: &str, cx: &mut Context<Self>) {
        let key = id.to_owned();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMMIT_DELAY).await;
            this.update(cx, |this, cx| this.commit_field(&key, false, cx)).ok();
        });
        if let Some(field) = self.fields.get_mut(id) {
            field.pending = Some(task);
        }
    }

    /// 换页、收起前把还没写回的输入框写回。
    pub fn commit_all(&mut self, cx: &mut Context<Self>) {
        let pending: Vec<String> =
            self.fields.iter().filter(|(_, field)| field.pending.is_some()).map(|(id, _)| id.clone()).collect();
        for id in pending {
            self.commit(&id, cx);
        }
    }

    /// 把输入框 `id` 里的文字写回配置文件；和当前的值一样时不写。
    fn commit(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commit_field(id, true, cx);
    }

    /// `settled` 为假时是打字停了一会儿：超出范围的先不写，等按回车、离开输入框或换页时再换成边界值，
    /// 打到一半的数（想打 16 先打出的 1）不会被改掉。
    fn commit_field(&mut self, id: &str, settled: bool, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get_mut(id) else {
            return;
        };
        let typed = field.input.read(cx).query().trim().to_owned();
        let (input, commit) = (field.input.clone(), field.commit.clone());
        let text = clamp_field(&commit, &typed);
        let clamped = text != typed;
        if clamped && !settled {
            return;
        }
        field.pending = None;
        if text == self.field_text(&commit) {
            if clamped {
                input.update(cx, |input, cx| input.set_query(text, cx));
            }
            self.errors.remove(id);
            cx.notify();
            return;
        }
        // 清空一行删掉了它，下面的行往上挪了一格。
        let mut removed = false;
        let written = match commit {
            Commit::Value(key) => self.write(key, self.single(key, text.clone()), cx),
            Commit::Item(key, ix) => {
                let mut values = self.list_values(key);
                match (ix < values.len(), text.is_empty()) {
                    (true, true) => {
                        values.remove(ix);
                        removed = true;
                    }
                    (true, false) => values[ix] = text.clone(),
                    (false, true) => {}
                    (false, false) => values.push(text.clone()),
                }
                self.write(key, values, cx)
            }
            Commit::Palette(ix) => {
                let mut values = self.file.values("palette");
                values.retain(|value| palette_index(value) != Some(ix));
                if !text.is_empty() {
                    values.push(format!("{ix}={text}"));
                }
                self.write("palette", values, cx)
            }
        };
        // 超出范围改成了能取的值，框里也换成它，免得写着一个没生效的数。
        if written.is_ok() && clamped {
            input.update(cx, |input, cx| input.set_query(text, cx));
        }
        let removed = removed && written.is_ok();
        match written {
            Ok(()) => drop(self.errors.remove(id)),
            Err(err) => drop(self.errors.insert(id.to_owned(), err)),
        }
        // 这个输入框还按行号对着这一行，现在那里是原来的下一行。重载配置时正在编辑的输入框不跟着换，
        // 这里换成挪上来的那一行：不然还空着，失去焦点时再写回一次，会把挪上来的那一行也删掉。
        if removed {
            let text = self.field_text(&commit);
            input.update(cx, |input, cx| input.set_query(text, cx));
        }
        cx.notify();
    }

    /// Esc：放弃输入框里改的，换回当前的值。
    fn revert(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get_mut(id) else {
            return;
        };
        field.pending = None;
        let (input, commit) = (field.input.clone(), field.commit.clone());
        let text = self.field_text(&commit);
        input.update(cx, |input, cx| input.set_query(text, cx));
        self.errors.remove(id);
        cx.notify();
    }

    /// 把 `key` 在 runode 配置文件里的值换成 `values` 并重载配置；`values` 为空就是删掉，回到
    /// Ghostty、主题或内置的值。新写的值先按读配置的规矩检查，文件里本来就有的不再查（手写错了的
    /// 那几行留着，读配置时照样跳过）。
    fn write(&mut self, key: &str, values: Vec<String>, cx: &mut Context<Self>) -> Result<(), String> {
        let old = self.file.values(key);
        for value in values.iter().filter(|value| !old.contains(value)) {
            runode_config::check_value(key, value)
                .map_err(|err| rust_i18n::t!("settings.invalid", err = config_error(&err)).into_owned())?;
        }
        self.write_unchecked(key, &values, cx)
    }

    /// 单值的项要写回的各行。值正是删掉这一项后会回到的值（Ghostty 的配置、主题或内置的值）时删掉这一项，
    /// 恢复按钮跟着消失。
    fn single(&self, key: &str, value: String) -> Vec<String> {
        if value.is_empty() || Config::load_without(self.config.dark, key).same_value(key, &value) {
            Vec::new()
        } else {
            vec![value]
        }
    }

    fn write_unchecked(&mut self, key: &str, values: &[String], cx: &mut Context<Self>) -> Result<(), String> {
        let path = runode_config::config_path().ok_or_else(|| rust_i18n::t!("settings.no_home").into_owned())?;
        self.file = crate::config::write_values(&path, key, values, cx)
            .map_err(|err| rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned())?;
        self.errors.remove(key);
        Ok(())
    }

    /// 写回，失败时把原因记在 `key` 这一项下面。
    fn write_or_report(&mut self, key: &str, values: Vec<String>, cx: &mut Context<Self>) {
        if let Err(err) = self.write(key, values, cx) {
            self.errors.insert(key.to_owned(), err);
        }
        cx.notify();
    }

    /// 左边的页面列表：顶上是搜索框，下面按组列出各页，最下面是打开配置文件。
    fn render_nav(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let item = |id: SharedString, icon: &'static str, label: String, selected: bool| {
            let color = if selected { theme.foreground } else { theme.subtle_foreground };
            div()
                .id(id)
                .aria_label(label.clone())
                .flex_none()
                .h(scaled(30.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(7.))
                .text_size(FontSize::Medium)
                .text_color(color)
                .map(|item| {
                    if selected {
                        item.bg(theme.tab_active).font_weight(gpui::FontWeight::SEMIBOLD)
                    } else {
                        item.hover(|item| item.bg(theme.accent).text_color(theme.foreground))
                    }
                })
                .child(svg().flex_none().path(icon).size(px(15.)).text_color(color))
                .child(div().min_w_0().truncate().child(label))
        };
        let groups: Vec<_> = Group::ALL
            .into_iter()
            .map(|group| {
                let title = group.title();
                let heading = div()
                    .id(SharedString::from(format!("settings-group-{}", group.id())))
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(title.clone())
                    .pt(px(14.))
                    .pb(px(6.))
                    .pl(px(10.))
                    .text_size(FontSize::Body)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(title.clone());
                let pages = div()
                    .id(SharedString::from(format!("settings-pages-{}", group.id())))
                    .role(Role::TabList)
                    .aria_label(title)
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .children(group.pages().map(|page| {
                        item(page.id().into(), page.icon(), page.title(), page == self.page)
                            .role(Role::Tab)
                            .aria_selected(page == self.page)
                            .on_press(cx, move |this, window, cx| this.select_page(page, window, cx))
                    }));
                div().flex().flex_col().child(heading).child(pages)
            })
            .collect();
        let open_config = item("settings-open-config".into(), CODE_ICON, tr("menu.open_config"), false)
            .role(Role::Button)
            .on_press(cx, |_, _, cx| crate::config::open(cx));
        div()
            .flex_none()
            .w(scaled(NAV_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.mix(0.035))
            .border_r_1()
            .border_color(theme.control_border)
            .id("settings-nav")
            .child(titlebar_strip())
            .child(
                div().flex_none().px(px(10.)).pb(px(4.)).flex().child(
                    Input::new(&self.search)
                        .prefix(svg().flex_none().path(SEARCH_ICON).size(px(13.)).text_color(theme.muted_foreground))
                        .flex_1()
                        .min_w_0()
                        .h(scaled(30.))
                        .px(px(10.))
                        .gap(px(8.))
                        .rounded(px(8.))
                        .bg(theme.field)
                        .text_size(FontSize::Medium),
                ),
            )
            // 界面字号调大、窗口又矮时一列放不下，自己滚。
            .child(
                div()
                    .id("settings-nav-items")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .px(px(10.))
                    .pb(px(10.))
                    .children(groups),
            )
            .child(div().flex_none().px(px(10.)).pt(px(6.)).pb(px(14.)).flex().flex_col().child(open_config))
    }

    /// Esc：焦点在页面本身（不在输入框、挑选浮层里，也没在录快捷键）时收起。
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape"
            && !event.keystroke.modifiers.modified()
            && self.focus_handle.is_focused(window)
            && self.picker.is_none()
        {
            cx.stop_propagation();
            cx.emit(Close);
        }
    }
}

/// 顶上那一条：放红绿灯，按住拖动窗口，双击缩放。
fn titlebar_strip() -> gpui::Div {
    div()
        .flex_none()
        .h(px(crate::window::TITLEBAR_HEIGHT))
        .w_full()
        .window_control_area(gpui::WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, crate::ui::drag_window)
}

/// `palette` 一行的序号。
fn palette_index(value: &str) -> Option<u8> {
    value.split_once('=')?.0.trim().parse().ok()
}

/// runode 的配置文件；读不出来时当作空的，写回时会再读一遍并报错。
fn read_file() -> ConfigFile {
    let Some(path) = runode_config::config_path() else {
        return ConfigFile::default();
    };
    ConfigFile::read(&path).unwrap_or_else(|err| {
        tracing::warn!("failed to read {}: {err}", path.display());
        ConfigFile::default()
    })
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.theme().clone();
        let nav = self.render_nav(theme, cx);
        let content = self.render_page(theme, window, cx);
        let picker = self.render_picker(theme, cx);
        // 挑选浮层挡住了后面的页面，开着时后面的不报给辅助工具。
        let modal = picker.is_some();
        // 盖在顶上那一条的右端：挡住下面的拖动区，按下才是点按钮，不是拖窗口。
        let close = Button::new("settings-close", rust_i18n::t!("settings.close_label").into_owned())
            .variant(ButtonVariant::Ghost)
            .icon(CLOSE_ICON)
            .icon_size(13.)
            .h(scaled(28.))
            .pl(px(9.))
            .pr(px(6.))
            .gap(px(7.))
            .rounded(px(7.))
            .text_color(theme.subtle_foreground)
            .occlude()
            .child(rust_i18n::t!("settings.close").into_owned())
            .child(Kbd::new("esc"))
            .on_press(cx, |_, _, cx| cx.emit(Close));
        let close = div()
            .id("settings-close-area")
            .aria_hidden(modal)
            .absolute()
            .top_0()
            .right(px(14.))
            .h(px(crate::window::TITLEBAR_HEIGHT))
            .flex()
            .items_center()
            .child(close);
        div()
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(Close)))
            .size_full()
            .relative()
            .flex()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_size(FontSize::Medium)
            .child(nav.aria_hidden(modal))
            .child(
                div().flex_1().min_w_0().h_full().flex().flex_col().child(titlebar_strip()).child(
                    div()
                        .id("settings-page")
                        .aria_hidden(modal)
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(div().mx_auto().w_full().max_w(px(760.)).px(px(32.)).pb(px(32.)).child(content)),
                ),
            )
            .child(close)
            .children(picker)
    }
}

/// 有上下限的项（界面字号）超出范围时换成能取的边界值，别的原样。
fn clamp_field(commit: &Commit, text: &str) -> String {
    use crate::ui::font_size::{UI_FONT_SIZE_MAX, UI_FONT_SIZE_MIN};
    match (commit, text.parse::<f32>()) {
        (Commit::Value("ui-font-size"), Ok(size)) if size.is_finite() => {
            size.clamp(UI_FONT_SIZE_MIN, UI_FONT_SIZE_MAX).to_string()
        }
        _ => text.to_owned(),
    }
}

/// 读配置时报的错是英文的（`runode_config` 只有英文），设置页上常见的几种换成界面语言，别的照原文。
pub(crate) fn config_error(err: &str) -> String {
    let key = match err {
        "expected a number" => "settings.config_error.number",
        "expected a positive number" => "settings.config_error.positive",
        "expected a non-negative number of seconds" => "settings.config_error.seconds",
        "expected true or false" => "settings.config_error.bool",
        "expected a port from 1 to 65535" => "settings.config_error.port",
        "not a color" => "settings.config_error.color",
        _ => return err.to_owned(),
    };
    rust_i18n::t!(key).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ui_font_size_is_clamped_when_written() {
        let size = Commit::Value("ui-font-size");
        assert_eq!(clamp_field(&size, "30"), "20");
        assert_eq!(clamp_field(&size, "9"), "11");
        assert_eq!(clamp_field(&size, "13.5"), "13.5");
        assert_eq!(clamp_field(&size, "abc"), "abc");
        assert_eq!(clamp_field(&Commit::Value("font-size"), "30"), "30");
    }

    /// 设置页上几种常见的错按读配置时报的原文换成界面语言：原文改了这里就对不上了。
    #[test]
    fn common_config_errors_are_translated() {
        for (key, value) in [
            ("preview-font-size", "abc"),
            ("cursor-style-blink-timeout", "-1"),
            ("remote-access-port", "0"),
            ("cursor-style-blink", "maybe"),
        ] {
            let err = runode_config::check_value(key, value).unwrap_err();
            assert_ne!(config_error(&err), err, "{key}={value}: {err}");
        }
    }

    #[test]
    fn palette_lines_have_an_index() {
        assert_eq!(palette_index("3=#ffffff"), Some(3));
        assert_eq!(palette_index(" 12 = red"), Some(12));
        assert_eq!(palette_index("x=#fff"), None);
    }
}
