//! 自己加的命令：添加和编辑的对话框，以及改写 runode 根目录的 `tasks.json`（`Dirs::tasks_file`）。
//! 一条命令要么只用于一个项目（`projects` 下按项目目录分，在项目目录里跑），要么通用（`global`，在
//! 终端当前目录里跑），对话框里选。添加到本项目时，项目目录取终端目录所在仓库的根目录，不在仓库里时取
//! 终端目录；同一份里同名的换成新的命令行。命令菜单里这些命令行尾的编辑按钮用同一个对话框改，改完留在
//! 原来的位置；删除按钮从文件里删掉它。对话框里还能录一个快捷键，存在同一个文件的 `keys` 里（见 `keys`）：
//! 通用命令的记在通用的那份，别的记在终端目录用的那个项目的那份；快捷键按名字找命令，改名、换份、删除时
//! 跟着改。文件由宿主列出来（`TaskSourceKind::Custom`、`Global`），手机上也看得到。Makefile、package.json
//! 的命令不能改，同一个对话框只录快捷键（`bind_task`），名字和命令行只写出来、不写命令。

use crate::ui::theme::ActiveTheme as _;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use gpui::{
    App, Context, Div, Entity, EntityId, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent, Keystroke,
    MouseButton, Role, Stateful, Subscription, Task, Window, div, prelude::*, px,
};
use runode_config::{
    Keybind,
    keybind::{self, Action},
};
use runode_paths::Dirs;
use serde_json::{Map, Value};

use super::{AddTask, BindTask, EditTask, TaskKeys};
use crate::ui::button::{Button, ButtonVariant};
use crate::ui::dialog::Dialog;
use crate::{
    config::AppConfig,
    settings::keybind_caps,
    ui::{
        a11y::Press,
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
    },
    window::{TITLEBAR_HEIGHT, WindowView},
};

pub(in crate::window) struct AddTaskDialog {
    /// 编辑时是改哪一份（项目目录，通用的为空）里的哪一条；添加时为空。
    editing: Option<(Option<PathBuf>, String)>,
    /// 只给 Makefile、package.json 里的命令录快捷键时，它的名字和命令行；这时没有输入框，焦点给对话框。
    binding: Option<(String, String)>,
    focus: FocusHandle,
    /// 选的是通用的。
    global: bool,
    name: Entity<InputState>,
    command: Entity<InputState>,
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

    /// 打开给 Makefile、package.json 里的一条命令录快捷键的对话框，一打开就在录。
    pub(in crate::window) fn bind_task(&mut self, action: &BindTask, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task_dialog(None, String::new(), String::new(), window, cx);
        let shortcut = self.task_shortcut(false, &action.name, cx);
        if let Some(dialog) = &mut self.add_task {
            dialog.shortcut = shortcut.clone();
            dialog.initial_shortcut = shortcut;
            dialog.binding = Some((action.name.clone(), action.command.clone()));
        }
        self.record_task_shortcut(window, cx);
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
                cx.new(|cx| InputState::editing(text, select, cx).with_placeholder(placeholder).with_label(label))
            };
            let name = field("tasks.name", "tasks.name_placeholder", name, cx);
            let command = field("tasks.command", "tasks.command_placeholder", command, cx);
            // 名字里回车跳到命令，命令里回车添加；Esc 关掉对话框。
            let on_name = cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Next => this.focus_add_task_field(true, window, cx),
                InputEvent::Dismiss => this.close_add_task(window, cx),
                InputEvent::Changed(_) | InputEvent::Previous => {}
            });
            let on_command = cx.subscribe_in(&command, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Next => this.save_task(window, cx),
                InputEvent::Dismiss => this.close_add_task(window, cx),
                InputEvent::Changed(_) | InputEvent::Previous => {}
            });
            let global = editing.as_ref().is_some_and(|(project, _)| project.is_none());
            let shortcut = editing.as_ref().and_then(|(_, name)| self.task_shortcut(global, name, cx));
            self.add_task = Some(AddTaskDialog {
                editing,
                binding: None,
                focus: cx.focus_handle(),
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

    /// 叫 `name` 的命令（`global` 时是通用命令）在这里绑着的快捷键，配置文件的写法。
    fn task_shortcut(&self, global: bool, name: &str, cx: &Context<Self>) -> Option<String> {
        let keys = cx.global::<TaskKeys>();
        let project = if global { None } else { Some(keys.project(&self.project_dir(cx))?) };
        keys.shortcut(project, name)
    }

    /// 焦点给命令框（`command`）或名字框。
    fn focus_add_task_field(&mut self, command: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.add_task else {
            return;
        };
        let field = match &dialog.binding {
            Some(_) => &dialog.focus,
            None if command => &dialog.command.focus_handle(cx),
            None => &dialog.name.focus_handle(cx),
        };
        window.focus(field, cx);
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

    /// Tab 在名字和命令之间切换；只录快捷键时回车保存、Esc 关掉。
    fn add_task_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.add_task else {
            return;
        };
        if dialog.binding.is_some() {
            match event.keystroke.unparse().as_str() {
                "enter" => self.save_task(window, cx),
                "escape" => self.close_add_task(window, cx),
                _ => return,
            }
            cx.stop_propagation();
        } else if event.keystroke.key == "tab"
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform
        {
            cx.stop_propagation();
            let in_name = dialog.name.focus_handle(cx).is_focused(window);
            self.focus_add_task_field(in_name, window, cx);
        }
    }

    /// 在后台把填好的命令写进文件，写好了关掉对话框、重列命令；写不了时把原因写在对话框里。名字
    /// 空着时用命令行当名字。编辑时换掉原来那一条，同一份里改了名字也留在原来的位置；换到另一份时
    /// 加在那一份的最后。快捷键没改过时原来绑的键跟着命令改名、换份，改了时换成录的这一个；新加的没录
    /// 快捷键时不碰这个名字上已经绑着的键。
    fn save_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.project_dir(cx);
        let Some(dialog) = &mut self.add_task else {
            return;
        };
        if let Some((name, _)) = dialog.binding.clone() {
            return self.save_task_shortcut(name, dir, window, cx);
        }
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
        let origin = dialog.name.entity_id();
        let (editing, global) = (dialog.editing.clone(), dialog.global);
        let keys =
            if dialog.shortcut == dialog.initial_shortcut { Keys::Keep } else { Keys::Set(dialog.shortcut.clone()) };
        let job = cx.background_spawn(async move {
            let file = Dirs::from_env().tasks_file().ok_or("no home directory")?;
            // 编辑本项目的命令时还是那个项目；加到本项目时现查终端目录在哪个仓库里。
            let project = match &editing {
                _ if global => None,
                Some((Some(project), _)) => Some(project.clone()),
                _ => Some(runode_git::repo_root(&dir).unwrap_or_else(|| dir.clone())),
            };
            let (old_project, old_name) = editing.unwrap_or_else(|| (project.clone(), name.clone()));
            let new = Some((project.as_deref(), name.as_str(), command.as_str()));
            write_task(&file, &dir, (old_project.as_deref(), &old_name), new, keys)
        });
        self.finish_saving(origin, job, window, cx);
    }

    /// 只录快捷键时保存：改了快捷键就在后台换掉绑在这个名字上的，没改时直接关掉。
    fn save_task_shortcut(&mut self, name: String, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.add_task else {
            return;
        };
        dialog.recording = None;
        if dialog.saving {
            return;
        }
        if dialog.shortcut == dialog.initial_shortcut {
            return self.close_add_task(window, cx);
        }
        dialog.saving = true;
        let origin = dialog.name.entity_id();
        let trigger = dialog.shortcut.clone();
        let job = cx.background_spawn(async move {
            let file = Dirs::from_env().tasks_file().ok_or("no home directory")?;
            write_task_key(&file, &dir, &name, trigger.as_deref())
        });
        self.finish_saving(origin, job, window, cx);
    }

    /// 等后台写完命令文件：写好了重读快捷键（随配置一起）、重列命令，关掉发起保存的对话框；写不了时把原因
    /// 写在那个对话框里。写的期间对话框还能关掉、再开一个新的（`origin` 是发起的那个），写完只动它。
    fn finish_saving(
        &mut self,
        origin: EntityId,
        job: Task<Result<(), String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let saved = job.await;
            this.update_in(cx, |this, window, cx| {
                let same_dialog = this.add_task.as_ref().is_some_and(|dialog| dialog.name.entity_id() == origin);
                match saved {
                    Ok(()) => {
                        crate::config::reload(cx);
                        this.workspace_mut().project.tasks_stale = true;
                        this.list_tasks(cx);
                        if same_dialog {
                            this.close_add_task(window, cx);
                        }
                    }
                    Err(err) => match &mut this.add_task {
                        Some(dialog) if same_dialog => {
                            dialog.saving = false;
                            dialog.error = Some(err);
                        }
                        _ => tracing::warn!("could not save the task: {err}"),
                    },
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(in crate::window) fn render_add_task(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.add_task.as_ref()?;
        let hover_bg = cx.theme().mix(0.06);
        let primary_bg = cx.theme().mix(0.16);
        let fg = cx.theme().foreground;
        let field = |label: String, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().text_color(fg.opacity(0.7)).child(label))
                .child(Input::new(input))
        };
        // 对话框底下的按钮：比 `ButtonSize::Default` 高一些，高度由上下留白撑开。
        let button = |id: &'static str, label: String, variant: ButtonVariant| {
            Button::new(id, label).variant(variant).h_auto().py(px(5.)).cursor_pointer()
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
            .h(scaled(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(cx.theme().background)
            .border_1()
            .border_color(if dialog.recording.is_some() { fg.opacity(0.6) } else { fg.opacity(0.2) })
            .cursor_pointer()
            .when(dialog.shortcut.is_none() || dialog.recording.is_some(), |shortcut| {
                shortcut.text_color(fg.opacity(0.55))
            })
            .child(shortcut_label)
            .on_press(cx, |this, window, cx| this.record_task_shortcut(window, cx));
        let clear_shortcut = dialog.shortcut.as_ref().filter(|_| dialog.recording.is_none()).map(|_| {
            Button::new("add-task-shortcut-clear", rust_i18n::t!("tasks.shortcut_clear"))
                .h_auto()
                .px(px(6.))
                .py(px(3.))
                .rounded(px(4.))
                .text_color(fg.opacity(0.6))
                .cursor_pointer()
                .on_press(cx, |this, _, cx| {
                    if let Some(dialog) = &mut this.add_task {
                        dialog.shortcut = None;
                        cx.notify();
                    }
                })
        });
        let dir = self.project_dir(cx);
        let taken = dialog.shortcut.as_deref().filter(|_| dialog.recording.is_none()).and_then(|trigger| {
            let typed = dialog.name.read(cx).query().trim().to_owned();
            let own = match (&dialog.binding, &dialog.editing) {
                (Some((name, _)), _) | (None, Some((_, name))) => name.as_str(),
                (None, None) => typed.as_str(),
            };
            shortcut_taken_by(trigger, own, dialog.binding.is_none() && dialog.global, &dir, cx)
        });
        let hint = if dialog.binding.is_some() {
            rust_i18n::t!("tasks.bind_hint")
        } else if dialog.global {
            rust_i18n::t!("tasks.hint_global", file = TASKS_FILE_HINT)
        } else {
            rust_i18n::t!("tasks.hint_project", file = TASKS_FILE_HINT)
        };
        let cancel = button("add-task-cancel", rust_i18n::t!("tasks.cancel").into_owned(), ButtonVariant::Ghost)
            .on_press(cx, |this, window, cx| this.close_add_task(window, cx));
        let (title, save) = match (&dialog.binding, &dialog.editing) {
            (Some((name, _)), _) => (rust_i18n::t!("tasks.bind_title", name = name), rust_i18n::t!("tasks.update")),
            (None, Some(_)) => (rust_i18n::t!("tasks.edit_title"), rust_i18n::t!("tasks.update")),
            (None, None) => (rust_i18n::t!("tasks.add_title"), rust_i18n::t!("tasks.save")),
        };
        let save = button("add-task-save", save.into_owned(), ButtonVariant::Secondary)
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_press(cx, |this, window, cx| this.save_task(window, cx));
        let (title, scope_label, hint) =
            (title.into_owned(), rust_i18n::t!("tasks.scope").into_owned(), hint.into_owned());
        let panel = Dialog::new("add-task-dialog", title.clone())
            .when(dialog.binding.is_some(), |panel| panel.track_focus(&dialog.focus))
            .on_key_down(cx.listener(Self::add_task_key))
            .w(px(420.))
            .max_w_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .id("add-task-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .when_some(dialog.binding.as_ref(), |panel, (_, command)| {
                panel.child(
                    div()
                        .id("add-task-bound-command")
                        .role(Role::Label)
                        .aria_label(command.clone())
                        .text_color(fg.opacity(0.7))
                        .child(command.clone()),
                )
            })
            .when(dialog.binding.is_none(), |panel| {
                panel
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
                                    .bg(cx.theme().background)
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
            })
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

/// `trigger` 在这里现在跑的是别的时，那是什么：绑着别的命令时是它的名字（项目的键也算上这里照样生效的
/// 通用的键，`global` 时只看通用的），绑着别的动作时是动作的说明；没绑或者跑的就是 `own` 这条命令时为空。
fn shortcut_taken_by(trigger: &str, own: &str, global: bool, dir: &Path, cx: &App) -> Option<String> {
    let keys = keybind::parse_trigger(trigger).ok()?;
    let tasks = cx.global::<TaskKeys>();
    let task = if global { tasks.name_at(None, &keys) } else { tasks.lookup(dir, &keys).map(|(_, name)| name) };
    if let Some(name) = task {
        return (name != own).then(|| name.to_owned());
    }
    let (_, action) =
        keybind::resolve(&cx.global::<AppConfig>().0.keybinds).into_iter().find(|(bound, _)| *bound == keys)?;
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
    default.or_else(listed).map_or_else(|| format!("{action:?}"), crate::i18n::describe)
}

/// 对话框里告诉用户命令存在哪。
const TASKS_FILE_HINT: &str = "~/.runode/tasks.json";

/// 改命令文件的读、改、写换上去整个拿着它：保存和删除各是一个后台任务，几个窗口也会同时改，两个一起读到
/// 同一份内容再各自换上去时，先写的那份改动就丢了。
static WRITING: Mutex<()> = Mutex::new(());

/// 快捷键怎么跟着命令改，见 `move_keys`。
pub(super) enum Keys {
    /// 原来绑的几个键原样跟着走。
    Keep,
    /// 换成这一个，为空时不绑。
    Set(Option<String>),
}

/// 把 `file` 里的 `old`（项目目录，通用的为空；名字）换成 `new`（项目目录；名字和命令行），`new` 为空时
/// 删掉 `old`；绑在它上面的快捷键按 `keys` 跟着改，项目命令的记在终端目录 `dir` 用的那份里（`key_project`）。
pub(super) fn write_task(
    file: &Path,
    dir: &Path,
    old: (Option<&Path>, &str),
    new: Option<(Option<&Path>, &str, &str)>,
    keys: Keys,
) -> Result<(), String> {
    edit_file(file, |root| {
        let projects = old.0.is_some() || new.is_some_and(|(project, ..)| project.is_some());
        let keys_at = projects.then(|| key_project(root, dir));
        let scope = |project: Option<&Path>| project.and(keys_at.as_deref());
        with_task(root, old, new)?;
        move_keys(root, (scope(old.0), old.1), new.map(|(project, name, _)| (scope(project), name)), &keys)
    })
}

/// 给终端目录 `dir` 列出来的、叫 `name` 的命令（Makefile、package.json 的）绑上 `trigger`，为空时去掉它的
/// 快捷键，记在 `dir` 用的那份里。
pub(super) fn write_task_key(file: &Path, dir: &Path, name: &str, trigger: Option<&str>) -> Result<(), String> {
    edit_file(file, |root| {
        let at = key_project(root, dir);
        move_keys(root, (Some(&at), name), Some((Some(&at), name)), &Keys::Set(trigger.map(str::to_owned)))
    })
}

/// 改命令文件 `file`：文件和它的目录不在时建出来，没有文件、改完也是空的（比如删除）时不建；读不懂原来的
/// 内容时不写，免得把手写的文件冲掉。
fn edit_file(file: &Path, edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>) -> Result<(), String> {
    let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let text = match fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{}: {err}", file.display())),
    };
    let updated = edit_text(text.as_deref(), edit).map_err(|err| format!("{}: {err}", file.display()))?;
    if text.is_none() && updated.trim() == "{}" {
        return Ok(());
    }
    let dir = file.parent().map(PathBuf::from).unwrap_or_default();
    // 用户手写过的文件：写临时文件再换上去，写到一半出错不留下空的或半截的。
    fs::create_dir_all(&dir)
        .and_then(|()| runode_paths::replace_file(file, updated.as_bytes()))
        .map_err(|err| format!("{}: {err}", file.display()))
}

/// 在命令文件的内容 `text`（没有文件时为空）上做 `edit`。删空了的项目、通用那份和快捷键那份一起拿掉，
/// 别的内容和先后原样留着。
fn edit_text(
    text: Option<&str>,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<String, String> {
    let mut json = match text.filter(|text| !text.trim().is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|err| err.to_string())?,
        None => Value::Object(Map::new()),
    };
    let root = json.as_object_mut().ok_or("not a JSON object")?;
    edit(root)?;
    if let Some(keys) = root.get_mut("keys").and_then(Value::as_object_mut) {
        prune(keys);
    }
    prune(root);
    let mut text = serde_json::to_string_pretty(&json).map_err(|err| err.to_string())?;
    text.push('\n');
    Ok(text)
}

/// 拿掉空了的项目，和空了的 `global`、`projects`、`keys`。
fn prune(root: &mut Map<String, Value>) {
    if let Some(projects) = root.get_mut("projects").and_then(Value::as_object_mut) {
        projects.retain(|_, tasks| tasks.as_object().is_none_or(|tasks| !tasks.is_empty()));
    }
    root.retain(|key, value| {
        !(matches!(key.as_str(), "global" | "projects" | "keys") && value.as_object().is_some_and(Map::is_empty))
    });
}

/// 把 `old` 换成 `new`。同一份里就在 `old` 原来的位置换，换到另一份时从原来那份删掉、加在新那份的最后；
/// `new` 为空时只删掉 `old`。新名字在那一份里别处已经有了时，那一条让位。
fn with_task(
    root: &mut Map<String, Value>,
    old: (Option<&Path>, &str),
    new: Option<(Option<&Path>, &str, &str)>,
) -> Result<(), String> {
    let same = new.is_some_and(|(project, ..)| project == old.0);
    replace(scope_tasks(root, old.0)?, Some(old.1), new.filter(|_| same).map(|(_, name, command)| (name, command)));
    if let Some((project, name, command)) = new.filter(|_| !same) {
        replace(scope_tasks(root, project)?, None, Some((name, command)));
    }
    Ok(())
}

/// 快捷键那一份 `from`（项目目录，通用的为空；名字）里绑在这个名字上的键都去掉，`to` 有时换到那里：
/// `Keep` 原来那几个键搬过去（同一份里就原地改名，先后不变），`Set` 只绑给的那一个，那份里同一个键（写法
/// 不同也算）原来跑别的命令时换成这一条。
fn move_keys(
    root: &mut Map<String, Value>,
    from: (Option<&Path>, &str),
    to: Option<(Option<&Path>, &str)>,
    keys: &Keys,
) -> Result<(), String> {
    let table = key_table(root, from.0)?;
    // 同一个键写了两种写法时后写的赢：原地改名才不会把被盖住的那个挪到后面、抢走别的命令的键。
    if let (Keys::Keep, Some((project, name))) = (keys, to)
        && project == from.0
    {
        for value in table.values_mut().filter(|value| value.as_str() == Some(from.1)) {
            *value = Value::String(name.to_owned());
        }
        return Ok(());
    }
    let bound: Vec<String> =
        table.iter().filter(|(_, name)| name.as_str() == Some(from.1)).map(|(trigger, _)| trigger.clone()).collect();
    table.retain(|_, name| name.as_str() != Some(from.1));
    let triggers = match keys {
        Keys::Keep => bound,
        Keys::Set(trigger) => trigger.iter().cloned().collect(),
    };
    if let Some((project, name)) = to {
        let table = key_table(root, project)?;
        for trigger in triggers {
            if let Keys::Set(_) = keys {
                let keys = keybind::parse_trigger(&trigger);
                table.retain(|bound, _| keys.is_err() || keybind::parse_trigger(bound) != keys);
            }
            table.insert(trigger, Value::String(name.to_owned()));
        }
    }
    Ok(())
}

/// 快捷键里 `project` 那一份（为空时是通用的），没有时建出来。
fn key_table<'a>(
    root: &'a mut Map<String, Value>,
    project: Option<&Path>,
) -> Result<&'a mut Map<String, Value>, String> {
    let keys = root.entry("keys").or_insert_with(|| Value::Object(Map::new()));
    scope_tasks(keys.as_object_mut().ok_or("`keys` is not a JSON object")?, project)
}

/// 终端目录 `dir` 的项目快捷键记在哪份：和 `TaskKeys::project` 一样先找已经有键的那份，没有时取自己加的
/// 命令所在的项目（`projects` 里最深的），再没有时取 `dir` 所在仓库的根目录，不在仓库里时取 `dir`。
fn key_project(root: &Map<String, Value>, dir: &Path) -> PathBuf {
    let deepest = |table: Option<&Value>| {
        let dirs = table.and_then(Value::as_object).into_iter().flatten().map(|(at, _)| Path::new(at));
        dirs.filter(|at| dir.starts_with(at)).max_by_key(|at| at.components().count()).map(Path::to_path_buf)
    };
    deepest(root.get("keys").and_then(|keys| keys.get("projects")))
        .or_else(|| deepest(root.get("projects")))
        .unwrap_or_else(|| runode_git::repo_root(dir).unwrap_or_else(|| dir.to_owned()))
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

    use runode_config::keybind::Action;

    use super::{Keys, action_name, edit_text, move_keys, write_task};

    /// 文本进、文本出地改命令：`edit_text` 套上 `with_task`。
    fn with_task(
        text: Option<&str>,
        old: (Option<&Path>, &str),
        new: Option<(Option<&Path>, &str, &str)>,
    ) -> Result<String, String> {
        edit_text(text, |root| super::with_task(root, old, new))
    }

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

    /// 几个后台任务同时改命令文件：各自的改动都留下，不会读到同一份内容再互相盖掉。
    #[test]
    fn concurrent_writes_keep_every_change() {
        let dir = std::env::temp_dir().join(format!("rdr-tasks-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("tasks.json");
        let names: Vec<String> = (0..16).map(|ix| format!("task{ix}")).collect();
        std::thread::scope(|scope| {
            for name in &names {
                let file = &file;
                scope.spawn(move || {
                    write_task(file, Path::new("/"), (None, name), Some((None, name, "true")), Keys::Keep).unwrap()
                });
            }
        });
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        for name in &names {
            assert!(json["global"].get(name).is_some(), "{name} is missing: {json}");
        }
    }

    /// 快捷键跟着命令走：`Keep` 把原来的键搬到新名字、新的一份，`Set` 换成给的那一个、挤掉同一份里同一个键
    /// （写法不同也算）上的别的命令，删掉命令时它的键一起去掉，删空了的那份拿掉；别的份和别的命令的键不动。
    #[test]
    fn keys_follow_their_task() {
        let app = Some(Path::new("/w/app"));
        let existing = r#"{"keys": {"global": {"cmd+r": "up"}, "projects": {"/w/app": {"cmd+1": "dev", "cmd+2": "dev", "command+t": "lint"}}}}"#;
        let edit = |from, to, keys: Keys| edit_text(Some(existing), |root| move_keys(root, from, to, &keys)).unwrap();
        assert_eq!(
            edit((app, "dev"), Some((None, "serve")), Keys::Keep),
            pretty(serde_json::json!({"keys": {
                "global": {"cmd+r": "up", "cmd+1": "serve", "cmd+2": "serve"},
                "projects": {"/w/app": {"command+t": "lint"}}
            }}))
        );
        assert_eq!(
            edit((app, "dev"), Some((app, "dev")), Keys::Set(Some("cmd+t".into()))),
            pretty(serde_json::json!({"keys": {"global": {"cmd+r": "up"}, "projects": {"/w/app": {"cmd+t": "dev"}}}}))
        );
        assert_eq!(
            edit((None, "up"), None, Keys::Keep),
            pretty(
                serde_json::json!({"keys": {"projects": {"/w/app": {"cmd+1": "dev", "cmd+2": "dev", "command+t": "lint"}}}})
            )
        );
        assert_eq!(edit_text(None, |root| move_keys(root, (None, "x"), None, &Keys::Keep)).unwrap(), "{}\n");
        // 同一个键两种写法、后写的 `lint` 生效：只改 `dev` 的名字不动先后，键还是 `lint` 的。
        let aliased = r#"{"keys": {"global": {"cmd+r": "dev", "command+r": "lint"}}}"#;
        let renamed =
            edit_text(Some(aliased), |root| move_keys(root, (None, "dev"), Some((None, "serve")), &Keys::Keep));
        assert_eq!(
            renamed.unwrap(),
            pretty(serde_json::json!({"keys": {"global": {"cmd+r": "serve", "command+r": "lint"}}}))
        );
    }

    /// 自己绑的动作按动作表认出来，认不出时写动作本身。
    #[test]
    fn names_actions_the_user_bound() {
        assert_eq!(action_name("shift-cmd-r", &Action::NewSplitRight), crate::i18n::describe("new_split"));
        assert_eq!(action_name("cmd-9", &Action::GotoTab(4)), "GotoTab(4)");
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
