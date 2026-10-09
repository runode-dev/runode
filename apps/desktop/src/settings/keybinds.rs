//! 快捷键那一页：按动作一行一行列出来，右边是绑在这个动作上的键，一个键一个键帽（⌘ ⇧ R）。
//! 点 + 录一个新键加上；点已有的键帽重新录、换掉它，录的时候旁边的 ⊘ 去掉它。带参数的动作按参数
//! 分成几行，参数标在名字旁边；`goto_tab`、`goto_workspace` 一行管数字 1 到 9（`cmd+digit`）。
//! 改动都写成配置文件里的 `keybind` 行，改掉或去掉默认绑定时先写一行 `触发键=unbind`。
//! 发送文字这类参数要自己填的动作只列出已经绑了的，新加的在配置文件里写。
//!
//! 录键时拦下这个窗口里按的键（`App::intercept_keystrokes`），不让它当快捷键或者菜单命令生效；
//! 一次只录一个按键，连按几个键的写法（`ctrl+a>n`）要在配置文件里写。

use gpui::{App, Context, Div, Keystroke, SharedString, Subscription, Window, div, prelude::*, px};
use runode_config::{
    Keybind,
    keybind::{self, Action, ActionSpec},
};

use super::{
    SettingsView,
    controls::{CONTROL_HEIGHT, Cards, Colors, button, icon_button, on_click, row, switch},
};

const KEY: &str = "keybind";

/// 一行管数字 1 到 9 的动作：不带参数、绑在 `digit` 上时按几就去第几个。
const DIGIT_ACTIONS: [&str; 2] = ["goto_tab", "goto_workspace"];

/// 分组：每组的第一个动作和组名在翻译里的 `settings.keybind.group.<名字>`，组里的动作按 `ACTIONS`
/// 的顺序排到下一组的第一个为止。
const GROUPS: &[(&str, &str)] = &[
    ("about", "app"),
    ("new_window", "window"),
    ("new_tab", "tab"),
    ("new_split", "split"),
    ("new_workspace", "workspace"),
    ("toggle_sidebar", "panels"),
    ("copy_to_clipboard", "edit"),
    ("scroll_to_top", "scroll"),
    ("text", "text"),
    ("start_search", "search"),
    ("write_screen_file", "other"),
];

#[derive(Default)]
pub(super) struct State {
    recording: Option<Recording>,
    /// 录到的键已经绑在别的动作上，等用户点替换或取消。
    conflict: Option<Conflict>,
}

struct Conflict {
    target: Record,
    trigger: String,
    /// 占着这个键的那几行的名字。
    others: Vec<String>,
}

struct Recording {
    target: Record,
    _intercept: Subscription,
}

impl State {
    /// 不录了，没定下的冲突也一起放弃。
    pub fn stop_recording(&mut self) {
        self.recording = None;
        self.conflict = None;
    }

    fn target(&self) -> Option<&Record> {
        self.recording.as_ref().map(|recording| &recording.target)
    }
}

/// 一行管的是什么：一个解析好的动作，或者 `goto_tab` 这种按数字分的。
#[derive(Clone, PartialEq)]
enum RowKey {
    Action(Action),
    Digits(&'static str),
}

/// 键帽组出自哪一行配置：默认绑定，或者配置文件里 `keybind` 的第几行。
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Default,
    User(usize),
}

/// 绑在一行上的一个键。
#[derive(Clone, PartialEq)]
struct Bound {
    /// 配置文件的写法，比如 `shift+cmd+t`、`cmd+digit`。
    trigger: String,
    source: Source,
}

struct Row {
    key: RowKey,
    /// 在 `ACTIONS` 里排第几，分组和排序用。
    order: usize,
    name: &'static str,
    /// 新加的键写成 `触发键=usage`。
    usage: String,
    bound: Vec<Bound>,
}

/// 录的键给谁用：给一行加一个，或者换掉它上面的 `replace`。
#[derive(Clone, PartialEq)]
struct Record {
    row: RowKey,
    usage: String,
    replace: Option<Bound>,
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

/// `动作` 或 `动作:参数`。
fn usage(name: &str, param: Option<&str>) -> String {
    match param {
        Some(param) => format!("{name}:{param}"),
        None => name.to_owned(),
    }
}

fn join(trigger: &str, name: &str, param: Option<&str>) -> String {
    format!("{trigger}={}", usage(name, param))
}

/// 一个动作默认列出的几行，各是写进配置的 `动作` 或 `动作:参数`：几个可选值的每个值一行，
/// `jump_to_prompt` 上下各一行，参数要自己填的（发送文字）不列。
fn base_usages(spec: &ActionSpec) -> Vec<String> {
    match spec.param {
        _ if DIGIT_ACTIONS.contains(&spec.name) || keybind::parse_action(spec.name).is_ok() => {
            vec![spec.name.to_owned()]
        }
        Some(hint) if hint.contains('|') => hint.split('|').map(|param| format!("{}:{param}", spec.name)).collect(),
        Some("N") => ["-1", "1"].map(|param| format!("{}:{param}", spec.name)).to_vec(),
        _ => Vec::new(),
    }
}

/// 一行 `动作` 或 `动作:参数` 归到哪一行；`digits` 是触发键的最后一个键写的 `digit`。
fn row_key(spec: &ActionSpec, usage: &str, digits: bool) -> Option<RowKey> {
    if digits && usage == spec.name && DIGIT_ACTIONS.contains(&spec.name) {
        return Some(RowKey::Digits(spec.name));
    }
    keybind::parse_action(usage).ok().map(RowKey::Action)
}

/// 触发键一个键一个键帽，连按的几个键各一组；`digit` 显示成 1…9，认不出的原样一个键帽。
fn caps(trigger: &str) -> Vec<Vec<String>> {
    let Ok(keys) = keybind::parse_trigger(trigger) else {
        return vec![vec![trigger.to_owned()]];
    };
    keys.split(' ')
        .map(|stroke| {
            let digits = stroke.strip_suffix("digit").map(|prefix| format!("{prefix}1"));
            let Ok(keystroke) = Keystroke::parse(digits.as_deref().unwrap_or(stroke)) else {
                return vec![stroke.to_owned()];
            };
            let m = keystroke.modifiers;
            let mut caps: Vec<String> = [(m.control, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.platform, "⌘")]
                .into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, symbol)| symbol.to_owned())
                .collect();
            caps.push(if digits.is_some() { "1…9".to_owned() } else { key_symbol(&keystroke.key) });
            caps
        })
        .collect()
}

fn key_symbol(key: &str) -> String {
    let symbol = match key {
        "enter" => "↩",
        "escape" => "⎋",
        "tab" => "⇥",
        "backspace" => "⌫",
        "delete" => "⌦",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "pageup" => "⇞",
        "pagedown" => "⇟",
        "home" => "↖",
        "end" => "↘",
        "space" => "␣",
        key => return key.to_uppercase(),
    };
    symbol.to_owned()
}

/// `line` 里的绑定在 `keybinds` 叠出来的表里还有一个生效。
fn live(line: &str, resolved: &[(String, Action)]) -> bool {
    keybind::parse(line).is_ok_and(|binds| {
        binds.iter().any(|bind| {
            matches!(bind, Keybind::Bind { keys, action } if resolved.iter().any(|(k, a)| k == keys && a == action))
        })
    })
}

fn report(view: &mut SettingsView, values: &[String], cx: &mut Context<SettingsView>) {
    if let Err(err) = view.write_unchecked(KEY, values, cx) {
        view.errors.insert(KEY.to_owned(), err);
    }
    cx.notify();
}

impl SettingsView {
    /// 每个动作一行，带上现在绑在它上面的键，按 `ACTIONS` 的顺序；另外是配置文件里写错了的几行。
    fn keybind_rows(&self) -> (Vec<Row>, Vec<(usize, String)>) {
        let resolved = keybind::resolve(&self.config.keybinds);
        let spec_of = |name: &str| keybind::ACTIONS.iter().enumerate().find(|(_, spec)| spec.name == name);
        let mut rows: Vec<Row> = keybind::ACTIONS
            .iter()
            .enumerate()
            .flat_map(|(order, spec)| {
                base_usages(spec).into_iter().filter_map(move |usage| {
                    let key = row_key(spec, &usage, DIGIT_ACTIONS.contains(&spec.name))?;
                    Some(Row { key, order, name: spec.name, usage, bound: Vec::new() })
                })
            })
            .collect();
        let mut bad = Vec::new();
        let defaults = keybind::DEFAULTS.iter().map(|line| (Source::Default, (*line).to_owned()));
        let user = self.file.values(KEY).into_iter().enumerate().map(|(ix, line)| (Source::User(ix), line));
        for (source, line) in defaults.chain(user) {
            if line == "clear" {
                continue;
            }
            if keybind::parse(&line).is_err() {
                if let Source::User(ix) = source {
                    bad.push((ix, line));
                }
                continue;
            }
            let Some((trigger, name, param)) = split(&line).filter(|_| live(&line, &resolved)) else {
                continue;
            };
            let Some((order, spec)) = spec_of(name) else {
                continue;
            };
            let Ok(keys) = keybind::parse_trigger(trigger) else {
                continue;
            };
            let usage = usage(name, param);
            let Some(key) = row_key(spec, &usage, keys.ends_with("digit")) else {
                continue;
            };
            let ix = rows.iter().position(|row| row.key == key).unwrap_or_else(|| {
                rows.push(Row { key, order, name: spec.name, usage, bound: Vec::new() });
                rows.len() - 1
            });
            // 同一个键写了两遍（比如照着默认的重写一遍），留后写的那条。
            let bound = &mut rows[ix].bound;
            bound.retain(|b| keybind::parse_trigger(&b.trigger).ok().as_ref() != Some(&keys));
            bound.push(Bound { trigger: trigger.to_owned(), source });
        }
        rows.sort_by_key(|row| row.order);
        (rows, bad)
    }

    fn start_recording(&mut self, target: Record, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let view = cx.entity().downgrade();
        let intercept = cx.intercept_keystrokes(move |event, window, cx: &mut App| {
            if window.window_handle() != handle {
                return;
            }
            cx.stop_propagation();
            let keystroke = event.keystroke.clone();
            view.update(cx, |view, cx| view.recorded(&keystroke, cx)).ok();
        });
        window.focus(&self.focus_handle, cx);
        self.keybinds.recording = Some(Recording { target, _intercept: intercept });
        cx.notify();
    }

    /// 录到了一个键。Esc 取消；只按了修饰键、写不进配置的键，以及管 1 到 9 的那行按的不是数字，
    /// 接着等。
    fn recorded(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(target) = self.keybinds.target().cloned() else {
            return;
        };
        if keystroke.unparse() == "escape" {
            self.keybinds.stop_recording();
            cx.notify();
            return;
        }
        let Some(mut trigger) = keybind::format_trigger(&keystroke.unparse()) else {
            return;
        };
        if matches!(target.row, RowKey::Digits(_)) {
            match trigger.strip_suffix(|c: char| ('1'..='9').contains(&c)) {
                Some(prefix) if prefix.is_empty() || prefix.ends_with('+') => trigger = format!("{prefix}digit"),
                _ => return,
            }
        }
        self.keybinds.stop_recording();
        let others = self.conflicts(&target, &trigger);
        if others.is_empty() {
            self.apply_recorded(target, &trigger, cx);
        } else {
            self.keybinds.conflict = Some(Conflict { target, trigger, others });
            cx.notify();
        }
    }

    /// `trigger` 绑到 `target` 那一行上时，要从哪几行抢过来：返回那几行的名字，带参数的加上参数。
    /// `digit` 展开成 1 到 9 各查一遍。
    fn conflicts(&self, target: &Record, trigger: &str) -> Vec<String> {
        let expand = |trigger: &str, usage: &str| -> Vec<String> {
            let binds = keybind::parse(&format!("{trigger}={usage}")).unwrap_or_default();
            binds
                .into_iter()
                .filter_map(|bind| if let Keybind::Bind { keys, .. } = bind { Some(keys) } else { None })
                .collect()
        };
        let wanted = expand(trigger, &target.usage);
        let (rows, _) = self.keybind_rows();
        rows.iter()
            .filter(|row| row.key != target.row)
            .filter(|row| row.bound.iter().any(|b| expand(&b.trigger, &row.usage).iter().any(|k| wanted.contains(k))))
            .map(|row| match row.usage.split_once(':') {
                Some((_, param)) => format!("{} {param}", keybind::describe(row.name)),
                None => keybind::describe(row.name),
            })
            .collect()
    }

    /// 把录到的键写进配置：换掉 `target.replace`，或者给那一行新加一条。
    fn apply_recorded(&mut self, target: Record, trigger: &str, cx: &mut Context<Self>) {
        self.keybinds.stop_recording();
        let mut values = self.file.values(KEY);
        match target.replace {
            Some(Bound { source: Source::User(ix), .. }) => {
                let Some((_, name, param)) = values.get(ix).and_then(|value| split(value)) else {
                    return;
                };
                values[ix] = join(trigger, name, param);
            }
            Some(Bound { trigger: old, source: Source::Default }) => {
                values.push(join(&old, "unbind", None));
                values.push(format!("{trigger}={}", target.usage));
            }
            None => values.push(format!("{trigger}={}", target.usage)),
        }
        report(self, &values, cx);
    }

    /// 去掉一行上的一个键：默认的写一行解绑；自己写的删掉那一行，它要是盖着一条一样的默认绑定，
    /// 删了默认的又露出来，再写一行解绑。
    fn remove_bound(&mut self, bound: &Bound, cx: &mut Context<Self>) {
        self.keybinds.stop_recording();
        let mut values = self.file.values(KEY);
        let unbind = join(&bound.trigger, "unbind", None);
        match bound.source {
            Source::Default => values.push(unbind),
            Source::User(ix) if ix < values.len() => {
                let line = values.remove(ix);
                let rest: Vec<_> = values.iter().filter_map(|value| keybind::parse(value).ok()).flatten().collect();
                if live(&line, &keybind::resolve(&rest)) {
                    values.push(unbind);
                }
            }
            Source::User(_) => return,
        }
        report(self, &values, cx);
    }

    pub(super) fn render_keybinds(&mut self, colors: Colors, _: &mut Window, cx: &mut Context<Self>) -> Div {
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
        let mut out = Cards::new(div().flex().flex_col(), colors);
        out.push(
            row(
                rust_i18n::t!("settings.keybind.use_defaults").into_owned(),
                Some(rust_i18n::t!("settings.keybind.use_defaults_hint").into_owned().into()),
                defaults,
                None,
                self.errors.get(KEY).cloned(),
                colors,
            )
            .into_any_element(),
        );

        let (rows, bad) = self.keybind_rows();
        for (ix, line) in bad {
            let err = keybind::parse(&line).err().unwrap_or_default();
            let remove = icon_button(("remove-bad-keybind", ix), "icons/minus.svg", colors).on_click(on_click(
                cx,
                move |this, _, cx| {
                    let mut values = this.file.values(KEY);
                    if ix < values.len() {
                        values.remove(ix);
                    }
                    report(this, &values, cx);
                },
            ));
            out.push(
                row(line, None, remove, None, Some(rust_i18n::t!("settings.invalid", err = err).into()), colors)
                    .into_any_element(),
            );
        }

        let group_start = |name: &str| keybind::ACTIONS.iter().position(|spec| spec.name == name).unwrap_or(0);
        for (gx, (first, group)) in GROUPS.iter().enumerate() {
            let start = group_start(first);
            let end = GROUPS.get(gx + 1).map_or(usize::MAX, |(next, _)| group_start(next));
            let group_rows: Vec<_> =
                rows.iter().enumerate().filter(|(_, row)| (start..end).contains(&row.order)).collect();
            let custom = *group == "text";
            if group_rows.is_empty() && !custom {
                continue;
            }
            let title = format!("settings.keybind.group.{group}");
            out.section(rust_i18n::t!(&title).into_owned());
            for (ix, row) in group_rows {
                out.push(self.render_keybind_row(ix, row, colors, cx).into_any_element());
            }
            if custom {
                let open = button("keybind-open-config", rust_i18n::t!("settings.open_config").into_owned(), colors)
                    .on_click(on_click(cx, |_, _, cx| crate::config::open(cx)));
                out.push(
                    div()
                        .py(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(11.5))
                                .text_color(colors.fg.opacity(0.55))
                                .child(rust_i18n::t!("settings.keybind.custom_hint").into_owned()),
                        )
                        .child(open)
                        .into_any_element(),
                );
            }
        }
        out.finish()
    }

    fn render_keybind_row(&mut self, ix: usize, row: &Row, colors: Colors, cx: &mut Context<Self>) -> Div {
        let recording = self.keybinds.target().filter(|target| target.row == row.key).cloned();
        let group = SharedString::from(format!("keybind-row-{ix}"));
        let tag = row.usage.split_once(':').map(|(_, param)| {
            div()
                .flex_none()
                .px(px(7.))
                .h(px(20.))
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(colors.border)
                .text_size(px(11.))
                .text_color(colors.fg.opacity(0.6))
                .child(param.to_owned())
        });
        let title = div()
            .flex_1()
            .min_w_0()
            .h(px(CONTROL_HEIGHT))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().min_w_0().truncate().child(keybind::describe(row.name)))
            .children(tag);

        let add_target = Record { row: row.key.clone(), usage: row.usage.clone(), replace: None };
        let adding = recording.as_ref() == Some(&add_target);
        let add = icon_button(SharedString::from(format!("keybind-add-{ix}")), "icons/plus.svg", colors)
            .flex_none()
            .mt(px((CONTROL_HEIGHT - 20.) / 2.))
            .when(!row.bound.is_empty() && !adding, |add| {
                add.invisible().group_hover(group.clone(), |add| add.visible())
            })
            .tooltip(crate::ui::tooltip::tooltip(
                rust_i18n::t!("settings.keybind.add").into_owned(),
                None,
                colors.fg_rgb,
                colors.bg_rgb,
            ))
            .on_click(on_click(cx, move |this, window, cx| {
                if this.keybinds.target() == Some(&add_target) {
                    this.keybinds.stop_recording();
                    cx.notify();
                } else {
                    this.start_recording(add_target.clone(), window, cx);
                }
            }));

        let mut keys = div().flex_none().flex().flex_col().items_end().gap(px(6.));
        for (jx, bound) in row.bound.iter().enumerate() {
            let target = Record { row: row.key.clone(), usage: row.usage.clone(), replace: Some(bound.clone()) };
            if recording.as_ref() == Some(&target) {
                let remove_bound = bound.clone();
                let remove =
                    icon_button(SharedString::from(format!("keybind-remove-{ix}-{jx}")), "icons/ban.svg", colors)
                        .tooltip(crate::ui::tooltip::tooltip(
                            rust_i18n::t!("settings.keybind.remove").into_owned(),
                            None,
                            colors.fg_rgb,
                            colors.bg_rgb,
                        ))
                        .on_click(on_click(cx, move |this, _, cx| this.remove_bound(&remove_bound, cx)));
                keys = keys.child(div().flex().items_center().gap(px(6.)).child(remove).child(press_box(
                    SharedString::from(format!("keybind-press-{ix}-{jx}")),
                    colors,
                    cx,
                )));
                continue;
            }
            keys = keys.child(
                div()
                    .id(SharedString::from(format!("keybind-keys-{ix}-{jx}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|keys| keys.opacity(0.75))
                    .children(caps(&bound.trigger).into_iter().map(|stroke| {
                        div().flex().gap(px(4.)).children(stroke.into_iter().map(|cap| keycap(cap, colors)))
                    }))
                    .on_click(on_click(cx, move |this, window, cx| this.start_recording(target.clone(), window, cx))),
            );
        }
        if adding {
            keys = keys.child(press_box(SharedString::from(format!("keybind-press-{ix}")), colors, cx));
        }

        let hint = recording.map(|target| {
            let hint = match target.row {
                RowKey::Digits(_) => rust_i18n::t!("settings.keybind.press_digit_hint"),
                RowKey::Action(_) => rust_i18n::t!("settings.keybind.press_hint"),
            };
            div().text_size(px(11.5)).text_color(colors.fg.opacity(0.55)).child(hint.into_owned())
        });
        let conflict =
            self.keybinds.conflict.as_ref().filter(|conflict| conflict.target.row == row.key).map(|conflict| {
                let keys: Vec<String> = caps(&conflict.trigger).into_iter().map(|stroke| stroke.concat()).collect();
                let message = rust_i18n::t!(
                    "settings.keybind.conflict",
                    keys = keys.join(" "),
                    actions = conflict.others.join(" / ")
                );
                let replace = button(
                    SharedString::from(format!("keybind-replace-{ix}")),
                    rust_i18n::t!("settings.keybind.replace").into_owned(),
                    colors,
                )
                .border_color(colors.accent)
                .on_click(on_click(cx, |this, _, cx| {
                    if let Some(Conflict { target, trigger, .. }) = this.keybinds.conflict.take() {
                        this.apply_recorded(target, &trigger, cx);
                    }
                }));
                let cancel = button(
                    SharedString::from(format!("keybind-cancel-{ix}")),
                    rust_i18n::t!("settings.keybind.cancel").into_owned(),
                    colors,
                )
                .on_click(on_click(cx, |this, _, cx| {
                    this.keybinds.stop_recording();
                    cx.notify();
                }));
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(11.5)).text_color(colors.error).child(message.into_owned()))
                    .child(cancel)
                    .child(replace)
            });
        div()
            .group(group)
            .py(px(8.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .border_b_1()
            .border_color(colors.border.opacity(0.4))
            .child(div().flex().items_start().gap(px(10.)).child(title).child(add).child(keys))
            .children(hint)
            .children(conflict)
    }
}

fn keycap(cap: String, colors: Colors) -> Div {
    div()
        .min_w(px(CONTROL_HEIGHT))
        .h(px(CONTROL_HEIGHT))
        .px(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(colors.control)
        .border_1()
        .border_color(colors.border)
        .text_size(px(12.))
        .child(cap)
}

/// 正在录键的框，点一下不录了。
fn press_box(id: SharedString, colors: Colors, cx: &mut Context<SettingsView>) -> impl IntoElement {
    button(id, rust_i18n::t!("settings.keybind.press").into_owned(), colors)
        .border_color(colors.accent)
        .text_color(colors.fg.opacity(0.7))
        .on_click(on_click(cx, |this, _, cx| {
            this.keybinds.stop_recording();
            cx.notify();
        }))
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
    }

    #[test]
    fn triggers_show_as_keycaps() {
        assert_eq!(caps("shift+cmd+t"), [["⇧", "⌘", "T"]]);
        assert_eq!(caps("cmd+digit"), [["⌘", "1…9"]]);
        assert_eq!(caps("ctrl+a>left"), [vec!["⌃", "A"], vec!["←"]]);
        assert_eq!(caps("nope+t"), [["nope+t"]]);
    }

    /// 每个动作都有行可放，默认绑定都落在某一行上，`goto_tab` 一行管 1 到 9。
    #[test]
    fn rows_cover_actions_and_defaults() {
        for spec in keybind::ACTIONS.iter().filter(|spec| !matches!(spec.name, "text" | "esc" | "csi")) {
            let usages = base_usages(spec);
            assert!(!usages.is_empty(), "{}", spec.name);
            for usage in usages {
                assert!(row_key(spec, &usage, DIGIT_ACTIONS.contains(&spec.name)).is_some(), "{usage}");
            }
        }
        let spec = keybind::ACTIONS.iter().find(|spec| spec.name == "goto_tab").unwrap();
        assert!(row_key(spec, "goto_tab", true) == Some(RowKey::Digits("goto_tab")));
        assert!(row_key(spec, "goto_tab:3", false) == Some(RowKey::Action(Action::GotoTab(2))));
        assert_eq!(
            GROUPS.len(),
            GROUPS.iter().filter(|(first, _)| keybind::ACTIONS.iter().any(|s| s.name == *first)).count()
        );
    }
}
