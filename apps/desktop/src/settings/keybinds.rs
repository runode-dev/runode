//! 快捷键那一页：上面是配置文件里写的 `keybind`，一行一条，触发键点一下再按新的键录下来，动作
//! 从列表里挑，带参数的动作旁边填参数；下面列出默认绑定，可以一条条解除，也可以整个不用。
//!
//! 录键时拦下这个窗口里按的键（`App::intercept_keystrokes`），不让它当快捷键或者菜单命令生效；
//! 一次只录一个按键，连按几个键的写法（`ctrl+a>n`）要在配置文件里写。

use gpui::{App, Context, Div, Keystroke, SharedString, Subscription, Window, div, prelude::*, px};
use runode_config::{ConfigFile, keybind};

use super::{
    SettingsView,
    controls::{Colors, button, dropdown, icon_button, input_box, on_click, row, section, switch},
    picker::{PickItem, PickTarget},
};
use crate::settings::Commit;

const KEY: &str = "keybind";

#[derive(Default)]
pub(super) struct State {
    recording: Option<Recording>,
}

struct Recording {
    target: Record,
    _intercept: Subscription,
}

/// 录的键给谁用。
#[derive(Clone, Copy, PartialEq)]
enum Record {
    /// 换掉第几条的触发键。
    Row(usize),
    /// 新加一条，录好了再挑动作。
    New,
}

/// 挑好的动作给谁用。
#[derive(Clone)]
pub(super) enum Target {
    Row(usize),
    /// 新加的一条，触发键已经录好。
    New(String),
}

impl State {
    pub fn stop_recording(&mut self) {
        self.recording = None;
    }

    fn recording(&self, target: Record) -> bool {
        self.recording.as_ref().is_some_and(|recording| recording.target == target)
    }
}

/// 一条 `keybind` 拆成触发键、动作名和参数；`clear` 和写错了的为 `None`。
fn split(value: &str) -> Option<(&str, &str, Option<&str>)> {
    let (trigger, action) = value.split_once('=')?;
    let action = action.trim();
    let (name, param) = match action.split_once(':') {
        Some((name, param)) => (name, Some(param)),
        None => (action, None),
    };
    Some((trigger.trim(), name, param))
}

fn join(trigger: &str, name: &str, param: Option<&str>) -> String {
    match param {
        Some(param) => format!("{trigger}={name}:{param}"),
        None => format!("{trigger}={name}"),
    }
}

/// 第 `row` 条的参数。
pub(super) fn param(file: &ConfigFile, row: usize) -> String {
    file.values(KEY).get(row).and_then(|value| split(value)?.2.map(str::to_owned)).unwrap_or_default()
}

/// 改第 `row` 条的参数，改完先检查，不对就不写。
pub(super) fn set_param(
    view: &mut SettingsView,
    row: usize,
    text: &str,
    cx: &mut Context<SettingsView>,
) -> Result<(), String> {
    let mut values = view.file.values(KEY);
    let Some((trigger, name, _)) = values.get(row).and_then(|value| split(value)) else {
        return Ok(());
    };
    let line = join(trigger, name, Some(text));
    keybind::parse(&line).map_err(|err| rust_i18n::t!("settings.invalid", err = err).into_owned())?;
    values[row] = line;
    view.write_unchecked(KEY, &values, cx)
}

/// 动作新选上时先填的参数：数字填 1，几个可选的值填第一个，文字留空等用户填。
fn default_param(name: &str) -> Option<String> {
    let spec = keybind::ACTIONS.iter().find(|spec| spec.name == name)?;
    Some(match spec.param? {
        "N" => "1".to_owned(),
        hint if hint.contains('|') => hint.split('|').next().unwrap_or_default().to_owned(),
        _ => String::new(),
    })
}

/// 触发键显示成按键符号，比如 `shift+cmd+t` 显示成 ⇧⌘T；认不出的原样显示。
fn pretty(trigger: &str) -> String {
    let Ok(keys) = keybind::parse_trigger(trigger) else {
        return trigger.to_owned();
    };
    keys.split(' ')
        .map(|stroke| Keystroke::parse(stroke).map_or_else(|_| stroke.to_owned(), |keystroke| keystroke.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn action_label(name: &str) -> String {
    match name {
        "unbind" => rust_i18n::t!("settings.keybind.unbind").into_owned(),
        name if keybind::ACTIONS.iter().any(|spec| spec.name == name) => keybind::describe(name),
        name => name.to_owned(),
    }
}

/// 挑好了动作。
pub(super) fn picked(
    view: &mut SettingsView,
    target: Target,
    name: String,
    _: &mut Window,
    cx: &mut Context<SettingsView>,
) {
    let mut values = view.file.values(KEY);
    match target {
        Target::Row(row) => {
            let Some((trigger, old, param)) = values.get(row).and_then(|value| split(value)) else {
                return;
            };
            // 换了动作就换成新动作的默认参数；没换就留着原来的。
            let param = if old == name { param.map(str::to_owned) } else { default_param(&name) };
            values[row] = join(trigger, &name, param.as_deref());
        }
        Target::New(trigger) => values.push(join(&trigger, &name, default_param(&name).as_deref())),
    }
    report(view, &values, cx);
}

fn report(view: &mut SettingsView, values: &[String], cx: &mut Context<SettingsView>) {
    if let Err(err) = view.write_unchecked(KEY, values, cx) {
        view.errors.insert(KEY.to_owned(), err);
    }
    cx.notify();
}

impl SettingsView {
    fn start_recording(&mut self, target: Record, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let view = cx.entity().downgrade();
        let intercept = cx.intercept_keystrokes(move |event, window, cx: &mut App| {
            if window.window_handle() != handle {
                return;
            }
            cx.stop_propagation();
            let keystroke = event.keystroke.clone();
            view.update(cx, |view, cx| view.recorded(&keystroke, window, cx)).ok();
        });
        window.focus(&self.focus_handle, cx);
        self.keybinds.recording = Some(Recording { target, _intercept: intercept });
        cx.notify();
    }

    /// 录到了一个键。Esc 取消；只按了修饰键、或者写不进配置的键，接着等。
    fn recorded(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = &self.keybinds.recording else {
            return;
        };
        let target = recording.target;
        if keystroke.unparse() == "escape" {
            self.keybinds.stop_recording();
            cx.notify();
            return;
        }
        let Some(trigger) = keybind::format_trigger(&keystroke.unparse()) else {
            return;
        };
        self.keybinds.stop_recording();
        match target {
            Record::Row(row) => {
                let mut values = self.file.values(KEY);
                let Some((_, name, param)) = values.get(row).and_then(|value| split(value)) else {
                    return;
                };
                values[row] = join(&trigger, name, param);
                report(self, &values, cx);
            }
            Record::New => self.open_action_picker(Target::New(trigger), None, window, cx),
        }
        cx.notify();
    }

    fn open_action_picker(
        &mut self,
        target: Target,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = keybind::ACTIONS
            .iter()
            .map(|spec| {
                let usage = match spec.param {
                    Some(param) => format!("{}:{param}", spec.name),
                    None => spec.name.to_owned(),
                };
                PickItem::new(spec.name, keybind::describe(spec.name)).with_detail(usage)
            })
            .chain([PickItem::new("unbind", action_label("unbind")).with_detail("unbind")])
            .collect();
        let title = rust_i18n::t!("settings.keybind.action").into_owned();
        self.open_picker(title, items, current, PickTarget::Keybind(target), window, cx);
    }

    pub(super) fn render_keybinds(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let values = self.file.values(KEY);
        let cleared = values.iter().any(|value| value == "clear");
        let defaults = switch("use-defaults", !cleared, colors).on_click(on_click(cx, move |this, _, cx| {
            let mut values = this.file.values(KEY);
            if cleared {
                values.retain(|value| value != "clear");
            } else {
                values.insert(0, "clear".to_owned());
            }
            report(this, &values, cx);
        }));
        let mut out = div().flex().flex_col().child(row(
            rust_i18n::t!("settings.keybind.use_defaults").into_owned(),
            Some(rust_i18n::t!("settings.keybind.use_defaults_hint").into_owned().into()),
            defaults,
            None,
            self.errors.get(KEY).cloned(),
            colors,
        ));

        out = out.child(section(rust_i18n::t!("settings.keybind.yours").into_owned(), colors));
        for (ix, value) in values.iter().enumerate() {
            if value == "clear" {
                continue;
            }
            out = out.child(self.render_keybind_row(ix, value, colors, window, cx));
        }
        let adding = self.keybinds.recording(Record::New);
        let add = button(
            "add-keybind",
            if adding {
                rust_i18n::t!("settings.keybind.press").into_owned()
            } else {
                rust_i18n::t!("settings.keybind.add").into_owned()
            },
            colors,
        )
        .when(adding, |button| button.border_color(colors.accent))
        .on_click(on_click(cx, move |this, window, cx| {
            if adding {
                this.keybinds.stop_recording();
                cx.notify();
            } else {
                this.start_recording(Record::New, window, cx);
            }
        }));
        out = out.child(div().py(px(10.)).flex().child(add));

        out = out.child(section(rust_i18n::t!("settings.keybind.defaults").into_owned(), colors));
        out.child(self.render_default_keybinds(cleared, colors, cx))
    }

    fn render_keybind_row(
        &mut self,
        ix: usize,
        value: &str,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let error = keybind::parse(value).err().map(|err| rust_i18n::t!("settings.invalid", err = err).into_owned());
        let remove = icon_button(("remove-keybind", ix), "icons/minus.svg", colors).on_click(on_click(
            cx,
            move |this, _, cx| {
                let mut values = this.file.values(KEY);
                if ix < values.len() {
                    values.remove(ix);
                }
                report(this, &values, cx);
            },
        ));
        let Some((trigger, name, param)) = split(value) else {
            // 写错了、拆不开的，原样显示，只能删。
            return div()
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().child(value.to_owned()))
                .child(remove);
        };
        let recording = self.keybinds.recording(Record::Row(ix));
        let keys = div()
            .id(("keybind-trigger", ix))
            .w(px(140.))
            .h(px(26.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(colors.control)
            .border_1()
            .border_color(if recording { colors.accent } else { colors.border })
            .text_size(px(12.))
            .hover(|keys| keys.bg(colors.hover))
            .child(div().truncate().child(if recording {
                rust_i18n::t!("settings.keybind.press").into_owned()
            } else {
                pretty(trigger)
            }))
            .on_click(on_click(cx, move |this, window, cx| {
                if this.keybinds.recording(Record::Row(ix)) {
                    this.keybinds.stop_recording();
                    cx.notify();
                } else {
                    this.start_recording(Record::Row(ix), window, cx);
                }
            }));
        let current = name.to_owned();
        let action = dropdown(("keybind-action", ix), action_label(name), 220., colors)
            .on_click(on_click(cx, move |this, window, cx| {
                this.open_action_picker(Target::Row(ix), Some(current.clone()), window, cx)
            }));
        let spec = keybind::ACTIONS.iter().find(|spec| spec.name == name);
        let takes_param = spec.is_some_and(|spec| spec.param.is_some()) || param.is_some();
        let param = takes_param.then(|| {
            let id = format!("keybind:{ix}:param");
            let placeholder = SharedString::from(spec.and_then(|spec| spec.param).unwrap_or_default());
            let input = self.field(&id, Commit::KeybindParam(ix), Some(placeholder), window, cx);
            input_box(input, 120., self.errors.contains_key(&id), colors)
        });
        let field_error = self.errors.get(&format!("keybind:{ix}:param")).cloned();
        div()
            .py(px(6.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .border_b_1()
            .border_color(colors.border.opacity(0.6))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(keys)
                    .child(action)
                    .children(param)
                    .child(div().flex_1())
                    .child(remove),
            )
            .children(field_error.or(error).map(|err| div().text_size(px(11.5)).text_color(colors.error).child(err)))
    }

    /// 默认绑定：被解除或者被改掉了的标出来，其余的可以一条条解除。整个不用（`clear`）时都淡下去。
    fn render_default_keybinds(&mut self, cleared: bool, colors: Colors, cx: &mut Context<Self>) -> Div {
        let resolved = keybind::resolve(&self.config.keybinds);
        let rows: Vec<_> = keybind::DEFAULTS
            .iter()
            .enumerate()
            .filter_map(|(ix, default)| {
                let (trigger, name, param) = split(default)?;
                let keys = keybind::parse_trigger(trigger).ok()?;
                let usage = match param {
                    Some(param) => format!("{name}:{param}"),
                    None => name.to_owned(),
                };
                let action = keybind::parse_action(&usage).ok()?;
                let active = resolved.iter().any(|(k, a)| *k == keys && *a == action);
                let status = if active {
                    let trigger = trigger.to_owned();
                    button(
                        ("unbind-default", ix),
                        rust_i18n::t!("settings.keybind.unbind_default").into_owned(),
                        colors,
                    )
                    .h(px(22.))
                    .on_click(on_click(cx, move |this, _, cx| {
                        let mut values = this.file.values(KEY);
                        values.push(join(&trigger, "unbind", None));
                        report(this, &values, cx);
                    }))
                    .into_any_element()
                } else {
                    div()
                        .text_size(px(11.))
                        .text_color(colors.fg.opacity(0.45))
                        .child(rust_i18n::t!("settings.keybind.overridden").into_owned())
                        .into_any_element()
                };
                Some(
                    div()
                        .h(px(30.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .border_b_1()
                        .border_color(colors.border.opacity(0.4))
                        .when(!active, |row| row.opacity(0.55))
                        .child(div().w(px(110.)).flex_none().truncate().child(pretty(trigger)))
                        .child(div().flex_1().min_w_0().truncate().child(keybind::describe(name)))
                        .child(div().flex_none().text_size(px(11.)).text_color(colors.fg.opacity(0.4)).child(usage))
                        .child(div().w(px(84.)).flex_none().flex().justify_end().child(status)),
                )
            })
            .collect();
        div().flex().flex_col().when(cleared, |list| list.opacity(0.6)).children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybind_lines_split_and_join() {
        assert_eq!(split("cmd+t=new_tab"), Some(("cmd+t", "new_tab", None)));
        assert_eq!(split("cmd+1 = goto_tab:1"), Some(("cmd+1", "goto_tab", Some("1"))));
        assert_eq!(split("clear"), None);
        assert_eq!(join("cmd+t", "text", Some(r"\x01")), r"cmd+t=text:\x01");
        assert_eq!(default_param("goto_tab").as_deref(), Some("1"));
        assert_eq!(default_param("new_split").as_deref(), Some("right"));
        assert_eq!(default_param("text").as_deref(), Some(""));
        assert_eq!(default_param("new_tab"), None);
        assert_eq!(default_param("unbind"), None);
    }

    #[test]
    fn triggers_show_as_key_symbols() {
        assert_eq!(pretty("shift+cmd+t"), Keystroke::parse("shift-cmd-t").unwrap().to_string());
        assert_eq!(pretty("nope+t"), "nope+t");
    }
}
