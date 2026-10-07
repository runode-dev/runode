//! 新建 workspace 的对话框，三种来源：浏览一个已有的文件夹（也可以从访达把文件夹拖到窗口上）、从 URL
//! 克隆一个 Git 仓库、新建一个空文件夹。克隆和新建都先用系统的存储面板选位置和名字，建好这个目录再在
//! 它上面开 workspace；克隆是在新 workspace 的第一个终端里跑 `git clone`，进度、要密码和出错都在终端
//! 里看得见。名字一律按目录取，之后可以重命名。

use std::path::PathBuf;

use gpui::{
    ClickEvent, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    MouseButton, PathPromptOptions, SharedString, Stateful, Subscription, Window, div, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{NewWorkspace, TITLEBAR_HEIGHT, WindowView, files::shell_quote, model::home_dir};
use crate::{
    assets::{CLOSE_ICON, FOLDER_OPEN_ICON, GLOBE_ICON, PLUS_ICON},
    ui::{
        hsla,
        text_field::{TextField, TextFieldEvent},
    },
};

pub(super) struct NewWorkspaceDialog {
    focus: FocusHandle,
    /// 点了「从 URL 克隆」后展开的地址框。
    url: Option<(Entity<TextField>, Subscription)>,
}

impl WindowView {
    /// 打开新建 workspace 的对话框；已经开着时把焦点给它。
    pub(super) fn new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        let dialog =
            self.new_workspace.get_or_insert_with(|| NewWorkspaceDialog { focus: cx.focus_handle(), url: None });
        match &dialog.url {
            Some((url, _)) => window.focus(&url.focus_handle(cx), cx),
            None => window.focus(&dialog.focus, cx),
        }
        cx.notify();
    }

    fn close_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_workspace.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    fn new_workspace_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.close_new_workspace(window, cx);
        }
    }

    /// 在 `dir` 上开 workspace，关掉对话框。
    fn open_new_workspace(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.new_workspace = None;
        self.open_workspace(dir, None, window, cx);
    }

    /// 弹出系统的目录选择框，选好就开 workspace；取消时对话框还在。
    fn browse_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!("workspace.choose").into_owned().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open_new_workspace(dir, window, cx)).ok();
        })
        .detach();
    }

    /// 展开克隆的地址框，焦点给它；已经展开时只给焦点。
    fn show_clone_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        let (url, _) = dialog.url.get_or_insert_with(|| {
            let url = cx.new(|cx| {
                TextField::editing(String::new(), 0, cx)
                    .with_placeholder(rust_i18n::t!("workspace.clone_url").into_owned())
            });
            // 回车是克隆，Esc 是关掉整个对话框。
            let events = cx.subscribe_in(&url, window, |this, _, event: &TextFieldEvent, window, cx| match event {
                TextFieldEvent::Next => this.clone_new_workspace(window, cx),
                TextFieldEvent::Dismiss => this.close_new_workspace(window, cx),
                TextFieldEvent::Changed(_) | TextFieldEvent::Previous => {}
            });
            (url, events)
        });
        window.focus(&url.focus_handle(cx), cx);
        cx.notify();
    }

    /// 按地址框里的地址克隆：先选放到哪里，名字默认是仓库名。
    fn clone_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((url, _)) = self.new_workspace.as_ref().and_then(|dialog| dialog.url.as_ref()) else {
            return;
        };
        let url = url.read(cx).query().trim().to_owned();
        if url.is_empty() {
            return;
        }
        let command = format!("git clone {} .", shell_quote(&url));
        self.new_workspace_dir(repo_name(&url), Some(command), window, cx);
    }

    /// 弹出系统的存储面板选位置和名字，建好这个目录后在它上面开 workspace；`command` 给了就在新
    /// workspace 的第一个终端里跑它。取消时对话框还在。
    fn new_workspace_dir(
        &mut self,
        suggested: Option<String>,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let path = cx.prompt_for_new_path(&home, suggested.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(dir))) = path.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = std::fs::create_dir_all(&dir) {
                    tracing::warn!("could not create {}: {err}", dir.display());
                    return;
                }
                let Some(command) = command else {
                    this.open_new_workspace(dir, window, cx);
                    return;
                };
                this.new_workspace = None;
                if let Some(view) = this.spawn_terminal(Some(&dir), window, cx) {
                    view.update(cx, |view, cx| view.run_command(command, cx));
                    this.insert_workspace(this.active + 1, dir, None, view, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_new_workspace(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.new_workspace.as_ref()?;
        let panel_bg = hsla(bg.mix(fg, 0.03));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let primary_bg = hsla(bg.mix(fg, 0.16));
        let primary_hover_bg = hsla(bg.mix(fg, 0.22));
        let card_bg = hsla(bg);
        let fg = hsla(fg);
        let border = fg.opacity(0.12);
        let card =
            || div().flex().flex_col().rounded(px(10.)).overflow_hidden().bg(card_bg).border_1().border_color(border);
        // 一个来源一行：左边图标，右边标题和一行说明，整行可点。
        let option = |id: &'static str, icon: &'static str, title: SharedString, hint: SharedString| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(14.))
                .px(px(16.))
                .py(px(12.))
                .cursor_pointer()
                .hover(move |row| row.bg(hover_bg))
                .child(svg().flex_none().path(icon).size(px(18.)).text_color(fg.opacity(0.75)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(fg.opacity(0.55)).child(hint)),
                )
        };
        let browse = option(
            "new-workspace-browse",
            FOLDER_OPEN_ICON,
            rust_i18n::t!("workspace.browse").into_owned().into(),
            rust_i18n::t!("workspace.browse_hint").into_owned().into(),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.browse_new_workspace(window, cx)));
        let clone = option(
            "new-workspace-clone",
            GLOBE_ICON,
            rust_i18n::t!("workspace.clone").into_owned().into(),
            rust_i18n::t!("workspace.clone_hint").into_owned().into(),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_clone_url(window, cx)));
        // 地址框缩进到和标题对齐，右边是克隆按钮。
        let clone_url = dialog.url.as_ref().map(|(url, _)| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(48.))
                .pr(px(16.))
                .pb(px(12.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(panel_bg)
                        .border_1()
                        .border_color(fg.opacity(0.2))
                        .child(url.clone()),
                )
                .child(
                    div()
                        .id("new-workspace-clone-go")
                        .flex_none()
                        .px(px(12.))
                        .py(px(5.))
                        .rounded(px(6.))
                        .bg(primary_bg)
                        .hover(move |button| button.bg(primary_hover_bg))
                        .cursor_pointer()
                        .child(rust_i18n::t!("workspace.clone_to").into_owned())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clone_new_workspace(window, cx))),
                )
        });
        let create = option(
            "new-workspace-create",
            PLUS_ICON,
            rust_i18n::t!("workspace.create").into_owned().into(),
            rust_i18n::t!("workspace.create_hint").into_owned().into(),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_workspace_dir(None, None, window, cx)));
        let close = div()
            .id("new-workspace-close")
            .flex_none()
            .p(px(4.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(move |button| button.bg(hover_bg))
            .child(svg().path(CLOSE_ICON).size(px(16.)).text_color(fg.opacity(0.7)))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_new_workspace(window, cx)));
        let panel = div()
            .id("new-workspace-dialog")
            .track_focus(&dialog.focus)
            .on_key_down(cx.listener(Self::new_workspace_key))
            .w(px(480.))
            .max_w_full()
            .p(px(24.))
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
                    .pb(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::BOLD)
                            .child(rust_i18n::t!("workspace.new").into_owned()),
                    )
                    .child(close),
            )
            .child(card().child(browse))
            .child(div().pt(px(4.)).text_color(fg.opacity(0.6)).child(rust_i18n::t!("workspace.other").into_owned()))
            .child(
                card().child(div().child(clone).children(clone_url)).child(div().h(px(1.)).bg(border)).child(create),
            );
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消；从访达拖来的文件夹放在窗口哪里都直接开。
        Some(
            div()
                .id("new-workspace-backdrop")
                .absolute()
                .size_full()
                .pt(px(TITLEBAR_HEIGHT + 48.))
                .px(px(16.))
                .flex()
                .justify_center()
                .items_start()
                .bg(Hsla::black().opacity(0.15))
                .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(Hsla::black().opacity(0.25)))
                .on_drop(cx.listener(|this, dropped: &ExternalPaths, window, cx| {
                    if let Some(dir) = dropped.paths().iter().find(|path| path.is_dir()) {
                        this.open_new_workspace(dir.clone(), window, cx);
                    }
                }))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_new_workspace(window, cx)),
                )
                .child(panel),
        )
    }
}

/// 仓库地址里的仓库名，作克隆目录的默认名：最后一段去掉 `.git`。
fn repo_name(url: &str) -> Option<String> {
    let last = url.trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::repo_name;

    #[test]
    fn names_the_clone_after_the_repo() {
        assert_eq!(repo_name("https://github.com/foo/bar.git").as_deref(), Some("bar"));
        assert_eq!(repo_name("https://github.com/foo/bar/").as_deref(), Some("bar"));
        assert_eq!(repo_name("git@github.com:foo/bar.git").as_deref(), Some("bar"));
        assert_eq!(repo_name("https://github.com/").as_deref(), Some("github.com"));
        assert_eq!(repo_name(".git"), None);
    }
}
