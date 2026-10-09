//! 右侧的预览栏：文件树里打开的文件在这里显示，一个文件一个标签。单击打开的是临时标签，标题
//! 斜体，再打开别的文件时被换掉；双击文件或标签把它固定下来。标签能拖动换位置，中键关掉，右键
//! 有关闭其他、关闭右侧这些。文本用终端的字体，带行号和相对 HEAD 的改动标记，能按行选中复制，
//! 语法高亮在后台做完再换上；图片按栏宽等比缩小；二进制、读不了、太大的文件只给一句说明。
//! 文件在磁盘上变了就重读，没显示的标签等切过去时再看要不要重读。Markdown 文件默认显示排版后的
//! 样子，在后台解析，见 `markdown`。
//!
//! 从 Git 面板点开的是 diff 标签，和同一个文件的普通标签分开，见 `diff`。
//!
//! 读文件、判断类型和高亮在 `runode_preview`，这里只管状态、后台任务和画：标签条在 `tabs`，正文的
//! 行和图片在 `body`，行号旁的改动标记在 `marks`，自动换行在 `wrap`，Markdown 的排版视图在 `markdown`。
//!
//! 标签条右边能打开自动换行、Markdown 文件在排版和源码之间切（这两个整个窗口一份，进存档），以及
//! 把预览栏放大到盖住终端区（每个 workspace 各自的，不进存档）。

mod body;
mod diff;
mod markdown;
mod marks;
mod tabs;
mod wrap;

use std::{
    borrow::Cow,
    cell::Cell,
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
    AccessibleAction, Action, App, ClipboardItem, Context, Div, Focusable as _, Image, ImageSource, MouseButton,
    MouseMoveEvent, RenderImage, Role, SMOOTH_SVG_SCALE_FACTOR, ScrollHandle, ScrollStrategy, SharedString, Stateful,
    SvgRenderer, UniformListScrollHandle, Window, div, linear_color_stop, linear_gradient, prelude::*, px,
};
use runode_git::{self as git, FileStatus, Section};
use runode_preview::{Content, ImageFormat, Span};
use runode_shared_types::color::Rgb;

pub(in crate::window) use diff::DiffTarget;
pub(in crate::window) use tabs::PreviewTabs;

use super::{
    TogglePaneZoom, WindowView,
    model::base_name,
    project::{PANEL_TOGGLES_INSET, panel_shell},
    titlebar::icon_toggle,
};
use crate::{
    assets::{CODE_ICON, MAXIMIZE_ICON, MINIMIZE_ICON, WRAP_ICON},
    config::AppConfig,
    ui::{
        actions::{Copy, SelectAll},
        hsla,
        tooltip::tooltip,
    },
};
use marks::{Mark, line_marks};
use tabs::tab_underline;

/// 行高比字号多出的部分。
const ROW_EXTRA_HEIGHT: f32 = 8.;
/// 一行最多画这么多列，再长的截掉；复制时仍是整行。
const MAX_COLUMNS: usize = 2000;
/// 正文上下留的空。
const BODY_PADDING: f32 = 6.;
/// 长行右边缘渐隐的宽度。
const FADE_WIDTH: f32 = 24.;

/// 预览栏打开的文件和读到的内容。
pub(in crate::window) struct Preview {
    pub path: PathBuf,
    /// 不为空时这是 `path` 的 diff 标签，和同一个文件的普通标签是两个。
    pub diff: Option<DiffTarget>,
    /// diff 标签：扫描到 git 状态变了，下次显示时重读。
    diff_stale: bool,
    /// 固定的标签；临时标签为假，下一个打开的文件会换掉它。
    pub pinned: bool,
    /// 文件的 git 状态，标签名按它上色；和 `marks` 一起重算。
    status: Option<FileStatus>,
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
    /// Markdown 排版视图的滚动位置、选区和下好的网络图片；重读时留着。
    markdown_view: markdown::View,
    /// 每读一次换一个新的，旧的置位让后台高亮停下。后台任务拿着读的那次的这一个，读完时
    /// 和这里的不是同一个（`Arc::ptr_eq`）就说明已经换了文件或又读了一次，丢掉结果。
    cancel: Arc<AtomicBool>,
    /// 行号旁的改动标记，由 `refresh_marks` 在 git 状态变了或重读了文件时重算，不每帧算。
    marks: HashMap<u32, Mark>,
    /// 要滚到的条目（文本的行或 diff 的行）和滚到哪儿。等画的时候再滚：内容可能还没读完，
    /// 换行时条目在第几行也要那时才知道。
    reveal: Cell<Option<(usize, ScrollStrategy)>>,
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
        /// 自动换行时折好的各段。
        wrap: wrap::WrapCache,
        /// Markdown 文件在后台解析、摊平好的排版视图，解析完之前为空；重读时和高亮一样先留着上一次的。
        markdown: Option<Arc<markdown::Doc>>,
    },
    Image(Arc<Image>),
    /// SVG 在后台画好的位图，太小的已经放大过。
    Svg(Arc<RenderImage>),
    /// diff 标签里改动的图片：改之前和改之后，新加的没有旧图，删掉的没有新图。
    ImageDiff {
        old: Option<Arc<Image>>,
        new: Option<Arc<Image>>,
    },
    /// diff 标签读到的整篇 diff。
    Diff(diff::DiffContent),
    Note(Note),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Note {
    Binary,
    TooLarge,
    Unreadable(String),
    /// diff 标签：文件已经没有这一种改动了（比如全暂存了）。
    NoChanges,
    /// diff 标签：只改了权限或者只改了名，内容没变。
    NoContent,
    /// diff 标签：改动太多或者文件太大，不显示。
    DiffTooLarge,
}

impl Preview {
    fn new(path: PathBuf, diff: Option<DiffTarget>, pinned: bool) -> Self {
        let real_path = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        Self {
            path,
            diff,
            diff_stale: false,
            pinned,
            status: None,
            real_path,
            content: None,
            stamp: None,
            selection: None,
            selecting: false,
            scroll: UniformListScrollHandle::new(),
            markdown_view: markdown::View::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            marks: HashMap::new(),
            reveal: Cell::new(None),
        }
    }

    /// 按 git 状态 `git` 重算改动标记和标签上的状态：子模块和嵌套仓库里的文件按它们自己那个
    /// 仓库算；不在仓库里或文件不在仓库下时都没有。
    pub fn refresh_marks(&mut self, git: Option<&git::Repos>) {
        (self.marks, self.status) = git
            .and_then(|git| {
                let rel = self.path.strip_prefix(&git.main.root).ok().map(Path::to_path_buf).or_else(|| {
                    let root = fs::canonicalize(&git.main.root).ok()?;
                    self.real_path.strip_prefix(root).ok().map(Path::to_path_buf)
                })?;
                let (repo, rel) = git.locate(&rel);
                Some((line_marks(repo, rel), repo.statuses.get(rel).copied()))
            })
            .unwrap_or_default();
    }

    /// 扫描到 git 状态从 `old` 变成了 `new`：重算改动标记；工作区和暂存区的 diff 标签在这个文件的
    /// 那一段改动变了时记下要重读，别的文件、别的仓库变了不管；提交里的不会变。
    pub fn git_changed(&mut self, old: Option<&git::Repos>, new: Option<&git::Repos>) {
        self.refresh_marks(new);
        if let Some(diff) = &self.diff
            && diff_file(old, diff) != diff_file(new, diff)
        {
            self.diff_stale = true;
        }
    }

    /// 标签上的名字；diff 标签带上和什么比：「a.rs（工作区）」「a.rs（已暂存）」「a.rs @ 1a2b3c4」。
    fn name(&self) -> SharedString {
        let name = base_name(&self.path);
        match self.diff.as_ref().map(|diff| &diff.side) {
            None => name,
            Some(git::DiffSide::Worktree) => rust_i18n::t!("preview.diff.worktree_tab", name = name).into_owned(),
            Some(git::DiffSide::Index) => rust_i18n::t!("preview.diff.staged_tab", name = name).into_owned(),
            Some(git::DiffSide::Commit { id, .. }) => format!("{name} @ {}", id.get(..7).unwrap_or(id)),
        }
        .into()
    }

    /// 监听到的这些路径里有没有正在预览的文件。diff 标签不看文件事件：文件变了会带来一次扫描，
    /// 扫描结果里这个文件的改动变了才重读，见 `git_changed`。
    pub fn affected_by(&self, paths: &[PathBuf]) -> bool {
        self.diff.is_none() && paths.iter().any(|path| *path == self.path || *path == self.real_path)
    }

    /// 这个标签要不要重读：还没读过；普通标签是文件在磁盘上变过，diff 标签是 git 状态变过。
    fn stale(&self) -> bool {
        self.content.is_none()
            || if self.diff.is_some() { self.diff_stale } else { file_stamp(&self.path) != self.stamp }
    }

    fn lines(&self) -> Option<&[String]> {
        match &self.content {
            Some(Loaded::Text { lines, .. }) => Some(lines),
            _ => None,
        }
    }

    /// 画的时候滚到等着的条目；换行时 `wrapped` 把条目换成它的第一段。
    fn apply_reveal(&self, wrapped: Option<&wrap::Wrapped>) {
        if let Some((item, strategy)) = self.reveal.take() {
            let row = wrapped.map_or(item, |wrapped| wrapped.span(item).start);
            self.scroll.scroll_to_item(row, strategy);
        }
    }

    fn selected_lines(&self) -> Option<Range<usize>> {
        let (anchor, head) = self.selection?;
        Some(anchor.min(head)..anchor.max(head) + 1)
    }

    /// 换下或关掉预览时调用：显示过的图片解码结果留在 GPUI 的全局缓存里，不清掉就一直占着内存。
    fn release_image(&self, cx: &mut App) {
        if let Some(content) = &self.content {
            content.release(cx);
        }
    }
}

impl Loaded {
    /// 放掉这份内容里图片的解码结果，见 `Preview::release_image`。
    fn release(&self, cx: &mut App) {
        match self {
            Loaded::Image(image) => ImageSource::Image(image.clone()).remove_asset(cx),
            Loaded::Svg(image) => cx.drop_image(image.clone(), None),
            Loaded::Text { markdown: Some(doc), .. } => doc.release(cx),
            Loaded::ImageDiff { old, new } => {
                for image in old.iter().chain(new) {
                    ImageSource::Image(image.clone()).remove_asset(cx);
                }
            }
            _ => {}
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// `git` 里 diff 标签 `diff` 看的那个文件在那一段的改动；提交里的为空（不随扫描变）。
fn diff_file<'a>(git: Option<&'a git::Repos>, diff: &DiffTarget) -> Option<&'a git::FileDiff> {
    let section = match diff.side {
        git::DiffSide::Worktree => Section::Unstaged,
        git::DiffSide::Index => Section::Staged,
        git::DiffSide::Commit { .. } => return None,
    };
    let repo = git?.iter().find(|repo| repo.root == diff.root)?;
    repo.files(section).iter().find(|file| file.path == diff.rel)
}

/// 长行往右还有内容时盖在右边缘的渐隐，提示能横着滚；滚到头了或者没有长行时没有。
fn right_fade(handle: &ScrollHandle, bg: Rgb) -> Option<Div> {
    let (offset, max) = (handle.offset().x, handle.max_offset().x);
    (max > px(1.) && -offset < max - px(1.)).then(|| {
        div().absolute().top_0().right_0().h_full().w(px(FADE_WIDTH)).bg(linear_gradient(
            90.,
            linear_color_stop(hsla(bg).opacity(0.), 0.),
            linear_color_stop(hsla(bg), 1.),
        ))
    })
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

/// SVG 按自身尺寸画太小时（图标常是 16×16）放大到长边有这么多逻辑像素，放大是重画不是拉伸，
/// 线条照样清楚。
const SVG_MIN_SIDE: f32 = 256.;

/// 画 SVG：先按自身尺寸画，太小的按 `SVG_MIN_SIDE` 再画一遍。
fn render_svg(renderer: &SvgRenderer, bytes: &[u8]) -> Result<Arc<RenderImage>, String> {
    let image = renderer.render_single_frame(bytes, 1.).map_err(|err| err.to_string())?;
    // 按 1 倍画出来的像素是逻辑尺寸的 `SMOOTH_SVG_SCALE_FACTOR` 倍。
    let size = image.size(0);
    let longest = size.width.0.max(size.height.0) as f32 / SMOOTH_SVG_SCALE_FACTOR;
    if longest <= 0. || longest >= SVG_MIN_SIDE {
        return Ok(image);
    }
    renderer.render_single_frame(bytes, SVG_MIN_SIDE / longest).map_err(|err| err.to_string())
}

/// 读到的内容换成预览栏存的样子，在后台做：文本包进 `Arc`、找出最长的行、画好 SVG，UI 线程只管换上。
fn loaded(content: Content, svg: &SvgRenderer) -> Loaded {
    match content {
        Content::Text(text) => {
            let widest = widest_line(&text.lines);
            Loaded::Text {
                lines: Arc::new(text.lines),
                truncated: text.truncated,
                highlights: None,
                widest,
                wrap: wrap::WrapCache::default(),
                markdown: None,
            }
        }
        Content::Image { format: ImageFormat::Svg, bytes } => match render_svg(svg, &bytes) {
            Ok(image) => Loaded::Svg(image),
            Err(err) => Loaded::Note(Note::Unreadable(err)),
        },
        Content::Image { format, bytes } => Loaded::Image(Arc::new(Image::from_bytes(gpui_format(format), bytes))),
        Content::Binary => Loaded::Note(Note::Binary),
        Content::TooLarge => Loaded::Note(Note::TooLarge),
        Content::Unreadable(err) => Loaded::Note(Note::Unreadable(err)),
    }
}

/// 字符数最多的那一行；等宽字体下它最宽。
fn widest_line(lines: &[String]) -> usize {
    lines.iter().enumerate().max_by_key(|(_, line)| line.chars().count().min(MAX_COLUMNS)).map_or(0, |(ix, _)| ix)
}

impl WindowView {
    /// 当前预览标签的文件。
    pub(super) fn preview(&self) -> Option<&Preview> {
        self.workspace().project.previews.active()
    }

    fn preview_mut(&mut self) -> Option<&mut Preview> {
        self.workspace_mut().project.previews.active_mut()
    }

    pub(super) fn preview_shown(&self) -> bool {
        !self.workspace().project.previews.tabs.is_empty()
    }

    /// 预览栏放大着，盖在终端区上。
    pub(super) fn preview_maximized(&self) -> bool {
        self.preview_shown() && self.workspace().project.previews.maximized
    }

    /// 预览栏在终端区右边占一栏：开着又没放大。
    pub(super) fn preview_in_column(&self) -> bool {
        self.preview_shown() && !self.workspace().project.previews.maximized
    }

    /// 把预览栏放大到盖住终端区，或者还原。放大时焦点交给预览栏，键盘不再落到被盖住的终端里。
    pub(super) fn toggle_preview_maximized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previews = &mut self.workspace_mut().project.previews;
        previews.maximized = !previews.maximized;
        if previews.maximized {
            window.focus(&self.preview_focus, cx);
        }
        cx.notify();
    }

    /// 切换自动换行。换行时列表不能横着滚，横向滚过的先滚回左边，不然文字一直错开着。
    fn toggle_preview_wrap(&mut self, cx: &mut Context<Self>) {
        self.preview_wrap = !self.preview_wrap;
        for preview in self.workspaces.iter().flat_map(|workspace| &workspace.project.previews.tabs) {
            let handle = preview.scroll.0.borrow().base_handle.clone();
            handle.set_offset(gpui::point(px(0.), handle.offset().y));
        }
        self.save(cx);
        cx.notify();
    }

    /// Markdown 文件在排版和源码之间切。
    fn toggle_preview_source(&mut self, cx: &mut Context<Self>) {
        self.preview_source = !self.preview_source;
        self.save(cx);
        cx.notify();
    }

    /// 在预览栏里打开 `path` 并切到它的标签：`pin` 时开成固定标签，否则开成临时标签；已经开着
    /// 时重读一次。文件树跟着定位到它。
    pub(super) fn open_preview(&mut self, path: &Path, pin: bool, cx: &mut Context<Self>) {
        self.open_tab(path, None, pin, cx);
    }

    /// 和 `open_preview` 一样打开 `path`，再选中第 `line` 行（从 0 数）、滚到中间。
    pub(super) fn open_preview_at(&mut self, path: &Path, line: usize, pin: bool, cx: &mut Context<Self>) {
        self.open_preview(path, pin, cx);
        if let Some(preview) = self.preview_mut().filter(|preview| preview.path == path) {
            preview.selection = Some((line, line));
            preview.reveal.set(Some((line, ScrollStrategy::Center)));
        }
    }

    /// 打开 `path` 的普通标签（`diff` 为空）或 diff 标签。
    fn open_tab(&mut self, path: &Path, diff: Option<DiffTarget>, pin: bool, cx: &mut Context<Self>) {
        let shown = self.preview_shown();
        if let Some(old) = self.preview().filter(|old| old.path != path || old.diff != diff) {
            old.release_image(cx);
        }
        // 提交里的文件工作区里不一定还有，文件树不跟过去。
        let reveal = diff.as_ref().is_none_or(|diff| !matches!(diff.side, git::DiffSide::Commit { .. }));
        let previews = &mut self.workspace_mut().project.previews;
        let replaced = previews.open(path, diff, pin);
        previews.scroll.scroll_to_item(previews.active);
        if let Some(old) = replaced {
            old.release_image(cx);
        }
        if !shown {
            self.sync_project_watch();
            self.refresh_project(cx);
        }
        if reveal {
            self.reveal_in_tree(path);
        }
        self.load_preview(cx);
    }

    /// 文件树跟着展开到预览的文件，选中它、滚到能看见。
    fn reveal_in_tree(&mut self, path: &Path) {
        self.with_tree(|project, root, filter| project.reveal_file(path, root, filter));
    }

    /// 切到第 `ix` 个预览标签：文件树跟着定位到它，还没读过或者磁盘上变过就读。
    fn activate_preview(&mut self, ix: usize, cx: &mut Context<Self>) {
        let previews = &mut self.workspace_mut().project.previews;
        let Some(tab) = previews.tabs.get(ix) else {
            return;
        };
        let (path, stale) = (tab.path.clone(), tab.stale());
        // 换下去的标签不显示了，图片的解码结果先放掉，切回来时再解码。
        if ix != previews.active
            && let Some(old) = previews.active()
        {
            old.release_image(cx);
        }
        previews.active = ix;
        previews.scroll.scroll_to_item(ix);
        self.reveal_in_tree(&path);
        if stale {
            self.load_preview(cx);
        }
        cx.notify();
    }

    /// 只留下 `keep` 为真的预览标签，当前标签关掉了就切到旁边的。都关掉时预览栏收起，焦点在
    /// 预览栏上的交回终端。
    pub(super) fn retain_previews(
        &mut self,
        keep: impl FnMut(usize, &Preview) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previews = &mut self.workspace_mut().project.previews;
        let before = previews.active().map(|tab| (tab.path.clone(), tab.diff.clone()));
        let closed = previews.retain(keep);
        if closed.is_empty() {
            return;
        }
        for tab in &closed {
            tab.release_image(cx);
        }
        if !self.preview_shown() {
            self.sync_project_watch();
            if self.preview_focus.is_focused(window) {
                window.focus(&self.focus_handle(cx), cx);
            }
        } else if self.preview().map(|tab| (tab.path.clone(), tab.diff.clone())) != before {
            self.activate_preview(self.workspace().project.previews.active, cx);
        }
        cx.notify();
    }

    /// `from` 改了名或挪到了 `to`：它和它下面的文件的预览标签跟过去，当前标签换了就重读。
    pub(super) fn move_previews(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for old in self.workspace_mut().project.previews.moved(from, to) {
            old.release_image(cx);
        }
        if self.preview().is_some_and(|tab| tab.content.is_none()) {
            self.load_preview(cx);
        }
    }

    /// 窗口切回前台时，当前预览的文件变过就重读；在后台时监听到的改动只等到这时。别的标签等
    /// 切过去时再看。
    pub(super) fn refresh_preview_if_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview()
            && preview.content.is_some()
            && preview.stale()
        {
            self.load_preview(cx);
        }
    }

    /// 在后台读当前 workspace 预览的文件，读完换上；是文本时接着在后台高亮。diff 标签读 diff。
    pub(super) fn load_preview(&mut self, cx: &mut Context<Self>) {
        if self.preview().is_some_and(|preview| preview.diff.is_some()) {
            self.load_diff(cx);
            return;
        }
        let id = self.workspace().id;
        let Some(preview) = self.preview_mut() else {
            return;
        };
        preview.cancel.store(true, Ordering::Relaxed);
        preview.cancel = Arc::new(AtomicBool::new(false));
        let cancel = preview.cancel.clone();
        let path = preview.path.clone();
        let is_markdown = runode_preview::is_markdown(&path);
        let svg = cx.svg_renderer();
        let job = cx.background_spawn({
            let path = path.clone();
            let svg = svg.clone();
            async move {
                let stamp = file_stamp(&path);
                (loaded(runode_preview::load(&path), &svg), stamp)
            }
        });
        cx.spawn(async move |this, cx| {
            let (mut content, stamp) = job.await;
            let lines = this
                .update(cx, |this, cx| {
                    let (preview, git) = this.preview_for(id, &cancel)?;
                    preview.stamp = stamp;
                    // 排版视图的旧文档在新文档解析完之前还显示着，它的图片等换上新文档时再按用没用到放。
                    let (old_highlights, old_markdown) = match preview.content.take() {
                        Some(Loaded::Text { highlights, markdown, .. }) => (highlights, markdown),
                        old => {
                            if let Some(old) = old {
                                old.release(cx);
                            }
                            (None, None)
                        }
                    };
                    let lines = match &mut content {
                        Loaded::Text { lines, highlights, markdown, .. } => {
                            *highlights = old_highlights;
                            *markdown = old_markdown;
                            if let Some((anchor, head)) = &mut preview.selection {
                                let last = lines.len().saturating_sub(1);
                                *anchor = (*anchor).min(last);
                                *head = (*head).min(last);
                            }
                            Some(lines.clone())
                        }
                        _ => {
                            if let Some(old) = old_markdown {
                                old.release(cx);
                            }
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
            if is_markdown {
                let doc = cx
                    .background_spawn({
                        let (cancel, lines, path) = (cancel.clone(), lines.clone(), path.clone());
                        async move {
                            let blocks = runode_preview::parse_markdown(&lines.join("\n"), &cancel)?;
                            let dir = path.parent().unwrap_or(Path::new("/"));
                            Some(markdown::Doc::new(blocks, dir, |path| markdown::picture(path, &svg)))
                        }
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if let Some(doc) = doc
                        && let Some((preview, _)) = this.preview_for(id, &cancel)
                        && let Some(Loaded::Text { markdown: slot, .. }) = &mut preview.content
                    {
                        let font_size = cx.global::<AppConfig>().0.preview_font_size;
                        preview.markdown_view.replace_doc(slot.as_deref(), &doc, font_size);
                        let doc = Arc::new(doc);
                        if let Some(old) = slot.replace(doc.clone()) {
                            old.release_unused(&doc, cx);
                        }
                        this.load_remote_images(id, &cancel, cx);
                        cx.notify();
                    }
                })
                .ok();
            }
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

    /// workspace `id` 里拿着 `cancel` 的那个预览标签和最近读到的 git 状态；标签关掉了、换了文件
    /// 或又读了一次时为空。
    fn preview_for(
        &mut self,
        id: super::model::WorkspaceId,
        cancel: &Arc<AtomicBool>,
    ) -> Option<(&mut Preview, Option<&git::Repos>)> {
        let workspace = self.workspaces.iter_mut().find(|workspace| workspace.id == id)?;
        let project = &mut workspace.project;
        let preview = project.previews.tabs.iter_mut().find(|preview| Arc::ptr_eq(&preview.cancel, cancel))?;
        Some((preview, project.git.as_ref()))
    }

    fn copy_preview(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview() else {
            return;
        };
        // 排版视图复制选中的文字；什么也没选时不复制，不会复制到看不见的源码。
        if self.markdown_shown(preview) {
            if let Some(text) = self.markdown_selected_text(preview) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            return;
        }
        if let (Some(lines), Some(range)) = (preview.lines(), preview.selected_lines()) {
            let text = lines.get(range).unwrap_or_default().join("\n");
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_all_preview(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.preview().is_some_and(|preview| self.markdown_shown(preview)) {
            self.select_all_markdown(cx);
            return;
        }
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_preview_panel(
        &self,
        width: f32,
        rightmost: bool,
        fg: Rgb,
        bg: Rgb,
        font: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let preview = self.preview()?;
        let font_size = cx.global::<AppConfig>().0.preview_font_size;
        let previews = &self.workspace().project.previews;
        // 标签条后面的空白处和标题栏一样能拖动窗口。
        let strip = div()
            .id("preview-tabs")
            .role(Role::TabList)
            .flex_initial()
            .min_w_0()
            .h_full()
            .flex()
            .overflow_x_scroll()
            .track_scroll(&previews.scroll)
            .children((0..previews.tabs.len()).map(|ix| self.render_preview_tab(ix, fg, bg, cx)).collect::<Vec<_>>());
        // 标签下面那条分隔线由各个标签和后面的空白各画一段，当前标签底下空着，和正文连在一起。
        let maximized = previews.maximized;
        let button =
            |id: &'static str, icon: &'static str, on: bool, text: Cow<'static, str>, action: Option<&dyn Action>| {
                icon_toggle(id, icon, 13., on, fg, bg)
                    .role(Role::Button)
                    .aria_label(SharedString::from(text.clone()))
                    .flex_none()
                    .size(px(22.))
                    .tooltip(tooltip(text, action, fg, bg))
            };
        // 这几个按钮只有按下的处理，辅助工具按不到，另外登记按下时做的事。
        let press = |f: fn(&mut Self, &mut Window, &mut Context<Self>), cx: &mut Context<Self>| {
            let view = cx.entity().downgrade();
            move |_: Option<&gpui::accesskit::ActionData>, window: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| f(this, window, cx)).ok();
            }
        };
        let is_markdown = preview.diff.is_none() && runode_preview::is_markdown(&preview.path);
        let typeset = self.markdown_shown(preview);
        let buttons = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .when(is_markdown, |buttons| {
                let text =
                    if typeset { rust_i18n::t!("preview.show_source") } else { rust_i18n::t!("preview.show_rendered") };
                // 悬停提示说按了会怎样，跟着状态换；报给辅助工具的名字不换，开着表示正显示源码。
                buttons.child(
                    button("preview-source", CODE_ICON, !typeset, text, None)
                        .aria_label(rust_i18n::t!("preview.show_source").into_owned())
                        .aria_toggled((!typeset).into())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_preview_source(cx);
                            }),
                        )
                        .on_a11y_action(
                            AccessibleAction::Click,
                            press(|this, _, cx| this.toggle_preview_source(cx), cx),
                        ),
                )
            })
            // 排版视图的正文总是按栏宽换行（代码块横着滚），自动换行的开关只在源码视图里有。
            .when(!typeset, |buttons| {
                buttons.child(
                    button("preview-wrap", WRAP_ICON, self.preview_wrap, rust_i18n::t!("preview.wrap"), None)
                        .aria_toggled(self.preview_wrap.into())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_preview_wrap(cx);
                            }),
                        )
                        .on_a11y_action(AccessibleAction::Click, press(|this, _, cx| this.toggle_preview_wrap(cx), cx)),
                )
            })
            .child(
                button(
                    "preview-maximize",
                    if maximized { MINIMIZE_ICON } else { MAXIMIZE_ICON },
                    false,
                    if maximized { rust_i18n::t!("preview.restore") } else { rust_i18n::t!("preview.maximize") },
                    Some(&TogglePaneZoom),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_preview_maximized(window, cx);
                    }),
                )
                .on_a11y_action(AccessibleAction::Click, press(Self::toggle_preview_maximized, cx)),
            );
        // 按钮靠右；预览栏在最右边时再让出右上角面板开关的宽度，标签和按钮不钻到开关底下。
        let filler = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .flex()
            .items_center()
            .justify_end()
            .pl(px(4.))
            .pr(px(if rightmost { PANEL_TOGGLES_INSET } else { 6. }))
            .child(tab_underline(fg))
            .child(buttons);
        let header = self.panel_header(fg, cx).border_b_0().px_0().gap_0().child(strip).child(filler);
        // 正文整个报成一篇文档，以标签上的名字为名。
        let body = div()
            .id("preview-body")
            .role(Role::Document)
            .aria_label(preview.name())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_preview_body(preview, width, font, fg, bg, window, cx));
        Some(
            panel_shell("preview-panel", width, fg, bg, cx)
                .key_context("Preview")
                .track_focus(&self.preview_focus)
                .on_action(cx.listener(Self::copy_preview))
                .on_action(cx.listener(Self::select_all_preview))
                // 焦点在预览栏里时，关标签页和关分屏的快捷键关的都是预览标签。
                .on_action(cx.listener(Self::close_preview_tab))
                .on_action(cx.listener(Self::close_preview_pane))
                .on_action(cx.listener(Self::close_other_previews))
                .on_action(cx.listener(Self::close_previews_to_right))
                .on_action(cx.listener(Self::close_all_previews))
                .on_action(cx.listener(Self::keep_preview_open))
                .on_action(cx.listener(Self::zoom_preview))
                .bg(hsla(bg))
                .text_size(px(font_size))
                .child(header)
                .child(body),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_widest_line() {
        let lines: Vec<String> = ["ab", "abcd", "中文字"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(widest_line(&lines), 1);
        assert_eq!(widest_line(&[]), 0);
    }
}
