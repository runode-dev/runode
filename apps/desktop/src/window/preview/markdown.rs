//! Markdown 文件的排版视图，样子照 GitHub 的 Markdown 渲染（github-markdown-css）来：字号、行高、
//! 间距都按 GitHub 的数值以预览字号为 1em 换算，颜色从当前终端主题取（正文是前景色，弱化色、边框、
//! 代码底色是背景色往前景色调，链接和提示块用 ANSI 的蓝、绿、品红、黄、红），深色浅色主题都适用。
//!
//! `runode_preview::parse_markdown` 解析出的块是嵌套的（列表、引用里还有块），这里在后台把它摊平成
//! 一行一行（`Row`）：每行是一个不再嵌套的块（标题、段落、代码块、表格、分割线、图片、提示块的
//! 标题），前面记着它套在哪几层引用和列表项里。列表用 `gpui::list`，只画看得见的那些行，几千行的
//! 文件滚起来也只画一屏；一行里的文字按栏宽自动换行。
//!
//! 文字能拖选：选区记成（行、行里第几段文字、字节偏移）的两端，见 `select`；每段文字画的时候把
//! 布局登记下来，鼠标事件据此换算位置，见 `text`。Cmd+C 复制选中部分的纯文本，Cmd+A 全选，双击选词、
//! 三击选一段。拖动不到几个像素就松开算点击，点在链接上就打开：网址交给浏览器，本地文件在预览栏里
//! 打开，`#锚点` 滚到对应的标题或脚注。网络图片在后台下载，见 `remote`。代码块悬停时右上角有复制
//! 按钮，整块复制。

mod remote;
mod select;
#[cfg(test)]
mod tests;
mod text;

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AccessibleAction, AnyElement, App, ClipboardItem, Context, CursorStyle, DispatchPhase, FontStyle, FontWeight,
    HighlightStyle, Hsla, Image, ImageSource, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, RenderImage, Role, SMOOTH_SVG_SCALE_FACTOR, SharedString, StrikethroughStyle,
    StyledText, SvgRenderer, UnderlineStyle, WeakEntity, Window, canvas, div, img, list, prelude::*, px, relative, svg,
};
use runode_preview::{Alert, Align, Block, Content, Image as MdImage, Inline, InlineStyle, Span};
use runode_shared_types::color::Rgb;
pub(super) use select::MdPos;
use select::TextKey;
use text::{MdText, TextBox, Texts};

use super::{Loaded, MAX_COLUMNS, Preview, body::ansi_palette, body::highlight_style, loaded};
use crate::{
    assets::{
        ALERT_CAUTION_ICON, ALERT_IMPORTANT_ICON, ALERT_NOTE_ICON, ALERT_TIP_ICON, ALERT_WARNING_ICON, CHECK_ICON,
    },
    config::AppConfig,
    ui::{a11y::Press, hsla, scrollbar::list_scrollbar},
    window::{WindowView, project::RENAMED},
};

/// 正文左右和上下留的空。
const PADDING_X: f32 = 16.;
const PADDING_Y: f32 = 16.;
/// 正文的行高是字号的这么多倍。
const LINE_HEIGHT: f32 = 1.5;
/// 列表只画看得见的行，上下再多画这么多，滚的时候不闪。
const OVERDRAW: f32 = 400.;
/// 按下后拖动不到这么远就松开算点击，不算拖选。
const DRAG_THRESHOLD: f32 = 3.;
/// 一篇文档最多读这么多个不同的本地图片文件，再多的只显示替代文字。
const MAX_LOCAL_IMAGES: usize = 256;
/// 一篇文档最多下载这么多个不同的网络图片，再多的只显示替代文字。
const MAX_REMOTE_IMAGES: usize = 64;

/// 一个预览标签的排版视图的状态：列表的滚动位置、选区、鼠标在干什么、下好的网络图片。重读文件时
/// 留着。
pub(super) struct View {
    pub list: ListState,
    /// 选区：按下的那端和现在拖到的那端。
    selection: Option<(MdPos, MdPos)>,
    drag: Option<Drag>,
    /// 鼠标底下的链接：哪段文字里的第几个链接，那个链接画下划线。
    hover: Option<(TextKey, usize)>,
    texts: Texts,
    /// 网络图片按地址：下好了是图，下不了为空；还在下的不在表里。
    remote: HashMap<String, Option<Picture>>,
    /// 按哪个栏宽给没量过的行估的高度，见 `render_markdown`。
    hinted_width: Cell<f32>,
}

/// 按着鼠标：按下的位置、拖动够远了没有、按下时点在哪个链接上。
struct Drag {
    origin: Point<Pixels>,
    moved: bool,
    link: Option<String>,
}

impl View {
    pub fn new() -> Self {
        Self {
            list: ListState::new(0, gpui::ListAlignment::Top, px(OVERDRAW)),
            selection: None,
            drag: None,
            hover: None,
            texts: Texts::default(),
            remote: HashMap::new(),
            // 和任何宽度都不等，第一次画就会去估。
            hinted_width: Cell::new(-1.),
        }
    }

    /// 换掉旧文档 `old`、换上新解析的 `doc`：列表按新的行数重来，滚动位置按原来的第几行留着，文件在
    /// 磁盘上改了重读时不跳回开头；选区两端的文字没变就留着，否则清掉。没量过的行按估的高度先占着，
    /// 滚动条一开始就差不多准。
    pub fn replace_doc(&mut self, old: Option<&Doc>, doc: &Doc, font_size: f32) {
        let top = self.list.logical_scroll_top();
        let rows = doc.rows.len();
        self.list.reset_with_uniform_height(rows, row_hint(font_size));
        self.list.scroll_to(ListOffset { item_ix: top.item_ix.min(rows), offset_in_item: top.offset_in_item });
        let kept = |pos| old.is_some_and(|old| select::still_valid(old, doc, pos));
        if self.selection.is_some_and(|(anchor, head)| !kept(anchor) || !kept(head)) {
            self.selection = None;
        }
        self.drag = None;
        self.hover = None;
    }
}

/// 没量过的行先估成多高：大约两行正文。
fn row_hint(font_size: f32) -> Pixels {
    px(font_size * LINE_HEIGHT * 2.)
}

/// 表格每列估计最宽的格子在第几行（表头算第 0 行）：按终端里占几格估，不量字体。表格只画看得见的
/// 行，每列另垫一个看不见的这格撑住列宽，滚动时列宽不跟着露出的行跳。
fn widest_cells(columns: usize, head: &[Rich], rows: &[Vec<Rich>]) -> Vec<usize> {
    let cells = || std::iter::once(head).chain(rows.iter().map(Vec::as_slice)).enumerate();
    let width = |rich: &Rich| rich.text.chars().map(|ch| usize::from(runode_terminal::cell_width(ch))).sum::<usize>();
    (0..columns.max(1))
        .map(|column| cells().max_by_key(|(_, row)| row.get(column).map_or(0, width)).map_or(0, |(row_ix, _)| row_ix))
        .collect()
}

/// 第 `ix` 行的表格要画表体的哪几行：露出来的，连同列表上下多画的 `OVERDRAW`。`top` 是列表滚到哪儿，
/// `viewport` 是列表的高，`head_h`、`body_h` 是表头和表体一行的高。从这一行的顶上算起，行上面的间距
/// 让范围往下偏一点，多画的那段盖得住。
fn shown_rows(top: ListOffset, viewport: f32, ix: usize, head_h: f32, body_h: f32, rows: usize) -> Range<usize> {
    let total = head_h + rows as f32 * body_h;
    let (from, to) = if top.item_ix == ix {
        let offset = f32::from(top.offset_in_item);
        (offset - OVERDRAW, offset + viewport + OVERDRAW)
    } else if top.item_ix < ix {
        // 从视口里或视口下面开始。
        (0., viewport + OVERDRAW)
    } else {
        // 整个在视口上面，只有底下多画的那段。
        (total - OVERDRAW, total)
    };
    let row = |y: f32| (((y - head_h) / body_h).max(0.) as usize).min(rows);
    row(from)..(row(to) + 1).min(rows)
}

/// 摊平后的整篇文档。
pub(super) struct Doc {
    pub rows: Vec<Row>,
    /// 锚点（标题的 id、`fn-标签`、`fnref-标签`）在第几行。
    pub anchors: HashMap<String, usize>,
}

/// 一行：不再嵌套的一个块，以及它从外到里套在哪几层引用和列表项里。
pub(super) struct Row {
    pub nest: Vec<Nest>,
    pub leaf: Leaf,
    /// 文末的脚注：字小一号、用弱化色。
    pub footnote: bool,
    /// 在 HTML 居中的块里：文字逐行居中，图片摆在中间。
    pub center: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Nest {
    /// 引用；`id` 分开挨着的两段引用，各画各的竖线。
    Quote { alert: Option<Alert>, id: usize },
    /// 列表项；记号只在它的第一行有。`list` 是所在列表的编号，分开挨着的两个列表。
    Item { marker: Option<Marker>, loose: bool, list: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Marker {
    Bullet,
    Number(u64),
    Task(bool),
}

pub(super) enum Leaf {
    Heading(u8, Rich),
    Paragraph(Rich),
    /// 提示块的标题行：种类和按界面语言翻好的名字。
    AlertTitle(Alert, SharedString),
    Code {
        /// 制表符已展开、各行用换行接起来的样子；高亮的范围是它里面的字节位置。
        text: SharedString,
        spans: Vec<Span>,
        /// 原文，复制按钮用；每帧画的时候都要交给按钮一份，共用不复制。
        source: SharedString,
    },
    Table {
        aligns: Vec<Align>,
        head: Vec<Rich>,
        rows: Vec<Vec<Rich>>,
        /// 每列估计最宽的格子在第几行（表头算第 0 行），见 `widest_cells`。
        widest: Vec<usize>,
    },
    /// 分割线；在脚注前面时是一条细线。
    Rule,
    /// 并排的一张或几张图片，放不下时折行。
    Images(Vec<RowImage>),
}

pub(super) struct RowImage {
    /// 替代文字；没写时是地址。
    pub alt: String,
    pub url: String,
    /// HTML `<img>` 上写的宽和高（像素）；只写了一边的另一边按比例算。
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 图片外面那层链接的地址，点图片打开它。
    pub link: Option<String>,
    /// 读出来的本地图片；读不了的、不是图片的为空，只显示替代文字。网络图片也为空，下好的在
    /// `View::remote` 里。
    pub picture: Option<Picture>,
    pub remote: bool,
}

impl Leaf {
    /// 这一行能选中的各段文字，顺序和 `TextKey` 的第二项一样。
    pub fn texts(&self) -> Vec<&str> {
        match self {
            Leaf::Heading(_, rich) | Leaf::Paragraph(rich) => vec![&rich.text],
            Leaf::AlertTitle(_, title) => vec![title],
            Leaf::Code { text, .. } => vec![text],
            Leaf::Table { head, rows, .. } => {
                head.iter().chain(rows.iter().flatten()).map(|cell| cell.text.as_ref()).collect()
            }
            Leaf::Rule | Leaf::Images(_) => Vec::new(),
        }
    }

    /// 第 `ix` 段文字带样式的那份；代码块和提示块的标题没有。
    fn rich(&self, ix: usize) -> Option<&Rich> {
        match self {
            Leaf::Heading(_, rich) | Leaf::Paragraph(rich) if ix == 0 => Some(rich),
            Leaf::Table { head, rows, .. } => head.iter().chain(rows.iter().flatten()).nth(ix),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(super) enum Picture {
    Bitmap(Arc<Image>),
    /// SVG 在后台画好的位图。
    Svg(Arc<RenderImage>),
}

/// 认同一张图：位图按字节的哈希（GPUI 也按它缓存解码结果），SVG 画好的位图按是不是同一份。
#[derive(PartialEq, Eq, Hash)]
enum PictureKey {
    Bitmap(u64),
    Svg(usize),
}

impl Picture {
    fn key(&self) -> PictureKey {
        match self {
            Picture::Bitmap(image) => PictureKey::Bitmap(image.id()),
            Picture::Svg(image) => PictureKey::Svg(Arc::as_ptr(image) as usize),
        }
    }

    /// 关掉或换掉时放掉解码结果，和 `Preview::release_image` 一样。
    fn release(&self, cx: &mut App) {
        match self {
            Picture::Bitmap(image) => ImageSource::Image(image.clone()).remove_asset(cx),
            Picture::Svg(image) => cx.drop_image(image.clone(), None),
        }
    }
}

/// 一段带样式的文字：行内片段接成一串，每段的样式和是否在链接里按字节范围记。
#[derive(Debug, Default, PartialEq)]
pub(super) struct Rich {
    pub text: SharedString,
    /// 不是普通样式的那些段，从前往后、不重叠；最后一项为真表示在链接里。
    pub runs: Vec<(Range<usize>, InlineStyle, bool)>,
    /// 链接占的范围和地址，挨着的同一个链接并成一段。
    pub links: Vec<(Range<usize>, String)>,
}

impl Rich {
    fn new(inlines: &[Inline]) -> Self {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut links: Vec<(Range<usize>, String)> = Vec::new();
        for inline in inlines.iter().filter(|inline| !inline.text.is_empty()) {
            let range = text.len()..text.len() + inline.text.len();
            text.push_str(&inline.text);
            if inline.style != InlineStyle::default() || inline.link.is_some() {
                runs.push((range.clone(), inline.style, inline.link.is_some()));
            }
            if let Some(url) = &inline.link {
                match links.last_mut() {
                    Some((last, last_url)) if last.end == range.start && last_url == url => last.end = range.end,
                    _ => links.push((range, url.clone())),
                }
            }
        }
        Self { text: text.into(), runs, links }
    }

    /// `offset` 处的链接是第几个。
    fn link_at(&self, offset: usize) -> Option<usize> {
        self.links.iter().position(|(range, _)| range.contains(&offset))
    }
}

impl Doc {
    /// 摊平 `blocks`；`dir` 是 Markdown 文件所在的目录，`picture` 读出这个目录下引用的本地图片文件，
    /// 同一个文件只读一次，最多读 `MAX_LOCAL_IMAGES` 个；网络图片不经它。
    pub fn new(blocks: Vec<Block>, dir: &Path, picture: impl Fn(&Path) -> Option<Picture>) -> Self {
        let mut flat = Flatten {
            rows: Vec::new(),
            anchors: HashMap::new(),
            dir,
            picture: &picture,
            pictures: HashMap::new(),
            footnote: false,
            center: 0,
            ids: 0,
        };
        flat.blocks(blocks, &mut Vec::new());
        Self { rows: flat.rows, anchors: flat.anchors }
    }

    /// 各行的图片。
    fn images(&self) -> impl Iterator<Item = &RowImage> {
        self.rows.iter().flat_map(|row| match &row.leaf {
            Leaf::Images(images) => images.as_slice(),
            _ => &[],
        })
    }

    /// 本地图片读出来的图，同一张图出现几次就有几个。
    fn pictures(&self) -> impl Iterator<Item = &Picture> {
        self.images().filter_map(|image| image.picture.as_ref())
    }

    /// 关掉时放掉本地图片的解码结果。
    pub fn release(&self, cx: &mut App) {
        for picture in self.pictures() {
            picture.release(cx);
        }
    }

    /// 换上新文档 `new` 以后，放掉这份旧文档里新文档不再用到的图片。新旧两份里同一个文件读出的位图
    /// 在 GPUI 里按字节的哈希缓存、是同一份解码结果，放掉就得重新解码，解码完之前图片的高度塌掉。
    pub fn release_unused(&self, new: &Doc, cx: &mut App) {
        for picture in self.unused_pictures(new) {
            picture.release(cx);
        }
    }

    fn unused_pictures<'a>(&'a self, new: &Doc) -> Vec<&'a Picture> {
        let kept: HashSet<PictureKey> = new.pictures().map(Picture::key).collect();
        self.pictures().filter(|picture| !kept.contains(&picture.key())).collect()
    }

    /// 文档里的网络图片地址，不重复，最多 `MAX_REMOTE_IMAGES` 个。
    fn remote_urls(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.images()
            .filter(|image| image.remote)
            .map(|image| &image.url)
            .filter(|url| seen.insert(url.as_str()))
            .take(MAX_REMOTE_IMAGES)
            .cloned()
            .collect()
    }

    /// 第 `key` 段文字里 `offset` 处的链接：第几个和地址。
    fn link_at(&self, key: TextKey, offset: usize) -> Option<(usize, &str)> {
        let rich = self.rows.get(key.0)?.leaf.rich(key.1)?;
        let ix = rich.link_at(offset)?;
        Some((ix, &rich.links[ix].1))
    }

    /// 锚点 `id`（`#` 后面的部分，已经解码）在第几行；GitHub 的锚点不分大小写也能对上。
    fn anchor(&self, id: &str) -> Option<usize> {
        self.anchors.get(id).or_else(|| self.anchors.get(&id.to_lowercase())).copied()
    }
}

/// 摊平时的状态。
struct Flatten<'a> {
    rows: Vec<Row>,
    anchors: HashMap<String, usize>,
    /// Markdown 文件所在的目录，本地图片的地址相对它。
    dir: &'a Path,
    picture: &'a dyn Fn(&Path) -> Option<Picture>,
    /// 读过的本地图片文件，读不了的也记着，不再读第二次。
    pictures: HashMap<PathBuf, Option<Picture>>,
    /// 正在摊平文末的脚注。
    footnote: bool,
    /// 套在几层 HTML 居中的块里。
    center: usize,
    /// 给引用和列表发编号。
    ids: usize,
}

impl Flatten<'_> {
    fn next_id(&mut self) -> usize {
        self.ids += 1;
        self.ids
    }

    fn blocks(&mut self, blocks: Vec<Block>, nest: &mut Vec<Nest>) {
        for block in blocks {
            let leaf = match block {
                Block::Quote { alert, blocks } => {
                    let id = self.next_id();
                    nest.push(Nest::Quote { alert, id });
                    if let Some(alert) = alert {
                        self.push(nest, Leaf::AlertTitle(alert, alert_title(alert).into()));
                    }
                    self.blocks(blocks, nest);
                    nest.pop();
                    continue;
                }
                Block::List { start, loose, items } => {
                    let list = self.next_id();
                    for (ix, item) in items.into_iter().enumerate() {
                        let marker = match (item.task, start) {
                            (Some(done), _) => Marker::Task(done),
                            (None, Some(start)) => Marker::Number(start.saturating_add(ix as u64)),
                            (None, None) => Marker::Bullet,
                        };
                        self.item(nest, Nest::Item { marker: Some(marker), loose, list }, item.blocks);
                    }
                    continue;
                }
                Block::Centered(blocks) => {
                    self.center += 1;
                    self.blocks(blocks, nest);
                    self.center -= 1;
                    continue;
                }
                Block::Footnotes(notes) => {
                    self.footnote = true;
                    self.push(nest, Leaf::Rule);
                    let list = self.next_id();
                    for (ix, note) in notes.into_iter().enumerate() {
                        self.anchors.entry(format!("fn-{}", note.label)).or_insert(self.rows.len());
                        let marker = Marker::Number(ix as u64 + 1);
                        self.item(nest, Nest::Item { marker: Some(marker), loose: false, list }, note.blocks);
                    }
                    self.footnote = false;
                    continue;
                }
                Block::Heading { level, inlines, id } => {
                    self.anchors.entry(id).or_insert(self.rows.len());
                    Leaf::Heading(level, self.rich(&inlines))
                }
                Block::Paragraph(inlines) => Leaf::Paragraph(self.rich(&inlines)),
                Block::Code { lines, highlights, .. } => {
                    let (text, spans) = code_text(&lines, &highlights);
                    Leaf::Code { text: text.into(), spans, source: lines.join("\n").into() }
                }
                Block::Table { aligns, head, rows } => {
                    let head: Vec<_> = head.iter().map(|cell| self.rich(cell)).collect();
                    let rows: Vec<Vec<_>> =
                        rows.iter().map(|row| row.iter().map(|cell| self.rich(cell)).collect()).collect();
                    let widest = widest_cells(aligns.len(), &head, &rows);
                    Leaf::Table { aligns, head, rows, widest }
                }
                Block::Rule => Leaf::Rule,
                Block::Images(images) => Leaf::Images(
                    images
                        .into_iter()
                        .map(|MdImage { url, alt, width, height, link }| {
                            let remote = is_remote(&url);
                            let picture = if remote { None } else { self.local_picture(&url) };
                            let alt = if alt.is_empty() { url.clone() } else { alt };
                            RowImage { alt, url, width, height, link, picture, remote }
                        })
                        .collect(),
                ),
            };
            self.push(nest, leaf);
        }
    }

    /// 本地图片地址 `url` 读出的图：按解析好的路径记着，同一个文件只读一次；读过 `MAX_LOCAL_IMAGES`
    /// 个文件以后不再读新的。
    fn local_picture(&mut self, url: &str) -> Option<Picture> {
        let path = image_path(self.dir, url)?;
        if let Some(picture) = self.pictures.get(&path) {
            return picture.clone();
        }
        if self.pictures.len() >= MAX_LOCAL_IMAGES {
            return None;
        }
        let picture = (self.picture)(&path);
        self.pictures.insert(path, picture.clone());
        picture
    }

    /// 一个列表项：里面的块各成一行，空的列表项也占一行，记号照画。
    fn item(&mut self, nest: &mut Vec<Nest>, level: Nest, blocks: Vec<Block>) {
        nest.push(level);
        let before = self.rows.len();
        self.blocks(blocks, nest);
        if self.rows.len() == before {
            self.push(nest, Leaf::Paragraph(Rich::default()));
        }
        nest.pop();
    }

    /// 带样式的文字；里面有脚注的引用时记下 `fnref-标签` 的锚点在这一行。
    fn rich(&mut self, inlines: &[Inline]) -> Rich {
        for inline in inlines.iter().filter(|inline| inline.style.footnote) {
            if let Some(label) = inline.link.as_deref().and_then(|link| link.strip_prefix("#fn-")) {
                self.anchors.entry(format!("fnref-{label}")).or_insert(self.rows.len());
            }
        }
        Rich::new(inlines)
    }

    fn push(&mut self, nest: &mut [Nest], leaf: Leaf) {
        self.rows.push(Row { nest: nest.to_vec(), leaf, footnote: self.footnote, center: self.center > 0 });
        // 列表项的记号只画在它的第一行。
        for level in nest.iter_mut() {
            if let Nest::Item { marker, .. } = level {
                *marker = None;
            }
        }
    }
}

fn alert_title(alert: Alert) -> String {
    crate::i18n::tr(match alert {
        Alert::Note => "preview.alert.note",
        Alert::Tip => "preview.alert.tip",
        Alert::Important => "preview.alert.important",
        Alert::Warning => "preview.alert.warning",
        Alert::Caution => "preview.alert.caution",
    })
}

/// 代码块各行展开制表符、过长的截断后用换行接起来，高亮的范围跟着换算。
fn code_text(lines: &[String], highlights: &[Vec<Span>]) -> (String, Vec<Span>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (ix, line) in lines.iter().enumerate() {
        if ix > 0 {
            text.push('\n');
        }
        let shown = runode_preview::display_line(line, highlights.get(ix).map_or(&[], Vec::as_slice), MAX_COLUMNS);
        let offset = text.len();
        spans.extend(
            shown
                .spans
                .into_iter()
                .map(|span| Span { range: span.range.start + offset..span.range.end + offset, ..span }),
        );
        text.push_str(&shown.text);
        if shown.cut {
            text.push('…');
        }
    }
    (text, spans)
}

/// 第 `ix` 行上面空多少，按预览字号的倍数，照 GitHub 的外边距：段落、列表、引用、表格、代码块之间
/// 1em，标题上面 1.5em、下面 1em，分割线上下 1.5em；同一个列表的相邻项 0.25em（松散列表 1em），
/// 子列表紧贴着上一级的文字；提示块的标题和正文之间 0.5em。
pub(super) fn gap(rows: &[Row], ix: usize) -> f32 {
    let (Some(prev), Some(row)) = (ix.checked_sub(1).and_then(|prev| rows.get(prev)), rows.get(ix)) else {
        return 0.;
    };
    let top: f32 = match row.leaf {
        Leaf::Heading(..) => 1.5,
        Leaf::Rule if !row.footnote => 1.5,
        _ => 0.,
    };
    if matches!(prev.leaf, Leaf::AlertTitle(..)) {
        return top.max(0.5);
    }
    if let Some(depth) = row.nest.iter().position(|level| matches!(level, Nest::Item { marker: Some(_), .. })) {
        let loose = matches!(row.nest[depth], Nest::Item { loose: true, .. });
        if continues(Some(prev), row, depth) {
            return top.max(if loose { 1. } else { 0.25 });
        }
        if depth > 0 && continues(Some(prev), row, depth - 1) && matches!(row.nest[depth - 1], Nest::Item { .. }) {
            return top.max(if loose { 1. } else { 0. });
        }
    }
    let bottom = match prev.leaf {
        Leaf::Rule if !prev.footnote => 1.5,
        _ => 1.,
    };
    top.max(bottom)
}

/// `row` 的第 `depth` 层（含）以外的套层和上一行 `prev` 是同一段引用、同一个列表：引用的竖线接着
/// 画，不在上面留空。
fn continues(prev: Option<&Row>, row: &Row, depth: usize) -> bool {
    let same = |a: &Nest, b: &Nest| match (a, b) {
        (Nest::Quote { id: a, .. }, Nest::Quote { id: b, .. }) => a == b,
        (Nest::Item { list: a, .. }, Nest::Item { list: b, .. }) => a == b,
        _ => false,
    };
    prev.is_some_and(|prev| {
        prev.nest.len() > depth
            && row.nest.len() > depth
            && prev.nest[..=depth].iter().zip(&row.nest[..=depth]).all(|(a, b)| same(a, b))
    })
}

/// 列表记号的文字：有序列表照 GitHub 按层数换样式，第一层 `1.`、第二层 `i.`、再往里 `a.`；无序列表
/// 是 `-`，任务列表是 `[x]`、`[ ]`。`depth` 从 1 数。复制用；无序列表的圆点画成图形。
pub(super) fn marker_text(marker: Marker, depth: usize) -> String {
    match marker {
        Marker::Bullet => "-".to_owned(),
        Marker::Task(done) => if done { "[x]" } else { "[ ]" }.to_owned(),
        Marker::Number(number) => match depth {
            0 | 1 => format!("{number}."),
            2 => format!("{}.", roman(number)),
            _ => format!("{}.", alpha(number)),
        },
    }
}

/// 小写罗马数字；0 和太大的数照写阿拉伯数字。
fn roman(mut number: u64) -> String {
    if number == 0 || number >= 4000 {
        return number.to_string();
    }
    let table =
        [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x")];
    let mut out = String::new();
    for (value, digits) in table.into_iter().chain([(9, "ix"), (5, "v"), (4, "iv"), (1, "i")]) {
        while number >= value {
            out.push_str(digits);
            number -= value;
        }
    }
    out
}

/// 小写字母序号：a 到 z，再往后 aa、ab……；0 照写数字。
fn alpha(number: u64) -> String {
    if number == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    let mut rest = number;
    while rest > 0 {
        rest -= 1;
        out.push(char::from(b'a' + (rest % 26) as u8));
        rest /= 26;
    }
    out.iter().rev().collect()
}

/// 点链接去哪儿。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LinkTarget {
    /// 交给系统浏览器（http、https、mailto）。
    Web(String),
    /// 相对 Markdown 文件所在目录的路径，在预览栏里打开。
    File(PathBuf),
    /// 同一篇文档里的锚点（`#` 后面的部分，已经百分号解码）。
    Anchor(String),
}

/// 链接或图片地址 `url` 指向哪儿；`dir` 是 Markdown 文件所在的目录。别的协议为空；本地路径去掉
/// `#锚点` 和 `?参数` 后做百分号解码，`..`、`.` 按字面消掉（不跟符号链接），和文件树里打开的同一个
/// 文件路径一样。
pub(super) fn link_target(dir: &Path, url: &str) -> Option<LinkTarget> {
    let url = url.trim();
    if let Some((scheme, _)) = url.split_once(':')
        && is_scheme(scheme)
    {
        let web = ["http", "https", "mailto"].iter().any(|known| scheme.eq_ignore_ascii_case(known));
        return web.then(|| LinkTarget::Web(url.to_owned()));
    }
    if let Some(anchor) = url.strip_prefix('#') {
        return (!anchor.is_empty()).then(|| LinkTarget::Anchor(percent_decode(anchor)));
    }
    let path = url.split(['#', '?']).next().unwrap_or_default();
    (!path.is_empty()).then(|| LinkTarget::File(normalize(&dir.join(percent_decode(path)))))
}

/// 按字面消掉路径里的 `.` 和 `..`：`..` 去掉前一段，到了根就停在根上。不读磁盘。
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(component);
                }
            }
            _ => out.push(component),
        }
    }
    out
}

/// 百分号解码（`my%20file.md` 是 `my file.md`）；解出来不是 UTF-8 时原样不动。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|&byte| char::from(byte).to_digit(16));
        match (bytes[ix], hex(ix + 1), hex(ix + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                ix += 3;
            }
            (byte, ..) => {
                out.push(byte);
                ix += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// 像 URL 的协议名：字母开头，两个字符以上（排除 `C:` 这类盘符）。
fn is_scheme(text: &str) -> bool {
    text.len() > 1
        && text.starts_with(|ch: char| ch.is_ascii_alphabetic())
        && text.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// 网络图片：`http`、`https` 开头的地址。
fn is_remote(url: &str) -> bool {
    let url = url.trim();
    ["http://", "https://"]
        .iter()
        .any(|scheme| url.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme)))
}

/// `dir` 下的 Markdown 引用的图片地址 `url` 是本地的图片文件时，它的路径。
fn image_path(dir: &Path, url: &str) -> Option<PathBuf> {
    let Some(LinkTarget::File(path)) = link_target(dir, url) else {
        return None;
    };
    runode_preview::image_format(&path).map(|_| path)
}

/// 读出 Markdown 引用的本地图片文件 `path`，和预览栏打开图片文件一样读、画 SVG；读不了、画不了时
/// 为空。在后台调用。
pub(super) fn picture(path: &Path, svg: &SvgRenderer) -> Option<Picture> {
    picture_of(runode_preview::load(path), svg)
}

/// 读到的图片换成能画的样子；不是图片、画不了时为空。SVG 按自身尺寸画（预览栏打开 SVG 文件时
/// 会把小图放大重画，这里不要，徽章这类小图按原大小排）。
fn picture_of(content: Content, svg: &SvgRenderer) -> Option<Picture> {
    if let Content::Image { format: runode_preview::ImageFormat::Svg, bytes } = &content {
        return svg.render_single_frame(bytes, 1.).ok().map(Picture::Svg);
    }
    match loaded(content, svg) {
        Loaded::Image(image) => Some(Picture::Bitmap(image)),
        Loaded::Svg(image) => Some(Picture::Svg(image)),
        _ => None,
    }
}

/// 排版视图用到的颜色，都从终端主题算出来，对应 GitHub 的几个颜色变量。
#[derive(Clone, Copy)]
struct Colors {
    fg: Hsla,
    /// fgColor-muted：引用、六级标题、脚注的文字。
    muted: Hsla,
    link: Hsla,
    /// neutral-muted：行内代码的底色。
    code_bg: Hsla,
    /// canvas-subtle：代码块、按键、表格偶数行的底色。
    subtle: Hsla,
    /// borderColor-default：引用竖线、表格边框、分割线。
    border: Hsla,
    /// borderColor-muted：一二级标题下面的线。
    border_muted: Hsla,
    selection: Hsla,
    /// 提示块的颜色，按 `Alert` 的顺序。
    alerts: [Hsla; 5],
}

impl Colors {
    fn new(fg: Rgb, bg: Rgb, palette: &[Rgb; 16]) -> Self {
        // GitHub 深色主题的灰比浅色的调得重，按背景的亮度分开调。
        let dark = 0.299 * f32::from(bg.0) + 0.587 * f32::from(bg.1) + 0.114 * f32::from(bg.2) < 128.;
        let (muted, code) = if dark { (0.6, 0.18) } else { (0.74, 0.08) };
        Self {
            fg: hsla(fg),
            muted: hsla(bg.mix(fg, muted)),
            link: hsla(palette[4]),
            code_bg: hsla(bg.mix(fg, code)),
            subtle: hsla(bg.mix(fg, 0.05)),
            border: hsla(bg.mix(fg, 0.2)),
            border_muted: hsla(bg.mix(fg, 0.14)),
            selection: hsla(bg.mix(RENAMED, 0.30)),
            alerts: [palette[4], palette[2], palette[5], palette[3], palette[1]].map(hsla),
        }
    }

    fn alert(&self, alert: Alert) -> Hsla {
        self.alerts[alert as usize]
    }
}

/// 一段文字的高亮：粗体、斜体、删除线，链接是蓝色，鼠标底下的那个链接（第 `hover` 个）带下划线。
fn rich_highlights(rich: &Rich, colors: Colors, hover: Option<usize>) -> Vec<(Range<usize>, HighlightStyle)> {
    let hovered = hover.and_then(|ix| rich.links.get(ix)).map(|(range, _)| range.clone());
    rich.runs
        .iter()
        .map(|(range, style, link)| {
            let underline =
                hovered.as_ref().is_some_and(|hovered| hovered.start <= range.start && range.end <= hovered.end);
            let highlight = HighlightStyle {
                color: link.then_some(colors.link),
                underline: underline.then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(colors.link),
                    wavy: false,
                }),
                font_weight: style.bold.then_some(FontWeight::SEMIBOLD),
                font_style: style.italic.then_some(FontStyle::Italic),
                strikethrough: style.strike.then_some(StrikethroughStyle { thickness: px(1.), color: None }),
                ..HighlightStyle::default()
            };
            (range.clone(), highlight)
        })
        .collect()
}

/// 画一行时共用的东西。
struct Paint {
    mono: SharedString,
    colors: Colors,
    selection: Option<(MdPos, MdPos)>,
    hover: Option<(TextKey, usize)>,
    texts: Texts,
}

impl Paint {
    /// 一段能选中的带样式文字：行内代码和按键换成终端字体、垫上圆角底色。
    fn rich(&self, rich: &Rich, key: TextKey) -> MdText {
        let hover = self.hover.filter(|(hovered, _)| *hovered == key).map(|(_, ix)| ix);
        let mono: Vec<_> = rich
            .runs
            .iter()
            .filter(|(_, style, _)| style.code || style.kbd)
            .map(|(range, ..)| (range.clone(), self.mono.clone()))
            .collect();
        let boxes = rich
            .runs
            .iter()
            .filter(|(_, style, _)| style.code || style.kbd)
            .map(|(range, style, _)| TextBox {
                range: range.clone(),
                background: if style.kbd { self.colors.subtle } else { self.colors.code_bg },
                border: style.kbd.then_some(self.colors.border),
            })
            .collect();
        let styled = StyledText::new(rich.text.clone())
            .with_highlights(rich_highlights(rich, self.colors, hover))
            .with_font_family_overrides(mono);
        self.text(styled, rich.text.len(), key).boxes(boxes)
    }

    fn text(&self, styled: StyledText, len: usize, key: TextKey) -> MdText {
        let selected = self.selection.and_then(|(start, end)| select::selected_range(start, end, key, len));
        MdText::new(styled, key, self.texts.clone()).selected(selected.map(|range| (range, self.colors.selection)))
    }
}

/// `rich` 里的链接报给辅助工具：链接画在文字里，不是单独的元素，这里每个链接另放一个不占地方的
/// 节点，以链接的文字为名，按下时和点击一样打开它。
fn link_nodes(rich: &Rich, view: &WeakEntity<WindowView>) -> Vec<AnyElement> {
    rich.links
        .iter()
        .enumerate()
        .map(|(ix, (range, url))| {
            let (view, url) = (view.clone(), url.clone());
            div()
                .id(("md-link", ix))
                .absolute()
                .role(Role::Link)
                .aria_label(rich.text.get(range.clone()).unwrap_or_default().to_owned())
                .aria_description(url.clone())
                .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                    view.update(cx, |this, cx| this.open_markdown_link(&url, cx)).ok();
                })
                .into_any_element()
        })
        .collect()
}

impl WindowView {
    /// 预览栏现在是不是 Markdown 的排版视图：普通标签、没截断的 Markdown 文件、没切到源码。
    pub(super) fn markdown_shown(&self, preview: &Preview) -> bool {
        !self.preview_source && preview.typesettable()
    }

    /// 排版视图；还没解析完时空着。`width` 是预览栏的宽度，图片据此缩小；`mono` 是终端字体。
    pub(super) fn render_markdown(
        &self,
        preview: &Preview,
        width: f32,
        mono: SharedString,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = &preview.markdown_view;
        // 这一帧画的文字重新登记。
        view.texts.borrow_mut().clear();
        let body = div().flex_1().min_h_0().relative().on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                window.focus(&this.preview_focus, cx);
                this.press_markdown(event, cx);
            }),
        );
        let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content else {
            return body.into_any_element();
        };
        // `gpui::list` 第一次排版和栏宽变了时把估的行高全丢掉，没量过的行算 0 高，滚动条就短了一大截。
        // 等它按新宽度排过一次，再按这个宽度重新估上。
        let laid = f32::from(view.list.viewport_bounds().size.width);
        if view.hinted_width.get() != laid {
            if laid > 0. {
                let top = view.list.logical_scroll_top();
                view.list
                    .reset_with_uniform_height(doc.rows.len(), row_hint(cx.global::<AppConfig>().0.preview_font_size));
                view.list.scroll_to(top);
            }
            view.hinted_width.set(laid);
            cx.notify();
        }
        let rows = list(
            view.list.clone(),
            cx.processor(move |this, ix: usize, window, cx| {
                this.render_markdown_row(ix, width, mono.clone(), fg, bg, window, cx)
            }),
        )
        .size_full();
        // 拖选时鼠标出了正文也接着跟；悬停的链接跟着鼠标换。
        let weak = cx.weak_entity();
        let events = canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let view = weak.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |this, cx| this.move_markdown(event, cx)).ok();
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        weak.update(cx, |this, cx| this.release_markdown(event, cx)).ok();
                    }
                });
            },
        )
        .absolute()
        .size_full();
        // 文字的颜色和留白都给到行上：`list` 不把自己的内边距套到各行，没设颜色的文字是 GPUI 默认的黑色。
        body.text_color(hsla(fg))
            .cursor(if view.hover.is_some() { CursorStyle::PointingHand } else { CursorStyle::IBeam })
            .child(rows)
            .child(events)
            .child(list_scrollbar("markdown-scroll", view.list.clone(), hsla(fg)))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_markdown_row(
        &self,
        ix: usize,
        width: f32,
        mono: SharedString,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(preview) = self.preview() else {
            return div().into_any_element();
        };
        let doc = match &preview.content {
            Some(Loaded::Text { markdown: Some(doc), .. }) => doc.clone(),
            _ => return div().into_any_element(),
        };
        let Some(row) = doc.rows.get(ix) else {
            return div().into_any_element();
        };
        let view = &preview.markdown_view;
        let font_size = cx.global::<AppConfig>().0.preview_font_size;
        let palette = ansi_palette(cx);
        let colors = Colors::new(fg, bg, &palette);
        let paint = Paint {
            mono: mono.clone(),
            colors,
            selection: view.selection.map(select::ordered),
            hover: view.hover,
            texts: view.texts.clone(),
        };
        // 间距按正文字号算；脚注的字小一号。
        let em = font_size;
        let size = if row.footnote { font_size * 0.75 } else { font_size };
        let line_height = size * LINE_HEIGHT;
        let gap = gap(&doc.rows, ix) * em;
        let prev = ix.checked_sub(1).and_then(|prev| doc.rows.get(prev));
        let next = doc.rows.get(ix + 1);
        // 提示块上下各有 0.5em 的内边距：标题行上面、最后一行下面。
        let alert_end = row.nest.iter().enumerate().any(|(depth, level)| {
            matches!(level, Nest::Quote { alert: Some(_), .. })
                && next.is_none_or(|next| !continues(Some(row), next, depth))
        });
        let bottom = if alert_end { em * 0.5 } else { 0. } + if ix + 1 == doc.rows.len() { PADDING_Y } else { 0. };
        let mut element = div()
            .id(("md-row", ix))
            .w_full()
            .flex()
            .px(px(PADDING_X))
            .when(ix == 0, |first| first.pt(px(PADDING_Y)))
            .pb(px(bottom))
            .text_size(px(size))
            .line_height(px(line_height));
        let weak = cx.entity().downgrade();
        let mut links = Vec::new();
        // 这一行报给辅助工具的样子：标题、段落、提示块标题和代码块报成整段文字；表格、图片在下面各自报。
        element = match &row.leaf {
            Leaf::Heading(level, rich) => {
                links = link_nodes(rich, &weak);
                element.role(Role::Heading).aria_level(usize::from(*level)).aria_label(rich.text.clone())
            }
            Leaf::Paragraph(rich) => {
                links = link_nodes(rich, &weak);
                element.role(Role::Paragraph).aria_label(rich.text.clone())
            }
            Leaf::AlertTitle(_, title) => element.role(Role::Label).aria_label(title.clone()),
            Leaf::Code { source, .. } => element.role(Role::Code).aria_label(source.clone()),
            Leaf::Table { .. } | Leaf::Rule | Leaf::Images(_) => element,
        };
        let mut nested_width = 0.;
        let mut quotes = 0;
        let mut depth = 0;
        // 最里面一层是普通引用时文字用弱化色；提示块里照常。
        let mut muted = row.footnote;
        for (level_ix, level) in row.nest.iter().enumerate() {
            match level {
                Nest::Quote { alert, .. } => {
                    let bar = em * 0.25;
                    nested_width += bar + 2. * em;
                    quotes += 1;
                    muted = alert.is_none() || row.footnote;
                    let top = if continues(prev, row, level_ix) { 0. } else { gap };
                    let color = alert.map_or(colors.border, |alert| colors.alert(alert));
                    element = element.child(div().flex_none().mt(px(top)).mr(px(em)).w(px(bar)).bg(color));
                }
                Nest::Item { marker, .. } => {
                    depth += 1;
                    let indent = if row.footnote { em } else { em * 2. };
                    nested_width += indent;
                    let color = if muted { colors.muted } else { colors.fg };
                    element = element.child(
                        div()
                            .flex_none()
                            .w(px(indent))
                            .pt(px(gap))
                            .pr(px(size * 0.4))
                            .flex()
                            .justify_end()
                            .children(marker.map(|marker| render_marker(marker, depth, size, color, colors, bg))),
                    );
                }
            }
        }
        let content: AnyElement = match &row.leaf {
            Leaf::Heading(level, rich) => {
                let size = em * [2., 1.5, 1.25, 1., 0.875, 0.85][usize::from((*level).clamp(1, 6) - 1)];
                div()
                    .text_size(px(size))
                    .line_height(relative(1.25))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(*level == 6, |heading| heading.text_color(colors.muted))
                    .when(*level <= 2, |heading| {
                        heading.pb(px(size * 0.3)).border_b_1().border_color(colors.border_muted)
                    })
                    .child(paint.rich(rich, (ix, 0)))
                    .into_any_element()
            }
            Leaf::Paragraph(rich) => div().child(paint.rich(rich, (ix, 0))).into_any_element(),
            Leaf::AlertTitle(alert, title) => {
                let color = colors.alert(*alert);
                let icon = match alert {
                    Alert::Note => ALERT_NOTE_ICON,
                    Alert::Tip => ALERT_TIP_ICON,
                    Alert::Important => ALERT_IMPORTANT_ICON,
                    Alert::Warning => ALERT_WARNING_ICON,
                    Alert::Caution => ALERT_CAUTION_ICON,
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(em * 0.5))
                    .pt(px(em * 0.5))
                    .text_color(color)
                    .font_weight(FontWeight::MEDIUM)
                    .line_height(relative(1.25))
                    .child(svg().path(icon).flex_none().size(px(em)).text_color(color))
                    .child(paint.text(StyledText::new(title.clone()), title.len(), (ix, 0)))
                    .into_any_element()
            }
            // shortcut: 代码块整块是一段文字、一次排版，几千行的代码块只要露出一点也整块排；遇到卡顿再
            // 拆成一行一个元素。
            Leaf::Code { text: code, spans, source } => {
                let runs: Vec<_> =
                    spans.iter().map(|span| (span.range.clone(), highlight_style(span.style, fg, &palette))).collect();
                let styled = StyledText::new(code.clone()).with_highlights(runs);
                let source = source.clone();
                let copied = source.clone();
                div()
                    .group("md-code")
                    .relative()
                    .w_full()
                    .rounded(px(6.))
                    .bg(colors.subtle)
                    .child(
                        div()
                            .id(("md-code-scroll", ix))
                            .font_family(mono)
                            .text_size(px(size * 0.85))
                            .line_height(relative(1.45))
                            .w_full()
                            .flex()
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .p(px(em))
                            .whitespace_nowrap()
                            // 按内容的宽度排，比栏宽时横着滚；直接放在块里会被拉成栏宽，滚不动。
                            .child(div().flex_none().child(paint.text(styled, code.len(), (ix, 0)))),
                    )
                    .child(
                        div()
                            .id("md-copy")
                            .role(Role::Button)
                            .aria_label(crate::i18n::tr("menu.copy"))
                            // 只有按下的处理，辅助工具按不到，另外登记；按钮悬停时才露出来，辅助工具照样按得到。
                            .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.to_string()));
                            })
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .px(px(6.))
                            .rounded(px(6.))
                            .bg(colors.subtle)
                            .border_1()
                            .border_color(colors.border)
                            .text_color(colors.muted)
                            .cursor_pointer()
                            .invisible()
                            .group_hover("md-code", |button| button.visible())
                            .hover(|button| button.text_color(colors.fg))
                            .child(crate::i18n::tr("menu.copy"))
                            // 按下就复制，也不让正文的拖选接着开始。
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                cx.write_to_clipboard(ClipboardItem::new_string(source.to_string()));
                            }),
                    )
                    .into_any_element()
            }
            Leaf::Table { aligns, head, rows, widest } => {
                let columns = aligns.len().max(1);
                let empty = Rich::default();
                let all = || std::iter::once(head).chain(rows);
                // 表头上下两条边、表体一条，格子一样高，按像素就知道哪几行露出来。
                let (head_h, body_h) = (line_height + 14., line_height + 13.);
                let viewport = f32::from(view.list.viewport_bounds().size.height);
                let shown = shown_rows(view.list.logical_scroll_top(), viewport, ix, head_h, body_h, rows.len());
                let (above, below) = (shown.start as f32 * body_h, (rows.len() - shown.end) as f32 * body_h);
                // 一列一列排：每列的宽是它最宽的格子（max-content），格子一样高，各行自然对齐。只画表头
                // 和露出来的行，上下用空白占住没画的行；每列垫一个看不见、不占高的估计最宽的格子撑住列宽。
                let table = div().flex().flex_none().whitespace_nowrap().children((0..columns).map(|column| {
                    let sizer = widest.get(column).and_then(|&row_ix| {
                        let cell = all().nth(row_ix)?.get(column)?;
                        let mono: Vec<_> = cell
                            .runs
                            .iter()
                            .filter(|(_, style, _)| style.code || style.kbd)
                            .map(|(range, ..)| (range.clone(), mono.clone()))
                            .collect();
                        let styled = StyledText::new(cell.text.clone())
                            .with_highlights(rich_highlights(cell, colors, None))
                            .with_font_family_overrides(mono);
                        let sizer = div().h_0().overflow_hidden().invisible().px(px(13.)).border_r_1();
                        Some(sizer.when(row_ix == 0, |cell| cell.font_weight(FontWeight::SEMIBOLD)).child(styled))
                    });
                    let body = rows.iter().enumerate().skip(shown.start).take(shown.len());
                    let cells = std::iter::once((0, head))
                        .chain(body.map(|(row_ix, row)| (row_ix + 1, row)))
                        .map(|(row_ix, row)| (row_ix, row.get(column).unwrap_or(&empty)));
                    let cells = cells.map(|(row_ix, cell)| {
                        // 表头算第 0 行；表体的偶数行（第 2、4……行）垫底色。
                        let border = if row_ix == 0 { 2. } else { 1. };
                        div()
                            .id(("md-cell", columns * row_ix + column))
                            .role(if row_ix == 0 { Role::ColumnHeader } else { Role::Cell })
                            .aria_label(cell.text.clone())
                            .aria_row_index(row_ix)
                            .aria_column_index(column)
                            .children(link_nodes(cell, &weak))
                            .h(px(line_height + 12. + border))
                            .px(px(13.))
                            .flex()
                            .items_center()
                            .map(|cell| match aligns.get(column) {
                                Some(Align::Center) => cell.justify_center(),
                                Some(Align::Right) => cell.justify_end(),
                                _ => cell,
                            })
                            .border_b_1()
                            .border_r_1()
                            .when(row_ix == 0, |cell| cell.border_t_1().font_weight(FontWeight::SEMIBOLD))
                            .when(column == 0, |cell| cell.border_l_1())
                            .border_color(colors.border)
                            .when(row_ix > 0 && row_ix % 2 == 0, |cell| cell.bg(colors.subtle))
                            .child(paint.rich(cell, (ix, columns * row_ix + column)))
                            .into_any_element()
                    });
                    let mut cells: Vec<AnyElement> = cells.collect();
                    if above > 0. {
                        cells.insert(1, div().h(px(above)).into_any_element());
                    }
                    if below > 0. {
                        cells.push(div().h(px(below)).into_any_element());
                    }
                    div().flex_none().flex().flex_col().children(sizer).children(cells)
                }));
                div()
                    .id(("md-table", ix))
                    .role(Role::Table)
                    .aria_row_count(rows.len() + 1)
                    .aria_column_count(columns)
                    .w_full()
                    .flex()
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .child(table)
                    .into_any_element()
            }
            Leaf::Rule if row.footnote => div().h(px(1.)).bg(colors.border).into_any_element(),
            Leaf::Rule => div().h(px(em * 0.25)).bg(colors.border).into_any_element(),
            Leaf::Images(images) => {
                let room = width - 1. - 2. * PADDING_X - nested_width;
                let images = images.iter().enumerate().map(|(image_ix, image)| {
                    let shown = self.render_markdown_image(image, room, line_height, colors, view, window, cx);
                    // 以替代文字为名报给辅助工具；外面套着链接的报成链接。
                    let item = div().id(("md-image", image_ix)).aria_label(image.alt.clone());
                    match &image.link {
                        Some(link) => {
                            let link = link.clone();
                            item.role(Role::Link)
                                .aria_description(link.clone())
                                .cursor_pointer()
                                .on_press(cx, move |this, _, cx| this.open_markdown_link(&link, cx))
                                .child(shown)
                                .into_any_element()
                        }
                        None => item.role(Role::Image).child(shown).into_any_element(),
                    }
                });
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap(px(em * 0.25))
                    .when(row.center, |images| images.justify_center())
                    .children(images)
                    .into_any_element()
            }
        };
        element
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt(px(gap))
                    .mr(px(em * quotes as f32))
                    .when(muted, |content| content.text_color(colors.muted))
                    .when(row.center, |content| content.text_center())
                    .child(content)
                    .children(links),
            )
            .into_any_element()
    }

    /// 一张图片：按栏宽 `room` 等比缩小，不放大；还没有图的显示替代文字。
    #[allow(clippy::too_many_arguments)]
    fn render_markdown_image(
        &self,
        image: &RowImage,
        room: f32,
        line_height: f32,
        colors: Colors,
        view: &View,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picture = if image.remote { view.remote.get(&image.url).cloned().flatten() } else { image.picture.clone() };
        let size = picture.as_ref().and_then(|picture| match picture {
            Picture::Bitmap(bitmap) => bitmap.clone().use_render_image(window, cx).map(|bitmap| {
                let size = bitmap.size(0);
                (size.width.0 as f32, size.height.0 as f32)
            }),
            Picture::Svg(svg) => {
                let size = svg.size(0);
                Some((size.width.0 as f32 / SMOOTH_SVG_SCALE_FACTOR, size.height.0 as f32 / SMOOTH_SVG_SCALE_FACTOR))
            }
        });
        match (picture, size) {
            (Some(picture), Some((w, h))) if w > 0. && h > 0. => {
                let (w, h) = match (image.width.map(|w| w as f32), image.height.map(|h| h as f32)) {
                    (Some(want_w), Some(want_h)) => (want_w, want_h),
                    (Some(want_w), None) => (want_w, h * want_w / w),
                    (None, Some(want_h)) => (w * want_h / h, want_h),
                    (None, None) => (w, h),
                };
                let fit = (room.max(1.) / w).min(1.);
                let source = match picture {
                    Picture::Bitmap(bitmap) => ImageSource::Image(bitmap),
                    Picture::Svg(svg) => ImageSource::Render(svg),
                };
                img(source).w(px(w * fit)).h(px(h * fit)).into_any_element()
            }
            // 还在解码的图片先空着，解好了列表重画时再量高度。
            (Some(Picture::Bitmap(_)), None) => div().h(px(line_height)).into_any_element(),
            // 网络图片下载中、下不了的，和读不了的本地图片一样显示替代文字。
            _ => div().italic().text_color(colors.muted).child(format!("[{}]", image.alt)).into_any_element(),
        }
    }

    /// 在排版视图里按下鼠标：单击把光标放在那里（按着 Shift 时把选区延到那里），双击选词，三击选
    /// 一整段；点在链接上的记下来，松开时没拖动就打开。
    fn press_markdown(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(preview) = self.preview_mut() else {
            return;
        };
        let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content else {
            return;
        };
        let doc = doc.clone();
        let view = &mut preview.markdown_view;
        let (pos, exact) = {
            let texts = view.texts.borrow();
            (text::hit(&texts, event.position), text::exact_hit(&texts, event.position))
        };
        let Some(pos) = pos else {
            view.selection = None;
            cx.notify();
            return;
        };
        let text = doc.rows.get(pos.row).and_then(|row| row.leaf.texts().get(pos.text).map(|text| text.to_string()));
        let at = |offset| MdPos { offset, ..pos };
        match (event.click_count, text) {
            (2, Some(text)) => {
                let word = crate::ui::text_field::word_range_at(&text, pos.offset.min(text.len()));
                view.selection = Some((at(word.start), at(word.end)));
            }
            (3.., Some(text)) => view.selection = Some((at(0), at(text.len()))),
            _ => {
                view.selection = match view.selection {
                    Some((anchor, _)) if event.modifiers.shift => Some((anchor, pos)),
                    _ => Some((pos, pos)),
                };
            }
        }
        let link = exact.and_then(|(key, offset)| doc.link_at(key, offset)).map(|(_, url)| url.to_owned());
        // 双击、三击是在选字，松开时不打开链接。
        view.drag = Some(Drag { origin: event.position, moved: event.click_count > 1, link });
        cx.notify();
    }

    /// 鼠标在窗口里移动：按着时延长选区，拖到正文上下边外面时跟着滚；没按着时找鼠标底下的链接。
    fn move_markdown(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let shown = self.preview().is_some_and(|preview| self.markdown_shown(preview));
        let Some(preview) = self.preview_mut().filter(|_| shown) else {
            return;
        };
        let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content else {
            return;
        };
        let doc = doc.clone();
        let view = &mut preview.markdown_view;
        if let Some(drag) = &mut view.drag {
            if event.pressed_button != Some(MouseButton::Left) {
                view.drag = None;
                return;
            }
            let moved = event.position - drag.origin;
            if !drag.moved && f32::from(moved.x).hypot(f32::from(moved.y)) < DRAG_THRESHOLD {
                return;
            }
            drag.moved = true;
            let viewport = view.list.viewport_bounds();
            if event.position.y < viewport.top() {
                view.list.scroll_by(event.position.y - viewport.top());
            } else if event.position.y > viewport.bottom() {
                view.list.scroll_by(event.position.y - viewport.bottom());
            }
            let head = text::hit(&view.texts.borrow(), event.position);
            if let (Some(head), Some((anchor, old))) = (head, view.selection)
                && head != old
            {
                view.selection = Some((anchor, head));
            }
            cx.notify();
            return;
        }
        let hover = view
            .list
            .viewport_bounds()
            .contains(&event.position)
            .then(|| text::exact_hit(&view.texts.borrow(), event.position))
            .flatten()
            .and_then(|(key, offset)| doc.link_at(key, offset).map(|(ix, _)| (key, ix)));
        if hover != view.hover {
            view.hover = hover;
            cx.notify();
        }
    }

    /// 松开鼠标：没拖动时清掉空的选区，按下时点在链接上就打开它。
    fn release_markdown(&mut self, _: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(view) = self.preview_mut().map(|preview| &mut preview.markdown_view) else {
            return;
        };
        let Some(drag) = view.drag.take() else {
            return;
        };
        if drag.moved {
            return;
        }
        if view.selection.is_some_and(|(anchor, head)| anchor == head) {
            view.selection = None;
            cx.notify();
        }
        if let Some(link) = drag.link {
            self.open_markdown_link(&link, cx);
        }
    }

    /// 排版视图里选中的纯文本；没选中时为空。
    pub(super) fn markdown_selected_text(&self, preview: &Preview) -> Option<String> {
        let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content else {
            return None;
        };
        let (start, end) = select::ordered(preview.markdown_view.selection?);
        (start != end).then(|| select::copy_text(doc, start, end)).filter(|text| !text.is_empty())
    }

    /// 排版视图全选。
    pub(super) fn select_all_markdown(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview_mut()
            && let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content
        {
            preview.markdown_view.selection = select::select_all(doc);
            cx.notify();
        }
    }

    /// 点了排版视图里的链接：网址交给系统浏览器，本地文件在预览栏里打开（和文件树单击一样开成
    /// 临时标签），`#锚点` 滚到对应的标题或脚注，别的不管。
    fn open_markdown_link(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(dir) = self.preview().and_then(|preview| preview.path.parent()).map(Path::to_path_buf) else {
            return;
        };
        match link_target(&dir, url) {
            Some(LinkTarget::Web(url)) => cx.open_url(&url),
            Some(LinkTarget::File(path)) if path.is_file() => self.open_preview(&path, false, cx),
            Some(LinkTarget::Anchor(id)) => {
                if let Some(preview) = self.preview_mut()
                    && let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content
                    && let Some(row) = doc.anchor(&id)
                {
                    preview.markdown_view.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// 换上解析好的文档后，在后台下载里面的网络图片，下好一张换上一张。`cancel` 是读这次文件的那个，
    /// 又重读了时丢掉结果（重读会再要一次，下过的直接拿到）。
    pub(super) fn load_remote_images(
        &mut self,
        id: crate::window::model::WorkspaceId,
        cancel: &Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        let Some((preview, _)) = self.preview_for(id, cancel) else {
            return;
        };
        let Some(Loaded::Text { markdown: Some(doc), .. }) = &preview.content else {
            return;
        };
        let svg = cx.svg_renderer();
        // 下好的不再要；下不了的再要一次，联网了能下好。
        let loaded = |url: &String| matches!(preview.markdown_view.remote.get(url), Some(Some(_)));
        for url in doc.remote_urls().into_iter().filter(|url| !loaded(url)) {
            let pending = remote::fetch(&url, &svg);
            let cancel = cancel.clone();
            cx.spawn(async move |this, cx| {
                let picture = pending.await.ok().flatten();
                this.update(cx, |this, cx| {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some((preview, _)) = this.preview_for(id, &cancel) {
                        preview.markdown_view.remote.insert(url, picture);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }
}

/// 列表项的记号：圆点按层数换成实心圆、空心圆、实心方块，有序列表是序号，任务列表是勾选框。
fn render_marker(marker: Marker, depth: usize, size: f32, color: Hsla, colors: Colors, bg: Rgb) -> AnyElement {
    let line = div().h(px(size * LINE_HEIGHT)).flex().items_center();
    match marker {
        Marker::Bullet => {
            let dot = (size * 0.36).round();
            let shape = match depth % 3 {
                1 => div().size(px(dot)).rounded_full().bg(color),
                2 => div().size(px(dot)).rounded_full().border_1().border_color(color),
                _ => div().size(px((size * 0.32).round())).bg(color),
            };
            line.child(shape).into_any_element()
        }
        Marker::Number(_) => div().text_color(color).child(marker_text(marker, depth)).into_any_element(),
        Marker::Task(done) => {
            let side = (size * 0.95).round();
            line.child(div().size(px(side)).rounded(px(3.)).border_1().flex().items_center().justify_center().map(
                |check| {
                    if done {
                        check
                            .bg(colors.link)
                            .border_color(colors.link)
                            .child(svg().path(CHECK_ICON).size(px(side - 3.)).text_color(hsla(bg)))
                    } else {
                        check.border_color(colors.muted)
                    }
                },
            ))
            .into_any_element()
        }
    }
}
