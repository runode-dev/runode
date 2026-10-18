//! 把右侧面板的文件树接到窗口上。文件树本身在 `file_tree`：一个窗口一个 `FileTree`，各 workspace 的树
//! 都在它里面。这里每次对它做什么之前告诉它当前是哪个 workspace、交给它 workspace 的名字、目录和单击打不打开
//! 文件，画面板外框和顶上的标签；后台读到的目录和 git 状态由 `refresh_project` 经 `FileTree::apply_scan` 交给它。
//! 它要窗口办的事（开预览、把路径打进终端、开新标签、预览跟着改名和删除、重读、焦点交回终端、开关变了要存档）
//! 经 `FileTreeEvent` 交回来。
//!
//! 把路径打进终端（`insert_paths`）和拖到终端上（`drop_paths_on_pane`）也在这里，预览栏、Git 面板和分屏都用。

use std::path::{Path, PathBuf};

use crate::file_tree::{FileTree, FileTreeEvent, Input};
use futures::FutureExt as _;
use gpui::{Context, Div, Entity, Focusable, Stateful, Window, div, prelude::*, px};
use runode_config::PreviewClick;

// 菜单在 `runode_ui::menu`，窗口里各处照旧从这里引。
pub(super) use crate::ui::menu::{MenuButton, MenuItem};

use super::{WindowView, cards, project::panel_shell};
use crate::{
    config::AppConfig,
    ui::{
        font_size::{self, FontSize},
        theme::ActiveTheme as _,
    },
};

/// 打进 shell 不用引号也不用转义的常见字符。
fn is_shell_plain(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | '%' | ':' | ',' | '=')
}

/// 打进 shell 的路径：只含常见字符时原样，否则用单引号括起来。开头是 `=` 或 `%` 时也括起来，
/// zsh 会把 `=foo` 展开成命令的路径。
pub(super) fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty() && !text.starts_with(['=', '%']) && text.chars().all(is_shell_plain);
    if plain { text.to_owned() } else { format!("'{}'", text.replace('\'', r"'\''")) }
}

/// Claude Code、Codex 会把粘贴进来的图片路径换成图片附件，这几种扩展名两边都认。
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|image| ext.eq_ignore_ascii_case(image)))
}

/// 和 `shell_quote` 一样只放过常见字符，其余字符前面加反斜杠，像访达把文件拖进终端时那样。
/// 图片路径用这种写法：Claude Code 按「空格后跟 `/`」拆开几个路径，引号只在整段两头时才去掉，
/// 几个带引号的路径连在一起它就认不出来了；反斜杠转义它和 Codex 都能还原。
fn shell_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for (i, c) in text.chars().enumerate() {
        let plain = is_shell_plain(c) && !(i == 0 && matches!(c, '=' | '%'));
        if !plain {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

impl WindowView {
    /// 当前 workspace 交给文件树的名字、目录和单击打不打开文件。
    fn file_tree_input(&self, cx: &Context<Self>) -> Input {
        let workspace = self.workspace();
        Input {
            name: workspace.name.clone(),
            dir: workspace.dir.clone(),
            single_click: cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single,
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

    /// 右侧面板的文件树页：外框和顶上的标签在这里画，里面是文件树。
    pub(super) fn render_files_panel(&mut self, width: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let input = self.file_tree_input(cx);
        self.file_tree.update(cx, |tree, _| tree.set_input(input));
        let font_size = FontSize::Medium.points() * font_size::scale(cx);
        panel_shell("files-panel", width, cards(cx), cx)
            .bg(cx.theme().mix(0.03))
            .text_size(px(font_size))
            .child(self.render_panel_tabs(cx))
            .child(div().flex_1().min_h_0().child(self.file_tree.clone()))
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
            FileTreeEvent::Open { path, line: None, pin } => self.open_preview(path, *pin, cx),
            FileTreeEvent::Open { path, line: Some(line), pin } => self.open_preview_at(path, *line, *pin, cx),
            FileTreeEvent::InsertPath(path) => self.insert_paths(vec![path.clone()], None, window, cx),
            FileTreeEvent::OpenTerminal(dir) => {
                if let Some(view) = self.spawn_terminal(Some(dir), window, cx) {
                    self.insert_tab(self.workspace().active + 1, view, window, cx);
                }
            }
            FileTreeEvent::Moved { from, to } => self.move_previews(from, to, cx),
            FileTreeEvent::Removed(path) => self.close_previews_under(path, window, cx),
            FileTreeEvent::Refresh => self.refresh_project(cx),
            FileTreeEvent::FocusTerminal => window.focus(&self.focus_handle(cx), cx),
            FileTreeEvent::OptionsChanged => self.save(cx),
        }
    }

    /// 把从文件树或访达拖来的 `paths` 放到终端 `pane` 上：切到那个终端，把路径一个个打进去。
    /// 一个路径粘贴一次，因为 Codex 只在一次粘贴里正好是一个路径时才把它当图片。
    pub(super) fn drop_paths_on_pane(
        &mut self,
        pane: gpui::EntityId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_active_tab(pane, window, cx);
        self.insert_paths(paths.to_vec(), None, window, cx);
    }

    /// 把 `paths` 一个个打进当前终端，在它的目录下时写相对路径，后面带上行号 `line` 和一个空格，
    /// 再把焦点交回终端。比目录要解符号链接，在单独的线程上算好再打进去（`fs_probe::off_thread`）。
    pub(super) fn insert_paths(
        &mut self,
        paths: Vec<PathBuf>,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.focused_view().cloned() else {
            return;
        };
        let cwd = view.read(cx).cwd();
        window.focus(&view.focus_handle(cx), cx);
        // 起不了线程时不比目录，照原样打进去。
        let fallback = paths.iter().map(|path| path_text(path, line, None)).collect();
        let job = crate::fs_probe::off_thread(
            move || paths.iter().map(|path| path_text(path, line, cwd.as_deref())).collect(),
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

/// `path` 打进终端时的写法，见 `WindowView::insert_paths`。图片写绝对路径：Claude Code 只把绝对路径认成
/// 图片附件。
fn path_text(path: &Path, line: Option<u32>, cwd: Option<&Path>) -> String {
    if line.is_none() && is_image(path) {
        let absolute = match cwd {
            Some(cwd) if path.is_relative() => cwd.join(path),
            _ => path.to_path_buf(),
        };
        return shell_escape(&absolute.display().to_string());
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
    if let Some(line) = line {
        text = format!("{text}:{line}");
    }
    shell_quote(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_for_the_shell() {
        assert_eq!(shell_quote("src/main.rs:12"), "src/main.rs:12");
        assert_eq!(shell_quote("my file.txt"), "'my file.txt'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("文件.md"), "文件.md");
        assert_eq!(shell_quote("=foo"), "'=foo'");
        assert_eq!(shell_quote("a=b"), "a=b");
    }

    #[test]
    fn escapes_image_paths_with_backslashes() {
        assert_eq!(shell_escape("/tmp/shot.png"), "/tmp/shot.png");
        assert_eq!(shell_escape("/tmp/Screen Shot (1).png"), r"/tmp/Screen\ Shot\ \(1\).png");
        assert_eq!(shell_escape("/tmp/it's.png"), r"/tmp/it\'s.png");
        assert_eq!(shell_escape("/tmp/截图.png"), "/tmp/截图.png");
        assert_eq!(shell_escape("=a=b"), r"\=a=b");
    }

    #[test]
    fn recognizes_images_by_extension() {
        assert!(is_image(Path::new("/tmp/a.PNG")));
        assert!(is_image(Path::new("b.jpeg")));
        assert!(!is_image(Path::new("c.svg")));
        assert!(!is_image(Path::new("png")));
    }
}
