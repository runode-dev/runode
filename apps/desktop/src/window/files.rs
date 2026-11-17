//! 把右侧面板的文件树接到窗口上。文件树本身在 `file_tree`：一个窗口一个 `FileTree`，各 workspace 的树
//! 都在它里面。这里每次对它做什么之前告诉它当前是哪个 workspace、交给它 workspace 的名字、目录和单击打不打开
//! 文件，画面板外框和顶上的标签；后台读到的目录和 git 状态由 `refresh_project` 经 `FileTree::apply_scan` 交给它。
//! 它要窗口办的事（开预览、把路径打进终端、开新标签、预览跟着改名和删除、重读、焦点交回终端、开关变了要存档）
//! 经 `FileTreeEvent` 交回来。
//!
//! 把路径打进终端（`insert_paths`）和拖到终端上（`drop_paths_on_pane`）也在这里，预览栏、Git 面板和分屏都用；
//! 终端在远端主机上时先把文件传过去。

use std::path::{Path, PathBuf};

use crate::file_tree::{FileTree, FileTreeEvent, Input};
use futures::FutureExt as _;
use gpui::{
    Context, Div, Entity, Focusable, FontWeight, PromptButton, PromptLevel, Stateful, Window, div, prelude::*, px,
};
use runode_config::PreviewClick;
use runode_types::shell::Quoting;

// 菜单部件在 `runode_ui::menu`，窗口里各处从这里引。
pub(super) use crate::ui::menu::{MenuButton, MenuItem};

use super::{WindowView, project::SidePanel};
use crate::{
    config::AppConfig,
    git_view,
    host_client::{
        hosts::HostId,
        ssh::{self, upload::Upload},
    },
    project_host::ProjectHost,
    terminal_view::TerminalView,
    ui::{
        a11y::{Press as _, StaticText as _},
        button::{Button, ButtonVariant},
        font,
        font_size::{self, FontSize, scaled},
        panel::{added_label, removed_label},
        theme::ActiveTheme as _,
    },
};

/// Claude Code、Codex 会把粘贴进来的图片路径换成图片附件，这几种扩展名两边都认。
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|image| ext.eq_ignore_ascii_case(image)))
}

impl WindowView {
    /// 当前 workspace 交给文件树的名字、目录和单击打不打开文件。
    fn file_tree_input(&self, cx: &Context<Self>) -> Input {
        let workspace = self.workspace();
        Input {
            name: workspace.name.clone(),
            dir: workspace.dir.clone(),
            single_click: cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single,
            project: ProjectHost::of(&workspace.host),
        }
    }

    /// 对当前 workspace 的文件树做 `f`：先交给它 `file_tree_input`，告诉它是哪个 workspace（换了时它换上那个
    /// workspace 的搜索词重搜）。会让文件树重画，不能在画的时候调。
    pub(super) fn with_file_tree<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut FileTree, &mut Context<FileTree>) -> R,
    ) -> R {
        let input = self.file_tree_input(cx);
        let id = self.workspace().id;
        self.file_tree.update(cx, |tree, cx| {
            tree.set_input(input);
            tree.switch_workspace(id, cx);
            f(tree, cx)
        })
    }

    /// 右侧面板的文件树页：外框和顶上的标签在这里画，里面是文件树，主仓库里有改动时底下是改动的摘要和
    /// 切到 Git 页的按钮。
    pub(super) fn render_files_panel(&mut self, width: f32, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let input = self.file_tree_input(cx);
        self.file_tree.update(cx, |tree, _| tree.set_input(input));
        let font_size = FontSize::Medium.points() * font_size::scale(cx);
        let summary = self.render_change_summary(cx);
        self.side_panel_shell("files-panel", width, window, cx)
            .text_size(px(font_size))
            .child(div().flex_1().min_h_0().child(self.file_tree.clone()))
            .children(summary)
    }

    /// 文件树页底下的一块：几个文件有改动、一共增删几行，下面是打开 Review 页看改动、提交的按钮。不在仓库里
    /// 或没有改动时为空。
    fn render_change_summary(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (files, added, removed) = self.workspace().project.git.as_deref().map(git_view::change_summary)?;
        if files == 0 {
            return None;
        }
        let theme = cx.theme();
        let (border, subtle, success, destructive) =
            (theme.border, theme.subtle_foreground, theme.success, theme.destructive);
        let mono = font::mono(cx);
        let text = rust_i18n::t!("panel.changed_files", count = files).into_owned();
        let stat = |label: Div, color| {
            label.text_color(color).when_some(mono.clone(), |label, family| label.font_family(family))
        };
        let line = div()
            .id("change-summary")
            .aria_static_text(format!("{text} +{added} -{removed}"))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(FontSize::Body)
            .text_color(subtle)
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(stat(added_label(added), success))
            .child(stat(removed_label(removed), destructive));
        let review = Button::new("review-commit", rust_i18n::t!("panel.review_commit").into_owned())
            .variant(ButtonVariant::Secondary)
            .w_full()
            .h(scaled(30.))
            .rounded(px(8.))
            .font_weight(FontWeight::MEDIUM)
            .on_press(cx, |this, window, cx| this.show_review(window, cx));
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(10.))
                .border_t_1()
                .border_color(border)
                .child(line)
                .child(review),
        )
    }

    /// 文件树要窗口办的事。
    pub(super) fn on_file_tree_event(
        &mut self,
        _: &Entity<FileTree>,
        event: &FileTreeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FileTreeEvent::Open { path, line: None, pin, .. } => self.open_preview(path, *pin, true, cx),
            FileTreeEvent::Open { path, line: Some(line), hit, pin } => {
                self.open_preview_at(path, *line, hit.clone(), *pin, true, cx)
            }
            FileTreeEvent::InsertPath(path) => self.insert_paths(vec![path.clone()], None, window, cx),
            FileTreeEvent::OpenTerminal(dir) => {
                if let Some(view) = self.spawn_terminal(Some(dir), window, cx) {
                    self.insert_tab(self.workspace().active + 1, view, window, cx);
                }
            }
            FileTreeEvent::Moved { from, to, host } => self.move_previews(from, to, host.as_deref(), cx),
            FileTreeEvent::Removed { path, host } => self.close_previews_under(path, host.as_deref(), window, cx),
            FileTreeEvent::Refresh => self.refresh_project(cx),
            FileTreeEvent::FocusTerminal => self.focus_terminal(window, cx),
            FileTreeEvent::OptionsChanged => self.save(cx),
        }
    }

    /// 把从文件树或访达拖来的 `paths` 放到终端 `pane` 上：切到那个终端，把路径一个个打进去。
    /// 一个路径粘贴一次，因为 Codex 只在一次粘贴里正好是一个路径时才把它当图片。`local` 的是这台电脑上的
    /// 文件，终端在远端主机上时先传过去，打的是远端的路径。
    pub(super) fn drop_paths_on_pane(
        &mut self,
        pane: gpui::EntityId,
        paths: &[PathBuf],
        local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_active_tab(pane, window, cx);
        if local
            && let Some(view) = self.focused_view().cloned()
            && !view.read(cx).host().is_local()
        {
            self.upload_to_view(view, Upload::Files(paths.to_vec()), window, cx);
            return;
        }
        self.insert_paths(paths.to_vec(), None, window, cx);
    }

    /// 远端终端里粘贴的图片传到那台主机上，再把它在远端的路径打进这个终端。
    pub(super) fn paste_image(
        &mut self,
        view: Entity<TerminalView>,
        image: &gpui::Image,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let secs =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let name = format!("clipboard-{secs}.{}", image.format.extension());
        self.upload_to_view(view, Upload::Bytes { name, bytes: image.bytes.clone() }, window, cx);
    }

    /// 远端终端里粘贴的本机文件传到那台主机上，再把它们在远端的路径打进这个终端。
    pub(super) fn paste_files(
        &mut self,
        view: Entity<TerminalView>,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.upload_to_view(view, Upload::Files(paths.to_vec()), window, cx);
    }

    /// 把本机的 `path` 照 `view` 前台 shell 的写法加好引号，当作粘贴打进去。
    pub(super) fn paste_path(
        &mut self,
        view: &Entity<TerminalView>,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = Quoting::of_session(view.read(cx).meta()).quote(&path.display().to_string());
        view.update(cx, |view, cx| view.paste_text(text, window, cx));
    }

    /// 远端终端里粘贴的屏幕内容存成文件传到那台主机上，再把它在远端的路径打进这个终端。
    pub(super) fn paste_screen(
        &mut self,
        view: Entity<TerminalView>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let millis =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis());
        let name = format!("screen-{millis}.txt");
        self.upload_to_view(view, Upload::Bytes { name, bytes: text.as_bytes().to_vec() }, window, cx);
    }

    /// 第 `wi` 个工作区里在 `host` 上的终端点开了那台机器上的 `path`：工作区就在那台机器上时，文件在预览栏里
    /// 开，目录在文件树里找到；工作区在别的机器上时那边的面板读不到它，不管。
    pub(super) fn open_remote_path(
        &mut self,
        wi: usize,
        host: &HostId,
        path: &Path,
        dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if wi != self.active || self.workspaces[wi].host != *host {
            tracing::info!("not opening {} from {host:?}: the workspace is elsewhere", path.display());
            return;
        }
        if dir {
            self.set_panel(Some(SidePanel::Files), window, cx);
            self.with_file_tree(cx, |tree, cx| tree.reveal_file(path, cx));
        } else {
            self.open_preview(path, false, false, cx);
        }
    }

    /// 把本机的文件或一份内容传到 `view` 所在的远端主机，再把它们在远端的绝对路径打进 `view`，后面各带一个
    /// 空格；传不上去时弹框说原因。
    fn upload_to_view(
        &mut self,
        view: Entity<TerminalView>,
        upload: Upload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = view.read(cx).host().clone();
        let job = match &id {
            HostId::Local => return,
            HostId::Ssh(name) => {
                let Some(host) = ssh::remote(name) else {
                    return;
                };
                cx.background_spawn(async move { ssh::upload::upload(&host, upload) })
            }
            // 配对的电脑没有 ssh，经读写项目的连接传。
            HostId::Paired(_) => {
                let project = ProjectHost::of(&id);
                cx.background_spawn(async move { crate::project_host::upload::upload(&project, upload) })
            }
        };
        let quoting = Quoting::of_session(view.read(cx).meta());
        let view = view.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let uploaded = job.await;
            view.update_in(cx, |view, window, cx| match uploaded {
                Ok(paths) => {
                    for path in paths {
                        view.paste_text(format!("{} ", path_text(Path::new(&path), None, None, quoting)), window, cx);
                    }
                }
                Err(err) => {
                    tracing::warn!("cannot upload to {id:?}: {err:#}");
                    let detail = format!("{err:#}");
                    let title = rust_i18n::t!("ssh.upload_failed", host = crate::remote_hosts::display_name(&id, cx))
                        .into_owned();
                    // 只说一声，但收的那头要留到框关掉。
                    let answer = crate::prompt::show(
                        window,
                        PromptLevel::Warning,
                        &title,
                        Some(&detail),
                        &[PromptButton::cancel(rust_i18n::t!("ssh.ok"))],
                        cx,
                    );
                    cx.foreground_executor().spawn(answer).detach();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 把 `paths` 一个个打进当前终端，在它的目录下时写相对路径，后面带上首尾行号 `lines` 和一个空格，
    /// 再把焦点交回终端。比目录要解符号链接，在单独的线程上算好再打进去（`fs_probe::off_thread`）。
    pub(super) fn insert_paths(
        &mut self,
        paths: Vec<PathBuf>,
        lines: Option<(u32, u32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.focused_view().cloned() else {
            return;
        };
        let cwd = view.read(cx).cwd();
        let quoting = Quoting::of_session(view.read(cx).meta());
        let paths = if view.read(cx).host().is_local() {
            paths
        } else {
            paths.iter().map(|path| crate::project_host::remote_path(path)).collect()
        };
        window.focus(&view.focus_handle(cx), cx);
        // 起不了线程时不比目录，照原样打进去。
        let fallback = paths.iter().map(|path| path_text(path, lines, None, quoting)).collect();
        let job = crate::fs_probe::off_thread(
            move || paths.iter().map(|path| path_text(path, lines, cwd.as_deref(), quoting)).collect(),
            fallback,
        );
        // 打进算的时候拿着焦点的那个终端；它关掉了就不打。上一次的先算完时等它打完，连点两次不会倒过来。
        let view = view.downgrade();
        let previous = self.inserting.take();
        let task = cx.spawn_in(window, async move |_, cx| {
            let texts: Vec<String> = job.await;
            if let Some(previous) = previous {
                previous.await;
            }
            view.update_in(cx, |view, window, cx| {
                for text in texts {
                    view.paste_text(format!("{text} "), window, cx);
                }
            })
            .ok();
        });
        self.inserting = Some(task.shared());
    }
}

/// `path` 照 `quoting` 打进终端时的写法。图片写绝对路径：Claude Code 只把
/// 绝对路径认成图片附件。
fn path_text(path: &Path, lines: Option<(u32, u32)>, cwd: Option<&Path>, quoting: Quoting) -> String {
    if lines.is_none() && is_image(path) {
        let absolute = match cwd {
            Some(cwd) if path.is_relative() => cwd.join(path),
            _ => path.to_path_buf(),
        };
        return quoting.escape(&absolute.display().to_string());
    }
    // 终端报的目录和面板里的路径可能一个经过符号链接、一个没有，对不上时都换成真实路径再比。
    let relative = |path: &Path, cwd: &Path| {
        path.strip_prefix(cwd).ok().map(Path::to_path_buf).or_else(|| {
            let (path, cwd) = (path.canonicalize().ok()?, cwd.canonicalize().ok()?);
            path.strip_prefix(cwd).ok().map(Path::to_path_buf)
        })
    };
    let rel = cwd.and_then(|cwd| relative(path, cwd)).filter(|rel| !rel.as_os_str().is_empty());
    let mut text = rel.as_deref().unwrap_or(path).display().to_string();
    match lines {
        Some((first, last)) if first == last => text = format!("{text}:{first}"),
        Some((first, last)) => text = format!("{text}:{first}-{last}"),
        None => {}
    }
    quoting.quote(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_line_ranges_after_relative_paths() {
        let cwd = Path::new("/repo");
        let text = |path, lines| path_text(Path::new(path), lines, Some(cwd), Quoting::Posix);
        assert_eq!(text("/repo/src/a.rs", None), "src/a.rs");
        assert_eq!(text("/repo/src/a.rs", Some((3, 3))), "src/a.rs:3");
        assert_eq!(text("/repo/src/a.rs", Some((3, 9))), "src/a.rs:3-9");
    }

    #[test]
    fn recognizes_images_by_extension() {
        assert!(is_image(Path::new("/tmp/a.PNG")));
        assert!(is_image(Path::new("b.jpeg")));
        assert!(!is_image(Path::new("c.svg")));
        assert!(!is_image(Path::new("png")));
    }
}
