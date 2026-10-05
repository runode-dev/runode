//! 右侧的文件树：当前终端所在仓库或目录下的文件，图标按文件类型，名字按 git 状态着色，行尾标出状态。
//! 单击目录展开收起，双击文件在预览栏里打开，也可以配置成单击打开、双击把路径打进终端。

use std::{ops::Range, path::Path};

use gpui::{
    AnyElement, Context, Div, Focusable, MouseButton, MouseDownEvent, ScrollStrategy, SharedString, Stateful, Window, div,
    img, prelude::*, px, svg, uniform_list,
};
use runode_config::PreviewClick;
use runode_shared_types::color::Rgb;

use super::{
    WindowView,
    project::{ADDED, Decoration, REMOVED, status_color},
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, EYE_ICON, EYE_OFF_ICON},
    config::AppConfig,
    file_icons::{file_icon, folder_icon},
    terminal_view::hsla,
    tooltip::tooltip,
};

/// 行高比字号多出的部分。
const ROW_EXTRA_HEIGHT: f32 = 10.;
/// 每深一层往右缩进的宽度。
const INDENT: f32 = 12.;
/// 行的左边距。
const ROW_PADDING: f32 = 4.;
/// 标题下面那行工具栏的高度。
const TOOLBAR_HEIGHT: f32 = 28.;

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
            .tooltip(tooltip(
                if show_ignored { rust_i18n::t!("tooltip.hide_ignored") } else { rust_i18n::t!("tooltip.show_ignored") },
                None,
                fg,
                bg,
            ))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.toggle_show_ignored(cx)));
        let header = self.panel_header(true, fg).child(div().flex_1().min_w_0().truncate().text_color(hsla(fg)).child(name));
        // 标题下面一行：左边是没提交的改动一共加减了多少行，右边是显示忽略文件的开关。
        let dirty = workspace.project.git.as_ref().filter(|git| !git.is_clean());
        let toolbar = div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .border_b_1()
            .border_color(hsla(fg).opacity(0.12))
            .when_some(dirty, |toolbar, git| {
                toolbar
                    .child(div().flex_none().text_color(hsla(ADDED)).child(format!("+{}", git.added())))
                    .child(div().flex_none().text_color(hsla(REMOVED)).child(format!("−{}", git.removed())))
            })
            .child(div().flex_1())
            .child(ignored_toggle);
        let font_size = cx.global::<AppConfig>().0.file_tree_font_size;
        let list = uniform_list(
            "files",
            workspace.project.file_rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| this.render_file_rows(range, font_size, fg, bg, cx)),
        )
        .track_scroll(&workspace.project.files_scroll)
        .flex_1()
        .p(px(4.));
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
            .text_size(px(font_size))
            .child(header)
            .child(toolbar)
            .child(list)
    }

    /// 文件树的行。行高、箭头和图标跟着字号 `font_size` 一起缩放。
    fn render_file_rows(
        &self,
        range: Range<usize>,
        font_size: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
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
                    Decoration::Status(status) | Decoration::ContainsChanges(status) => hsla(status_color(status)),
                };
                let selected = project.selected.as_ref() == Some(&row.path);
                let chevron = row.is_dir.then(|| {
                    svg()
                        .path(if row.expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                        .size(px(font_size))
                        .text_color(fg.opacity(0.5))
                });
                // 并成一行的目录按最里层的名字挑图标。
                let last = row.path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
                let icon = if row.is_dir { folder_icon(&last, row.expanded) } else { file_icon(&last) };
                // 缩进里每一层一格，格子中间一条竖线，对准那一层的箭头，上下相邻的行连成一条。
                let guides = (0..row.depth).map(|_| {
                    div().flex_none().w(px(INDENT)).h_full().flex().justify_center().child(div().w(px(1.)).h_full().bg(guide))
                });
                // 行尾的 git 标记：文件写状态字母，含改动的目录画一个点。
                let badge = match row.decoration {
                    Decoration::Status(status) => Some(status.letter()),
                    Decoration::ContainsChanges(_) => Some("•"),
                    Decoration::None | Decoration::Ignored => None,
                };
                let badge = badge.map(|text| div().flex_none().pl(px(6.)).text_color(color).child(text));
                let path = row.path.clone();
                let is_dir = row.is_dir;
                div()
                    .id(("file", ix))
                    .flex_none()
                    .h(px(font_size + ROW_EXTRA_HEIGHT))
                    // 占满整行宽度，名字后面的空白处也能点、也有悬停底色。
                    .w_full()
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
                            .child(div().flex_none().w(px(font_size)).flex().items_center().children(chevron))
                            .child(img(icon).flex_none().size(px(font_size + 2.)))
                            .child(div().min_w_0().truncate().text_color(color).child(row.name.clone())),
                    )
                    .children(badge)
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

    /// 单击选中，目录同时展开或收起，有改动的文件在改动栏里滚到它。文件按配置的
    /// `PreviewClick` 双击或单击在预览栏里打开；单击打开时，双击把路径打进终端。
    fn click_file(&mut self, path: &Path, is_dir: bool, clicks: usize, window: &mut Window, cx: &mut Context<Self>) {
        let show_ignored = self.show_ignored;
        let workspace = self.workspace_mut();
        workspace.project.selected = Some(path.to_path_buf());
        if is_dir {
            let root = workspace.project.root.clone().unwrap_or_else(|| workspace.dir.clone());
            workspace.project.toggle_dir(path, &root, show_ignored);
            cx.notify();
            return;
        }
        let single = cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single;
        if clicks >= 2 {
            if single {
                self.insert_path(path, None, window, cx);
            } else {
                self.open_preview(path, cx);
            }
        } else {
            if self.changes_shown {
                let project = &mut self.workspace_mut().project;
                if let Some(row) = project.reveal_diff(path) {
                    project.changes_scroll.scroll_to_item(row, ScrollStrategy::Top);
                }
            }
            if single {
                self.open_preview(path, cx);
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
