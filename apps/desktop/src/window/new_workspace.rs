//! 新建 workspace 的对话框：上面选在哪台机器上开（本机、连过的 SSH 主机和配对的电脑，或者新加一台），下面三种来源：
//! 打开一个已有的文件夹、从 URL 克隆一个 Git 仓库、新建一个空文件夹。本机上用系统的目录选择框和存储面板
//! 选位置和名字，也可以从访达把文件夹拖到窗口上；远端上在对话框里一级级浏览那台机器的目录，名字填在框里。
//! 克隆和新建都先建好目录再在它上面开 workspace；克隆是在新 workspace 的第一个终端里跑 `git clone`，进度、
//! 要密码和出错都在终端里看得见。名字一律按目录取，之后可以重命名。

use crate::host_client::hosts::HostId;
use crate::ui::dialog::{backdrop, is_dismiss_key};
use crate::ui::theme::ActiveTheme as _;
use std::path::PathBuf;

use gpui::{
    App, Axis, Context, Div, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    MouseButton, PathPromptOptions, Role, ScrollHandle, SharedString, Stateful, Subscription, Window, div, prelude::*,
    px, svg,
};

use super::{NewWorkspace, WindowView, files::Quoting, model::home_dir};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::{
    ui::icons::{CLOSE_ICON, FOLDER_OPEN_ICON, GLOBE_ICON, PLUS_ICON},
    ui::{
        a11y::{Press, StaticText as _},
        card::Card,
        dialog::Dialog,
        font_size::FontSize,
        input::{Input, InputEvent, InputState},
        scrollbar::scrollbar,
    },
};

mod paired;
mod ssh;

/// 设置页里的「添加 SSH 主机」「Runode 配对」（`pair`）：收起最前面窗口的设置页，打开新建工作区对话框，
/// 展开对应的一栏。
pub(crate) fn add_remote_host(pair: bool, cx: &mut App) {
    cx.defer(move |cx| {
        let Some(handle) = super::remote::front_window(cx) else { return };
        let added = handle.update(cx, |view, window, cx| {
            view.close_settings(window, cx);
            view.new_workspace(&NewWorkspace, window, cx);
            if pair {
                view.add_paired_machine(&paired::AddPairedMachine, window, cx);
            } else {
                view.add_ssh_host(&ssh::AddSshHost, window, cx);
            }
        });
        if let Err(err) = added {
            tracing::warn!("failed to open the new workspace dialog: {err:#}");
        }
    });
}

/// 对话框里的一种来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Browse,
    Clone,
    Create,
}

pub(super) struct NewWorkspaceDialog {
    focus: FocusHandle,
    /// 在哪台机器上开。
    pub(super) host: HostId,
    /// 展开着的来源：本机上只有克隆展开（填地址），远端上三种都展开、在里面浏览目录。
    pub(super) source: Option<Source>,
    /// 克隆的地址框。
    url: Option<(Entity<InputState>, Subscription)>,
    /// 远端上新建、克隆出的目录名的框。
    pub(super) name: Option<(Entity<InputState>, Subscription)>,
    /// 远端的目录浏览：选着的主机连上了、展开了一种来源才有。
    browse: Option<ssh::Browse>,
    /// 点了「添加 SSH 主机」后展开的一栏。
    adding: Option<ssh::AddHost>,
    /// 点了「Runode 配对」后展开的一栏。
    pairing: Option<paired::Pairing>,
    /// 在远端建目录没建成的原因。
    pub(super) error: Option<String>,
    /// 对话框比窗口高时里面的滚动。
    scroll: ScrollHandle,
}

impl NewWorkspaceDialog {
    pub(super) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// 地址框里的地址，去掉两头的空白。
    pub(super) fn url_text(&self, cx: &App) -> String {
        self.url.as_ref().map(|(url, _)| url.read(cx).query().trim().to_owned()).unwrap_or_default()
    }

    /// 名字框里的名字，去掉两头的空白。
    pub(super) fn name_text(&self, cx: &App) -> String {
        self.name.as_ref().map(|(name, _)| name.read(cx).query().trim().to_owned()).unwrap_or_default()
    }
}

/// 在 shell 里把 `url` 克隆进当前目录的命令，`quoting` 是那个 shell 加引号的写法。`--` 把地址和选项隔开：
/// 粘贴进来的文本以 `-` 开头时不能被 git 当成选项。
pub(super) fn clone_command(url: &str, quoting: Quoting) -> String {
    format!("git clone -- {} .", quoting.quote(url))
}

impl WindowView {
    /// 打开新建 workspace 的对话框；已经开着时把焦点给它。
    pub(super) fn new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        // 建好的 workspace 要露出来，不能落在新建任务那一页后面。
        if !self.leave_todo_page(window, cx) {
            return;
        }
        let dialog = self.new_workspace.get_or_insert_with(|| NewWorkspaceDialog {
            focus: cx.focus_handle(),
            host: HostId::Local,
            source: None,
            url: None,
            name: None,
            browse: None,
            adding: None,
            pairing: None,
            error: None,
            scroll: ScrollHandle::new(),
        });
        let field = match (&dialog.adding, &dialog.url, &dialog.name) {
            _ if dialog.pairing.is_some() => dialog.pairing.as_ref().map(|pairing| pairing.focus_handle(cx)),
            (Some(adding), _, _) => Some(adding.focus_handle(cx)),
            (None, Some((url, _)), _) if dialog.source == Some(Source::Clone) => Some(url.focus_handle(cx)),
            (None, _, Some((name, _))) if dialog.source == Some(Source::Create) => Some(name.focus_handle(cx)),
            _ => None,
        };
        window.focus(&field.unwrap_or_else(|| dialog.focus.clone()), cx);
        cx.notify();
    }

    pub(super) fn close_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_workspace.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    fn new_workspace_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if is_dismiss_key(&event.keystroke) {
            cx.stop_propagation();
            self.close_new_workspace(window, cx);
        }
    }

    /// 在本机的 `dir` 上开 workspace，关掉对话框。
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
            let Ok(result) = paths.await else {
                return;
            };
            match result {
                Ok(Some(paths)) => {
                    let Some(dir) = paths.into_iter().next() else {
                        return;
                    };
                    this.update_in(cx, |this, window, cx| this.open_new_workspace(dir, window, cx)).ok();
                }
                Ok(None) => {}
                Err(err) => {
                    this.update(cx, |this, cx| this.report_picker_failed(&err, cx)).ok();
                }
            }
        })
        .detach();
    }

    /// 系统的选择面板打不开时在对话框里说明，给出把文件夹拖到窗口上这条路。
    fn report_picker_failed(&mut self, err: &anyhow::Error, cx: &mut Context<Self>) {
        tracing::warn!("could not open the system file picker: {err:#}");
        let key = if cfg!(target_os = "linux") { "workspace.picker_failed_linux" } else { "workspace.picker_failed" };
        self.report_new_workspace_error(rust_i18n::t!(key).into_owned(), cx);
    }

    fn report_new_workspace_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.new_workspace {
            dialog.error = Some(message);
            cx.notify();
        }
    }

    /// 点了一种来源：本机上打开、新建交给系统的面板；克隆和远端上的各种在对话框里展开，焦点给要填的框。
    fn pick_source(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_workspace else {
            return;
        };
        match (dialog.host.is_local(), source) {
            (true, Source::Browse) => {
                self.clear_local_error(cx);
                self.browse_new_workspace(window, cx)
            }
            (true, Source::Create) => {
                self.clear_local_error(cx);
                self.new_workspace_dir(None, None, window, cx)
            }
            _ => self.show_source(source, window, cx),
        }
    }

    fn clear_local_error(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.new_workspace
            && dialog.error.take().is_some()
        {
            cx.notify();
        }
    }

    /// 展开 `source`，建好它要的框，焦点给第一个要填的框；远端的接着开始浏览目录。
    pub(super) fn show_source(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        let local = dialog.host.is_local();
        // 名字框新建和克隆共用：换了来源就清空，克隆时空着才用得上仓库名。
        if dialog.source != Some(source) {
            dialog.error = None;
            if let Some((name, _)) = &dialog.name {
                name.update(cx, |name, cx| name.set_query(String::new(), cx));
            }
        }
        dialog.source = Some(source);
        if source == Source::Clone && dialog.url.is_none() {
            let url = cx.new(|cx| {
                InputState::editing(String::new(), 0, cx).with_label(rust_i18n::t!("workspace.clone").into_owned())
            });
            // 回车是克隆（远端上名字框也填好了才克隆），Esc 是关掉整个对话框。
            let events = cx.subscribe_in(&url, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Next => this.clone_new_workspace(window, cx),
                InputEvent::Dismiss => this.close_new_workspace(window, cx),
                InputEvent::Changed(_) | InputEvent::Previous => {}
            });
            dialog.url = Some((url, events));
        }
        if let Some((url, _)) = &dialog.url {
            let key = if local { "workspace.clone_url" } else { "workspace.clone_url_remote" };
            url.update(cx, |url, _| url.set_placeholder(rust_i18n::t!(key).into_owned()));
        }
        if !local && source != Source::Browse && dialog.name.is_none() {
            let name = cx.new(|cx| {
                InputState::new(String::new(), cx).with_label(rust_i18n::t!("workspace.remote_name").into_owned())
            });
            let events = cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Next => this.create_remote_workspace(window, cx),
                InputEvent::Dismiss => this.close_new_workspace(window, cx),
                InputEvent::Changed(_) | InputEvent::Previous => {}
            });
            dialog.name = Some((name, events));
        }
        if let Some((name, _)) = &dialog.name {
            let key = if source == Source::Clone { "workspace.remote_clone_name" } else { "workspace.remote_name" };
            name.update(cx, |name, _| name.set_placeholder(rust_i18n::t!(key).into_owned()));
        }
        let field = match source {
            Source::Clone => dialog.url.as_ref().map(|(url, _)| url.focus_handle(cx)),
            Source::Create => dialog.name.as_ref().map(|(name, _)| name.focus_handle(cx)),
            Source::Browse => None,
        };
        window.focus(&field.unwrap_or_else(|| dialog.focus.clone()), cx);
        if !local {
            self.start_browse(cx);
        }
        cx.notify();
    }

    /// 按地址框里的地址克隆：本机上先选放到哪里，名字默认是仓库名；远端上克隆到浏览到的目录下。
    fn clone_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_workspace else {
            return;
        };
        if !dialog.host.is_local() {
            self.create_remote_workspace(window, cx);
            return;
        }
        let url = dialog.url_text(cx);
        if url.is_empty() {
            return;
        }
        // 新开的终端跑的是 `default_shell`，照它的写法加引号。
        let quoting = Quoting::of_program(&runode_terminal::pty::default_shell());
        self.new_workspace_dir(repo_name(&url), Some(clone_command(&url, quoting)), window, cx);
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
            let dir = match path.await {
                Ok(Ok(Some(dir))) => dir,
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| this.report_picker_failed(&err, cx)).ok();
                    return;
                }
                Ok(Ok(None)) | Err(_) => return,
            };
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = std::fs::create_dir_all(&dir) {
                    tracing::warn!("could not create {}: {err}", dir.display());
                    let message = rust_i18n::t!(
                        "workspace.create_failed",
                        dir = dir.display().to_string(),
                        error = err.to_string()
                    );
                    this.report_new_workspace_error(message.into_owned(), cx);
                    return;
                }
                let Some(command) = command else {
                    this.open_new_workspace(dir, window, cx);
                    return;
                };
                this.new_workspace = None;
                if let Some(view) = this.spawn_terminal_on(&HostId::Local, Some(&dir), None, window, cx) {
                    view.update(cx, |view, cx| view.run_command(command, cx));
                    this.insert_workspace(this.active + 1, dir, None, view, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_new_workspace(&self, window: &Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.new_workspace.as_ref()?;
        let local = dialog.host.is_local();
        let panel_bg = cx.theme().mix(0.03);
        let hover_bg = cx.theme().accent;
        let fg = cx.theme().foreground;
        let border = fg.opacity(0.12);
        // 内容比窗口高时整个对话框滚动，各块不压扁。
        let card = || Card::new().flex_none().flex().flex_col().overflow_hidden();
        // 一个来源一行：左边图标，右边标题和一行说明，整行可点。
        let option = |source: Source, icon: &'static str, title: SharedString, hint: SharedString| {
            let id = match source {
                Source::Browse => "new-workspace-browse",
                Source::Clone => "new-workspace-clone",
                Source::Create => "new-workspace-create",
            };
            // 本机上打开、新建弹的是系统的面板，不在这里展开。
            let expands = !local || source == Source::Clone;
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(title.clone())
                .aria_description(hint.clone())
                .when(expands, |row| row.aria_expanded(dialog.source == Some(source)))
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
                        .child(div().text_size(FontSize::Large).font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(fg.opacity(0.55)).child(hint)),
                )
        };
        let press = |source: Source| {
            move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| this.pick_source(source, window, cx)
        };
        let expanded = |source: Source| dialog.source == Some(source);
        let browse_hint = if local { "workspace.browse_hint" } else { "workspace.browse_hint_remote" };
        let browse = option(
            Source::Browse,
            FOLDER_OPEN_ICON,
            rust_i18n::t!("workspace.browse").into_owned().into(),
            rust_i18n::t!(browse_hint).into_owned().into(),
        )
        .on_press(cx, press(Source::Browse));
        let browse_body =
            (!local && expanded(Source::Browse)).then(|| self.render_remote_source(Source::Browse, cx)).flatten();
        let clone = option(
            Source::Clone,
            GLOBE_ICON,
            rust_i18n::t!("workspace.clone").into_owned().into(),
            rust_i18n::t!("workspace.clone_hint").into_owned().into(),
        )
        .on_press(cx, press(Source::Clone));
        // 地址框缩进到和标题对齐；本机上右边是克隆按钮，远端上的按钮在名字框旁边。
        let clone_url = dialog.url.as_ref().filter(|_| expanded(Source::Clone)).map(|(url, _)| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(48.))
                .pr(px(16.))
                .pb(px(12.))
                .child(Input::new(url).flex_1().min_w_0().bg(panel_bg))
                .when(local, |row| {
                    row.child(
                        Button::new("new-workspace-clone-go", rust_i18n::t!("workspace.clone_to"))
                            .variant(ButtonVariant::Secondary)
                            .h_auto()
                            .py(px(5.))
                            .cursor_pointer()
                            .on_press(cx, |this, window, cx| this.clone_new_workspace(window, cx)),
                    )
                })
        });
        let clone_body =
            (!local && expanded(Source::Clone)).then(|| self.render_remote_source(Source::Clone, cx)).flatten();
        let create = option(
            Source::Create,
            PLUS_ICON,
            rust_i18n::t!("workspace.create").into_owned().into(),
            rust_i18n::t!("workspace.create_hint").into_owned().into(),
        )
        .on_press(cx, press(Source::Create));
        let create_body =
            (!local && expanded(Source::Create)).then(|| self.render_remote_source(Source::Create, cx)).flatten();
        let host_row = self.render_host_picker(window, cx);
        // 远端的错误写在展开的那一块里。
        let local_error = dialog.error.clone().filter(|_| local).map(|error| {
            div()
                .id("new-workspace-error")
                .aria_static_text(error.clone())
                .flex_none()
                .text_color(cx.theme().destructive)
                .child(error)
        });
        let add_host = dialog.adding.as_ref().map(|adding| card().child(self.render_add_host(adding, cx)));
        let pairing = dialog.pairing.as_ref().map(|pairing| card().child(self.render_pairing(pairing, cx)));
        let close = Button::new("new-workspace-close", rust_i18n::t!("workspace.close"))
            .icon(CLOSE_ICON)
            .icon_size(16.)
            .with_size(ButtonSize::Icon)
            .rounded(px(6.))
            .cursor_pointer()
            .on_press(cx, |this, window, cx| this.close_new_workspace(window, cx));
        let title = rust_i18n::t!("workspace.new").into_owned();
        let other = rust_i18n::t!("workspace.other").into_owned();
        // 比窗口高时对话框里这一层滚动，滚动条叠在对话框上，和它一样大。
        let body = div()
            .id("new-workspace-body")
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&dialog.scroll)
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .pb(px(4.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("new-workspace-title")
                            .role(Role::Heading)
                            .aria_level(1)
                            .aria_label(title.clone())
                            .text_size(FontSize::Heading)
                            .font_weight(FontWeight::BOLD)
                            .child(title.clone()),
                    )
                    .child(close),
            )
            .children(host_row)
            .children(add_host)
            .children(pairing)
            .children(local_error)
            .child(card().child(div().child(browse).children(browse_body)))
            .child(
                div()
                    .id("new-workspace-other")
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(other.clone())
                    .flex_none()
                    .pt(px(4.))
                    .text_color(fg.opacity(0.6))
                    .child(other),
            )
            .child(
                card()
                    .child(div().child(clone).children(clone_url).children(clone_body))
                    .child(div().h(px(1.)).bg(border))
                    .child(div().child(create).children(create_body)),
            );
        let panel = Dialog::new("new-workspace-dialog", title)
            .track_focus(&dialog.focus)
            .on_key_down(cx.listener(Self::new_workspace_key))
            // 主机菜单的各项派发到对话框上：窗口上的动作在对话框开着时不办。
            .on_action(cx.listener(Self::pick_workspace_host))
            .on_action(cx.listener(Self::add_ssh_host))
            .on_action(cx.listener(Self::add_paired_machine))
            .w(px(480.))
            .max_w_full()
            .max_h_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(body)
            .child(scrollbar("new-workspace-scrollbar", dialog.scroll.clone(), Axis::Vertical));
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消；在本机上开时，从访达拖来的文件夹放在窗口哪里
        // 都直接开。
        Some(
            backdrop("new-workspace-backdrop")
                .when(local, |backdrop| {
                    backdrop.drag_over::<ExternalPaths>(|style, _, _, _| style.bg(Hsla::black().opacity(0.25))).on_drop(
                        cx.listener(|_, dropped: &ExternalPaths, window, cx| {
                            let paths = dropped.paths().to_vec();
                            cx.spawn_in(window, async move |this, cx| {
                                let Some(dir) = super::model::dropped_dirs(paths).await.into_iter().next() else {
                                    return;
                                };
                                this.update_in(cx, |this, window, cx| {
                                    // 看的时候对话框可能已经关了、换到了远端主机上。
                                    if this.new_workspace.as_ref().is_some_and(|dialog| dialog.host.is_local()) {
                                        this.open_new_workspace(dir, window, cx);
                                    }
                                })
                                .ok();
                            })
                            .detach();
                        }),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_new_workspace(window, cx)),
                )
                .child(panel),
        )
    }
}

/// 仓库地址里的仓库名，作克隆目录的默认名：最后一段去掉 `.git`。
pub(super) fn repo_name(url: &str) -> Option<String> {
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
