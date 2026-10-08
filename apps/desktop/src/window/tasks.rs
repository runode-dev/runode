//! 标题栏右上角的项目命令：终端目录往上最近的自己加的命令、Makefile 的目标和 package.json 的 scripts，
//! 由宿主列出、拼好命令行（`ClientMsg::ListProjectTasks`），和手机会话卡片上的是同一份。右侧面板开关
//! 左边的按钮弹出按文件分组的菜单，点一条开一个新标签在列命令的目录里跑它；菜单最后一项添加自己的
//! 命令，对话框在 `add`。

mod add;

use std::path::Path;

use gpui::{Action, Context, Div, Focusable, MouseButton, SharedString, Stateful, Window, prelude::*, px};
use runode_shared_types::color::Rgb;

pub(super) use add::AddTaskDialog;

use super::{
    files::{labeled_item, menu_item},
    model::display_dir,
    project::{TOGGLE_HEIGHT, TOGGLE_WIDTH},
    titlebar::icon_toggle,
};
use crate::{assets::PLAY_ICON, host_client, ui::tooltip::tooltip, window::WindowView};

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

/// 命令菜单最后一项：打开添加命令的对话框。
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
            .tooltip(tooltip(rust_i18n::t!("tooltip.tasks"), None, fg, bg))
            // 菜单开着时在捕获阶段就关掉、不再往下传：菜单自己的「点到外面就关」和下面再打开的都不跑。
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if this.dropdown_open() {
                    cx.stop_propagation();
                    this.file_menu = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_tasks_menu(cx);
                }),
            )
            .children(self.render_dropdown(fg, bg, window, cx))
    }

    /// 在命令按钮下面弹出命令菜单：每个文件一段，段首灰着写文件，在列命令的目录里的写相对路径，
    /// 在上层目录里的写 `~/…`；命令名后面淡淡地写它的说明。最后是添加命令。
    fn open_tasks_menu(&mut self, cx: &mut Context<Self>) {
        let mut items = Vec::new();
        let listed = self.workspace().project.tasks.as_ref();
        for (dir, source) in
            listed.into_iter().flat_map(|(dir, sources)| sources.iter().map(move |source| (dir, source)))
        {
            if source.tasks.is_empty() {
                continue;
            }
            if !items.is_empty() {
                items.push(None);
            }
            let label = match source.file.strip_prefix(dir) {
                Ok(rel) => rel.display().to_string(),
                Err(_) => display_dir(&source.file),
            };
            items.push(Some(labeled_item(label, None, None)));
            items.extend(source.tasks.iter().map(|task| {
                let description = task.description.clone().filter(|text| *text != task.name);
                let action = RunTask { command: task.command.clone() };
                Some(labeled_item(task.name.clone(), description.map(SharedString::from), Some(Box::new(action))))
            }));
        }
        if !items.is_empty() {
            items.push(None);
        }
        items.push(Some(menu_item("tasks.add", Box::new(AddTask), true, cx)));
        let target = self.focus_handle(cx);
        self.open_dropdown(items, target, cx);
        // shortcut: 面板收着时不监听目录，Makefile、package.json 改了要到下次打开菜单才看得到；要即时的话
        // 面板收着时也监听这几个文件。
        self.workspace_mut().project.tasks_stale = true;
        self.list_tasks(cx);
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
