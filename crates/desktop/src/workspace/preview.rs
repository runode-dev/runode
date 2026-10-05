//! 右侧的预览栏：单击文件树里的文件在这里显示它。文本用终端的字体，带行号和相对 HEAD 的
//! 改动标记，能按行选中复制，语法高亮在后台做完再换上；图片按栏宽等比缩小；二进制、读不了、
//! 太大的文件只给一句说明。文件在磁盘上变了就重读。
//!
//! 读文件、判断类型和高亮在 `runode_preview`，这里只管状态、后台任务和画。

use std::{
    collections::HashMap,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, Focusable as _, FontStyle, FontWeight, HighlightStyle, Image, ImageSource,
    ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, Stateful,
    StyledText, UniformListScrollHandle, Window, div, img, prelude::*, px, uniform_list,
};
use runode_git_status::{self as git, LineKind, Section};
use runode_preview::{Content, ImageFormat, Span};
use runode_shared_types::{color::Rgb, theme};

use super::{
    WindowView,
    project::{ADDED, MODIFIED, REMOVED, RENAMED, panel_message, panel_shell},
};
use crate::{
    config::AppConfig,
    terminal_view::{Copy, SelectAll, hsla},
    tooltip::tooltip,
};

/// 行高比字号多出的部分。
const ROW_EXTRA_HEIGHT: f32 = 8.;
/// 一行最多画这么多列，再长的截掉；复制时仍是整行。
const MAX_COLUMNS: usize = 2000;
/// 改动标记的宽度；只删了行的地方在下一行顶上画一小段，这么高。
const MARK_WIDTH: f32 = 3.;
const REMOVED_MARK_HEIGHT: f32 = 5.;

/// 预览栏打开的文件和读到的内容。
pub(in crate::workspace) struct Preview {
    pub path: PathBuf,
    /// 打开时解析过符号链接的路径；监听到的事件里是真实路径，按两个都比一下。
    real_path: PathBuf,
    /// 读完之前为空；重读时先留着旧的，读完再换。
    content: Option<Loaded>,
    /// 上次读时文件的大小和修改时间，窗口切回前台时据此判断要不要重读。
    stamp: Option<(u64, SystemTime)>,
    /// 选中的行：按下的那行和现在拖到的那行，从 0 数。
    selection: Option<(usize, usize)>,
    /// 按着鼠标在拖选。
    selecting: bool,
    pub scroll: UniformListScrollHandle,
    /// 每读一次换一个新的，旧的置位让后台高亮停下。后台任务拿着读的那次的这一个，读完时
    /// 和这里的不是同一个（`Arc::ptr_eq`）就说明已经换了文件或又读了一次，丢掉结果。
    cancel: Arc<AtomicBool>,
    /// 行号旁的改动标记，由 `refresh_marks` 在 git 状态变了或重读了文件时重算，不每帧算。
    marks: HashMap<u32, Mark>,
}

enum Loaded {
    Text {
        /// 和后台高亮共用，不用再复制一份。
        lines: Arc<Vec<String>>,
        /// 文件太大，只读了前面这些行。
        truncated: bool,
        /// 后台高亮完之前为空；重读时留着上一次的，行对不上也只是颜色暂时不准。
        highlights: Option<Arc<Vec<Vec<Span>>>>,
        /// 最长的一行，列表按它的宽度横向滚动。
        widest: usize,
    },
    Image(Arc<Image>),
    Note(Note),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Note {
    Binary,
    TooLarge,
    Unreadable(String),
}

impl Preview {
    fn new(path: PathBuf) -> Self {
        let real_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        Self {
            path,
            real_path,
            content: None,
            stamp: None,
            selection: None,
            selecting: false,
            scroll: UniformListScrollHandle::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            marks: HashMap::new(),
        }
    }

    /// 按仓库的 git 状态 `git` 重算改动标记；不在仓库里或文件不在仓库下时没有标记。
    pub fn refresh_marks(&mut self, git: Option<&git::Snapshot>) {
        self.marks = git
            .and_then(|git| {
                let rel = self.path.strip_prefix(&git.root).ok().map(Path::to_path_buf).or_else(|| {
                    let root = fs::canonicalize(&git.root).ok()?;
                    self.real_path.strip_prefix(root).ok().map(Path::to_path_buf)
                })?;
                Some(line_marks(git, &rel))
            })
            .unwrap_or_default();
    }

    /// 监听到的这些路径里有没有正在预览的文件。
    pub fn affected_by(&self, paths: &[PathBuf]) -> bool {
        paths.iter().any(|path| *path == self.path || *path == self.real_path)
    }

    fn lines(&self) -> Option<&[String]> {
        match &self.content {
            Some(Loaded::Text { lines, .. }) => Some(lines),
            _ => None,
        }
    }

    fn selected_lines(&self) -> Option<Range<usize>> {
        let (anchor, head) = self.selection?;
        Some(anchor.min(head)..anchor.max(head) + 1)
    }

    /// 换下或关掉预览时调用：显示过的图片解码结果留在 GPUI 的全局缓存里，不清掉就一直占着内存。
    fn release_image(&self, cx: &mut App) {
        if let Some(Loaded::Image(image)) = &self.content {
            ImageSource::Image(image.clone()).remove_asset(cx);
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn file_stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn gpui_format(format: ImageFormat) -> gpui::ImageFormat {
    match format {
        ImageFormat::Png => gpui::ImageFormat::Png,
        ImageFormat::Jpeg => gpui::ImageFormat::Jpeg,
        ImageFormat::Gif => gpui::ImageFormat::Gif,
        ImageFormat::Webp => gpui::ImageFormat::Webp,
        ImageFormat::Bmp => gpui::ImageFormat::Bmp,
        ImageFormat::Tiff => gpui::ImageFormat::Tiff,
        ImageFormat::Ico => gpui::ImageFormat::Ico,
        ImageFormat::Svg => gpui::ImageFormat::Svg,
    }
}

/// 读到的内容换成预览栏存的样子，在后台做：文本包进 `Arc`、找出最长的行，UI 线程只管换上。
fn loaded(content: Content) -> Loaded {
    match content {
        Content::Text(text) => {
            let widest = widest_line(&text.lines);
            Loaded::Text { lines: Arc::new(text.lines), truncated: text.truncated, highlights: None, widest }
        }
        Content::Image { format, bytes } => Loaded::Image(Arc::new(Image::from_bytes(gpui_format(format), bytes))),
        Content::Binary => Loaded::Note(Note::Binary),
        Content::TooLarge => Loaded::Note(Note::TooLarge),
        Content::Unreadable(err) => Loaded::Note(Note::Unreadable(err)),
    }
}

/// 字符数最多的那一行；等宽字体下它最宽。
fn widest_line(lines: &[String]) -> usize {
    lines
        .iter()
        .enumerate()
        .max_by_key(|(_, line)| line.chars().count().min(MAX_COLUMNS))
        .map_or(0, |(ix, _)| ix)
}

/// 行号旁的改动标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum Mark {
    Added,
    Modified,
    /// 这一行上面删掉了行。
    Removed,
}

/// 一个文件的改动换算成新文件里每一行的标记，键是从 1 数的行号。一串连着的删除和新增里，
/// 有删除的新增行算修改；只删不增的标在删除处的下一行，删在末尾时标在新文件的最后一行之后。
fn diff_marks(diff: &git::FileDiff) -> HashMap<u32, Mark> {
    let mut marks = HashMap::new();
    for hunk in &diff.hunks {
        let mut removed = 0;
        let mut added = Vec::new();
        let mut last_new = 0;
        let flush = |removed: &mut usize, added: &mut Vec<u32>, next: u32, marks: &mut HashMap<u32, Mark>| {
            if added.is_empty() && *removed > 0 {
                marks.entry(next.max(1)).or_insert(Mark::Removed);
            }
            let mark = if *removed > 0 { Mark::Modified } else { Mark::Added };
            for line in added.drain(..) {
                marks.insert(line, mark);
            }
            *removed = 0;
        };
        for line in &hunk.lines {
            match line.kind {
                LineKind::Removed => removed += 1,
                LineKind::Added => added.extend(line.new),
                LineKind::Context => flush(&mut removed, &mut added, line.new.unwrap_or(last_new + 1), &mut marks),
            }
            if let Some(new) = line.new {
                last_new = new;
            }
        }
        flush(&mut removed, &mut added, last_new + 1, &mut marks);
    }
    marks
}

/// 暂存区里的第 `line` 行在工作区里是第几行；工作区里删掉了就为空。`unstaged` 是工作区相对
/// 暂存区的改动。
fn index_to_worktree(unstaged: &git::FileDiff, line: u32) -> Option<u32> {
    let mut delta: i64 = 0;
    for diff_line in unstaged.hunks.iter().flat_map(|hunk| &hunk.lines) {
        match diff_line.old {
            Some(old) if old == line => {
                return if diff_line.kind == LineKind::Context { diff_line.new } else { None };
            }
            Some(old) if old > line => break,
            _ => {}
        }
        match diff_line.kind {
            LineKind::Added => delta += 1,
            LineKind::Removed => delta -= 1,
            LineKind::Context => {}
        }
    }
    u32::try_from(i64::from(line) + delta).ok().filter(|line| *line > 0)
}

/// 工作区里这个文件相对 HEAD 改了哪些行：未暂存的改动直接用，已暂存的换算到工作区的行号上。
fn line_marks(snapshot: &git::Snapshot, rel: &Path) -> HashMap<u32, Mark> {
    let find = |section: Section| snapshot.files(section).iter().find(|file| file.path == rel);
    let unstaged = find(Section::Unstaged);
    let mut marks = HashMap::new();
    if let Some(staged) = find(Section::Staged) {
        for (line, mark) in diff_marks(staged) {
            let line = match unstaged {
                Some(unstaged) => index_to_worktree(unstaged, line),
                None => Some(line),
            };
            if let Some(line) = line {
                marks.insert(line, mark);
            }
        }
    }
    if let Some(unstaged) = unstaged {
        marks.extend(diff_marks(unstaged));
    }
    marks
}

/// 当前终端主题的 ANSI 16 色：默认配色上盖上配置里改过的那几项。
fn ansi_palette(cx: &App) -> [Rgb; 16] {
    let mut colors = theme::ANSI;
    for &(ix, rgb) in &cx.global::<AppConfig>().0.palette {
        if let Some(slot) = colors.get_mut(usize::from(ix)) {
            *slot = rgb;
        }
    }
    colors
}

fn highlight_style(style: runode_preview::Style, fg: Rgb, palette: &[Rgb; 16]) -> HighlightStyle {
    let color = match style.color {
        runode_preview::Color::Foreground => fg,
        runode_preview::Color::Ansi(ix) => palette[usize::from(ix.min(15))],
    };
    HighlightStyle {
        color: Some(hsla(color)),
        font_weight: style.bold.then_some(FontWeight::BOLD),
        font_style: style.italic.then_some(FontStyle::Italic),
        ..HighlightStyle::default()
    }
}

impl WindowView {
    pub(super) fn preview(&self) -> Option<&Preview> {
        self.workspace().project.preview.as_ref()
    }

    fn preview_mut(&mut self) -> Option<&mut Preview> {
        self.workspace_mut().project.preview.as_mut()
    }

    pub(super) fn preview_shown(&self) -> bool {
        self.preview().is_some()
    }

    /// 在预览栏里打开 `path`，替换原来预览的文件；就是这个文件时重读一次。
    pub(super) fn open_preview(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.preview().is_none_or(|preview| preview.path != path) {
            if let Some(old) = self.preview() {
                old.release_image(cx);
            }
            self.workspace_mut().project.preview = Some(Preview::new(path.to_path_buf()));
            self.sync_project_watch();
            self.refresh_project(cx);
        }
        self.load_preview(cx);
    }

    pub(super) fn close_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.preview_focus.is_focused(window);
        if let Some(old) = self.workspace_mut().project.preview.take() {
            old.release_image(cx);
        }
        self.sync_project_watch();
        if focused {
            window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// 窗口切回前台时，预览的文件变过就重读；在后台时监听到的改动只等到这时。
    pub(super) fn refresh_preview_if_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview()
            && preview.content.is_some()
            && file_stamp(&preview.path) != preview.stamp
        {
            self.load_preview(cx);
        }
    }

    /// 在后台读当前 workspace 预览的文件，读完换上；是文本时接着在后台高亮。
    pub(super) fn load_preview(&mut self, cx: &mut Context<Self>) {
        let id = self.workspace().id;
        let Some(preview) = self.preview_mut() else {
            return;
        };
        preview.cancel.store(true, Ordering::Relaxed);
        preview.cancel = Arc::new(AtomicBool::new(false));
        let cancel = preview.cancel.clone();
        let path = preview.path.clone();
        let job = cx.background_spawn({
            let path = path.clone();
            async move {
                let stamp = file_stamp(&path);
                (loaded(runode_preview::load(&path)), stamp)
            }
        });
        cx.spawn(async move |this, cx| {
            let (mut content, stamp) = job.await;
            let lines = this
                .update(cx, |this, cx| {
                    let (preview, git) = this.preview_for(id, &cancel)?;
                    preview.stamp = stamp;
                    preview.release_image(cx);
                    let old_highlights = match preview.content.take() {
                        Some(Loaded::Text { highlights, .. }) => highlights,
                        _ => None,
                    };
                    let lines = match &mut content {
                        Loaded::Text { lines, highlights, .. } => {
                            *highlights = old_highlights;
                            if let Some((anchor, head)) = &mut preview.selection {
                                let last = lines.len().saturating_sub(1);
                                *anchor = (*anchor).min(last);
                                *head = (*head).min(last);
                            }
                            Some(lines.clone())
                        }
                        _ => {
                            preview.selection = None;
                            None
                        }
                    };
                    preview.content = Some(content);
                    preview.refresh_marks(git);
                    cx.notify();
                    lines
                })
                .ok()
                .flatten();
            let Some(lines) = lines else {
                return;
            };
            let highlights = cx
                .background_spawn({
                    let cancel = cancel.clone();
                    async move { runode_preview::highlight(&path, &lines, &cancel) }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some((preview, _)) = this.preview_for(id, &cancel)
                    && let Some(Loaded::Text { highlights: slot, .. }) = &mut preview.content
                {
                    *slot = highlights.map(Arc::new);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// workspace `id` 的预览和它最近读到的 git 状态，预览还是拿着 `cancel` 的那次读的时才有。
    fn preview_for(
        &mut self,
        id: super::model::WorkspaceId,
        cancel: &Arc<AtomicBool>,
    ) -> Option<(&mut Preview, Option<&git::Snapshot>)> {
        let workspace = self.workspaces.iter_mut().find(|workspace| workspace.id == id)?;
        let project = &mut workspace.project;
        let preview = project.preview.as_mut().filter(|preview| Arc::ptr_eq(&preview.cancel, cancel))?;
        Some((preview, project.git.as_ref()))
    }

    fn copy_preview(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview() else {
            return;
        };
        if let (Some(lines), Some(range)) = (preview.lines(), preview.selected_lines()) {
            let text = lines.get(range).unwrap_or_default().join("\n");
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_all_preview(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview_mut()
            && let Some(count) = preview.lines().map(<[String]>::len)
        {
            preview.selection = Some((0, count.saturating_sub(1)));
            cx.notify();
        }
    }

    /// 在第 `ix` 行按下鼠标：按着 Shift 时把选区延到这一行，否则从这一行重新选。
    fn press_preview_line(&mut self, ix: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.preview_focus, cx);
        if let Some(preview) = self.preview_mut() {
            preview.selection = match preview.selection {
                Some((anchor, _)) if extend => Some((anchor, ix)),
                _ => Some((ix, ix)),
            };
            preview.selecting = true;
        }
        cx.notify();
    }

    /// 拖选时鼠标移到第 `ix` 行；松开了按键就停。
    fn drag_preview_line(&mut self, ix: usize, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(preview) = self.preview_mut().filter(|preview| preview.selecting) else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            preview.selecting = false;
            return;
        }
        if let Some((anchor, head)) = preview.selection
            && head != ix
        {
            preview.selection = Some((anchor, ix));
            cx.notify();
        }
    }

    pub(super) fn render_preview_panel(
        &self,
        width: f32,
        rightmost: bool,
        fg: Rgb,
        bg: Rgb,
        font: SharedString,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let preview = self.preview()?;
        let font_size = cx.global::<AppConfig>().0.preview_font_size;
        let dim = hsla(fg).opacity(0.5);
        let name = preview.path.file_name().map_or_else(|| preview.path.display().to_string(), |name| name.to_string_lossy().into_owned());
        let full_path = SharedString::from(preview.path.display().to_string());
        let title = div()
            .id("preview-title")
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(hsla(fg))
            .child(name)
            .tooltip(tooltip(full_path, None, fg, bg));
        let close = div()
            .id("preview-close")
            .flex_none()
            .size(px(20.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(dim)
            .hover(|close| close.bg(hsla(bg.mix(fg, 0.14))).text_color(hsla(fg)))
            .child("×")
            .tooltip(tooltip(rust_i18n::t!("tooltip.close_preview"), None, fg, bg))
            // 标题栏按下会拖动窗口，按钮自己接住。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_preview(window, cx);
                }),
            );
        let header = self.panel_header(rightmost, fg).child(title).child(close);
        let body: AnyElement = match &preview.content {
            None => div().flex_1().into_any_element(),
            Some(Loaded::Note(note)) => {
                let text = match note {
                    Note::Binary => rust_i18n::t!("preview.binary").into_owned(),
                    Note::TooLarge => rust_i18n::t!("preview.too_large").into_owned(),
                    Note::Unreadable(err) => rust_i18n::t!("preview.unreadable", error = err).into_owned(),
                };
                panel_message(text, fg).into_any_element()
            }
            Some(Loaded::Image(image)) => div()
                .flex_1()
                .min_h_0()
                .p(px(12.))
                .flex()
                .justify_center()
                .items_start()
                .child(img(image.clone()).max_w_full().max_h_full())
                .into_any_element(),
            Some(Loaded::Text { lines, truncated, widest, .. }) => {
                let count = lines.len() + usize::from(*truncated);
                let digits = lines.len().to_string().len();
                // 行号一栏按位数定宽，等宽字体一个数字大约 0.6 个字号宽。
                let gutter = (digits as f32 * font_size * 0.62 + 16.).ceil();
                uniform_list(
                    "preview",
                    count,
                    cx.processor(move |this, range: Range<usize>, _, cx| this.render_preview_rows(range, font_size, gutter, fg, bg, cx)),
                )
                .with_width_from_item(Some(*widest))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .track_scroll(&preview.scroll)
                .flex_1()
                .font_family(font)
                .into_any_element()
            }
        };
        Some(
            panel_shell("preview-panel", width, fg)
                .key_context("Preview")
                .track_focus(&self.preview_focus)
                .on_action(cx.listener(Self::copy_preview))
                .on_action(cx.listener(Self::select_all_preview))
                .bg(hsla(bg))
                .text_size(px(font_size))
                .child(header)
                .child(body),
        )
    }

    /// 预览栏的行，行高跟着字号 `font_size` 缩放。
    fn render_preview_rows(
        &self,
        range: Range<usize>,
        font_size: f32,
        gutter: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(preview) = self.preview() else {
            return Vec::new();
        };
        let Some(Loaded::Text { lines, highlights, .. }) = &preview.content else {
            return Vec::new();
        };
        let marks = &preview.marks;
        let palette = ansi_palette(cx);
        let selected = preview.selected_lines().unwrap_or(0..0);
        let selected_bg = hsla(bg.mix(RENAMED, 0.30));
        let dim = hsla(fg).opacity(0.4);
        let last = lines.len();
        let row_height = font_size + ROW_EXTRA_HEIGHT;
        range
            .map(|ix| {
                let row = div().flex_none().h(px(row_height)).w_full().flex().items_center().whitespace_nowrap();
                let Some(line) = lines.get(ix) else {
                    // 截断了的文件末尾多一行说明。
                    return row
                        .pl(px(gutter + MARK_WIDTH + 8.))
                        .italic()
                        .text_color(dim)
                        .child(rust_i18n::t!("preview.truncated", count = last).into_owned())
                        .into_any_element();
                };
                let number = u32::try_from(ix + 1).unwrap_or(u32::MAX);
                // 删在文件末尾的标记落在最后一行之后，挪到最后一行上。
                let mark = marks.get(&number).copied().or_else(|| {
                    (ix + 1 == last).then(|| marks.get(&(number + 1)).copied().filter(|m| *m == Mark::Removed)).flatten()
                });
                let marker = div().flex_none().w(px(MARK_WIDTH)).h_full().flex().flex_col().children(mark.map(|mark| {
                    let (color, height) = match mark {
                        Mark::Added => (ADDED, row_height),
                        Mark::Modified => (MODIFIED, row_height),
                        Mark::Removed => (REMOVED, REMOVED_MARK_HEIGHT),
                    };
                    div().w_full().h(px(height)).bg(hsla(color))
                }));
                let spans = highlights.as_ref().and_then(|all| all.get(ix)).map_or(&[][..], Vec::as_slice);
                let shown = runode_preview::display_line(line, spans, MAX_COLUMNS);
                let runs: Vec<_> =
                    shown.spans.iter().map(|span| (span.range.clone(), highlight_style(span.style, fg, &palette))).collect();
                let mut content = shown.text;
                if shown.cut {
                    content.push('…');
                }
                row.id(("preview-line", ix))
                    .when(selected.contains(&ix), |row| row.bg(selected_bg))
                    .child(marker)
                    .child(
                        div()
                            .flex_none()
                            .w(px(gutter))
                            .pr(px(10.))
                            .flex()
                            .justify_end()
                            .text_color(dim)
                            .child(number.to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .pr(px(12.))
                            .text_color(hsla(fg))
                            .child(StyledText::new(content).with_highlights(runs)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_preview_line(ix, event.modifiers.shift, window, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        this.drag_preview_line(ix, event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| {
                            if let Some(preview) = this.preview_mut() {
                                preview.selecting = false;
                            }
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use git::{FileDiff, FileStatus, Hunk, Line};

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> Line {
        Line { kind, old, new, text: String::new() }
    }

    fn diff(lines: Vec<Line>) -> FileDiff {
        FileDiff {
            path: "a.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            added: 0,
            removed: 0,
            hunks: vec![Hunk { header: String::new(), lines }],
            binary: false,
            truncated: false,
        }
    }

    use LineKind::{Added as A, Context as C, Removed as R};

    #[test]
    fn marks_added_modified_and_removed_lines() {
        let file = diff(vec![
            line(C, Some(1), Some(1)),
            line(R, Some(2), None),
            line(A, None, Some(2)),
            line(C, Some(3), Some(3)),
            line(A, None, Some(4)),
            line(C, Some(4), Some(5)),
            line(R, Some(5), None),
            line(C, Some(6), Some(6)),
            line(R, Some(7), None),
        ]);
        let marks = diff_marks(&file);
        assert_eq!(marks.get(&2), Some(&Mark::Modified));
        assert_eq!(marks.get(&4), Some(&Mark::Added));
        assert_eq!(marks.get(&6), Some(&Mark::Removed));
        // 删在末尾：标在最后一行之后，画的时候挪到最后一行。
        assert_eq!(marks.get(&7), Some(&Mark::Removed));
        assert_eq!(marks.len(), 4);
    }

    #[test]
    fn maps_index_lines_through_unstaged_changes() {
        // 工作区在第 1 行后加了两行，删了原来的第 4 行。
        let unstaged = diff(vec![
            line(C, Some(1), Some(1)),
            line(A, None, Some(2)),
            line(A, None, Some(3)),
            line(C, Some(2), Some(4)),
            line(C, Some(3), Some(5)),
            line(R, Some(4), None),
            line(C, Some(5), Some(6)),
        ]);
        assert_eq!(index_to_worktree(&unstaged, 1), Some(1));
        assert_eq!(index_to_worktree(&unstaged, 2), Some(4));
        assert_eq!(index_to_worktree(&unstaged, 4), None);
        assert_eq!(index_to_worktree(&unstaged, 5), Some(6));
        // 改动块之外的行按前面增减的行数平移。
        assert_eq!(index_to_worktree(&unstaged, 20), Some(21));
    }

    #[test]
    fn combines_staged_and_unstaged_marks() {
        let mut staged = diff(vec![line(C, Some(1), Some(1)), line(A, None, Some(2)), line(C, Some(2), Some(3))]);
        staged.path = "a.rs".into();
        let unstaged = diff(vec![line(A, None, Some(1)), line(C, Some(1), Some(2)), line(C, Some(2), Some(3))]);
        let snapshot = git::Snapshot {
            root: "/repo".into(),
            git_dir: "/repo/.git".into(),
            staged: vec![staged],
            unstaged: vec![unstaged],
            statuses: HashMap::new(),
            ignored: HashSet::new(),
        };
        let marks = line_marks(&snapshot, Path::new("a.rs"));
        // 工作区新加的第 1 行，以及暂存区里加的第 2 行，在工作区里是第 3 行。
        assert_eq!(marks.get(&1), Some(&Mark::Added));
        assert_eq!(marks.get(&3), Some(&Mark::Added));
        assert_eq!(marks.len(), 2);
    }

    #[test]
    fn picks_the_widest_line() {
        let lines: Vec<String> = ["ab", "abcd", "中文字"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(widest_line(&lines), 1);
        assert_eq!(widest_line(&[]), 0);
    }
}
