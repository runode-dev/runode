//! 自己加的命令：添加的对话框，以及改写项目目录下的 `CUSTOM_TASKS_FILE`。添加时填名字和命令行，
//! 已经有这个文件时写进列出来的那一个，没有时建在终端目录所在仓库的根目录，不在仓库里时建在终端
//! 目录；同名的命令换成新的命令行。命令菜单里这些命令行尾的删除按钮从文件里删掉它。文件和 Makefile
//! 一样由宿主列出来，手机上也看得到；改命令行直接改文件。

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui::{
    ClickEvent, Context, Div, Entity, Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton, Stateful, Subscription,
    Window, div, prelude::*, px,
};
use runode_protocol::{CUSTOM_TASKS_FILE, TaskSourceKind};
use runode_shared_types::color::Rgb;
use serde_json::{Map, Value};

use super::AddTask;
use crate::{
    ui::{
        hsla,
        text_field::{TextField, TextFieldEvent},
    },
    window::{TITLEBAR_HEIGHT, WindowView},
};

pub(in crate::window) struct AddTaskDialog {
    name: Entity<TextField>,
    command: Entity<TextField>,
    /// 上次写文件失败的原因。
    error: Option<String>,
    /// 正在后台写文件，写完之前不再写。
    saving: bool,
    _events: [Subscription; 2],
}

impl WindowView {
    /// 打开添加命令的对话框，焦点给名字；已经开着时只给焦点。
    pub(in crate::window) fn add_task(&mut self, _: &AddTask, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_task.is_none() {
            let field = |key: &str, cx: &mut Context<Self>| {
                let placeholder = rust_i18n::t!(key).into_owned();
                cx.new(|cx| TextField::editing(String::new(), 0, cx).with_placeholder(placeholder))
            };
            let name = field("tasks.name_placeholder", cx);
            let command = field("tasks.command_placeholder", cx);
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
            self.add_task =
                Some(AddTaskDialog { name, command, error: None, saving: false, _events: [on_name, on_command] });
        }
        self.focus_add_task_field(false, window, cx);
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
    /// 空着时用命令行当名字。
    fn save_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.project_dir(cx);
        let listed = self.workspace().project.tasks.as_ref().and_then(|(listed, sources)| {
            let source = sources.iter().find(|source| source.kind == TaskSourceKind::Custom)?;
            (*listed == dir).then(|| source.file.clone())
        });
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
        let job = cx.background_spawn(async move {
            let file = listed.unwrap_or_else(|| runode_git::repo_root(&dir).unwrap_or(dir).join(CUSTOM_TASKS_FILE));
            write_task(&file, &name, Some(&command))
        });
        cx.spawn_in(window, async move |this, cx| {
            let saved = job.await;
            this.update_in(cx, |this, window, cx| {
                match saved {
                    Ok(()) => {
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
                .flex_none()
                .px(px(12.))
                .py(px(5.))
                .rounded(px(6.))
                .bg(bg)
                .hover(move |button| button.bg(hover))
                .cursor_pointer()
                .child(label)
        };
        let cancel = button("add-task-cancel", rust_i18n::t!("tasks.cancel").into_owned(), panel_bg, hover_bg)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_add_task(window, cx)));
        let save = button("add-task-save", rust_i18n::t!("tasks.save").into_owned(), primary_bg, primary_hover_bg)
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_task(window, cx)));
        let panel = div()
            .id("add-task-dialog")
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
                    .text_size(px(16.))
                    .font_weight(FontWeight::BOLD)
                    .child(rust_i18n::t!("tasks.add_title").into_owned()),
            )
            .child(field(rust_i18n::t!("tasks.name").into_owned(), &dialog.name))
            .child(field(rust_i18n::t!("tasks.command").into_owned(), &dialog.command))
            .child(
                div()
                    .text_color(fg.opacity(0.55))
                    .child(rust_i18n::t!("tasks.add_hint", file = CUSTOM_TASKS_FILE).into_owned()),
            )
            .children(dialog.error.clone().map(|error| div().text_color(gpui::red()).child(error)))
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

/// 把 `name` 的命令行 `command` 写进 `file`，文件和它的目录不在时建出来；`command` 为空时从文件里
/// 删掉 `name`。读不懂原来的内容时不写，免得把手写的文件冲掉。
pub(super) fn write_task(file: &Path, name: &str, command: Option<&str>) -> Result<(), String> {
    let text = match fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound && command.is_none() => return Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{}: {err}", file.display())),
    };
    let updated = with_task(text.as_deref(), name, command).map_err(|err| format!("{}: {err}", file.display()))?;
    let dir = file.parent().map(PathBuf::from).unwrap_or_default();
    fs::create_dir_all(&dir).and_then(|()| fs::write(file, updated)).map_err(|err| format!("{}: {err}", file.display()))
}

/// 在命令文件的内容 `text`（没有文件时为空）的 `tasks` 里加上或换掉 `name`，`command` 为空时删掉它，
/// 别的内容和先后原样留着。
fn with_task(text: Option<&str>, name: &str, command: Option<&str>) -> Result<String, String> {
    let mut json = match text.filter(|text| !text.trim().is_empty()) {
        Some(text) => serde_json::from_str(text).map_err(|err| err.to_string())?,
        None => Value::Object(Map::new()),
    };
    let tasks = json
        .as_object_mut()
        .ok_or("not a JSON object")?
        .entry("tasks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("`tasks` is not a JSON object")?;
    match command {
        Some(command) => tasks.insert(name.to_owned(), Value::String(command.to_owned())),
        None => tasks.shift_remove(name),
    };
    let mut text = serde_json::to_string_pretty(&json).map_err(|err| err.to_string())?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::with_task;

    #[test]
    fn adds_to_new_and_existing_files() {
        assert_eq!(
            with_task(None, "dev", Some("cargo run")).unwrap(),
            "{\n  \"tasks\": {\n    \"dev\": \"cargo run\"\n  }\n}\n"
        );
        let existing = r#"{"note": 1, "tasks": {"dev": "old", "lint": "cargo clippy"}}"#;
        let updated: serde_json::Value =
            serde_json::from_str(&with_task(Some(existing), "dev", Some("cargo run")).unwrap()).unwrap();
        assert_eq!(updated, serde_json::json!({"note": 1, "tasks": {"dev": "cargo run", "lint": "cargo clippy"}}));
        assert!(with_task(Some("[1]"), "dev", Some("x")).is_err());
        assert!(with_task(Some("{oops"), "dev", Some("x")).is_err());
    }

    /// 删掉一条，其余的先后不变。
    #[test]
    fn removes_a_task_keeping_the_order() {
        let existing = r#"{"tasks": {"a": "1", "b": "2", "c": "3"}}"#;
        let updated = with_task(Some(existing), "a", None).unwrap();
        assert_eq!(updated, "{\n  \"tasks\": {\n    \"b\": \"2\",\n    \"c\": \"3\"\n  }\n}\n");
    }
}
