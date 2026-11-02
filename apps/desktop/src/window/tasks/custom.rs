//! 自己加的命令的添加和编辑对话框，改的是 runode 根目录的 `tasks.json`（`Dirs::tasks_file`）。
//! 一条命令要么只用于一个项目（在项目目录里跑），要么通用（`global`，在终端当前目录里跑）。对话框
//! 里还能录一个快捷键，存在同一个文件的 `keys` 里（见 `keys`），按名字找命令，改名、换份、删除时
//! 跟着改。Makefile、package.json 的命令不能改，同一个对话框只录快捷键（`bind_task`）。

use crate::ui::dialog::backdrop;
use crate::ui::theme::ActiveTheme as _;
use std::path::{Path, PathBuf};

use gpui::{
    App, Context, Div, Entity, EntityId, FocusHandle, Focusable, FontWeight, KeyDownEvent, Keystroke, MouseButton,
    Role, Stateful, Subscription, Task, Window, div, prelude::*, px,
};
use runode_config::{
    Keybind,
    keybind::{self, Action},
};
use runode_paths::Dirs;
use runode_tasks::file::{Keys, Rules, write_task, write_task_key};

use super::{AddTask, BindTask, EditTask, TaskKeys};
use crate::ui::button::{Button, ButtonVariant};
use crate::ui::dialog::Dialog;
use crate::ui::segmented::Segmented;
use crate::{
    config::AppConfig,
    ui::{
        a11y::Press,
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
    },
    window::WindowView,
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
        if keystroke.unparse(cx.platform_info().os) != "escape" {
            let Some(trigger) = keybind::format_trigger(&keystroke.unparse(cx.platform_info().os)) else {
                return;
            };
            dialog.shortcut = Some(trigger);
        }
        dialog.recording = None;
        self.focus_add_task_field(true, window, cx);
    }

    fn set_add_task_global(&mut self, global: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.add_task {
            dialog.global = global;
            cx.notify();
        }
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
            match event.keystroke.unparse(cx.platform_info().os).as_str() {
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
    /// 空着时用命令行当名字。编辑时换掉原来那一条。
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
            write_task(&file, &dir, (old_project.as_deref(), &old_name), new, keys, &RULES)
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
            write_task_key(&file, &dir, &name, trigger.as_deref(), &RULES)
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
        let shortcut_label = match (&dialog.recording, &dialog.shortcut) {
            (Some(_), _) => rust_i18n::t!("tasks.shortcut_press").into_owned(),
            (None, Some(trigger)) => crate::keybinds::caps_text(trigger),
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
                                Segmented::new("add-task-scope", scope_label)
                                    .option(
                                        "add-task-project",
                                        rust_i18n::t!("tasks.scope_project").into_owned(),
                                        !dialog.global,
                                        cx,
                                        |this, _, cx| this.set_add_task_global(false, cx),
                                    )
                                    .option(
                                        "add-task-global",
                                        rust_i18n::t!("tasks.scope_global").into_owned(),
                                        dialog.global,
                                        cx,
                                        |this, _, cx| this.set_add_task_global(true, cx),
                                    ),
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
            backdrop("add-task-backdrop")
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

/// 改 `tasks.json` 时认键和找项目的办法。
pub(super) const RULES: Rules = Rules { same_key, project_of };

/// 两种写法（`cmd+t` 和 `command+t`）是不是同一个键；写错了的键不和别的算同一个。
fn same_key(a: &str, b: &str) -> bool {
    keybind::parse_trigger(a).is_ok_and(|a| keybind::parse_trigger(b).is_ok_and(|b| a == b))
}

/// 不在哪个记过的项目里的目录，项目快捷键记在它所在仓库的根目录，不在仓库里时记在它自己。
fn project_of(dir: &Path) -> PathBuf {
    runode_git::repo_root(dir).unwrap_or_else(|| dir.to_owned())
}

#[cfg(test)]
mod tests {
    use runode_config::keybind::Action;

    use super::action_name;

    /// 自己绑的动作按动作表认出来，认不出时写动作本身。
    #[test]
    fn names_actions_the_user_bound() {
        assert_eq!(action_name("shift-cmd-r", &Action::NewSplitRight), crate::i18n::describe("new_split"));
        assert_eq!(action_name("cmd-9", &Action::GotoTab(4)), "GotoTab(4)");
    }
}
