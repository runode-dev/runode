//! 自己加的命令：添加和编辑的对话框，以及改写 runode 根目录的 `tasks.json`（`Dirs::tasks_file`）。
//! 一条命令要么只用于一个项目（`projects` 下按项目目录分，在项目目录里跑），要么通用（`global`，在
//! 终端当前目录里跑），对话框里选。添加到本项目时，项目目录取终端目录所在仓库的根目录，不在仓库里时取
//! 终端目录；同一份里同名的换成新的命令行。命令菜单里这些命令行尾的编辑按钮用同一个对话框改，改完留在
//! 原来的位置；删除按钮从文件里删掉它。对话框里还能录一个快捷键，存成配置里的 `keybind = 触发键=run_task:名字`；
//! 快捷键按名字找命令，改名、删除时一起改掉、删掉，别的份里还有同名命令时留着原来的。文件由宿主列出来
//! （`TaskSourceKind::Custom`、`Global`），手机上也看得到。

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
    /// 打开时填上的快捷键：保存时没改过它就不动它绑着的那几行。
    initial_shortcut: Option<String>,
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
                initial_shortcut: shortcut.clone(),
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
    /// 加在那一份的最后。快捷键改不成（比如绑在 `config-file` 引入的文件里）时命令照样存好，对话框
    /// 留着、写上原因，再保存就是编辑存好的那一条。
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
        // 快捷键没改过时只跟着改名，原来的几行各留各的键；改了时换掉原名字上的；新加的没录快捷键时
        // 不碰配置，同名的别的命令绑着的键留着。
        let unchanged = dialog.shortcut == dialog.initial_shortcut;
        let keybind = match (&editing, &dialog.shortcut) {
            (Some((_, old)), _) if unchanged && *old == name => None,
            (Some((_, old)), _) if unchanged => Some(TaskKeybind::Rename { old: old.clone(), new: name.clone() }),
            (Some((_, old)), shortcut) => Some(TaskKeybind::Rebind {
                old: Some(old.clone()),
                new: shortcut.clone().map(|trigger| (trigger, name.clone())),
            }),
            (None, Some(shortcut)) => {
                Some(TaskKeybind::Rebind { old: None, new: Some((shortcut.clone(), name.clone())) })
            }
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
            write_task(&file, (old_project.as_deref(), &old_name), Some((project.as_deref(), &name, &command)))?;
            // 原名字在别的份里还有命令时，它绑着的键是那条命令也在用的。
            let elsewhere = task_named_elsewhere(&file, &old_name, project.as_deref());
            Ok((project, elsewhere))
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = job.await;
            this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok((project, elsewhere)) => {
                        let keybind = if elsewhere { keybind.and_then(TaskKeybind::keep_old) } else { keybind };
                        let bound = keybind.map_or(Ok(()), |keybind| write_task_keybind(&keybind, cx));
                        this.workspace_mut().project.tasks_stale = true;
                        this.list_tasks(cx);
                        match bound {
                            Ok(()) => this.close_add_task(window, cx),
                            Err(err) => {
                                let shortcut = task_shortcut(&new_name, cx);
                                if let Some(dialog) = &mut this.add_task {
                                    dialog.editing = Some((project, new_name));
                                    dialog.initial_shortcut = shortcut;
                                    dialog.saving = false;
                                    dialog.error = Some(err);
                                }
                            }
                        }
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

/// `trigger` 现在绑着别的动作时，那个动作的说明；没绑或者绑的就是跑 `own` 这条命令时为空。
fn shortcut_taken_by(trigger: &str, own: &str, cx: &App) -> Option<String> {
    let keys = keybind::parse_trigger(trigger).ok()?;
    let (_, action) =
        keybind::resolve(&cx.global::<AppConfig>().0.keybinds).into_iter().find(|(bound, _)| *bound == keys)?;
    if let Action::RunTask(name) = &action {
        return (name != own).then(|| format!("run_task:{name}"));
    }
    Some(action_name(&keys, &action))
}

/// `keys` 上绑的 `action` 的说明：动作名先从默认绑定里找，带数字参数的（`goto_tab:3`）这样才认得出；
/// 自己在配置里绑的再按动作表里不带参数或者参数可选值的写法认；都认不出时写动作本身。
fn action_name(keys: &str, action: &Action) -> String {
    let bind = Keybind::Bind { keys: keys.to_owned(), action: action.clone() };
    let default = keybind::DEFAULTS
        .iter()
        .find(|line| keybind::parse(line).is_ok_and(|binds| binds.contains(&bind)))
        .and_then(|line| line.split_once('='))
        .and_then(|(_, usage)| usage.split(':').next());
    let listed = || {
        let spec = keybind::ACTIONS.iter().find(|spec| {
            let params = spec.param.into_iter().flat_map(|params| params.split('|'));
            let mut usages = std::iter::once(spec.name.to_owned()).chain(params.map(|p| format!("{}:{p}", spec.name)));
            usages.any(|usage| keybind::parse_action(&usage).as_ref() == Ok(action))
        });
        spec.map(|spec| spec.name)
    };
    default.or_else(listed).map_or_else(|| format!("{action:?}"), keybind::describe)
}

/// 保存或删除命令时快捷键怎么跟着改，见 `with_task_keybind`。
#[derive(Debug)]
pub(super) enum TaskKeybind {
    /// 去掉绑在 `run_task:old` 上的行（`old` 为空时不去），`new`（触发键，名字）有时在最后绑上
    /// `触发键=run_task:名字`。
    Rebind { old: Option<String>, new: Option<(String, String)> },
    /// 改了名字、没改快捷键：每行的 `run_task:old` 换成 `run_task:new`，触发键各自留着。
    Rename { old: String, new: String },
}

impl TaskKeybind {
    /// 原名字在别的份里还有命令在用时：它绑着的行都留着，只绑上新录的键；没有新键时不用改。
    fn keep_old(self) -> Option<Self> {
        match self {
            Self::Rebind { new: Some(new), .. } => Some(Self::Rebind { old: None, new: Some(new) }),
            Self::Rebind { new: None, .. } | Self::Rename { .. } => None,
        }
    }
}

/// 按 `change` 改配置里的 `keybind`，没变时不写；写完（配置随之重载）再按合并了 `config-file` 引入的
/// 文件的配置核对一遍，没改成时返回原因：只改得了主配置文件，绑在引入的文件里的行改不到。
pub(super) fn write_task_keybind(change: &TaskKeybind, cx: &mut App) -> Result<(), String> {
    let path = runode_config::config_path().ok_or("no home directory")?;
    let values = ConfigFile::read(&path).map_err(|err| format!("{}: {err}", path.display()))?.values("keybind");
    let renamed: Vec<String> = match change {
        TaskKeybind::Rename { old, .. } => bound_keys(&cx.global::<AppConfig>().0.keybinds, old),
        TaskKeybind::Rebind { .. } => Vec::new(),
    };
    let updated = with_task_keybind(&values, change);
    if updated != values {
        crate::config::write_values(&path, "keybind", &updated, cx)
            .map_err(|err| format!("{}: {err}", path.display()))?;
    }
    let applied = keybind_applied(&cx.global::<AppConfig>().0.keybinds, change, &renamed);
    applied.then_some(()).ok_or_else(|| rust_i18n::t!("tasks.shortcut_not_applied").into_owned())
}

/// 叠上默认绑定后绑在 `run_task:name` 上的触发键，GPUI 的写法。
fn bound_keys(keybinds: &[Keybind], name: &str) -> Vec<String> {
    let wanted = Action::RunTask(name.to_owned());
    keybind::resolve(keybinds).into_iter().filter(|(_, action)| *action == wanted).map(|(keys, _)| keys).collect()
}

/// 配置里的 `keybinds` 是不是已经照 `change` 改好了。`renamed` 是改名前绑在原名字上的触发键。
fn keybind_applied(keybinds: &[Keybind], change: &TaskKeybind, renamed: &[String]) -> bool {
    let table = keybind::resolve(keybinds);
    let runs = |keys: &str, name: &str| table.iter().any(|(k, a)| k == keys && *a == Action::RunTask(name.to_owned()));
    match change {
        TaskKeybind::Rebind { old, new } => {
            let new_keys = new.as_ref().map(|(trigger, name)| (keybind::parse_trigger(trigger), name));
            let new_ok = match &new_keys {
                Some((Ok(keys), name)) => runs(keys, name),
                Some((Err(_), _)) => false,
                None => true,
            };
            // 原名字上只剩新绑的那个键（名字没改时）。
            let old_gone = old.as_ref().is_none_or(|old| {
                bound_keys(keybinds, old)
                    .iter()
                    .all(|keys| matches!(&new_keys, Some((Ok(new_keys), name)) if new_keys == keys && *name == old))
            });
            new_ok && old_gone
        }
        TaskKeybind::Rename { old, new } => {
            old == new || (bound_keys(keybinds, old).is_empty() && renamed.iter().all(|keys| runs(keys, new)))
        }
    }
}

/// 在 `keybind` 的几行 `values` 里按 `change` 改。`Rebind` 去掉绑在 `run_task:old` 上的行（解绑的行和
/// 别的动作占着新键的行都留着，新行加在最后，叠起来时后写的赢，键照样归这条命令），再在最后加上新键
/// 那一行，原来就有一模一样的一行时先去掉它；`Rename` 把整行都是 `run_task:old` 的换成新名字，触发键
/// 照原样写。
fn with_task_keybind(values: &[String], change: &TaskKeybind) -> Vec<String> {
    let binds_only = |value: &str, name: &str| {
        let wanted = Action::RunTask(name.to_owned());
        keybind::parse(value).is_ok_and(|binds| {
            !binds.is_empty()
                && binds.iter().all(|bind| matches!(bind, Keybind::Bind { action, .. } if *action == wanted))
        })
    };
    match change {
        TaskKeybind::Rebind { old, new } => {
            let line = new.as_ref().map(|(trigger, name)| format!("{trigger}=run_task:{name}"));
            let mut out: Vec<String> = values
                .iter()
                .filter(|value| line.as_ref() != Some(*value) && old.as_ref().is_none_or(|old| !binds_only(value, old)))
                .cloned()
                .collect();
            out.extend(line);
            out
        }
        TaskKeybind::Rename { old, new } => values
            .iter()
            .map(|value| match value.split_once('=') {
                Some((trigger, _)) if binds_only(value, old) => format!("{}=run_task:{new}", trigger.trim()),
                _ => value.clone(),
            })
            .collect(),
    }
}

/// 命令文件 `file` 里除了 `except` 那一份（项目目录，通用的为空）以外，别的份里有没有叫 `name` 的
/// 命令。快捷键按名字找命令，有时它的快捷键别的命令也在用。读不懂文件时当作有，免得删掉别处在用的键。
pub(super) fn task_named_elsewhere(file: &Path, name: &str, except: Option<&Path>) -> bool {
    match fs::read_to_string(file) {
        Ok(text) => named_elsewhere(&text, name, except),
        Err(err) => err.kind() != io::ErrorKind::NotFound,
    }
}

/// `task_named_elsewhere` 按文件内容 `text` 判断。
fn named_elsewhere(text: &str, name: &str, except: Option<&Path>) -> bool {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return !text.trim().is_empty();
    };
    let global = json.get("global").filter(|_| except.is_some());
    let projects = json.get("projects").and_then(Value::as_object).into_iter().flatten();
    let projects = projects.filter(|(dir, _)| except != Some(Path::new(dir.as_str()))).map(|(_, tasks)| tasks);
    global.into_iter().chain(projects).any(|tasks| tasks.get(name).is_some())
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

    use runode_config::{
        Keybind,
        keybind::{self, Action},
    };

    use super::{TaskKeybind, action_name, keybind_applied, named_elsewhere, with_task, with_task_keybind};

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

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn rebind(old: Option<&str>, new: Option<(&str, &str)>) -> TaskKeybind {
        TaskKeybind::Rebind { old: old.map(String::from), new: new.map(|(t, n)| (t.to_owned(), n.to_owned())) }
    }

    fn binds(values: &[String]) -> Vec<Keybind> {
        values.iter().flat_map(|value| keybind::parse(value).unwrap()).collect()
    }

    /// 换键时只去掉原名字的行，新行加在最后；占着新键的别的动作（包括一行顶九条的 `digit`）和解绑的行
    /// 都留着，叠起来时新行在后面，键照样归这条命令。
    #[test]
    fn rebinding_keeps_other_actions_on_the_key() {
        let values = lines(&[
            "cmd+alt+r=run_task:dev",
            "cmd+t=unbind",
            "cmd+shift+r=new_split:right",
            "cmd+digit=goto_workspace",
            "cmd+2=run_task:lint",
        ]);
        let updated = with_task_keybind(&values, &rebind(Some("dev"), Some(("cmd+shift+r", "serve"))));
        assert_eq!(updated, [&values[1..], &lines(&["cmd+shift+r=run_task:serve"])].concat());
        let updated = with_task_keybind(&values, &rebind(Some("dev"), Some(("cmd+1", "dev"))));
        assert_eq!(updated, [&values[1..], &lines(&["cmd+1=run_task:dev"])].concat());
        let table = keybind::resolve(&binds(&updated));
        let action = |keys: &str| table.iter().find(|(k, _)| k == keys).map(|(_, a)| a.clone());
        assert_eq!(action("cmd-1"), Some(Action::RunTask("dev".into())));
        assert_eq!(action("cmd-3"), Some(Action::GotoWorkspace(2)));
        assert_eq!(action("shift-cmd-r"), Some(Action::NewSplitRight));
        // 一模一样的行挪到最后；删除只去掉它自己的行。
        assert_eq!(with_task_keybind(&values, &rebind(None, Some(("cmd+2", "lint")))), values);
        assert_eq!(with_task_keybind(&values, &rebind(Some("lint"), None)), values[..4]);
    }

    /// 只改名字时每行换成新名字，各自的触发键留着。
    #[test]
    fn renaming_keeps_every_key() {
        let values = lines(&["cmd+1=run_task:dev", "cmd+t=new_tab", "cmd+2 = run_task:dev", "cmd+3=run_task:devx"]);
        let rename = TaskKeybind::Rename { old: "dev".into(), new: "serve".into() };
        assert_eq!(
            with_task_keybind(&values, &rename),
            ["cmd+1=run_task:serve", "cmd+t=new_tab", "cmd+2=run_task:serve", "cmd+3=run_task:devx"]
        );
    }

    /// 写完核对：绑在引入的文件里、主文件改不到的行让核对不过。
    #[test]
    fn checks_the_merged_keybinds() {
        let check = |values: &[&str], change: &TaskKeybind, before: &[String]| {
            keybind_applied(&binds(&lines(values)), change, before)
        };
        let rename = TaskKeybind::Rename { old: "dev".into(), new: "serve".into() };
        let before = lines(&["cmd-1", "cmd-2"]);
        assert!(check(&["cmd+1=run_task:serve", "cmd+2=run_task:serve"], &rename, &before));
        assert!(!check(&["cmd+1=run_task:serve", "cmd+2=run_task:dev"], &rename, &before));
        let change = rebind(Some("dev"), Some(("cmd+3", "dev")));
        assert!(check(&["cmd+3=run_task:dev"], &change, &[]));
        assert!(!check(&["cmd+3=run_task:dev", "cmd+1=run_task:dev"], &change, &[]));
        assert!(!check(&["cmd+3=run_task:dev", "cmd+3=new_tab"], &change, &[]));
        assert!(check(&["cmd+1=run_task:lint"], &rebind(Some("dev"), None), &[]));
    }

    /// 自己绑的动作按动作表认出来，认不出时写动作本身。
    #[test]
    fn names_actions_the_user_bound() {
        assert_eq!(action_name("shift-cmd-r", &Action::NewSplitRight), keybind::describe("new_split"));
        assert_eq!(action_name("cmd-9", &Action::GotoTab(4)), "GotoTab(4)");
    }

    /// 只看别的份；读不懂的内容当作有。
    #[test]
    fn finds_tasks_named_elsewhere() {
        let text = r#"{"global": {"lint": "x"}, "projects": {"/a": {"dev": "1"}, "/b": {"dev": "2"}}}"#;
        assert!(named_elsewhere(text, "dev", Some(Path::new("/b"))));
        assert!(!named_elsewhere(r#"{"projects": {"/b": {"dev": "2"}}}"#, "dev", Some(Path::new("/b"))));
        assert!(named_elsewhere(text, "lint", Some(Path::new("/a"))));
        assert!(!named_elsewhere(text, "lint", None));
        assert!(named_elsewhere("{oops", "dev", None));
        assert!(!named_elsewhere("", "dev", None));
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
