//! 自己加的命令：添加和编辑的对话框，以及改写 runode 根目录的 `tasks.json`（`Dirs::tasks_file`）。
//! 一条命令要么只用于一个项目（`projects` 下按项目目录分，在项目目录里跑），要么通用（`global`，在
//! 终端当前目录里跑），对话框里选。添加到本项目时，项目目录取终端目录所在仓库的根目录，不在仓库里时取
//! 终端目录；同一份里同名的换成新的命令行。命令菜单里这些命令行尾的编辑按钮用同一个对话框改，改完留在
//! 原来的位置；删除按钮从文件里删掉它。对话框里还能录一个快捷键，存成配置里的 `keybind = 触发键=run_task:名字`；
//! 快捷键按名字找命令，改名、删除时一起改掉、删掉。文件由宿主列出来（`TaskSourceKind::Custom`、`Global`），手机上
//! 也看得到。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui::{
    App, Context, Div, Entity, Focusable, FontWeight, Hsla, KeyDownEvent, Keystroke, MouseButton, Role, Stateful,
    Subscription, Window, div, prelude::*, px,
};
use runode_config::{
    ConfigFile, Keybind,
    keybind::{self, Action},
};
use runode_paths::Dirs;
use runode_shared_types::color::Rgb;
use serde_json::{Map, Value};

use super::{AddTask, EditTask};
use crate::{
    config::AppConfig,
    settings::keybind_caps,
    ui::{
        a11y::Press,
        hsla,
        text_field::{TextField, TextFieldEvent},
    },
    window::{TITLEBAR_HEIGHT, WindowView},
};

pub(in crate::window) struct AddTaskDialog {
    /// 编辑时是改哪一份（项目目录，通用的为空）里的哪一条；添加时为空。
    editing: Option<(Option<PathBuf>, String)>,
    /// 选的是通用的。
    global: bool,
    name: Entity<TextField>,
    command: Entity<TextField>,
    /// 快捷键，配置文件的写法（`alt+cmd+r`）；为空时不绑。编辑时先填上现在绑着的。
    shortcut: Option<String>,
    /// 正在录快捷键：拦下这个窗口里按的键，不让它生效。
    recording: Option<Subscription>,
    /// 上次写文件失败的原因。
    error: Option<String>,
    /// 正在后台写文件，写完之前不再写。
    saving: bool,
    _events: [Subscription; 2],
}

impl WindowView {
    /// 打开添加命令的对话框，默认加到本项目，焦点给名字；已经开着时只给焦点。
    pub(in crate::window) fn add_task(&mut self, _: &AddTask, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_task.is_none() {
            self.open_task_dialog(None, String::new(), String::new(), window, cx);
        }
        self.focus_add_task_field(false, window, cx);
    }

    /// 打开编辑 `project`（通用的为空）里 `name` 这条命令的对话框，填好原来的名字和命令行，名字全选着。
    pub(in crate::window) fn edit_task(&mut self, action: &EditTask, window: &mut Window, cx: &mut Context<Self>) {
        let EditTask { project, name, command } = action.clone();
        self.open_task_dialog(Some((project, name.clone())), name, command, window, cx);
        self.focus_add_task_field(false, window, cx);
    }

    /// 换上一个新的对话框，两个输入框里先填好 `name` 和 `command`。
    fn open_task_dialog(
        &mut self,
        editing: Option<(Option<PathBuf>, String)>,
        name: String,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        {
            let field = |label: &str, placeholder: &str, text: String, cx: &mut Context<Self>| {
                let (label, placeholder) = (rust_i18n::t!(label).into_owned(), rust_i18n::t!(placeholder).into_owned());
                let select = text.len();
                cx.new(|cx| TextField::editing(text, select, cx).with_placeholder(placeholder).with_label(label))
            };
            let name = field("tasks.name", "tasks.name_placeholder", name, cx);
            let command = field("tasks.command", "tasks.command_placeholder", command, cx);
            // 名字里回车跳到命令，命令里回车添加；Esc 关掉对话框。
            let on_name = cx.subscribe_in(&name, window, |this, _, event: &TextFieldEvent, window, cx| match event {
                TextFieldEvent::Next => this.focus_add_task_field(true, window, cx),
                TextFieldEvent::Dismiss => this.close_add_task(window, cx),
                TextFieldEvent::Changed(_) | TextFieldEvent::Previous => {}
            });
            let on_command =
                cx.subscribe_in(&command, window, |this, _, event: &TextFieldEvent, window, cx| match event {
                    TextFieldEvent::Next => this.save_task(window, cx),
                    TextFieldEvent::Dismiss => this.close_add_task(window, cx),
                    TextFieldEvent::Changed(_) | TextFieldEvent::Previous => {}
                });
            let global = editing.as_ref().is_some_and(|(project, _)| project.is_none());
            let shortcut = editing.as_ref().and_then(|(_, name)| task_shortcut(name, cx));
            self.add_task = Some(AddTaskDialog {
                editing,
                global,
                name,
                command,
                shortcut,
                recording: None,
                error: None,
                saving: false,
                _events: [on_name, on_command],
            });
        }
    }

    /// 焦点给命令框（`command`）或名字框。
    fn focus_add_task_field(&mut self, command: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.add_task else {
            return;
        };
        let field = if command { &dialog.command } else { &dialog.name };
        window.focus(&field.focus_handle(cx), cx);
        cx.notify();
    }

    /// 点了快捷键框：开始录，焦点先挪开，免得按的键打进输入框；正在录时不录了。
    fn record_task_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.focus_handle(cx);
        let Some(dialog) = &mut self.add_task else {
            return;
        };
        if dialog.recording.take().is_none() {
            let handle = window.window_handle();
            let view = cx.entity().downgrade();
            dialog.recording = Some(cx.intercept_keystrokes(move |event, window, cx: &mut App| {
                if window.window_handle() != handle {
                    return;
                }
                cx.stop_propagation();
                let keystroke = event.keystroke.clone();
                view.update(cx, |view, cx| view.recorded_task_shortcut(&keystroke, window, cx)).ok();
            }));
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// 录到了一个键：Esc 不录了、原来的留着；只按了修饰键或者配置里写不出来的键接着等。
    fn recorded_task_shortcut(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.add_task else {
            return;
        };
        if keystroke.unparse() != "escape" {
            let Some(trigger) = keybind::format_trigger(&keystroke.unparse()) else {
                return;
            };
            dialog.shortcut = Some(trigger);
        }
        dialog.recording = None;
        self.focus_add_task_field(true, window, cx);
    }

    fn close_add_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_task.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// Tab 在名字和命令之间切换。
    fn add_task_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.add_task else {
            return;
        };
        if event.keystroke.key == "tab" && !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform {
            cx.stop_propagation();
            let in_name = dialog.name.focus_handle(cx).is_focused(window);
            self.focus_add_task_field(in_name, window, cx);
        }
    }

    /// 在后台把填好的命令写进文件，写好了关掉对话框、重列命令；写不了时把原因写在对话框里。名字
    /// 空着时用命令行当名字。编辑时换掉原来那一条，同一份里改了名字也留在原来的位置；换到另一份时
    /// 加在那一份的最后。
    fn save_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.project_dir(cx);
        let Some(dialog) = &mut self.add_task else {
            return;
        };
        let command = dialog.command.read(cx).query().trim().to_owned();
        if command.is_empty() || dialog.saving {
            if command.is_empty() {
                self.focus_add_task_field(true, window, cx);
            }
            return;
        }
        let name = dialog.name.read(cx).query().trim().to_owned();
        let name = if name.is_empty() { command.clone() } else { name };
        dialog.saving = true;
        dialog.recording = None;
        let (editing, global) = (dialog.editing.clone(), dialog.global);
        // 编辑时换掉原名字上的快捷键；新加的没录快捷键时不碰配置，同名的别的命令绑着的键留着。
        let keybind = match (&editing, &dialog.shortcut) {
            (Some((_, old)), shortcut) => Some((old.clone(), shortcut.clone())),
            (None, Some(shortcut)) => Some((name.clone(), Some(shortcut.clone()))),
            (None, None) => None,
        };
        let new_name = name.clone();
        let job = cx.background_spawn(async move {
            let file = Dirs::from_env().tasks_file().ok_or("no home directory")?;
            // 编辑本项目的命令时还是那个项目；加到本项目时现查终端目录在哪个仓库里。
            let project = match &editing {
                _ if global => None,
                Some((Some(project), _)) => Some(project.clone()),
                _ => Some(runode_git::repo_root(&dir).unwrap_or(dir)),
            };
            let (old_project, old_name) = editing.unwrap_or_else(|| (project.clone(), name.clone()));
            write_task(&file, (old_project.as_deref(), &old_name), Some((project.as_deref(), &name, &command)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = job.await;
            this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(()) => {
                        if let Some((old, shortcut)) = &keybind {
                            let new = shortcut.as_deref().map(|trigger| (trigger, new_name.as_str()));
                            if let Err(err) = write_task_keybind(old, new, cx) {
                                tracing::warn!("could not save the task shortcut: {err}");
                            }
                        }
                        this.close_add_task(window, cx);
                        this.workspace_mut().project.tasks_stale = true;
                        this.list_tasks(cx);
                    }
                    Err(err) => {
                        if let Some(dialog) = &mut this.add_task {
                            dialog.saving = false;
                            dialog.error = Some(err);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(in crate::window) fn render_add_task(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.add_task.as_ref()?;
        let panel_bg = hsla(bg.mix(fg, 0.03));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let primary_bg = hsla(bg.mix(fg, 0.16));
        let primary_hover_bg = hsla(bg.mix(fg, 0.22));
        let fg = hsla(fg);
        let border = fg.opacity(0.12);
        let field = |label: String, input: &Entity<TextField>| {
            div().flex().flex_col().gap(px(6.)).child(div().text_color(fg.opacity(0.7)).child(label)).child(
                div()
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(hsla(bg))
                    .border_1()
                    .border_color(fg.opacity(0.2))
                    .child(input.clone()),
            )
        };
        let button = |id: &'static str, label: String, bg: Hsla, hover: Hsla| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label.clone())
                .flex_none()
                .px(px(12.))
                .py(px(5.))
                .rounded(px(6.))
                .bg(bg)
                .hover(move |button| button.bg(hover))
                .cursor_pointer()
                .child(label)
        };
        // 本项目还是所有目录：两段按钮，选中的那段底色亮一些。
        let scope = |id: &'static str, global: bool, label: String, cx: &mut Context<Self>| {
            div()
                .id(id)
                .role(Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled((dialog.global == global).into())
                .px(px(10.))
                .py(px(3.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(dialog.global == global, |pill| pill.bg(primary_bg))
                .when(dialog.global != global, |pill| pill.hover(move |pill| pill.bg(hover_bg)))
                .child(label)
                .on_press(cx, move |this, _, cx| {
                    if let Some(dialog) = &mut this.add_task {
                        dialog.global = global;
                        cx.notify();
                    }
                })
        };
        let shortcut_label = match (&dialog.recording, &dialog.shortcut) {
            (Some(_), _) => rust_i18n::t!("tasks.shortcut_press").into_owned(),
            (None, Some(trigger)) => {
                keybind_caps(trigger).iter().map(|stroke| stroke.concat()).collect::<Vec<_>>().join(" ")
            }
            (None, None) => rust_i18n::t!("tasks.shortcut_none").into_owned(),
        };
        let shortcut_title = rust_i18n::t!("tasks.shortcut").into_owned();
        let shortcut_box = div()
            .id("add-task-shortcut")
            .role(Role::Button)
            .aria_label(shortcut_title.clone())
            .aria_value(shortcut_label.clone())
            .h(px(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(hsla(bg))
            .border_1()
            .border_color(if dialog.recording.is_some() { fg.opacity(0.6) } else { fg.opacity(0.2) })
            .cursor_pointer()
            .when(dialog.shortcut.is_none() || dialog.recording.is_some(), |shortcut| {
                shortcut.text_color(fg.opacity(0.55))
            })
            .child(shortcut_label)
            .on_press(cx, |this, window, cx| this.record_task_shortcut(window, cx));
        let clear_shortcut = dialog.shortcut.as_ref().filter(|_| dialog.recording.is_none()).map(|_| {
            div()
                .id("add-task-shortcut-clear")
                .role(Role::Button)
                .aria_label(rust_i18n::t!("tasks.shortcut_clear").into_owned())
                .px(px(6.))
                .py(px(3.))
                .rounded(px(4.))
                .text_color(fg.opacity(0.6))
                .hover(move |clear| clear.bg(hover_bg))
                .cursor_pointer()
                .child(rust_i18n::t!("tasks.shortcut_clear").into_owned())
                .on_press(cx, |this, _, cx| {
                    if let Some(dialog) = &mut this.add_task {
                        dialog.shortcut = None;
                        cx.notify();
                    }
                })
        });
        let taken = dialog.shortcut.as_deref().filter(|_| dialog.recording.is_none()).and_then(|trigger| {
            let typed = dialog.name.read(cx).query().trim().to_owned();
            let own = dialog.editing.as_ref().map_or(typed.as_str(), |(_, name)| name.as_str());
            shortcut_taken_by(trigger, own, cx)
        });
        let hint = if dialog.global {
            rust_i18n::t!("tasks.hint_global", file = TASKS_FILE_HINT)
        } else {
            rust_i18n::t!("tasks.hint_project", file = TASKS_FILE_HINT)
        };
        let cancel = button("add-task-cancel", rust_i18n::t!("tasks.cancel").into_owned(), panel_bg, hover_bg)
            .on_press(cx, |this, window, cx| this.close_add_task(window, cx));
        let (title, save) = match dialog.editing {
            Some(_) => (rust_i18n::t!("tasks.edit_title"), rust_i18n::t!("tasks.update")),
            None => (rust_i18n::t!("tasks.add_title"), rust_i18n::t!("tasks.save")),
        };
        let save = button("add-task-save", save.into_owned(), primary_bg, primary_hover_bg)
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_press(cx, |this, window, cx| this.save_task(window, cx));
        let (title, scope_label, hint) =
            (title.into_owned(), rust_i18n::t!("tasks.scope").into_owned(), hint.into_owned());
        let panel = div()
            .id("add-task-dialog")
            .role(Role::Dialog)
            .aria_label(title.clone())
            .on_key_down(cx.listener(Self::add_task_key))
            .w(px(420.))
            .max_w_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(12.))
            .bg(panel_bg)
            .border_1()
            .border_color(border)
            .shadow_lg()
            .text_size(px(12.))
            .text_color(fg)
            // 点在对话框里不算点到外面。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id("add-task-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(px(16.))
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .child(field(rust_i18n::t!("tasks.name").into_owned(), &dialog.name))
            .child(field(rust_i18n::t!("tasks.command").into_owned(), &dialog.command))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(fg.opacity(0.7)).child(scope_label.clone()))
                    .child(
                        div()
                            .id("add-task-scope")
                            .role(Role::RadioGroup)
                            .aria_label(scope_label)
                            .flex()
                            .p(px(2.))
                            .gap(px(2.))
                            .rounded(px(6.))
                            .bg(hsla(bg))
                            .child(scope(
                                "add-task-project",
                                false,
                                rust_i18n::t!("tasks.scope_project").into_owned(),
                                cx,
                            ))
                            .child(scope(
                                "add-task-global",
                                true,
                                rust_i18n::t!("tasks.scope_global").into_owned(),
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(fg.opacity(0.7)).child(shortcut_title))
                    .child(shortcut_box)
                    .children(clear_shortcut),
            )
            .children(taken.map(|what| {
                let text = rust_i18n::t!("tasks.shortcut_taken", what = what).into_owned();
                div()
                    .id("add-task-shortcut-taken")
                    .role(Role::Label)
                    .aria_label(text.clone())
                    .text_color(fg.opacity(0.7))
                    .child(text)
            }))
            .child(
                div()
                    .id("add-task-hint")
                    .role(Role::Label)
                    .aria_label(hint.clone())
                    .text_color(fg.opacity(0.55))
                    .child(hint),
            )
            .children(dialog.error.clone().map(|error| {
                div()
                    .id("add-task-error")
                    .role(Role::Label)
                    .aria_label(error.clone())
                    .text_color(gpui::red())
                    .child(error)
            }))
            .child(div().pt(px(4.)).flex().justify_end().gap(px(8.)).child(cancel).child(save));
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消。
        Some(
            div()
                .id("add-task-backdrop")
                .absolute()
                .size_full()
                .pt(px(TITLEBAR_HEIGHT + 48.))
                .px(px(16.))
                .flex()
                .justify_center()
                .items_start()
                .bg(Hsla::black().opacity(0.15))
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_add_task(window, cx)))
                .child(panel),
        )
    }
}

/// 现在绑在 `run_task:名字` 上的快捷键，配置文件的写法。
fn task_shortcut(name: &str, cx: &App) -> Option<String> {
    let wanted = Action::RunTask(name.to_owned());
    keybind::resolve(&cx.global::<AppConfig>().0.keybinds)
        .into_iter()
        .find(|(_, action)| *action == wanted)
        .and_then(|(keys, _)| keybind::format_trigger(&keys))
}

/// `trigger` 现在绑着别的动作时，那个动作的说明；没绑、绑的就是跑 `own` 这条命令，或者认不出是哪个
/// 动作时为空。
fn shortcut_taken_by(trigger: &str, own: &str, cx: &App) -> Option<String> {
    let keys = keybind::parse_trigger(trigger).ok()?;
    let (_, action) =
        keybind::resolve(&cx.global::<AppConfig>().0.keybinds).into_iter().find(|(bound, _)| *bound == keys)?;
    if let Action::RunTask(name) = &action {
        return (name != own).then(|| format!("run_task:{name}"));
    }
    // 动作名从默认绑定里找，自己在配置里绑的别的动作不提示。
    let bind = Keybind::Bind { keys, action };
    let line = keybind::DEFAULTS.iter().find(|line| keybind::parse(line).is_ok_and(|binds| binds.contains(&bind)))?;
    let (_, usage) = line.split_once('=')?;
    Some(keybind::describe(usage.split(':').next()?))
}

/// 把配置里绑在 `run_task:old` 上的键去掉，`new`（触发键，名字）有时绑上 `触发键=run_task:名字`，没变时
/// 不写。
pub(super) fn write_task_keybind(old: &str, new: Option<(&str, &str)>, cx: &mut App) -> Result<(), String> {
    let path = runode_config::config_path().ok_or("no home directory")?;
    let values = ConfigFile::read(&path).map_err(|err| format!("{}: {err}", path.display()))?.values("keybind");
    let updated = with_task_keybind(&values, old, new);
    if updated != values {
        crate::config::write_values(&path, "keybind", &updated, cx)
            .map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
}

/// 在 `keybind` 的几行 `values` 里去掉绑在 `run_task:old` 上的，以及 `new` 的触发键上原来绑的（盖掉了也
/// 不生效，留着只会让人糊涂；解绑的行留着），再加上 `new` 那一行；这一行原来就有时留在原处。
fn with_task_keybind(values: &[String], old: &str, new: Option<(&str, &str)>) -> Vec<String> {
    let old = Action::RunTask(old.to_owned());
    let line = new.map(|(trigger, name)| format!("{trigger}=run_task:{name}"));
    let new_keys = new.and_then(|(trigger, _)| keybind::parse_trigger(trigger).ok());
    let mut kept = false;
    let mut out = Vec::new();
    for value in values {
        if !kept && line.as_ref() == Some(value) {
            kept = true;
            out.push(value.clone());
            continue;
        }
        let stale = keybind::parse(value).is_ok_and(|binds| {
            binds.iter().any(|bind| {
                matches!(bind, Keybind::Bind { keys, action } if *action == old || Some(keys) == new_keys.as_ref())
            })
        });
        if !stale {
            out.push(value.clone());
        }
    }
    if !kept {
        out.extend(line);
    }
    out
}

/// 对话框里告诉用户命令存在哪。
const TASKS_FILE_HINT: &str = "~/.runode/tasks.json";

/// 把 `file` 里的 `old`（项目目录，通用的为空；名字）换成 `new`（项目目录；名字和命令行），文件和它的
/// 目录不在时建出来；`new` 为空时删掉 `old`。读不懂原来的内容时不写，免得把手写的文件冲掉。
pub(super) fn write_task(
    file: &Path,
    old: (Option<&Path>, &str),
    new: Option<(Option<&Path>, &str, &str)>,
) -> Result<(), String> {
    let text = match fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound && new.is_none() => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{}: {err}", file.display())),
    };
    let updated = with_task(text.as_deref(), old, new).map_err(|err| format!("{}: {err}", file.display()))?;
    let dir = file.parent().map(PathBuf::from).unwrap_or_default();
    fs::create_dir_all(&dir).and_then(|()| fs::write(file, updated)).map_err(|err| format!("{}: {err}", file.display()))
}

/// 在命令文件的内容 `text`（没有文件时为空）里把 `old` 换成 `new`。同一份里就在 `old` 原来的位置换，
/// 换到另一份时从原来那份删掉、加在新那份的最后；`new` 为空时只删掉 `old`。新名字在那一份里别处已经
/// 有了时，那一条让位。删空了的项目和通用那份一起拿掉，别的内容和先后原样留着。
fn with_task(
    text: Option<&str>,
    old: (Option<&Path>, &str),
    new: Option<(Option<&Path>, &str, &str)>,
) -> Result<String, String> {
    let mut json = match text.filter(|text| !text.trim().is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|err| err.to_string())?,
        None => Value::Object(Map::new()),
    };
    let root = json.as_object_mut().ok_or("not a JSON object")?;
    let same = new.is_some_and(|(project, ..)| project == old.0);
    replace(scope_tasks(root, old.0)?, Some(old.1), new.filter(|_| same).map(|(_, name, command)| (name, command)));
    if let Some((project, name, command)) = new.filter(|_| !same) {
        replace(scope_tasks(root, project)?, None, Some((name, command)));
    }
    if let Some(projects) = root.get_mut("projects").and_then(Value::as_object_mut) {
        projects.retain(|_, tasks| tasks.as_object().is_none_or(|tasks| !tasks.is_empty()));
    }
    root.retain(|key, value| {
        !(matches!(key.as_str(), "global" | "projects") && value.as_object().is_some_and(Map::is_empty))
    });
    let mut text = serde_json::to_string_pretty(&json).map_err(|err| err.to_string())?;
    text.push('\n');
    Ok(text)
}

/// `project` 那一份（为空时是通用的）的命令表，没有时建出来。
fn scope_tasks<'a>(
    root: &'a mut Map<String, Value>,
    project: Option<&Path>,
) -> Result<&'a mut Map<String, Value>, String> {
    let object = || Value::Object(Map::new());
    let tasks = match project {
        None => root.entry("global").or_insert_with(object),
        Some(project) => root
            .entry("projects")
            .or_insert_with(object)
            .as_object_mut()
            .ok_or("`projects` is not a JSON object")?
            .entry(project.to_string_lossy())
            .or_insert_with(object),
    };
    tasks.as_object_mut().ok_or_else(|| "a task list is not a JSON object".to_owned())
}

/// 在命令表里把 `old` 换成 `new`，就在 `old` 的位置；没有 `old` 时 `new` 加在最后，`new` 为空时删掉
/// `old`。`new` 的名字别处已经有了时，那一条让位。
fn replace(tasks: &mut Map<String, Value>, old: Option<&str>, new: Option<(&str, &str)>) {
    let entry = |(name, command): (&str, &str)| (name.to_owned(), Value::String(command.to_owned()));
    let mut placed = false;
    let mut rebuilt = Map::new();
    for (key, value) in std::mem::take(tasks) {
        if Some(key.as_str()) == old {
            rebuilt.extend(new.map(entry));
            placed = true;
        } else if new.is_none_or(|(name, _)| key != name) {
            rebuilt.insert(key, value);
        }
    }
    if !placed {
        rebuilt.extend(new.map(entry));
    }
    *tasks = rebuilt;
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{with_task, with_task_keybind};

    fn pretty(json: serde_json::Value) -> String {
        serde_json::to_string_pretty(&json).unwrap() + "\n"
    }

    #[test]
    fn adds_to_new_and_existing_files() {
        let app = Some(Path::new("/w/app"));
        assert_eq!(
            with_task(None, (app, "dev"), Some((app, "dev", "cargo run"))).unwrap(),
            pretty(serde_json::json!({"projects": {"/w/app": {"dev": "cargo run"}}}))
        );
        let existing = r#"{"note": 1, "global": {"dev": "old", "lint": "cargo clippy"}}"#;
        let updated = with_task(Some(existing), (None, "dev"), Some((None, "dev", "cargo run"))).unwrap();
        assert_eq!(
            updated,
            pretty(serde_json::json!({"note": 1, "global": {"dev": "cargo run", "lint": "cargo clippy"}}))
        );
        assert!(with_task(Some("[1]"), (None, "dev"), Some((None, "dev", "x"))).is_err());
        assert!(with_task(Some("{oops"), (None, "dev"), Some((None, "dev", "x"))).is_err());
    }

    /// 改名、换键时去掉原来那行和占着新键的那行，解绑和别的行原样留着；没变时一行不动。
    #[test]
    fn rewrites_task_keybinds() {
        let values: Vec<String> = ["cmd+alt+r=run_task:dev", "cmd+t=unbind", "cmd+t=new_window", "cmd+1=run_task:lint"]
            .map(String::from)
            .into();
        assert_eq!(
            with_task_keybind(&values, "dev", Some(("cmd+t", "serve"))),
            ["cmd+t=unbind", "cmd+1=run_task:lint", "cmd+t=run_task:serve"]
        );
        assert_eq!(with_task_keybind(&values, "dev", Some(("cmd+alt+r", "dev"))), values);
        assert_eq!(with_task_keybind(&values, "lint", None), values[..3]);
    }

    /// 删掉一条，其余的先后不变；删空了的那份一起拿掉。
    #[test]
    fn removes_a_task_keeping_the_order() {
        let existing = r#"{"global": {"a": "1", "b": "2", "c": "3"}, "projects": {"/w": {"x": "1"}}}"#;
        let updated = with_task(Some(existing), (None, "a"), None).unwrap();
        assert_eq!(
            updated,
            pretty(serde_json::json!({"global": {"b": "2", "c": "3"}, "projects": {"/w": {"x": "1"}}}))
        );
        let updated = with_task(Some(existing), (Some(Path::new("/w")), "x"), None).unwrap();
        assert_eq!(updated, pretty(serde_json::json!({"global": {"a": "1", "b": "2", "c": "3"}})));
    }

    /// 同一份里改名留在原来的位置，新名字别处已经有了时那一条让位；换到另一份时加在最后。
    #[test]
    fn renames_in_place_and_moves_between_scopes() {
        let existing = r#"{"global": {"a": "1", "b": "2", "c": "3"}}"#;
        let updated = with_task(Some(existing), (None, "b"), Some((None, "z", "9"))).unwrap();
        assert_eq!(updated, pretty(serde_json::json!({"global": {"a": "1", "z": "9", "c": "3"}})));
        let updated = with_task(Some(existing), (None, "c"), Some((None, "a", "9"))).unwrap();
        assert_eq!(updated, pretty(serde_json::json!({"global": {"b": "2", "a": "9"}})));
        let app = Some(Path::new("/w/app"));
        let updated = with_task(Some(existing), (None, "b"), Some((app, "b", "2"))).unwrap();
        assert_eq!(
            updated,
            pretty(serde_json::json!({"global": {"a": "1", "c": "3"}, "projects": {"/w/app": {"b": "2"}}}))
        );
    }
}
