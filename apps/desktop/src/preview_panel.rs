//! 右侧的预览栏：文件树里打开的文件在这里显示，一个文件一个标签。单击打开的是临时标签，标题
//! 斜体，再打开别的文件时被换掉；双击文件或标签把它固定下来。标签能拖动换位置，中键关掉，右键
//! 有关闭其他、关闭右侧这些。文本用终端的字体，带行号和相对 HEAD 的改动标记，能按行选中复制，
//! 语法高亮在后台做完再换上；图片按栏宽等比缩小；二进制、读不了、太大的文件只给一句说明。
//! 文件在磁盘上变了就重读，没显示的标签等切过去时再看要不要重读。Markdown 文件默认显示排版后的
//! 样子，在后台解析，见 `markdown`。
//!
//! 从 Git 面板点开的是 diff 标签，和同一个文件的普通标签分开，见 `diff`。
//!
//! 预览栏是一个单独的视图（`PreviewPanel`），一个窗口一个，标签按 workspace 各记一份（`PreviewTabs`），
//! 显示的是 app 经 `set_workspace` 说的那个 workspace 的。字体、栏宽和配置里的预览字号由 app 每次画之前
//! 经 `set_input` 交进来，颜色（连同语法高亮用的 ANSI 16 色）画的时候从主题（`ActiveTheme`）读，各
//! workspace 最近读到的 git 状态经 `set_git` 交进来；要 app 办的事（文件树定位、预览栏出现收起和放大还原、
//! 开关变了要存档、把路径打进终端、按块暂存）经 `PreviewEvent` 交出去。右键菜单存在页自己身上。
//!
//! 读文件、判断类型和高亮在 `runode_preview`，这里只管状态、后台任务和画：标签条在 `tabs`，正文的
//! 行和图片在 `body`，行号旁的改动标记在 `marks`，自动换行在 `wrap`，Markdown 的排版视图在 `markdown`。
//!
//! 标签条右边能打开自动换行、Markdown 文件在排版和源码之间切（这两个整个窗口一份，app 随窗口存档），以及
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

use crate::ui::{
    self as ui,
    a11y::A11yPress,
    actions::{Copy, SelectAll, TogglePaneZoom},
    base_name,
    button::{Button, ButtonSize, Toggle},
    font_size::FontSize,
    icons::{
        ALERT_CAUTION_ICON, ALERT_IMPORTANT_ICON, ALERT_NOTE_ICON, ALERT_TIP_ICON, ALERT_WARNING_ICON, ARROW_UP_ICON,
        CODE_ICON, DIFF_ICON, MAXIMIZE_ICON, MINIMIZE_ICON, WRAP_ICON,
    },
    menu::{Menu, MenuHost},
    panel::{panel_header, panel_shell},
    theme::ActiveTheme as _,
    tooltip::tooltip,
};
use gpui::{
    App, ClipboardItem, Context, Div, EventEmitter, FocusHandle, Focusable, Image, MouseButton, MouseMoveEvent, Render,
    RenderImage, Role, SMOOTH_SVG_SCALE_FACTOR, ScrollHandle, ScrollStrategy, SharedString, Stateful, SvgRenderer,
    UniformListScrollHandle, Window, div, linear_color_stop, linear_gradient, prelude::*, px,
};
use runode_git::{self as git, FileStatus, HunkAction, Section};
use runode_preview::{Content, ImageFormat, Span};

pub use diff::DiffTarget;

use marks::{Mark, line_marks};
use tabs::{PreviewTabs, tab_underline};

/// 行高比字号多出的部分。
const ROW_EXTRA_HEIGHT: f32 = 8.;
/// 一行最多画这么多列，再长的截掉；复制时仍是整行。
const MAX_COLUMNS: usize = 2000;
/// 正文上下留的空。
const BODY_PADDING: f32 = 6.;
/// 长行右边缘渐隐的宽度。
const FADE_WIDTH: f32 = 24.;

/// workspace 的标识，由 app 给，挪动位置后不变。
pub type WorkspaceId = u64;

/// app 每次画预览栏之前交进来的东西，见 `PreviewPanel::set_input`。
#[derive(Clone)]
pub struct Input {
    /// 预览栏的宽度：在终端区右边占一栏时是那一栏的宽，放大时是终端区的宽。
    pub width: f32,
    /// 标签条右边的按钮再往左让出的宽度：预览栏在窗口最右边时让出右上角面板开关的地方。
    pub right_inset: f32,
    /// 正文用的终端字体。
    pub font: SharedString,
    /// 配置的预览字号（`preview-font-size`），正文按它排。
    pub font_size: f32,
    /// 窗口用卡片样式：预览栏是一张卡片，顶上一条和分屏的标题条一样高。
    pub cards: bool,
    /// 窗口底部状态栏这时占的高度，藏起来时是 0；diff 里并排的两张图按除去它的高度摆。
    pub status_bar_height: f32,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            width: 0.,
            right_inset: 6.,
            font: SharedString::default(),
            font_size: 13.,
            cards: false,
            status_bar_height: 0.,
        }
    }
}

/// 要 app 办的事。
pub enum PreviewEvent {
    /// 当前 workspace 的预览栏从收着变成显示：app 跟着监听目录、重读文件树，重排窗口。
    Opened,
    /// 当前 workspace 的标签都关掉了，预览栏收起；`focused` 时焦点原来在预览栏上，app 把它交回终端。
    Closed { focused: bool },
    /// 预览栏放大到盖住终端区，或者还原了：app 重排窗口。
    ZoomChanged,
    /// 在文件树里展开到这个文件，选中它、滚到能看见。
    Reveal(PathBuf),
    /// 自动换行或 Markdown 显示源码的开关变了，app 存档；下次经 `set_options` 交回。
    OptionsChanged,
    /// 把「路径:行号」打进当前终端：双击了工作区 diff 里没删掉的一行。
    InsertPath { path: PathBuf, line: u32 },
    /// 按块暂存、丢弃或取消暂存 `root` 仓库里 `file` 的第 `hunk` 块；丢弃已经问过了。做完后 app 调
    /// `PreviewPanel::reload_diff`，接着点下一块时块还对得上。
    ApplyHunk { root: PathBuf, file: git::FileDiff, hunk: usize, action: HunkAction },
}

/// 预览栏打开的文件和读到的内容。
struct Preview {
    path: PathBuf,
    /// 不为空时这是 `path` 的 diff 标签，和同一个文件的普通标签是两个。
    diff: Option<DiffTarget>,
    /// diff 标签：扫描到 git 状态变了，下次显示时重读。
    diff_stale: bool,
    /// 固定的标签；临时标签为假，下一个打开的文件会换掉它。
    pinned: bool,
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
    scroll: UniformListScrollHandle,
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
    /// 能排版显示：不是 diff 标签的 Markdown 文件，也没截断。截断的文件排版到一半会悄悄结束、代码块
    /// 可能切成两半，只给看源码，源码视图末尾有截断的说明。
    fn typesettable(&self) -> bool {
        self.diff.is_none()
            && runode_preview::is_markdown(&self.path)
            && !matches!(self.content, Some(Loaded::Text { truncated: true, .. }))
    }

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
    fn refresh_marks(&mut self, git: Option<&git::Repos>) {
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
    fn git_changed(&mut self, old: Option<&git::Repos>, new: Option<&git::Repos>) {
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
    fn affected_by(&self, paths: &[PathBuf]) -> bool {
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
            Loaded::Image(image) => ui::release_bitmap(image.clone(), cx),
            Loaded::Svg(image) => ui::drop_render_image(image.clone(), cx),
            Loaded::Text { markdown: Some(doc), .. } => doc.release(cx),
            Loaded::ImageDiff { old, new } => {
                for image in old.iter().chain(new) {
                    ui::release_bitmap(image.clone(), cx);
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
fn right_fade(handle: &ScrollHandle, cx: &App) -> Option<Div> {
    let (offset, max) = (handle.offset().x, handle.max_offset().x);
    let bg = cx.theme().background;
    (max > px(1.) && -offset < max - px(1.)).then(|| {
        div().absolute().top_0().right_0().h_full().w(px(FADE_WIDTH)).bg(linear_gradient(
            90.,
            linear_color_stop(bg.opacity(0.), 0.),
            linear_color_stop(bg, 1.),
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

/// 预览栏，见模块说明。
pub struct PreviewPanel {
    /// 各 workspace 的标签，没开过预览的 workspace 不在表里。
    workspaces: HashMap<WorkspaceId, PreviewTabs>,
    /// 显示的是哪个 workspace 的标签，见 `set_workspace`。
    ws: WorkspaceId,
    input: Input,
    /// 预览栏的焦点：点了预览的文字后 cmd+c 复制选中的行。
    focus: FocusHandle,
    /// 开着的标签右键菜单。
    menu: Option<Menu>,
    /// 预览的文本和 diff 按栏宽自动换行。整个窗口一份，app 存档。
    wrap: bool,
    /// Markdown 文件显示源码而不是排版后的样子。整个窗口一份，app 存档。
    source: bool,
    /// 一个字在终端里占几格，Markdown 表格估列宽用；由 app 给，和终端排版用同一张表。
    cell_width: fn(char) -> u8,
}

impl EventEmitter<PreviewEvent> for PreviewPanel {}

impl Focusable for PreviewPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MenuHost for PreviewPanel {
    fn menu_slot(&mut self) -> &mut Option<Menu> {
        &mut self.menu
    }

    fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
}

impl PreviewPanel {
    /// `cell_width` 说一个字在终端里占几格，见 `PreviewPanel::cell_width`。
    pub fn new(cell_width: fn(char) -> u8, cx: &mut Context<Self>) -> Self {
        Self {
            workspaces: HashMap::new(),
            ws: 0,
            input: Input::default(),
            focus: cx.focus_handle(),
            menu: None,
            wrap: false,
            source: false,
            cell_width,
        }
    }

    /// 之后的操作和画的都是 workspace `ws` 的标签。app 每次对预览栏做什么、画它之前都先调。
    pub fn set_workspace(&mut self, ws: WorkspaceId) {
        self.ws = ws;
    }

    /// 换上 app 画预览栏时的配色、字体、栏宽这些，见 `Input`。
    pub fn set_input(&mut self, input: Input) {
        self.input = input;
    }

    /// workspace `ws` 最近读到的 git 状态，改动标记和 diff 按它算。和上次交来的不是同一份时，重算各标签的
    /// 改动标记，这个文件的改动变了的 diff 标签记下要重读。
    pub fn set_git(&mut self, ws: WorkspaceId, git: Option<Arc<git::Repos>>) {
        let tabs = self.workspaces.entry(ws).or_default();
        let same = match (&tabs.git, &git) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (old, new) => old.is_none() && new.is_none(),
        };
        if same {
            return;
        }
        let old = std::mem::replace(&mut tabs.git, git);
        for preview in &mut tabs.tabs {
            preview.git_changed(old.as_deref(), tabs.git.as_deref());
        }
    }

    /// workspace `ws` 关掉了，丢掉它的标签。
    pub fn remove_workspace(&mut self, ws: WorkspaceId) {
        self.workspaces.remove(&ws);
    }

    /// workspace `ws` 有开着的预览标签，预览栏显示着（放大与否都算）。
    pub fn shown(&self, ws: WorkspaceId) -> bool {
        self.workspaces.get(&ws).is_some_and(|tabs| !tabs.tabs.is_empty())
    }

    /// workspace `ws` 的预览栏放大着，盖在终端区上。
    pub fn maximized(&self, ws: WorkspaceId) -> bool {
        self.workspaces.get(&ws).is_some_and(|tabs| !tabs.tabs.is_empty() && tabs.maximized)
    }

    /// 还原 workspace `ws` 放大着的预览栏，焦点要交给终端时用。
    pub fn restore_zoom(&mut self, ws: WorkspaceId) {
        if let Some(tabs) = self.workspaces.get_mut(&ws) {
            tabs.maximized = false;
        }
    }

    /// 自动换行、Markdown 显示源码这两个开关，app 随窗口存档。
    pub fn wrap(&self) -> bool {
        self.wrap
    }

    pub fn source(&self) -> bool {
        self.source
    }

    /// 换上存档里的两个开关。
    pub fn set_options(&mut self, wrap: bool, source: bool) {
        (self.wrap, self.source) = (wrap, source);
    }

    /// 当前 workspace 的标签。
    fn tabs(&self) -> Option<&PreviewTabs> {
        self.workspaces.get(&self.ws)
    }

    fn tabs_mut(&mut self) -> &mut PreviewTabs {
        self.workspaces.entry(self.ws).or_default()
    }

    /// 当前预览标签的文件。
    fn preview(&self) -> Option<&Preview> {
        self.tabs()?.active()
    }

    fn preview_mut(&mut self) -> Option<&mut Preview> {
        self.workspaces.get_mut(&self.ws)?.active_mut()
    }

    fn preview_shown(&self) -> bool {
        self.shown(self.ws)
    }

    /// 把预览栏放大到盖住终端区，或者还原。放大时焦点交给预览栏，键盘不再落到被盖住的终端里。
    fn toggle_preview_maximized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previews = self.tabs_mut();
        previews.maximized = !previews.maximized;
        if previews.maximized {
            window.focus(&self.focus, cx);
        }
        cx.emit(PreviewEvent::ZoomChanged);
        cx.notify();
    }

    /// 切换自动换行。换行时列表不能横着滚，横向滚过的先滚回左边，不然文字一直错开着。
    fn toggle_preview_wrap(&mut self, cx: &mut Context<Self>) {
        self.wrap = !self.wrap;
        for preview in self.workspaces.values().flat_map(|tabs| &tabs.tabs) {
            let handle = preview.scroll.0.borrow().base_handle.clone();
            handle.set_offset(gpui::point(px(0.), handle.offset().y));
        }
        cx.emit(PreviewEvent::OptionsChanged);
        cx.notify();
    }

    /// Markdown 文件在排版和源码之间切。
    fn toggle_preview_source(&mut self, cx: &mut Context<Self>) {
        self.source = !self.source;
        cx.emit(PreviewEvent::OptionsChanged);
        cx.notify();
    }

    /// 在预览栏里打开 `path` 并切到它的标签：`pin` 时开成固定标签，否则开成临时标签；已经开着
    /// 时重读一次。文件树跟着定位到它。
    pub fn open(&mut self, path: &Path, pin: bool, cx: &mut Context<Self>) {
        self.open_tab(path, None, pin, cx);
    }

    /// 和 `open` 一样打开 `path`，再选中第 `line` 行（从 0 数）、滚到中间。
    pub fn open_at(&mut self, path: &Path, line: usize, pin: bool, cx: &mut Context<Self>) {
        self.open(path, pin, cx);
        // 排版视图不按行定位，显示着排版的 Markdown 先切到源码，选中的那一行才看得到。
        if self.preview().is_some_and(|preview| preview.path == path && self.markdown_shown(preview)) {
            self.toggle_preview_source(cx);
        }
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
        let previews = self.tabs_mut();
        let replaced = previews.open(path, diff, pin);
        previews.scroll.scroll_to_item(previews.active);
        if let Some(old) = replaced {
            old.release_image(cx);
        }
        if !shown {
            cx.emit(PreviewEvent::Opened);
        }
        if reveal {
            cx.emit(PreviewEvent::Reveal(path.to_path_buf()));
        }
        self.load_preview(cx);
        cx.notify();
    }

    /// 切到第 `ix` 个预览标签：文件树跟着定位到它，还没读过或者磁盘上变过就读。
    fn activate_preview(&mut self, ix: usize, cx: &mut Context<Self>) {
        let previews = self.tabs_mut();
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
        cx.emit(PreviewEvent::Reveal(path));
        if stale {
            self.load_preview(cx);
        }
        cx.notify();
    }

    /// 只留下 `keep` 为真的预览标签，当前标签关掉了就切到旁边的。都关掉时预览栏收起，焦点在
    /// 预览栏上的交回终端。
    fn retain_previews(
        &mut self,
        keep: impl FnMut(usize, &Preview) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previews = self.tabs_mut();
        let before = previews.active().map(|tab| (tab.path.clone(), tab.diff.clone()));
        let closed = previews.retain(keep);
        if closed.is_empty() {
            return;
        }
        for tab in &closed {
            tab.release_image(cx);
        }
        if !self.preview_shown() {
            cx.emit(PreviewEvent::Closed { focused: self.focus.is_focused(window) });
        } else if self.preview().map(|tab| (tab.path.clone(), tab.diff.clone())) != before {
            let active = self.tabs_mut().active;
            self.activate_preview(active, cx);
        }
        cx.notify();
    }

    /// 关掉 `path` 和它下面的文件的标签（连同它们的 diff 标签）：文件删掉了。
    pub fn close_under(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_previews(|_, preview| !preview.path.starts_with(path), window, cx);
    }

    /// `from` 改了名或挪到了 `to`：它和它下面的文件的预览标签跟过去，当前标签换了就重读。
    pub fn moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for old in self.tabs_mut().moved(from, to) {
            old.release_image(cx);
        }
        if self.preview().is_some_and(|tab| tab.content.is_none()) {
            self.load_preview(cx);
        }
        cx.notify();
    }

    /// 窗口切回前台、切回这个 workspace 时，当前预览的文件变过就重读；在后台时监听到的改动只等到这时。
    /// 别的标签等切过去时再看。
    pub fn refresh_if_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview()
            && preview.content.is_some()
            && preview.stale()
        {
            self.load_preview(cx);
        }
    }

    /// 监听到 `paths` 变了：当前预览的文件在里面就重读。
    pub fn files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if self.preview().is_some_and(|preview| preview.affected_by(paths)) {
            self.load_preview(cx);
        }
    }

    /// 在后台读当前 workspace 预览的文件，读完换上；是文本时接着在后台高亮。diff 标签读 diff。
    fn load_preview(&mut self, cx: &mut Context<Self>) {
        if self.preview().is_some_and(|preview| preview.diff.is_some()) {
            self.load_diff(cx);
            return;
        }
        let id = self.ws;
        let Some(preview) = self.preview_mut() else {
            return;
        };
        preview.cancel.store(true, Ordering::Relaxed);
        preview.cancel = Arc::new(AtomicBool::new(false));
        let cancel = preview.cancel.clone();
        let path = preview.path.clone();
        let is_markdown = runode_preview::is_markdown(&path);
        let svg = cx.svg_renderer();
        let cell_width = self.cell_width;
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
                        Loaded::Text { lines, highlights, markdown, truncated, .. } => {
                            *highlights = old_highlights;
                            if *truncated {
                                if let Some(old) = old_markdown {
                                    old.release(cx);
                                }
                            } else {
                                *markdown = old_markdown;
                            }
                            if let Some((anchor, head)) = &mut preview.selection {
                                let last = lines.len().saturating_sub(1);
                                *anchor = (*anchor).min(last);
                                *head = (*head).min(last);
                            }
                            Some((lines.clone(), *truncated))
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
            let Some((lines, truncated)) = lines else {
                return;
            };
            if is_markdown && !truncated {
                let doc = cx
                    .background_spawn({
                        let (cancel, lines, path) = (cancel.clone(), lines.clone(), path.clone());
                        async move {
                            let blocks = runode_preview::parse_markdown(&lines.join("\n"), &cancel)?;
                            let dir = path.parent().unwrap_or(Path::new("/"));
                            Some(markdown::Doc::new(blocks, dir, cell_width, |path| markdown::picture(path, &svg)))
                        }
                    })
                    .await;
                this.update(cx, |this, cx| {
                    let font_size = this.input.font_size;
                    if let Some(doc) = doc
                        && let Some((preview, _)) = this.preview_for(id, &cancel)
                        && let Some(Loaded::Text { markdown: slot, .. }) = &mut preview.content
                    {
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
        id: WorkspaceId,
        cancel: &Arc<AtomicBool>,
    ) -> Option<(&mut Preview, Option<&git::Repos>)> {
        let tabs = self.workspaces.get_mut(&id)?;
        let preview = tabs.tabs.iter_mut().find(|preview| Arc::ptr_eq(&preview.cancel, cancel))?;
        Some((preview, tabs.git.as_deref()))
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
        window.focus(&self.focus, cx);
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

    fn render_preview_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let preview = self.preview()?;
        let previews = self.tabs()?;
        let Input { width, right_inset, font_size, cards, .. } = self.input;
        let font = self.input.font.clone();
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
            .children((0..previews.tabs.len()).map(|ix| self.render_preview_tab(ix, cx)).collect::<Vec<_>>());
        // 标签下面那条分隔线由各个标签和后面的空白各画一段，当前标签底下空着，和正文连在一起。
        let maximized = previews.maximized;
        let toggle = |id: &'static str, icon: &'static str, on: bool, text: Cow<'static, str>| {
            Toggle::new(id, icon, SharedString::from(text.clone()))
                .pressed(on)
                .icon_size(13.)
                .flex_none()
                .size(px(22.))
                .tooltip(tooltip(text, None))
        };
        // 这几个按钮只有按下的处理，辅助工具按不到，另外登记按下时做的事。
        let view = cx.entity().downgrade();
        let typeset = self.markdown_shown(preview);
        let buttons = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .when(preview.typesettable(), |buttons| {
                let text =
                    if typeset { rust_i18n::t!("preview.show_source") } else { rust_i18n::t!("preview.show_rendered") };
                // 悬停提示说按了会怎样，跟着状态换；报给辅助工具的名字不换，开着表示正显示源码。
                buttons.child(
                    toggle("preview-source", CODE_ICON, !typeset, text)
                        .aria_label(rust_i18n::t!("preview.show_source").into_owned())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_preview_source(cx);
                            }),
                        )
                        .on_a11y_press(view.clone(), |this, _, cx| this.toggle_preview_source(cx)),
                )
            })
            // 排版视图的正文总是按栏宽换行（代码块横着滚），自动换行的开关只在源码视图里有。
            .when(!typeset, |buttons| {
                buttons.child(
                    toggle("preview-wrap", WRAP_ICON, self.wrap, rust_i18n::t!("preview.wrap"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_preview_wrap(cx);
                            }),
                        )
                        .on_a11y_press(view.clone(), |this, _, cx| this.toggle_preview_wrap(cx)),
                )
            })
            .child({
                let text = if maximized { rust_i18n::t!("preview.restore") } else { rust_i18n::t!("preview.maximize") };
                Button::new("preview-maximize", SharedString::from(text.clone()))
                    .icon(if maximized { MINIMIZE_ICON } else { MAXIMIZE_ICON })
                    .icon_size(13.)
                    .with_size(ButtonSize::Icon)
                    .size(px(22.))
                    .tooltip(tooltip(text, Some(&TogglePaneZoom)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.toggle_preview_maximized(window, cx);
                        }),
                    )
                    .on_a11y_press(view, Self::toggle_preview_maximized)
            });
        // 按钮靠右；预览栏在最右边时再让出右上角面板开关的宽度，标签和按钮不钻到开关底下。不设
        // `min_w_0`：标签多了挤的是标签条（它自己横着滚），这里至少留出按钮的宽度，按钮不叠到标签上。
        let filler = div()
            .flex_1()
            .h_full()
            .relative()
            .flex()
            .items_center()
            .justify_end()
            .pl(px(4.))
            .pr(px(right_inset))
            .child(tab_underline(cx))
            .child(buttons);
        // 标签条是界面，不跟着正文的 `preview-font-size`。
        let header =
            panel_header(cards, cx).border_b_0().px_0().gap_0().text_size(FontSize::Medium).child(strip).child(filler);
        // 正文整个报成一篇文档，以标签上的名字为名。
        let body = div()
            .id("preview-body")
            .role(Role::Document)
            .aria_label(preview.name())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_preview_body(preview, width, font, window, cx));
        Some(
            panel_shell("preview-panel", width, cards, cx)
                .key_context("Preview")
                .track_focus(&self.focus)
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
                .bg(cx.theme().background)
                .text_size(px(font_size))
                .child(header)
                .child(body),
        )
    }
}

impl Render for PreviewPanel {
    /// 当前 workspace 的预览栏；没有标签时什么也不画（app 也不会把它摆出来）。放大着时盖满终端区，挡住
    /// 下面终端的鼠标事件；终端不改尺寸，还原后原样露出来。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        let Some(panel) = self.render_preview_panel(window, cx) else {
            return div().into_any_element();
        };
        let maximized = self.maximized(self.ws);
        panel
            .when(maximized, |panel| panel.absolute().top_0().left_0().size_full().occlude())
            .children(self.render_context_menu(window, cx))
            .into_any_element()
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
