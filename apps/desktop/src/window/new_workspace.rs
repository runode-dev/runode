//! 新建 workspace 的对话框：选目录、填名字，确定后在当前 workspace 下面建一个。名字空着时按目录取名。

use std::path::PathBuf;

use gpui::{
    Context, Div, Entity, Focusable, FontWeight, Hsla, MouseButton, PathPromptOptions, SharedString, Stateful,
    Subscription, Window, div, prelude::*, px,
};
use runode_shared_types::color::Rgb;

use super::{
    NewWorkspace, TITLEBAR_HEIGHT, WindowView,
    model::{display_dir, workspace_name},
};
use crate::ui::{
    hsla,
    text_field::{TextField, TextFieldEvent},
};

pub(super) struct NewWorkspaceDialog {
    name: Entity<TextField>,
    /// 选好的目录，还没选时为空。
    dir: Option<PathBuf>,
    _events: Subscription,
}

impl WindowView {
    /// 打开新建 workspace 的对话框；已经开着时把焦点给它的名字框。
    pub(super) fn new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &self.new_workspace {
            window.focus(&dialog.name.focus_handle(cx), cx);
            return;
        }
        let name = cx.new(|cx| {
            TextField::editing(String::new(), 0, cx).with_placeholder(rust_i18n::t!("workspace.name_hint").into_owned())
        });
        // 名字框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是创建和取消。
        let events = cx.subscribe_in(&name, window, |this, _, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Next => this.confirm_new_workspace(window, cx),
            TextFieldEvent::Dismiss => this.close_new_workspace(window, cx),
            TextFieldEvent::Changed(_) | TextFieldEvent::Previous => {}
        });
        window.focus(&name.focus_handle(cx), cx);
        self.new_workspace = Some(NewWorkspaceDialog { name, dir: None, _events: events });
        cx.notify();
    }

    fn close_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_workspace.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 弹出系统的目录选择框，选好后记进对话框，名字框的提示换成按目录取的名字。
    fn choose_new_workspace_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            this.update_in(cx, |this, window, cx| {
                let Some(dialog) = &mut this.new_workspace else {
                    return;
                };
                let placeholder = workspace_name(&dir);
                dialog.name.update(cx, |name, cx| name.set_placeholder(placeholder, cx));
                dialog.dir = Some(dir);
                window.focus(&dialog.name.focus_handle(cx), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 还没选目录时先去选；选好了就关掉对话框，建 workspace。
    fn confirm_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_workspace else {
            return;
        };
        let Some(dir) = dialog.dir.clone() else {
            self.choose_new_workspace_dir(window, cx);
            return;
        };
        let name = Some(dialog.name.read(cx).query().trim().to_owned()).filter(|name| !name.is_empty());
        self.new_workspace = None;
        self.open_workspace(dir, name, window, cx);
    }

    pub(super) fn render_new_workspace(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.new_workspace.as_ref()?;
        let panel_bg = hsla(bg.mix(fg, 0.05));
        let hover_bg = hsla(bg.mix(fg, 0.09));
        let primary_bg = hsla(bg.mix(fg, 0.16));
        let primary_hover_bg = hsla(bg.mix(fg, 0.22));
        let fg = hsla(fg);
        let row = |label: SharedString| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(48.)).flex_none().text_color(fg.opacity(0.6)).child(label))
        };
        // 名字框和目录框同一个样子，两行左右对齐。
        let field = || {
            div()
                .flex_1()
                .min_w_0()
                .h(px(26.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .bg(hsla(bg))
                .border_1()
                .border_color(fg.opacity(0.2))
        };
        let name = field().child(dialog.name.clone());
        // 目录整格可点，点了弹系统的目录选择框；选好后照样可以点着换一个。
        let (dir_text, dir_color) = match &dialog.dir {
            Some(dir) => (display_dir(dir), fg),
            None => (rust_i18n::t!("workspace.no_dir").into_owned(), fg.opacity(0.45)),
        };
        let dir = field()
            .id("new-workspace-dir")
            .cursor_pointer()
            .hover(move |field| field.bg(hover_bg))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_start()
                    .text_color(dir_color)
                    .child(dir_text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.choose_new_workspace_dir(window, cx);
                }),
            );
        let cancel = button("new-workspace-cancel", rust_i18n::t!("workspace.cancel").into_owned(), fg, hover_bg)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_new_workspace(window, cx);
                }),
            );
        let create =
            button("new-workspace-create", rust_i18n::t!("workspace.create").into_owned(), fg, primary_hover_bg)
                .bg(primary_bg)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.confirm_new_workspace(window, cx);
                    }),
                );
        let panel = div()
            .id("new-workspace-dialog")
            .w(px(420.))
            .max_w_full()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(8.))
            .bg(panel_bg)
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .text_size(px(12.))
            .text_color(fg)
            // 点在对话框里不算点到外面。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .pb(px(4.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(rust_i18n::t!("workspace.new").into_owned()),
            )
            // 目录必填、名字默认取自目录，所以目录在前。
            .child(row(rust_i18n::t!("workspace.dir").into_owned().into()).child(dir))
            .child(row(rust_i18n::t!("workspace.name").into_owned().into()).child(name))
            .child(div().pt(px(6.)).flex().justify_end().gap(px(8.)).child(cancel).child(create));
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消。
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
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_new_workspace(window, cx)),
                )
                .child(panel),
        )
    }
}

fn button(id: &'static str, label: String, fg: Hsla, hover_bg: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(12.))
        .py(px(4.))
        .rounded(px(5.))
        .border_1()
        .border_color(fg.opacity(0.15))
        .hover(move |button| button.bg(hover_bg))
        .child(label)
}
