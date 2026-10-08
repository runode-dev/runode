//! 标题栏右上角的项目命令：终端目录往上最近的自己加的命令、Makefile 的目标和 package.json 的 scripts，
//! 由宿主列出、拼好命令行（`ClientMsg::ListProjectTasks`），和手机会话卡片上的是同一份。右侧面板开关
//! 左边的按钮弹出按文件分组的菜单，点一条开一个新标签在列命令的目录里跑它；菜单最上面一项添加自己的
//! 命令，自己加的命令行尾有编辑和删除按钮，添加、编辑的对话框和改写命令文件在 `custom`。

mod custom;

use std::path::{Path, PathBuf};

use gpui::{Action, App, Context, Div, Focusable, MouseButton, SharedString, Stateful, Window, prelude::*, px};
use runode_protocol::TaskSourceKind;
use runode_shared_types::color::Rgb;

pub(super) use custom::AddTaskDialog;

use super::{
    ToggleTasks,
    files::{MenuButton, MenuItem, group_item, labeled_item, menu_item},
    model::display_dir,
    project::{TOGGLE_HEIGHT, TOGGLE_WIDTH},
    titlebar::icon_toggle,
};
use crate::{
    assets::{PENCIL_ICON, PLAY_ICON, TRASH_ICON},
    host_client,
    ui::tooltip::tooltip,
    window::WindowView,
};

/// 宿主从这些文件里读项目命令（锁文件决定用哪个包管理器），它们变了就重列。
const TASK_FILES: [&str; 10] = [
    "tasks.json",
    "GNUmakefile",
    "makefile",
    "Makefile",
    "package.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "package-lock.json",
];

/// 命令菜单里的一条：开一个新标签跑 `command`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct RunTask {
    command: String,
}

/// 命令菜单里分组的标题：收起或展开 `file` 里列出的命令。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct ToggleTaskGroup {
    file: PathBuf,
}

/// 自己加的命令行尾的删除按钮：从 `file` 里删掉 `name`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct DeleteTask {
    file: PathBuf,
    name: String,
}

/// 自己加的命令行尾的编辑按钮：打开对话框改 `file` 里的 `name`，原来的命令行是 `command`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct EditTask {
    file: PathBuf,
    name: String,
    command: String,
}

/// 命令菜单最上面一项：打开添加命令的对话框。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct AddTask;

pub(in crate::window) fn is_task_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| TASK_FILES.iter().any(|file| name == *file))
}

impl WindowView {
    /// 终端目录换了或者命令文件变了时，在后台请宿主重列项目命令。
    pub(in crate::window) fn list_tasks(&mut self, cx: &mut Context<Self>) {
        let root = self.project_dir(cx);
        let workspace = &mut self.workspaces[self.active];
        let id = workspace.id;
        let project = &mut workspace.project;
        let fresh = !project.tasks_stale && project.tasks.as_ref().is_some_and(|(dir, _)| *dir == root);
        if project.tasks_listing || fresh {
            return;
        }
        project.tasks_listing = true;
        project.tasks_stale = false;
        let job = cx.background_spawn({
            let root = root.clone();
            async move { host_client::list_project_tasks(root) }
        });
        cx.spawn(async move |this, cx| {
            let listed = job.await;
            this.update(cx, |this, cx| {
                // 列的时候 workspace 可能已经关掉了。
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                let project = &mut workspace.project;
                project.tasks_listing = false;
                let sources = listed.unwrap_or_else(|err| {
                    tracing::debug!("failed to list the tasks in {}: {err:#}", root.display());
                    Vec::new()
                });
                project.tasks = Some((root, sources));
                cx.notify();
                // 列的期间终端目录换了或者文件又变了。
                if this.workspace().id == id {
                    this.list_tasks(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 命令菜单里点了一条：在当前标签右边开一个新标签，在列命令的目录里跑它。
    pub(super) fn run_task(&mut self, action: &RunTask, window: &mut Window, cx: &mut Context<Self>) {
        let Some((dir, _)) = &self.workspace().project.tasks else {
            return;
        };
        let dir = dir.clone();
        let Some(view) = self.spawn_terminal(Some(&dir), window, cx) else {
            return;
        };
        let command = action.command.clone();
        view.update(cx, |view, cx| view.run_command(command, cx));
        self.insert_tab(self.workspace().active + 1, view, window, cx);
    }

    /// 按快捷键打开命令菜单、选中第一条；开着时关掉。
    pub(super) fn toggle_tasks(&mut self, _: &ToggleTasks, window: &mut Window, cx: &mut Context<Self>) {
        if self.dropdown_open() {
            self.close_menu(window, cx);
        } else {
            self.open_tasks_menu(true, cx);
        }
    }

    /// 标题栏右上角、面板开关左边的命令按钮，菜单挂在它下面，开着时底色亮一些。
    pub(super) fn render_tasks_button(
        &self,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        icon_toggle("tasks", PLAY_ICON, 16., self.dropdown_open(), fg, bg)
            .w(px(TOGGLE_WIDTH))
            .h(px(TOGGLE_HEIGHT))
            .tooltip(tooltip(rust_i18n::t!("tooltip.tasks"), Some(&ToggleTasks), fg, bg))
            // 菜单开着时在捕获阶段就关掉、不再往下传：菜单自己的「点到外面就关」和下面再打开的都不跑。
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                if this.dropdown_open() {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_tasks_menu(false, cx);
                }),
            )
            .children(self.render_dropdown(fg, bg, window, cx))
    }

    /// 在命令按钮下面弹出命令菜单，各项见 `tasks_menu_items`。从键盘打开时选中第一组里的第一条命令，
    /// 那一组收着时往下找，一条都没有时选中添加命令。
    fn open_tasks_menu(&mut self, from_keyboard: bool, cx: &mut Context<Self>) {
        let items = self.tasks_menu_items(cx);
        let target = self.focus_handle(cx);
        // 第 2 项是第一组的标题，前面是添加命令和分隔线。
        self.open_dropdown(items, target, from_keyboard.then_some(2), cx);
        // shortcut: 面板收着时不监听目录，Makefile、package.json 改了要到下次打开菜单才看得到；要即时的话
        // 面板收着时也监听这几个文件。
        self.workspace_mut().project.tasks_stale = true;
        self.list_tasks(cx);
    }

    /// 命令菜单里点了分组的标题：收起或展开这一组，菜单开着不关。
    pub(super) fn toggle_task_group(&mut self, action: &ToggleTaskGroup, _: &mut Window, cx: &mut Context<Self>) {
        let folded = &mut self.workspace_mut().project.tasks_folded;
        if !folded.remove(&action.file) {
            folded.insert(action.file.clone());
        }
        let items = self.tasks_menu_items(cx);
        self.replace_menu_items(items, cx);
    }

    /// 命令菜单里自己加的命令行尾按了删除：先从菜单里拿掉，菜单开着不关，再在后台改文件，改完重列；
    /// 改不了时记日志，重列后它又回来。
    pub(super) fn delete_task(&mut self, action: &DeleteTask, _: &mut Window, cx: &mut Context<Self>) {
        let project = &mut self.workspace_mut().project;
        let sources = project.tasks.iter_mut().flat_map(|(_, sources)| sources.iter_mut());
        for source in sources.filter(|source| source.file == action.file) {
            source.tasks.retain(|task| task.name != action.name);
        }
        let items = self.tasks_menu_items(cx);
        self.replace_menu_items(items, cx);
        let id = self.workspace().id;
        let DeleteTask { file, name } = action.clone();
        let job = cx.background_spawn(async move { custom::write_task(&file, &name, None) });
        cx.spawn(async move |this, cx| {
            if let Err(err) = job.await {
                tracing::warn!("could not delete the task: {err}");
            }
            this.update(cx, |this, cx| {
                if let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) {
                    workspace.project.tasks_stale = true;
                }
                if this.workspace().id == id {
                    this.list_tasks(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 命令菜单的各项：最上面是添加命令，下面每个文件一组。组的标题是文件，在列命令的目录里的写相对
    /// 路径，在上层目录里的写 `~/…`，后面是命令条数，点了收起或展开，收起的组只留标题；命令名后面
    /// 淡淡地写它的说明，自己加的命令选中时行尾有编辑和删除按钮。
    fn tasks_menu_items(&self, cx: &App) -> Vec<Option<MenuItem>> {
        let mut items = vec![Some(menu_item("tasks.add", Box::new(AddTask), true, cx))];
        let project = &self.workspace().project;
        let listed = project.tasks.as_ref();
        for (dir, source) in
            listed.into_iter().flat_map(|(dir, sources)| sources.iter().map(move |source| (dir, source)))
        {
            if source.tasks.is_empty() {
                continue;
            }
            items.push(None);
            let label = match source.file.strip_prefix(dir) {
                Ok(rel) => rel.display().to_string(),
                Err(_) => display_dir(&source.file),
            };
            let folded = project.tasks_folded.contains(&source.file);
            let count = Some(source.tasks.len().to_string().into());
            let toggle = ToggleTaskGroup { file: source.file.clone() };
            items.push(Some(group_item(label, count, folded, Box::new(toggle))));
            if folded {
                continue;
            }
            let custom = source.kind == TaskSourceKind::Custom;
            items.extend(source.tasks.iter().map(|task| {
                let description = task.description.clone().filter(|text| *text != task.name);
                let action = RunTask { command: task.command.clone() };
                let item = labeled_item(task.name.clone(), description.map(SharedString::from), Some(Box::new(action)));
                if !custom {
                    return Some(item);
                }
                // 自己加的命令的说明就是写在文件里的命令行。
                let edit = EditTask {
                    file: source.file.clone(),
                    name: task.name.clone(),
                    command: task.description.clone().unwrap_or_default(),
                };
                let delete = DeleteTask { file: source.file.clone(), name: task.name.clone() };
                let edit_button = MenuButton {
                    icon: PENCIL_ICON,
                    tooltip: rust_i18n::t!("tasks.edit").into(),
                    keep_open: false,
                    cmd_key: Some("e"),
                };
                let delete_button = MenuButton {
                    icon: TRASH_ICON,
                    tooltip: rust_i18n::t!("tasks.delete").into(),
                    keep_open: true,
                    cmd_key: Some("backspace"),
                };
                Some(item.with_button(edit_button, Box::new(edit)).with_button(delete_button, Box::new(delete)))
            }));
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_task_files() {
        assert!(is_task_file(Path::new("/a/Makefile")));
        assert!(is_task_file(Path::new("/a/web/package.json")));
        assert!(is_task_file(Path::new("pnpm-lock.yaml")));
        assert!(!is_task_file(Path::new("/a/Makefile.bak")));
        assert!(!is_task_file(Path::new("/a/src/main.rs")));
    }
}
