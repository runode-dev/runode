//! 预览栏的编辑器：普通标签里读全了的文本文件，正文是代码编辑器（gpui-component 的 `EditorState`），打开就能改，
//! ⌘S 存回磁盘。改过没存的标签名后面带一个圆点，关掉它之前先问一句；文件在磁盘上被别人改过时存之前也先问。
//! 语法高亮、查找替换、折叠和缩进由编辑器自己做，配色经 `theme` 跟着窗口的主题。
//!
//! 从 Dock 退出、注销、关机这类系统发起的退出弹不了框，没存的修改会丢。

mod theme;

use std::{
    io,
    ops::Range,
    path::{Path, PathBuf},
};

use crate::ui::{
    font_size::{FontSize, scaled},
    input::{Input, InputEvent as FieldEvent, InputState},
};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, Modifiers, PromptButton, PromptLevel, Role,
    SharedString, Subscription, Task, Window, div, point, prelude::*, px,
};
use gpui_component::input::{Editor as CodeEditor, EditorState, InputEvent, Rope, RopeExt as _};
use runode_files::Stamp;
use runode_preview::MAX_TEXT_BYTES;

use crate::project_host::ProjectHost;

use super::{Loaded, Preview, PreviewPanel, ROW_EXTRA_HEIGHT, WorkspaceId};
use crate::ui::theme::ActiveTheme as _;

gpui::actions!(
    preview,
    [
        /// 在编辑器上面打开输入行号的小条，回车跳过去。
        GoToLine,
        /// 光标在词上、没有选区时选中这个词；有选区时把下一处相同的文字加选上，到文末从头找。
        SelectNextOccurrence
    ]
);

/// `PreviewPanel::ask_unsaved` 的回答。
pub(super) const UNSAVED_SAVE: usize = 0;
pub(super) const UNSAVED_DISCARD: usize = 1;

pub(super) struct Editor {
    pub(super) area: Entity<EditorState>,
    /// 读进来或上次存下的文字（换行是 `\n`），和输入框里的比就知道改没改。
    saved: String,
    dirty: bool,
    /// 文件原来用 `\r\n` 换行、开头带 BOM：输入框里一律是 `\n`，存的时候换回去。
    crlf: bool,
    bom: bool,
    /// 读进来或上次存下时文件的大小和修改时间，存之前和磁盘上的比。
    stamp: Option<Stamp>,
    /// 正在存：这时再按 ⌘S 不管，免得两次写的先后乱了，旧的盖掉新的。
    saving: bool,
    /// `SelectNextOccurrence` 上次之后的活动选区、加选到的位置、是不是从光标所在的词选起的（是就只加选整词）；
    /// 文字改了就作废。
    occurrence: Option<(Range<usize>, usize, bool)>,
    /// 等编辑器排好版后滚到正中的行（从 0 数）。
    pub(super) center: Option<usize>,
    /// 开着的「跳到行号」输入条。
    go_to_line: Option<(Entity<InputState>, Subscription)>,
    /// 这个文件在语言服务器上开着的文档；没有可用的服务器时为空。
    pub(super) lsp: Option<super::lsp::Document>,
    _changed: Subscription,
}

/// 读进来要编辑的文件：去掉 BOM、换行统一成 `\n` 的文字，原来的样子，和读时的大小、修改时间。
struct Source {
    text: String,
    crlf: bool,
    bom: bool,
    stamp: Option<Stamp>,
}

/// 经 `project` 读 `path` 的原文；太大、含 NUL 或不是 UTF-8 的不能编辑。
fn read_source(project: &ProjectHost, path: &Path) -> io::Result<Source> {
    let stamp = project.stamp(path);
    let read = project.read_limited(path, MAX_TEXT_BYTES as u64, true).map_err(io::Error::other)?;
    if read.cut || read.bytes.contains(&0) {
        return Err(io::Error::other("not an editable text file"));
    }
    let text = String::from_utf8(read.bytes).map_err(io::Error::other)?;
    let (bom, text) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, text),
    };
    Ok(Source { crlf: text.contains("\r\n"), bom, text: text.replace("\r\n", "\n"), stamp })
}

/// 输入框里的文字存回磁盘的字节：换行和 BOM 照文件原来的样子。
fn encode(text: &str, crlf: bool, bom: bool) -> Vec<u8> {
    let text = if crlf { text.replace('\n', "\r\n") } else { text.to_owned() };
    [if bom { "\u{feff}" } else { "" }, &text].concat().into_bytes()
}

/// 在后台经 `project` 读 `path` 的原文。
fn read_in_background(project: ProjectHost, path: &Path, cx: &mut Context<PreviewPanel>) -> Task<io::Result<Source>> {
    let path = path.to_path_buf();
    cx.background_spawn(async move { read_source(&project, &path) })
}

/// 弹框报错，只有一个「好」。
fn report(title: &str, err: &io::Error, window: &mut Window, cx: &mut Context<PreviewPanel>) {
    // 只说一声，但收的那头要留到框关掉。
    let answer = crate::prompt::show(
        window,
        PromptLevel::Critical,
        title,
        Some(&err.to_string()),
        &[PromptButton::cancel(rust_i18n::t!("git.ok"))],
        cx,
    );
    cx.foreground_executor().spawn(answer).detach();
}

/// `text` 里 `from` 之后下一处和 `selected` 那段一样的文字，到文末从头找；除了 `selected` 自己再没有时为空。
/// `whole_word` 时两边紧挨着字母、数字或下划线的不算。
fn next_occurrence(text: &str, selected: &Range<usize>, from: usize, whole_word: bool) -> Option<Range<usize>> {
    let needle = &text[selected.clone()];
    let in_word = |c: char| c.is_alphanumeric() || c == '_';
    let whole = |at: &usize| {
        !whole_word
            || !(text[..*at].chars().next_back().is_some_and(in_word)
                || text[at + needle.len()..].chars().next().is_some_and(in_word))
    };
    let after = text.get(from..).into_iter().flat_map(|rest| rest.match_indices(needle)).map(|(at, _)| from + at);
    let start = after.chain(text.match_indices(needle).map(|(at, _)| at)).find(whole)?;
    (start != selected.start).then(|| start..start + needle.len())
}

/// 编辑器按什么语言高亮 `path`：扩展名，Makefile 按文件名认。
fn language_of(path: &Path) -> String {
    let lower = |name: Option<&std::ffi::OsStr>| name.and_then(|name| name.to_str()).unwrap_or_default().to_lowercase();
    let name = lower(path.file_name());
    match lower(path.extension()) {
        ext if ext == "mk" || name == "makefile" || name == "gnumakefile" => "make".into(),
        ext => ext,
    }
}

/// `text` 和 `saved` 一样。
fn same(text: &Rope, saved: &str) -> bool {
    let mut rest = saved;
    text.len() == saved.len()
        && text.chunks().all(|chunk| match rest.strip_prefix(chunk) {
            Some(tail) => {
                rest = tail;
                true
            }
            None => false,
        })
}

/// 选中这几行（从 0 数，不含 `end`），不含末行的换行符，滚到能看见。超出的行号收到最后一行。
fn select_lines(area: &mut EditorState, lines: Range<usize>, cx: &mut Context<EditorState>) {
    let text = area.text();
    let last = text.lines_len().saturating_sub(1);
    let start = text.line_start_offset(lines.start.min(last));
    let end = text.line_end_offset(lines.end.saturating_sub(1).min(last));
    area.set_selected_range(start..end.max(start), cx);
}

/// 选中第 `line` 行（从 0 数）里字节范围 `range` 那段，滚到能看见。超出那一行的收到行尾。
fn select_in_line(area: &mut EditorState, line: usize, range: Range<usize>, cx: &mut Context<EditorState>) {
    let text = area.text();
    let line = line.min(text.lines_len().saturating_sub(1));
    let (start, end) = (text.line_start_offset(line), text.line_end_offset(line));
    area.set_selected_range((start + range.start).min(end)..(start + range.end).min(end), cx);
}

/// 把第 `line` 行（从 0 数）滚到编辑器正中；还没排过版、不知道行高和视口时返回假。
fn center_line(area: &mut EditorState, line: usize, cx: &mut Context<EditorState>) -> bool {
    let (Some(line_height), Some(bounds)) = (area.line_height(), area.text_bounds()) else {
        return false;
    };
    let top = line_height * area.display_map().buffer_line_to_display_row(line) as f32;
    let y = (top - (bounds.size.height - line_height) / 2.).max(px(0.));
    area.set_scroll_offset(point(area.scroll_offset().x, -y), cx);
    true
}

impl Editor {
    /// 把第 `line` 行（从 0 数）滚到正中；还没排过版时等 `render_editor` 排完再滚。
    pub(super) fn center_on(&mut self, line: usize, cx: &mut App) {
        let centered = self.area.update(cx, |area, cx| center_line(area, line, cx));
        self.center = (!centered).then_some(line);
    }

    /// 选中这几行（从 0 数），滚到能看见。
    pub(super) fn select_lines(&self, lines: Range<usize>, cx: &mut App) {
        self.area.update(cx, |area, cx| select_lines(area, lines, cx));
    }

    /// 选中第 `line` 行（从 0 数）里字节范围 `range` 那段，滚到能看见。
    pub(super) fn select_in_line(&self, line: usize, range: Range<usize>, cx: &mut App) {
        self.area.update(cx, |area, cx| select_in_line(area, line, range, cx));
    }
}

impl Preview {
    /// 在编辑，改过还没存。
    pub(super) fn dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(|editor| editor.dirty)
    }

    /// 改过还没存的输入框。
    pub(super) fn unsaved_area(&self) -> Option<Entity<EditorState>> {
        self.editor.as_ref().filter(|editor| editor.dirty).map(|editor| editor.area.clone())
    }

    /// 正文能换成编辑器：普通标签里读全了的文本。
    pub(super) fn editable(&self) -> bool {
        self.diff.is_none() && matches!(self.content, Some(Loaded::Text { truncated: false, .. }))
    }

    /// 编辑器里选区跨过的行，从 0 数；结束在行首时不算那一行。不在编辑或没选东西时是 `None`。
    pub(super) fn editor_selected_lines(&self, cx: &App) -> Option<Range<usize>> {
        let area = self.editor.as_ref()?.area.read(cx);
        let range = area.selected_range();
        if range.is_empty() {
            return None;
        }
        let (start, end) = (area.text().offset_to_point(range.start), area.text().offset_to_point(range.end));
        Some(start.row..if end.column == 0 && end.row > start.row { end.row } else { end.row + 1 })
    }
}

impl PreviewPanel {
    /// `preview` 正显示着的编辑器；Markdown 显示着排版时编辑器留着不显示。
    pub(super) fn shown_editor<'a>(&self, preview: &'a Preview) -> Option<&'a Editor> {
        preview.editor.as_ref().filter(|_| !self.markdown_shown(preview))
    }

    /// 当前标签显示着编辑器时它的焦点。
    pub(super) fn editor_focus(&self, cx: &App) -> Option<FocusHandle> {
        Some(self.shown_editor(self.preview()?)?.area.focus_handle(cx))
    }

    /// 在后台读 workspace `ws` 里 `path` 普通标签的原文，读好了换成输入框；焦点不动，读不了时不进编辑也不报错。
    pub(super) fn start_editing_quietly(&mut self, ws: WorkspaceId, path: PathBuf, cx: &mut Context<Self>) {
        let project = self.workspaces.get(&ws).map(|tabs| tabs.project.clone()).unwrap_or_default();
        let job = read_in_background(project, &path, cx);
        let window = self.window;
        cx.spawn(async move |this, cx| {
            if let Ok(source) = job.await {
                window
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| this.install_editor(ws, &path, source, window, cx)).ok();
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// 把 workspace `ws` 里 `path` 普通标签的正文换成装着 `source` 的输入框，预览里选着的行在输入框里照样
    /// 选中。读的期间标签关了、又进过一次编辑的不管，返回空。
    fn install_editor(
        &mut self,
        ws: WorkspaceId,
        path: &Path,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<EditorState>> {
        let wrap = self.wrap;
        let tabs = self.workspaces.get_mut(&ws)?;
        let preview =
            tabs.tabs.iter_mut().find(|tab| tab.path == path && tab.diff.is_none() && tab.editor.is_none())?;
        let selected = preview.selected_lines();
        let hit = preview.hit.take().filter(|(line, _)| preview.selection == Some((*line, *line)));
        // 语言按扩展名认，认不出的当纯文本。
        let language = language_of(path);
        let area = cx.new(|cx| {
            let mut area =
                EditorState::new(window, cx).language(language).line_number(true).folding(true).soft_wrap(wrap);
            area.set_value(source.text.clone(), window, cx);
            match (hit, selected.clone()) {
                (Some((line, range)), _) => select_in_line(&mut area, line, range, cx),
                (None, Some(lines)) => select_lines(&mut area, lines, cx),
                (None, None) => {}
            }
            area
        });
        let changed = cx.subscribe(&area, |this, area, event: &InputEvent, cx| {
            if let InputEvent::Change = event
                && let Some(preview) = this.editing_tab_mut(&area)
                && let Some(editor) = &mut preview.editor
            {
                if let Some(document) = &mut editor.lsp {
                    document.changed(area.read(cx).text());
                    document.show_diagnostics(&area, cx);
                }
                editor.occurrence = None;
                editor.dirty = !same(area.read(cx).text(), &editor.saved);
                // 改过的临时标签固定下来，免得打开别的文件时连同没存的修改一起被换掉。
                preview.pinned |= editor.dirty;
                cx.notify();
            }
        });
        preview.editor = Some(Editor {
            area: area.clone(),
            saved: source.text,
            dirty: false,
            crlf: source.crlf,
            bom: source.bom,
            stamp: source.stamp,
            saving: false,
            occurrence: None,
            center: selected.map(|lines| lines.start),
            go_to_line: None,
            lsp: None,
            _changed: changed,
        });
        self.attach_language_server(ws, path, cx);
        cx.notify();
        Some(area)
    }

    /// workspace `ws` 里 `path` 的普通标签重读过以后：编辑着、没改过的输入框换成磁盘上现在的文字；文件读不成
    /// 能编辑的文本了（删了、变成二进制）时退出编辑，正文说为什么读不了。改过没存的不动，存的时候再问要不要覆盖。
    pub(super) fn follow_disk(&mut self, ws: WorkspaceId, path: &Path, cx: &mut Context<Self>) {
        let Some(tabs) = self.workspaces.get_mut(&ws) else {
            return;
        };
        let project = tabs.project.clone();
        let Some(preview) = tabs.tabs.iter_mut().find(|tab| tab.path == path && tab.diff.is_none()) else {
            return;
        };
        let Some(editor) = preview.editor.as_ref().filter(|editor| !editor.dirty && !editor.saving) else {
            return;
        };
        if !preview.editable() {
            preview.editor = None;
            cx.notify();
            return;
        }
        if preview.stamp.is_some() && preview.stamp == editor.stamp {
            return;
        }
        let area = editor.area.clone();
        let job = read_in_background(project, path, cx);
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let Ok(source) = job.await else {
                return;
            };
            let reload = |window: &mut Window, cx: &mut App| {
                this.update(cx, |this, cx| {
                    // 读的期间打了字、开始存了的不换。标签已经换上了别的版本时也不换：几次重读的结果先后不定，晚到的
                    // 旧内容不能盖掉新的；磁盘上比标签还新的，标签重读完会再来一次。
                    if this.editor(&area).is_none_or(|editor| editor.dirty || editor.saving)
                        || this
                            .editing_tab_mut(&area)
                            .is_some_and(|tab| tab.stamp.is_some() && tab.stamp != source.stamp)
                    {
                        return;
                    }
                    area.update(cx, |area, cx| area.set_value(source.text.clone(), window, cx));
                    if let Some(editor) = this.editor_mut(&area) {
                        // 整篇换掉不算用户的改动，编辑器不报，语言服务器那边自己告诉。
                        if let Some(document) = &mut editor.lsp {
                            document.changed(&Rope::from(source.text.as_str()));
                            document.show_diagnostics(&area, cx);
                        }
                        (editor.saved, editor.stamp, editor.crlf, editor.bom) =
                            (source.text, source.stamp, source.crlf, source.bom);
                    }
                })
                .ok();
            };
            window.update(cx, |_, window, cx| reload(window, cx)).ok();
        })
        .detach();
    }

    /// 输入框 `area` 所在的标签，在哪个 workspace 都找。
    fn editing_tab_mut(&mut self, area: &Entity<EditorState>) -> Option<&mut Preview> {
        self.workspaces
            .values_mut()
            .flat_map(|tabs| &mut tabs.tabs)
            .find(|tab| tab.editor.as_ref().is_some_and(|editor| editor.area == *area))
    }

    fn editor(&self, area: &Entity<EditorState>) -> Option<&Editor> {
        self.workspaces
            .values()
            .flat_map(|tabs| &tabs.tabs)
            .filter_map(|tab| tab.editor.as_ref())
            .find(|editor| editor.area == *area)
    }

    /// 输入框 `area` 所在的编辑器，在哪个 workspace 都找。
    fn editor_mut(&mut self, area: &Entity<EditorState>) -> Option<&mut Editor> {
        self.editing_tab_mut(area)?.editor.as_mut()
    }

    /// 问名字是 `names` 的这几个标签里没存的修改怎么办：答 `UNSAVED_SAVE` 是存、`UNSAVED_DISCARD` 是放弃，别的是不办了。
    pub(super) fn ask_unsaved(
        &self,
        names: &[SharedString],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> futures::channel::oneshot::Receiver<usize> {
        let title = match names {
            [name] => rust_i18n::t!("preview.save_unsaved_title_one", name = name),
            _ => rust_i18n::t!("preview.save_unsaved_title", count = names.len()),
        };
        crate::prompt::show(
            window,
            PromptLevel::Warning,
            &title,
            Some(&rust_i18n::t!("preview.save_unsaved_detail")),
            &[
                PromptButton::new(rust_i18n::t!("preview.save")),
                PromptButton::new(rust_i18n::t!("preview.dont_save")),
                PromptButton::cancel(rust_i18n::t!("git.cancel")),
            ],
            cx,
        )
    }

    /// 把编辑器 `area` 里的文字存回它的文件。文件在磁盘上比读进来、上次存下时变过（别人改了、删了）先问
    /// 要不要覆盖；写是先写临时文件再换上（`replace_file`），写不成时原文件不动、报错。存下了交回 `true`，没存
    /// （不覆盖、写不成、正在存）交回 `false`。
    pub(super) fn save(
        &mut self,
        area: Entity<EditorState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let Some((project, path)) = self.workspaces.values().find_map(|tabs| {
            let tab = tabs.tabs.iter().find(|tab| tab.editor.as_ref().is_some_and(|editor| editor.area == area))?;
            Some((tabs.project.clone(), tab.path.clone()))
        }) else {
            return Task::ready(false);
        };
        let Some(editor) = self.editor_mut(&area).filter(|editor| !editor.saving) else {
            return Task::ready(false);
        };
        editor.saving = true;
        let text = area.read(cx).value().to_string();
        let (bytes, known) = (encode(&text, editor.crlf, editor.bom), editor.stamp);
        cx.spawn_in(window, async move |this, cx| {
            let current = {
                let (project, path) = (project.clone(), path.clone());
                crate::fs_probe::off_thread(move || project.stamp(&path), None).await
            };
            let overwrite = current == known || {
                let answer = this.update_in(cx, |_, window, cx| {
                    crate::prompt::show(
                        window,
                        PromptLevel::Warning,
                        &rust_i18n::t!("preview.changed_on_disk_title"),
                        Some(&rust_i18n::t!("preview.changed_on_disk_detail")),
                        &[
                            PromptButton::new(rust_i18n::t!("preview.overwrite")),
                            PromptButton::cancel(rust_i18n::t!("git.cancel")),
                        ],
                        cx,
                    )
                });
                match answer {
                    Ok(answer) => answer.await.ok() == Some(0),
                    Err(_) => false,
                }
            };
            let written = match overwrite {
                true => Some(cx.background_spawn(async move { project.replace_file(&path, &bytes) }).await),
                false => None,
            };
            this.update_in(cx, |this, window, cx| {
                let Some(editor) = this.editor_mut(&area) else {
                    return false;
                };
                editor.saving = false;
                match written {
                    Some(Ok(stamp)) => {
                        editor.dirty = !same(area.read(cx).text(), &text);
                        editor.saved = text;
                        editor.stamp = stamp;
                        if let Some(document) = &editor.lsp {
                            document.saved();
                        }
                        cx.notify();
                        true
                    }
                    Some(Err(err)) => {
                        report(&rust_i18n::t!("preview.save_failed"), &err, window, cx);
                        false
                    }
                    None => false,
                }
            })
            .unwrap_or(false)
        })
    }

    /// 编辑模式下的正文：撑满的代码编辑器，报给辅助工具的名字是 `name`，用终端的字体和预览字号。⌘S（Linux、
    /// Windows 上是 Ctrl+S）存。
    pub(super) fn render_editor(
        &self,
        editor: &Editor,
        name: SharedString,
        font: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let font_size = self.input.font_size;
        let area = editor.area.clone();
        // 新开的编辑器这一帧才排版，下一帧再滚；还滚不了就再画一帧。
        if let Some(line) = editor.center {
            let (view, area) = (cx.entity().downgrade(), area.clone());
            window.on_next_frame(move |_, cx| {
                let centered = area.update(cx, |area, cx| center_line(area, line, cx));
                view.update(cx, |this, cx| match this.editor_mut(&area) {
                    Some(editor) if centered => editor.center = None,
                    _ => cx.notify(),
                })
                .ok();
            });
        }
        if theme::stale(cx.theme(), window.is_window_active(), cx) {
            let theme = cx.theme().clone();
            cx.defer(move |cx| theme::apply(&theme, cx));
        }
        let go_to_line = editor.go_to_line.as_ref().map(|(field, _)| {
            div()
                .flex_none()
                .px(px(8.))
                .py(px(4.))
                .border_b_1()
                .border_color(cx.theme().border)
                .text_size(FontSize::Medium)
                .child(Input::new(field).h(scaled(26.)).px(px(8.)).rounded(px(6.)))
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children(go_to_line)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "s" && event.keystroke.modifiers == Modifiers::secondary_key(cx) {
                    this.save(area.clone(), window, cx).detach();
                    cx.stop_propagation();
                }
            }))
            .child(
                CodeEditor::new(&editor.area)
                    .appearance(false)
                    .bordered(false)
                    .flex_1()
                    .min_h_0()
                    .role(Role::MultilineTextInput)
                    .aria_label(name)
                    // 没被高亮上色的字用继承来的文字颜色画。
                    .text_color(cx.theme().foreground)
                    .font_family(font)
                    .text_size(px(font_size))
                    .line_height(px(font_size + ROW_EXTRA_HEIGHT)),
            )
    }

    pub(super) fn select_next_occurrence(&mut self, _: &SelectNextOccurrence, _: &mut Window, cx: &mut Context<Self>) {
        let shown = self.preview().is_some_and(|preview| self.shown_editor(preview).is_some());
        let Some(editor) = self.preview_mut().filter(|_| shown).and_then(|preview| preview.editor.as_mut()) else {
            cx.propagate();
            return;
        };
        let occurrence = &mut editor.occurrence;
        editor.area.update(cx, |area, cx| {
            let active = area.selected_range();
            if active.is_empty() {
                if let Some(word) = area.text().word_range(active.start) {
                    area.set_selected_range(word, cx);
                    let word = area.selected_range();
                    *occurrence = Some((word.clone(), word.end, true));
                }
                return;
            }
            let (from, whole_word) = occurrence
                .take()
                .filter(|(of, ..)| *of == active)
                .map_or((active.end, false), |(_, from, word)| (from, word));
            let text = area.text().to_string();
            if let Some(found) = next_occurrence(&text, &active, from, whole_word) {
                area.add_selection(found.clone(), cx);
                *occurrence = Some((area.selected_range(), found.end, whole_word));
            }
        });
    }

    /// 在当前标签的编辑器上面打开「跳到行号」的输入条：回车把光标放到那一行行首，Esc 关掉，焦点都回编辑器。
    pub(super) fn go_to_line(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) =
            self.preview().and_then(|preview| self.shown_editor(preview)).map(|editor| editor.area.clone())
        else {
            return;
        };
        let label = rust_i18n::t!("preview.go_to_line").into_owned();
        let field = cx.new(|cx| InputState::new(String::new(), cx).with_label(label.clone()).with_placeholder(label));
        let events = cx.subscribe_in(&field, window, {
            let area = area.clone();
            move |this, field, event: &FieldEvent, window, cx| {
                let line = match event {
                    FieldEvent::Next => field.read(cx).query().trim().parse::<usize>().ok(),
                    FieldEvent::Dismiss => None,
                    FieldEvent::Changed(_) | FieldEvent::Previous => return,
                };
                if let Some(line) = line {
                    area.update(cx, |area, cx| select_in_line(area, line.saturating_sub(1), 0..0, cx));
                }
                window.focus(&area.focus_handle(cx), cx);
                if let Some(editor) = this.editor_mut(&area) {
                    editor.go_to_line = None;
                }
                cx.notify();
            }
        });
        window.focus(&field.focus_handle(cx), cx);
        if let Some(editor) = self.editor_mut(&area) {
            editor.go_to_line = Some((field, events));
        }
        cx.notify();
    }

    /// 各编辑器的自动换行跟上 `wrap`。
    pub(super) fn wrap_editors(&self, cx: &mut Context<Self>) {
        let wrap = self.wrap;
        let areas: Vec<_> = self
            .workspaces
            .values()
            .flat_map(|tabs| &tabs.tabs)
            .filter_map(|tab| tab.editor.as_ref())
            .map(|editor| editor.area.clone())
            .collect();
        let window = self.window;
        // 这时多半正在处理这个窗口的事件，窗口要等这一轮过了才拿得到。
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    for area in areas {
                        area.update(cx, |area, cx| area.set_soft_wrap(wrap, window, cx));
                    }
                })
                .ok();
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn makefiles_are_recognized_by_name() {
        for path in ["/p/Makefile", "/p/GNUmakefile", "/p/rules.mk"] {
            assert_eq!(language_of(Path::new(path)), "make");
        }
        assert_eq!(language_of(Path::new("/p/a.RS")), "rs");
        assert_eq!(language_of(Path::new("/p/LICENSE")), "");
    }

    #[test]
    fn next_occurrence_wraps_and_skips_the_selection_itself() {
        let text = "ab 中 ab x ab";
        assert_eq!(next_occurrence(text, &(0..2), 2, false), Some(7..9));
        assert_eq!(next_occurrence(text, &(0..2), 9, false), Some(12..14));
        assert_eq!(next_occurrence(text, &(7..9), 14, false), Some(0..2));
        assert_eq!(next_occurrence(text, &(0..2), 14, false), None);
        assert_eq!(next_occurrence(text, &(3..6), 6, false), None);
        let text = "app apple happy app_ app";
        assert_eq!(next_occurrence(text, &(0..3), 3, false), Some(4..7));
        assert_eq!(next_occurrence(text, &(0..3), 3, true), Some(21..24));
        assert_eq!(next_occurrence(text, &(0..3), 24, true), None);
    }

    /// 存回去的字节保留文件原来的换行和 BOM，只改了一处的文件不会整篇都变。
    #[test]
    fn saving_keeps_line_endings_and_bom() {
        let dir = std::env::temp_dir().join(format!("runode-edit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        for original in ["\u{feff}one\r\ntwo\r\n", "one\ntwo", ""] {
            fs::write(&path, original).unwrap();
            let source = read_source(&ProjectHost::Local, &path).unwrap();
            assert!(!source.text.contains('\r'));
            assert_eq!(encode(&source.text, source.crlf, source.bom), original.as_bytes());
        }
        fs::write(&path, b"a\0b").unwrap();
        assert!(read_source(&ProjectHost::Local, &path).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn same_compares_across_chunks() {
        let text = "行\n".repeat(4000);
        let rope = Rope::from(text.as_str());
        assert!(same(&rope, &text));
        assert!(!same(&rope, &text.replacen('行', "列", 1)));
        assert!(!same(&rope, &text[..text.len() - 1]));
    }
}
