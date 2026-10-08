//! 「生成提交信息」对话框：选 agent、写 CLI 参数和提示词模板，下面照着写的实时列出实际要跑的整条命令。
//! 按「生成」用这些写一次；按「保存」存成这个仓库自己的预设或所有仓库的默认值，之后说明框里的 ✨ 按钮
//! 直接按它写。打开时填好存着的预设。

use std::path::{Path, PathBuf};

use gpui::{
    Action, ClickEvent, Context, Div, Entity, Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent,
    Stateful, Subscription, Window, div, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    DEFAULT_TEMPLATE, Recipe, VARIABLES,
    agents::{AGENTS, AgentSpec, CUSTOM_AGENT, agent},
    command_preview, save_recipe, saved_recipe, variable_preview,
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CLOSE_ICON, SPARKLE_ICON, TERMINAL_ICON},
    ui::{
        hsla,
        text_area::{TextArea, TextAreaEvent},
        text_field::{TextField, TextFieldEvent},
        tooltip::tooltip,
    },
    window::{
        TITLEBAR_HEIGHT, WindowView,
        agents::logo::{agent_logo, colored_logo, logo_path},
        files::check_item,
        project::RENAMED,
    },
};

/// 对话框里 agent 菜单的一项：换成 `AGENTS` 里第 `index` 个，为空时换成自己定义的命令。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(in crate::window) struct SelectCommitAgent {
    index: Option<usize>,
}

/// 保存以后在按钮旁边写的话。
enum Status {
    Saved,
    Failed(String),
}

pub(in crate::window) struct CommitMessageDialog {
    /// 给哪个仓库写。
    root: PathBuf,
    /// 选的 agent；为空时跑自己定义的命令。
    agent: Option<&'static AgentSpec>,
    args: Entity<TextField>,
    command: Entity<TextField>,
    template: Entity<TextArea>,
    /// 存成这个仓库自己的；否则存成所有仓库的默认值。
    repo_only: bool,
    status: Option<Status>,
    /// 正在后台写文件，写完之前不再写。
    saving: bool,
    _events: [Subscription; 3],
}

impl WindowView {
    /// 打开给根目录是 `root` 的仓库写提交说明的对话框，填好它存着的预设，焦点给 CLI 参数或者命令。
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
        let field = |text: &str, placeholder: &'static str, cx: &mut Context<Self>| {
            cx.new(|cx| TextField::editing(text.to_owned(), text.len(), cx).with_placeholder(placeholder))
        };
        let args = field(&recipe.args, "--model sonnet", cx);
        let command = field(&recipe.command, "MODEL=small my-agent --print {prompt}", cx);
        let template = cx.new(|cx| {
            let mut area = TextArea::new(cx);
            area.set_line_limits(6, 12, cx);
            area.set_text(recipe.template.clone(), cx);
            area
        });
        let on_field =
            |this: &mut Self, event: &TextFieldEvent, window: &mut Window, cx: &mut Context<Self>| match event {
                TextFieldEvent::Next => this.generate_from_dialog(window, cx),
                TextFieldEvent::Dismiss => this.close_commit_message_dialog(window, cx),
                // 改了参数，下面「将运行」的命令跟着变。
                TextFieldEvent::Changed(_) => cx.notify(),
                TextFieldEvent::Previous => {}
            };
        let on_args =
            cx.subscribe_in(&args, window, move |this, _, event, window, cx| on_field(this, event, window, cx));
        let on_command =
            cx.subscribe_in(&command, window, move |this, _, event, window, cx| on_field(this, event, window, cx));
        let on_template =
            cx.subscribe_in(&template, window, |this, _, event: &TextAreaEvent, window, cx| match event {
                TextAreaEvent::Submit => this.generate_from_dialog(window, cx),
                TextAreaEvent::Changed => {}
            });
        let agent = (recipe.agent != CUSTOM_AGENT).then(|| agent(&recipe.agent).unwrap_or(&AGENTS[0]));
        window.focus(&if agent.is_some() { &args } else { &command }.focus_handle(cx), cx);
        self.commit_message_dialog = Some(CommitMessageDialog {
            root,
            agent,
            args,
            command,
            template,
            repo_only,
            status,
            saving: false,
            _events: [on_args, on_command, on_template],
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
            agent: dialog.agent.map_or(CUSTOM_AGENT, |agent| agent.id).to_owned(),
            args: dialog.args.read(cx).query().trim().to_owned(),
            template: if template.is_empty() { DEFAULT_TEMPLATE.to_owned() } else { template },
            command: dialog.command.read(cx).query().trim().to_owned(),
        }
    }

    /// 按对话框里填的写一次，不存；对话框关掉。没有要提交的改动或者仓库正忙时什么也不做，原因写在
    /// 生成按钮的悬停提示里。
    fn generate_from_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.commit_message_dialog else {
            return;
        };
        if self.commit_message_blocker(&dialog.root).is_some() {
            return;
        }
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

    /// agent 菜单里选了一个，焦点给它下面要填的框：CLI 参数或者命令。
    pub(in crate::window) fn select_commit_agent(
        &mut self,
        action: &SelectCommitAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = &mut self.commit_message_dialog else {
            return;
        };
        dialog.agent = action.index.and_then(|index| AGENTS.get(index));
        dialog.status = None;
        let field = if dialog.agent.is_some() { &dialog.args } else { &dialog.command };
        window.focus(&field.focus_handle(cx), cx);
        cx.notify();
    }

    /// 在点的位置弹出 agent 菜单，选着的那个打勾；最后是自己定义的命令。
    fn open_commit_agent_menu(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(dialog) = &self.commit_message_dialog else {
            return;
        };
        let mut items: Vec<_> = AGENTS
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let icon = spec.kind.and_then(logo_path).or(spec.logo);
                let checked = dialog.agent.is_some_and(|agent| agent.id == spec.id);
                let action = Box::new(SelectCommitAgent { index: Some(index) });
                Some(check_item(spec.label.to_owned(), icon, checked, action))
            })
            .collect();
        let custom = rust_i18n::t!("git.ai_message.custom").into_owned();
        let action = Box::new(SelectCommitAgent { index: None });
        items.extend([None, Some(check_item(custom, Some(TERMINAL_ICON), dialog.agent.is_none(), action))]);
        let target = self.focus_handle(cx);
        self.open_menu(event.position, items, target, cx);
    }

    fn commit_message_dialog_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close_commit_message_dialog(window, cx);
        }
    }

    /// 现在能不能生成：不能时是原因的翻译键（没有要提交的改动，或者仓库正忙）。
    fn commit_message_blocker(&self, root: &Path) -> Option<&'static str> {
        let project = &self.workspace().project;
        let repo = project.git.as_ref().and_then(|git| git.iter().find(|repo| repo.root == root));
        if repo.is_none_or(|repo| repo.is_clean()) {
            Some("git.ai_message.nothing")
        } else if project.git_panel.busy(root).is_some() {
            Some("git.ai_message.busy")
        } else {
            None
        }
    }

    pub(in crate::window) fn render_commit_message_dialog(
        &self,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let dialog = self.commit_message_dialog.as_ref()?;
        let (fg_rgb, bg_rgb) = (fg, bg);
        let panel_bg = hsla(bg.mix(fg, 0.03));
        let field_bg = hsla(bg);
        let hover_bg = hsla(bg.mix(fg, 0.08));
        let selected_bg = hsla(bg.mix(fg, 0.16));
        let fg = hsla(fg);
        let border = fg.opacity(0.12);
        let accent = hsla(RENAMED);
        let mono = self.font_family(cx);
        let section = |key: &str| {
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(fg.opacity(0.6))
                .child(rust_i18n::t!(key).into_owned())
        };
        let note = |text: String| div().text_size(px(11.)).text_color(fg.opacity(0.5)).child(text);
        let field_box = || {
            div()
                .px(px(8.))
                .rounded(px(6.))
                .bg(field_bg)
                .border_1()
                .border_color(fg.opacity(0.2))
                .font_family(mono.clone())
        };

        let close = div()
            .id("commit-message-close")
            .flex_none()
            .size(px(24.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |close| close.bg(hover_bg))
            .child(svg().path(CLOSE_ICON).size(px(12.)).text_color(fg.opacity(0.6)))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_commit_message_dialog(window, cx)));
        let header = div()
            .flex()
            .items_start()
            .gap(px(8.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .child(rust_i18n::t!("git.ai_message.title").into_owned()),
                    )
                    .child(note(rust_i18n::t!("git.ai_message.subtitle").into_owned())),
            )
            .child(close);

        let (logo, name) = match dialog.agent {
            Some(spec) => {
                let logo = spec.kind.and_then(|kind| agent_logo(kind, px(14.), fg));
                (logo.or_else(|| spec.logo.map(|path| colored_logo(path, px(14.)))), spec.label.to_owned())
            }
            None => (
                Some(svg().path(TERMINAL_ICON).flex_none().size(px(14.)).text_color(fg).into_any_element()),
                rust_i18n::t!("git.ai_message.custom").into_owned(),
            ),
        };
        let agent_button = div()
            .id("commit-message-agent")
            .flex_none()
            .h(px(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .bg(field_bg)
            .border_1()
            .border_color(fg.opacity(0.2))
            .cursor_pointer()
            .hover(move |button| button.bg(hover_bg))
            .children(logo)
            .child(name)
            .child(svg().path(CHEVRON_DOWN_ICON).size(px(12.)).text_color(fg.opacity(0.6)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_commit_agent_menu(event, cx);
                }),
            );

        // CLI 参数（自己定义的命令时是整条命令），下面是照现在填的实际要跑的整条命令。
        let (input_label, input, hint) = match dialog.agent {
            Some(_) => ("git.ai_message.args", &dialog.args, None),
            None => (
                "git.ai_message.command",
                &dialog.command,
                Some(note(rust_i18n::t!("git.ai_message.command_hint").into_owned())),
            ),
        };
        let preview = match command_preview(&Self::dialog_recipe(dialog, cx)) {
            Ok((line, stdin)) => {
                let how = if stdin { "git.ai_message.via_stdin" } else { "git.ai_message.via_argv" };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .bg(hsla(bg_rgb.mix(fg_rgb, 0.06)))
                            .font_family(mono.clone())
                            .text_size(px(11.))
                            .text_color(fg.opacity(0.85))
                            .child(format!("$ {line}")),
                    )
                    .child(note(rust_i18n::t!(how).into_owned()))
            }
            Err(err) => div().text_size(px(11.)).text_color(gpui::red()).child(err),
        };
        let command_section = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(section(input_label))
            .child(field_box().h(px(28.)).flex().items_center().child(input.clone()))
            .children(hint)
            .child(section("git.ai_message.will_run"))
            .child(preview);

        // 点变量插到模板的光标处；鼠标停在上面时说明它是什么、展开后什么样。
        let variables = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .text_size(px(11.))
            .child(
                div()
                    .mr(px(2.))
                    .text_color(fg.opacity(0.5))
                    .child(rust_i18n::t!("git.ai_message.variables").into_owned()),
            )
            .children(VARIABLES.iter().map(|&name| {
                div()
                    .id(name)
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(hsla(bg_rgb.mix(fg_rgb, 0.06)))
                    .font_family(mono.clone())
                    .text_color(fg.opacity(0.85))
                    .cursor_pointer()
                    .hover(move |chip| chip.bg(selected_bg))
                    .tooltip(tooltip(variable_preview(name), None, fg_rgb, bg_rgb))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(dialog) = &this.commit_message_dialog {
                            let template = dialog.template.clone();
                            template.update(cx, |area, cx| area.insert(&format!("{{{name}}}"), cx));
                            window.focus(&template.focus_handle(cx), cx);
                        }
                    }))
                    .child(format!("{{{name}}}"))
            }));
        let template_section = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(section("git.ai_message.template"))
            .child(field_box().py(px(6.)).child(dialog.template.clone()))
            .child(variables);

        // 底栏左边存成预设：存到哪（两段按钮）和保存按钮；右边是生成。
        let scope = |id: &'static str, repo_only: bool, key: &str| {
            div()
                .id(id)
                .px(px(8.))
                .py(px(2.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(dialog.repo_only == repo_only, |pill| pill.bg(selected_bg))
                .when(dialog.repo_only != repo_only, |pill| {
                    pill.text_color(fg.opacity(0.6)).hover(move |pill| pill.bg(hover_bg))
                })
                .child(rust_i18n::t!(key).into_owned())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(dialog) = &mut this.commit_message_dialog {
                        dialog.repo_only = repo_only;
                        dialog.status = None;
                        cx.notify();
                    }
                }))
        };
        let save = div()
            .id("commit-message-save")
            .flex_none()
            .px(px(10.))
            .py(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(fg.opacity(0.2))
            .cursor_pointer()
            .hover(move |button| button.bg(hover_bg))
            .when(dialog.saving, |button| button.opacity(0.5))
            .child(rust_i18n::t!("git.ai_message.save").into_owned())
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save_dialog_recipe(window, cx)));
        let status = match &dialog.status {
            Some(Status::Saved) => Some(note(rust_i18n::t!("git.ai_message.saved").into_owned())),
            Some(Status::Failed(err)) => Some(div().text_size(px(11.)).text_color(gpui::red()).child(err.clone())),
            None => None,
        };
        let blocker = self.commit_message_blocker(&dialog.root);
        let on_accent = gpui::white();
        let generate = div()
            .id("commit-message-generate")
            .flex_none()
            .px(px(12.))
            .py(px(5.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .bg(accent)
            .text_color(on_accent)
            .child(svg().path(SPARKLE_ICON).size(px(13.)).text_color(on_accent))
            .child(rust_i18n::t!("git.ai_message.generate").into_owned())
            .child(div().text_color(on_accent.opacity(0.6)).child("⌘↩"))
            .map(|button| match blocker {
                Some(reason) => button.opacity(0.5).tooltip(tooltip(rust_i18n::t!(reason), None, fg_rgb, bg_rgb)),
                None => button
                    .cursor_pointer()
                    .hover(move |button| button.bg(accent.opacity(0.85)))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.generate_from_dialog(window, cx))),
            });
        let footer = div()
            .pt(px(12.))
            .border_t_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .text_color(fg.opacity(0.6))
                            .child(rust_i18n::t!("git.ai_message.scope").into_owned()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .p(px(2.))
                            .gap(px(2.))
                            .rounded(px(6.))
                            .bg(field_bg)
                            .child(scope("commit-message-repo", true, "git.ai_message.scope_repo"))
                            .child(scope("commit-message-all", false, "git.ai_message.scope_all")),
                    )
                    .child(save)
                    .child(div().flex_1())
                    .child(generate),
            )
            .children(status);

        let panel = div()
            .id("commit-message-dialog")
            .on_key_down(cx.listener(Self::commit_message_dialog_key))
            .w(px(560.))
            .max_w_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .rounded(px(12.))
            .bg(panel_bg)
            .border_1()
            .border_color(border)
            .shadow_lg()
            .text_size(px(12.))
            .text_color(fg)
            // 点在对话框里不算点到外面。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(section("git.ai_message.agent"))
                    .child(div().flex().child(agent_button)),
            )
            .child(command_section)
            .child(template_section)
            .child(footer);
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消。
        Some(
            div()
                .id("commit-message-backdrop")
                .absolute()
                .size_full()
                .pt(px(TITLEBAR_HEIGHT + 32.))
                .px(px(16.))
                .flex()
                .justify_center()
                .items_start()
                .bg(Hsla::black().opacity(0.25))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_commit_message_dialog(window, cx)),
                )
                .child(panel),
        )
    }
}
