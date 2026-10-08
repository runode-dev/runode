//! 「生成提交信息」对话框：选 agent、写 CLI 参数和提示词模板，按「生成」用这些写一次；按「保存默认值」
//! 存成这个仓库自己的预设或所有仓库的默认值，之后说明框里的 ✨ 按钮直接按它写。打开时填好存着的预设。

use std::path::PathBuf;

use gpui::{
    Action, ClickEvent, Context, Div, Entity, Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent,
    Stateful, Subscription, Window, div, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    DEFAULT_TEMPLATE, Recipe, VARIABLES,
    agents::{AGENTS, AgentSpec, agent},
    save_recipe, saved_recipe,
};
use crate::{
    assets::CHEVRON_DOWN_ICON,
    ui::{
        hsla,
        text_area::{TextArea, TextAreaEvent},
        text_field::{TextField, TextFieldEvent},
    },
    window::{
        TITLEBAR_HEIGHT, WindowView,
        agents::logo::{agent_logo, logo_path},
        files::check_item,
    },
};

/// 对话框里 agent 菜单的一项：换成 `AGENTS` 里第 `index` 个。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(in crate::window) struct SelectCommitAgent {
    index: usize,
}

/// 保存以后在按钮旁边写的话。
enum Status {
    Saved,
    Failed(String),
}

pub(in crate::window) struct CommitMessageDialog {
    /// 给哪个仓库写。
    root: PathBuf,
    agent: &'static AgentSpec,
    args: Entity<TextField>,
    template: Entity<TextArea>,
    /// 存成这个仓库自己的；否则存成所有仓库的默认值。
    repo_only: bool,
    status: Option<Status>,
    /// 正在后台写文件，写完之前不再写。
    saving: bool,
    _events: [Subscription; 2],
}

impl WindowView {
    /// 打开给根目录是 `root` 的仓库写提交说明的对话框，填好它存着的预设，焦点给 CLI 参数。
    pub(in crate::window) fn open_commit_message_dialog(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (saved, status) = match saved_recipe(&root) {
            Ok(saved) => (saved, None),
            Err(err) => (None, Some(Status::Failed(err))),
        };
        let repo_only = saved.as_ref().is_some_and(|(_, own)| *own);
        let recipe = saved.map(|(recipe, _)| recipe).unwrap_or_default();
        let args = cx.new(|cx| {
            let end = recipe.args.len();
            TextField::editing(recipe.args.clone(), end, cx).with_placeholder("--model sonnet")
        });
        let template = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_line_limits(6, 12, cx);
            area.set_text(recipe.template.clone(), cx);
            area
        });
        let on_args = cx.subscribe_in(&args, window, |this, _, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Next => this.generate_from_dialog(window, cx),
            TextFieldEvent::Dismiss => this.close_commit_message_dialog(window, cx),
            TextFieldEvent::Changed(_) | TextFieldEvent::Previous => {}
        });
        let on_template = cx.subscribe_in(&template, window, |this, _, event: &TextAreaEvent, window, cx| {
            if let TextAreaEvent::Submit = event {
                this.generate_from_dialog(window, cx);
            }
        });
        window.focus(&args.focus_handle(cx), cx);
        self.commit_message_dialog = Some(CommitMessageDialog {
            root,
            agent: agent(&recipe.agent).unwrap_or(&AGENTS[0]),
            args,
            template,
            repo_only,
            status,
            saving: false,
            _events: [on_args, on_template],
        });
        cx.notify();
    }

    fn close_commit_message_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.commit_message_dialog.take().is_some() {
            window.focus(&self.git_focus.clone(), cx);
            cx.notify();
        }
    }

    /// 对话框里填的预设。
    fn dialog_recipe(dialog: &CommitMessageDialog, cx: &Context<Self>) -> Recipe {
        let template = dialog.template.read(cx).text().trim().to_owned();
        Recipe {
            agent: dialog.agent.id.to_owned(),
            args: dialog.args.read(cx).query().trim().to_owned(),
            template: if template.is_empty() { DEFAULT_TEMPLATE.to_owned() } else { template },
        }
    }

    /// 按对话框里填的写一次，不存；对话框关掉。
    fn generate_from_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.commit_message_dialog else {
            return;
        };
        let (root, recipe) = (dialog.root.clone(), Self::dialog_recipe(dialog, cx));
        self.close_commit_message_dialog(window, cx);
        self.write_commit_message(&root, recipe, window, cx);
    }

    /// 在后台把对话框里填的存起来，存好了在按钮旁边说一声，对话框留着。
    fn save_dialog_recipe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.commit_message_dialog else {
            return;
        };
        if dialog.saving {
            return;
        }
        dialog.saving = true;
        let recipe = Self::dialog_recipe(dialog, cx);
        let root = dialog.repo_only.then(|| dialog.root.clone());
        let job = cx.background_spawn(async move { save_recipe(root.as_deref(), recipe) });
        cx.spawn_in(window, async move |this, cx| {
            let saved = job.await;
            this.update(cx, |this, cx| {
                if let Some(dialog) = &mut this.commit_message_dialog {
                    dialog.saving = false;
                    dialog.status = Some(match saved {
                        Ok(()) => Status::Saved,
                        Err(err) => Status::Failed(err),
                    });
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// agent 菜单里选了一个。
    pub(in crate::window) fn select_commit_agent(
        &mut self,
        action: &SelectCommitAgent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let (Some(dialog), Some(spec)) = (&mut self.commit_message_dialog, AGENTS.get(action.index)) {
            dialog.agent = spec;
            dialog.status = None;
            cx.notify();
        }
    }

    /// 在点的位置弹出 agent 菜单，选着的那个打勾。
    fn open_commit_agent_menu(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(dialog) = &self.commit_message_dialog else {
            return;
        };
        let items = AGENTS
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let icon = spec.kind.and_then(logo_path);
                let checked = spec.id == dialog.agent.id;
                Some(check_item(spec.label.to_owned(), icon, checked, Box::new(SelectCommitAgent { index })))
            })
            .collect();
        let target = self.focus_handle(cx);
        self.open_menu(event.position, items, target, cx);
    }

    fn commit_message_dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close_commit_message_dialog(window, cx);
        }
    }

    pub(in crate::window) fn render_commit_message_dialog(
        &self,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let dialog = self.commit_message_dialog.as_ref()?;
        let panel_bg = hsla(bg.mix(fg, 0.03));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let primary_bg = hsla(bg.mix(fg, 0.16));
        let primary_hover_bg = hsla(bg.mix(fg, 0.22));
        let fg = hsla(fg);
        let border = fg.opacity(0.12);
        let label = |key: &str| div().text_color(fg.opacity(0.7)).child(rust_i18n::t!(key).into_owned());
        let boxed = || div().px(px(8.)).rounded(px(6.)).bg(hsla(bg)).border_1().border_color(fg.opacity(0.2));
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
        let spec = dialog.agent;
        let agent_button = boxed()
            .id("commit-message-agent")
            .flex_none()
            .h(px(28.))
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .hover(move |button| button.bg(hover_bg))
            .children(spec.kind.and_then(|kind| agent_logo(kind, px(14.), fg)))
            .child(spec.label)
            .child(svg().path(CHEVRON_DOWN_ICON).size(px(12.)).text_color(fg.opacity(0.6)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_commit_agent_menu(event, cx);
                }),
            );
        let variables = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .text_size(px(11.))
            .text_color(fg.opacity(0.55))
            .child(rust_i18n::t!("git.ai_message.variables").into_owned())
            .children(VARIABLES.iter().map(|var| div().text_color(fg.opacity(0.8)).child(*var)));
        // 只存到这个仓库还是存成所有仓库的默认值：两段按钮，选中的那段底色亮一些。
        let scope = |id: &'static str, repo_only: bool, key: &str| {
            div()
                .id(id)
                .px(px(10.))
                .py(px(3.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(dialog.repo_only == repo_only, |pill| pill.bg(primary_bg))
                .when(dialog.repo_only != repo_only, |pill| pill.hover(move |pill| pill.bg(hover_bg)))
                .child(rust_i18n::t!(key).into_owned())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(dialog) = &mut this.commit_message_dialog {
                        dialog.repo_only = repo_only;
                        dialog.status = None;
                        cx.notify();
                    }
                }))
        };
        let status = dialog.status.as_ref().map(|status| match status {
            Status::Saved => {
                div().text_color(fg.opacity(0.6)).child(rust_i18n::t!("git.ai_message.saved").into_owned())
            }
            Status::Failed(err) => div().text_color(gpui::red()).child(err.clone()),
        });
        let save = button("commit-message-save", rust_i18n::t!("git.ai_message.save").into_owned(), panel_bg, hover_bg)
            .border_1()
            .border_color(border)
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_dialog_recipe(window, cx)));
        let generate = button(
            "commit-message-generate",
            rust_i18n::t!("git.ai_message.generate").into_owned(),
            primary_bg,
            primary_hover_bg,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.generate_from_dialog(window, cx)));
        let panel = div()
            .id("commit-message-dialog")
            .on_key_down(cx.listener(Self::commit_message_dialog_key))
            .w(px(520.))
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
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::BOLD)
                            .child(rust_i18n::t!("git.ai_message.title").into_owned()),
                    )
                    .child(
                        div().text_color(fg.opacity(0.55)).child(rust_i18n::t!("git.ai_message.subtitle").into_owned()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label("git.ai_message.agent"))
                    .child(div().flex().child(agent_button)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label("git.ai_message.args"))
                    .child(boxed().h(px(28.)).flex().items_center().child(dialog.args.clone())),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(label("git.ai_message.template"))
                    .child(boxed().py(px(6.)).child(dialog.template.clone()))
                    .child(variables),
            )
            .child(
                div().flex().items_center().gap(px(8.)).child(label("git.ai_message.scope")).child(
                    div()
                        .flex()
                        .p(px(2.))
                        .gap(px(2.))
                        .rounded(px(6.))
                        .bg(hsla(bg))
                        .child(scope("commit-message-repo", true, "git.ai_message.scope_repo"))
                        .child(scope("commit-message-all", false, "git.ai_message.scope_all")),
                ),
            )
            .child(
                div()
                    .pt(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().children(status))
                    .child(save)
                    .child(generate),
            );
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消。
        Some(
            div()
                .id("commit-message-backdrop")
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
                    cx.listener(|this, _, window, cx| this.close_commit_message_dialog(window, cx)),
                )
                .child(panel),
        )
    }
}
