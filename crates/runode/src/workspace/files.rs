//! 右侧的文件树：当前终端所在仓库或目录下的文件，图标按文件类型，名字按 git 状态着色。
//! 单击目录展开收起，双击文件把它的路径打进当前终端。

use std::{ops::Range, path::Path};

use gpui::{
    AnyElement, Context, Div, Focusable, MouseButton, MouseDownEvent, ScrollStrategy, SharedString, Stateful, Window, div,
    img, prelude::*, px, svg, uniform_list,
};
use runode_model::color::Rgb;

use super::{
    WindowView,
    project::{Decoration, MODIFIED, status_color},
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, EYE_ICON, EYE_OFF_ICON},
    file_icons::{file_icon, folder_icon},
    terminal_view::hsla,
};

const ROW_HEIGHT: f32 = 22.;
/// 每深一层往右缩进的宽度。
const INDENT: f32 = 12.;
/// 行的左边距。
const ROW_PADDING: f32 = 4.;

/// 打进 shell 的路径：只含常见字符时原样，否则用单引号括起来。开头是 `=` 或 `%` 时也括起来，
/// zsh 会把 `=foo` 展开成命令的路径。
fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && !text.starts_with(['=', '%'])
        && text.chars().all(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | '%' | ':' | ',' | '='));
    if plain { text.to_owned() } else { format!("'{}'", text.replace('\'', r"'\''")) }
}

impl WindowView {
    pub(super) fn render_files_panel(&self, width: f32, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = self.workspace();
        // 显示的是终端所在的仓库或目录，不一定是 workspace 的目录。
        let name = match workspace.project.root.as_deref().and_then(Path::file_name) {
            Some(name) if workspace.project.root.as_ref() != Some(&workspace.dir) => {
                SharedString::from(name.to_string_lossy().into_owned())
            }
            _ => workspace.name.clone(),
        };
        let show_ignored = self.show_ignored;
        let ignored_toggle = div()
            .id("toggle-ignored")
            .flex_none()
            .size(px(20.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .when(show_ignored, |button| button.bg(hsla(bg.mix(fg, 0.10))))
            .hover(|button| button.bg(hsla(bg.mix(fg, 0.14))))
            .child(
                svg()
                    .path(if show_ignored { EYE_ICON } else { EYE_OFF_ICON })
                    .size(px(14.))
                    .text_color(hsla(fg).opacity(if show_ignored { 0.9 } else { 0.55 })),
            )
            // 标题栏按下会拖动窗口，按钮自己接住。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_show_ignored(cx);
                }),
            );
        let header = self
            .panel_header(true, fg)
            .child(div().flex_1().min_w_0().truncate().text_color(hsla(fg)).child(name))
            .child(ignored_toggle);
        let list = uniform_list(
            "files",
            workspace.project.file_rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| this.render_file_rows(range, fg, bg, cx)),
        )
        .track_scroll(&workspace.project.files_scroll)
        .flex_1()
        .py(px(4.));
        div()
            .id("files-panel")
            .flex_none()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .bg(hsla(bg.mix(fg, 0.03)))
            .border_l_1()
            .border_color(hsla(fg).opacity(0.12))
            .text_size(px(12.))
            .child(header)
            .child(list)
    }

    fn render_file_rows(&self, range: Range<usize>, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let project = &self.workspace().project;
        let selected_bg = hsla(bg.mix(fg, 0.12));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let guide = hsla(fg).opacity(0.2);
        let fg = hsla(fg);
        range
            .filter_map(|ix| project.file_rows.get(ix).map(|row| (ix, row)))
            .map(|(ix, row)| {
                let color = match row.decoration {
                    Decoration::None | Decoration::Ignored => fg.opacity(0.85),
                    Decoration::Status(status) => hsla(status_color(status)),
                    Decoration::ContainsChanges => hsla(MODIFIED),
                };
                let selected = project.selected.as_ref() == Some(&row.path);
                let chevron = row.is_dir.then(|| {
                    svg()
                        .path(if row.expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                        .size(px(12.))
                        .text_color(fg.opacity(0.5))
                });
                // 并成一行的目录按最里层的名字挑图标。
                let last = row.path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
                let icon = if row.is_dir { folder_icon(&last, row.expanded) } else { file_icon(&last) };
                // 缩进里每一层一格，格子中间一条竖线，对准那一层的箭头，上下相邻的行连成一条。
                let guides = (0..row.depth).map(|_| {
                    div().flex_none().w(px(INDENT)).h_full().flex().justify_center().child(div().w(px(1.)).h_full().bg(guide))
                });
                let path = row.path.clone();
                let is_dir = row.is_dir;
                div()
                    .id(("file", ix))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .mx(px(4.))
                    .pl(px(ROW_PADDING))
                    .pr(px(6.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .when(row.decoration == Decoration::Ignored, |item| item.opacity(0.45))
                    .map(|item| if selected { item.bg(selected_bg) } else { item.hover(|item| item.bg(hover_bg)) })
                    .children(guides)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(div().flex_none().w(px(12.)).flex().items_center().children(chevron))
                            .child(img(icon).flex_none().size(px(14.)))
                            .child(div().min_w_0().truncate().text_color(color).child(row.name.clone())),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.click_file(&path, is_dir, event.click_count, window, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// 单击选中，目录同时展开或收起，有改动的文件在改动栏里滚到它；双击文件把路径打进终端。
    fn click_file(&mut self, path: &Path, is_dir: bool, clicks: usize, window: &mut Window, cx: &mut Context<Self>) {
        let show_ignored = self.show_ignored;
        let workspace = self.workspace_mut();
        workspace.project.selected = Some(path.to_path_buf());
        if is_dir {
            let root = workspace.project.root.clone().unwrap_or_else(|| workspace.dir.clone());
            workspace.project.toggle_dir(path, &root, show_ignored);
        } else if clicks >= 2 {
            self.insert_path(path, None, window, cx);
        } else if self.changes_shown {
            let project = &mut self.workspace_mut().project;
            if let Some(row) = project.reveal_diff(path) {
                project.changes_scroll.scroll_to_item(row, ScrollStrategy::Top);
            }
        }
        cx.notify();
    }

    /// 把 `path` 打进当前终端，在它的目录下时写相对路径，后面带上行号 `line` 和一个空格，
    /// 再把焦点交回终端。
    pub(super) fn insert_path(&mut self, path: &Path, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.tab().focused_view().clone();
        let cwd = view.read(cx).cwd();
        // 终端报的目录和面板里的路径可能一个经过符号链接、一个没有，对不上时都换成真实路径再比。
        let relative = |path: &Path, cwd: &Path| {
            path.strip_prefix(cwd).ok().map(Path::to_path_buf).or_else(|| {
                let (path, cwd) = (path.canonicalize().ok()?, cwd.canonicalize().ok()?);
                path.strip_prefix(cwd).ok().map(Path::to_path_buf)
            })
        };
        let rel = cwd.as_deref().and_then(|cwd| relative(path, cwd)).filter(|rel| !rel.as_os_str().is_empty());
        let mut text = rel.as_deref().unwrap_or(path).display().to_string();
        if let Some(line) = line {
            text = format!("{text}:{line}");
        }
        let text = format!("{} ", shell_quote(&text));
        view.update(cx, |view, cx| view.paste_text(text, window, cx));
        window.focus(&view.focus_handle(cx), cx);
    }
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
}
