//! 文件树底部的项目命令：根目录往上最近的 Makefile 的目标和 package.json 的 scripts，由宿主列出、
//! 拼好命令行（`ClientMsg::ListProjectTasks`），和手机会话卡片上的是同一份。按文件分组，分组可以
//! 收起；双击一条或点它行尾的运行按钮，开一个新标签在根目录里跑它。

use std::path::Path;

use gpui::{Axis, ClickEvent, Context, Div, FontWeight, MouseButton, Window, div, img, prelude::*, px, relative, svg};
use runode_shared_types::color::Rgb;

use super::TOOLBAR_HEIGHT;
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, PLAY_ICON},
    host_client,
    ui::{file_icons::file_icon, hsla, scrollbar::scrollbar, tooltip::tooltip},
    window::{WindowView, divider_color, model::display_dir},
};

/// 宿主从这些文件里读项目命令（锁文件决定用哪个包管理器），它们变了就重列。
const TASK_FILES: [&str; 9] = [
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

/// 命令那一行的悬停分组：悬停在行上时显示行尾的运行按钮。
const TASK_ROW: &str = "task-row";

pub(in crate::window) fn is_task_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| TASK_FILES.iter().any(|file| name == *file))
}

impl WindowView {
    /// 文件树显示着、根目录换了或者命令文件变了时，在后台请宿主重列项目命令。
    pub(in crate::window) fn list_tasks(&mut self, cx: &mut Context<Self>) {
        if !self.files_shown {
            return;
        }
        let workspace = &mut self.workspaces[self.active];
        let id = workspace.id;
        let project = &mut workspace.project;
        let Some(root) = project.root.clone() else {
            return;
        };
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
                // 列的期间根目录换了或者文件又变了。
                if this.workspace().id == id {
                    this.list_tasks(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 在当前标签右边开一个新标签，在列命令的目录里跑 `command`。
    fn run_task(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((dir, _)) = &self.workspace().project.tasks else {
            return;
        };
        let dir = dir.clone();
        let Some(view) = self.spawn_terminal(Some(&dir), window, cx) else {
            return;
        };
        view.update(cx, |view, cx| view.run_command(command, cx));
        self.insert_tab(self.workspace().active + 1, view, window, cx);
    }

    /// 文件树底部的项目命令：一条可以收起的标题，下面按文件分组列出命令，最多占面板四成高，
    /// 多了在里面滚动。一条命令都没有时整段不显示。
    pub(super) fn render_tasks(&self, font_size: f32, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Div> {
        let project = &self.workspace().project;
        let (dir, sources) = project.tasks.as_ref()?;
        if sources.iter().all(|source| source.tasks.is_empty()) {
            return None;
        }
        let collapsed = self.tasks_collapsed;
        let dim = hsla(fg).opacity(0.5);
        let faint = hsla(fg).opacity(0.4);
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let run_hover_bg = hsla(bg.mix(fg, 0.14));
        let header = div()
            .id("tasks-header")
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .hover(move |header| header.bg(hover_bg))
            .child(
                svg()
                    .path(if collapsed { CHEVRON_RIGHT_ICON } else { CHEVRON_DOWN_ICON })
                    .size(px(12.))
                    .text_color(dim),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hsla(fg).opacity(0.7))
                    .child(rust_i18n::t!("files.tasks").into_owned()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.tasks_collapsed = !this.tasks_collapsed;
                this.save(cx);
                cx.notify();
            }));
        let mut rows = Vec::new();
        for (group, source) in sources.iter().enumerate().filter(|(_, source)| !source.tasks.is_empty()) {
            let name = source.file.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
            // 在根目录里的写相对路径，在上层目录里的写 `~/…`。
            let label = match source.file.strip_prefix(dir) {
                Ok(rel) => rel.display().to_string(),
                Err(_) => display_dir(&source.file),
            };
            let folded = project.tasks_folded.contains(&source.file);
            let file = source.file.clone();
            // 分组的标题像文件树里的目录：点一下收起、展开这个文件的命令，行尾是命令条数。
            rows.push(
                Self::file_row_shell(("task-group", group), 0, font_size, fg)
                    .gap(px(4.))
                    .cursor_pointer()
                    .hover(move |row| row.bg(hover_bg))
                    .child(
                        svg()
                            .path(if folded { CHEVRON_RIGHT_ICON } else { CHEVRON_DOWN_ICON })
                            .flex_none()
                            .size(px(font_size))
                            .text_color(dim),
                    )
                    .child(img(file_icon(&name)).flex_none().size(px(font_size + 2.)))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().flex_none().pl(px(6.)).text_color(dim).child(source.tasks.len().to_string()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let folded = &mut this.workspace_mut().project.tasks_folded;
                        if !folded.remove(&file) {
                            folded.insert(file.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if folded {
                continue;
            }
            for (ix, task) in source.tasks.iter().enumerate() {
                let command = task.command.clone();
                let description = task.description.clone().filter(|text| *text != task.name);
                // 单击不跑，免得误点了 `install`、`release` 这类：双击整行，或者点悬停时行尾出来的运行按钮。
                let run = div()
                    .id(("task-run", group * 1000 + ix))
                    .flex_none()
                    .ml(px(4.))
                    .p(px(2.))
                    .rounded(px(4.))
                    .opacity(0.)
                    .group_hover(TASK_ROW, |button| button.opacity(1.))
                    .hover(move |button| button.bg(run_hover_bg))
                    .tooltip(tooltip(rust_i18n::t!("files.run_task", command = command), None, fg, bg))
                    .child(svg().path(PLAY_ICON).size(px(font_size)).text_color(hsla(fg).opacity(0.8)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener({
                        let command = command.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.run_task(command.clone(), window, cx);
                        }
                    }));
                rows.push(
                    Self::file_row_shell(("task", group * 1000 + ix), 1, font_size, fg)
                        .group(TASK_ROW)
                        .gap(px(6.))
                        .hover(move |row| row.bg(hover_bg))
                        .child(div().flex_none().max_w(relative(0.6)).truncate().child(task.name.clone()))
                        .child(div().flex_1().min_w_0().truncate().text_color(faint).children(description))
                        .child(run)
                        // 按下时不往外传，免得文件树把选中的行取消掉。
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if event.click_count() >= 2 {
                                this.run_task(command.clone(), window, cx);
                            }
                        }))
                        .into_any_element(),
                );
            }
        }
        // 外层只有高度上限、没有定高，`flex_1` 分不到空间会被压成零：两层都按内容撑开，超过上限时
        // 收缩、在里面滚动。
        let body = div()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("tasks")
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&project.tasks_scroll)
                    .px(px(4.))
                    .pb(px(4.))
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .child(scrollbar("tasks-scroll", project.tasks_scroll.clone(), Axis::Vertical, hsla(fg)));
        Some(
            div()
                .flex_none()
                .max_h(relative(0.4))
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(divider_color(hsla(fg)))
                .child(header)
                .when(!collapsed, |section| section.child(body)),
        )
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
