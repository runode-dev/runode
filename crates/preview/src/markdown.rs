//! Markdown 解析成不碰界面的块结构：标题、段落、列表、引用、代码块、表格、分割线、图片，段落里是
//! 带样式的行内片段。照 GitHub 的写法支持 GFM 的表格、删除线、任务列表、裸网址自动成链接、提示块
//! （`> [!NOTE]` 这类）和脚注；标题按 GitHub 的规则算出锚点。HTML 块和行内 HTML 不解释，原文当
//! 普通文字，只认 `<kbd>`。代码块按标的语言高亮，颜色和 `highlight` 一样是调色板语义的。
//!
//! 图片在段落里单独成块（段落从图片处断开），界面才好按栏宽画；标题和表格单元格里的图片只留替代
//! 文字。
//!
//! 脚注和 GitHub 一样按第一次引用的先后编号，引用处是上标数字的链接（`#fn-标签`），定义集中在文末
//! 的 `Block::Footnotes` 里，各条末尾带一个指回引用处（`#fnref-标签`）的返回箭头。

use std::{
    collections::HashMap,
    mem,
    ops::Range,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::highlight::{Span, highlight_code};

/// 一段同样样式、同一个链接的文字。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inline {
    pub text: String,
    pub style: InlineStyle,
    /// 在链接里时是链接的地址，原样不解析。
    pub link: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    /// 行内代码。
    pub code: bool,
    /// `<kbd>` 里的按键。
    pub kbd: bool,
    /// 脚注的引用：文字是上标的编号，链接是 `#fn-标签`，它自己的锚点是 `fnref-标签`。
    pub footnote: bool,
}

/// GitHub 的提示块（`> [!NOTE]` 这类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

/// 表格一列的对齐；没写对齐的按靠左。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// 级别 1 到 6；`id` 是 GitHub 规则算出的锚点（`#id` 的链接指向它），同名的依次加 `-1`、`-2`。
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        id: String,
    },
    /// 段落；HTML 块也是一个段落，原文一个片段，换行照留。
    Paragraph(Vec<Inline>),
    /// `start` 是有序列表第一项的序号，无序列表为空；`loose` 是各项之间空了行的松散列表。
    List {
        start: Option<u64>,
        loose: bool,
        items: Vec<ListItem>,
    },
    /// 引用；`alert` 是提示块的种类，普通引用为空。
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    Code {
        /// 代码块开头标的语言（信息串的第一个词），没标时为空。
        lang: Option<String>,
        lines: Vec<String>,
        /// 和 `lines` 一样多的行；认不出语言时为空。
        highlights: Vec<Vec<Span>>,
    },
    Table {
        aligns: Vec<Align>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    Image {
        url: String,
        alt: String,
    },
    /// 文末的脚注定义，按编号排好；只有被引用过的才在。
    Footnotes(Vec<Footnote>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Footnote {
    /// 标签（小写），锚点是 `fn-标签`。
    pub label: String,
    /// 定义的内容，最后一段末尾带着返回箭头的链接。
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListItem {
    /// 任务列表的勾选框：勾上为真；不是任务时为空。
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// 按扩展名判断是不是 Markdown（md、markdown、mdx），不分大小写。mdx 只按 Markdown 解析。
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ["md", "markdown", "mdx"].iter().any(|known| ext.eq_ignore_ascii_case(known)))
}

/// 引用、列表、列表项和脚注定义最多套这么多层，再往里的不再新建一层，内容并进最里面那层。解析结果、
/// 界面摊平和释放块树都按层数递归，几万层的 `> ` 不设限会把栈撑爆。
const MAX_NESTING: usize = 64;

/// 裸网址最长认这么多字节，一长串没有空白的文字里每个候选网址都只往后看这么多，整体是线性的。
const MAX_AUTOLINK: usize = 2048;

/// 主循环和找裸网址时每处理这么多个事件、候选看一次 `cancel`。
const CANCEL_EVERY: usize = 1024;

/// 解析 `text`。`cancel` 被置上时停下、返回空。
pub fn parse_markdown(text: &str, cancel: &AtomicBool) -> Option<Vec<Block>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut builder = Builder { cancel: Some(cancel), ..Builder::default() };
    for (ix, event) in Parser::new_ext(text, options).enumerate() {
        if ix.is_multiple_of(CANCEL_EVERY) && cancel.load(Ordering::Relaxed) {
            return None;
        }
        builder.event(event);
    }
    builder.flush_implicit();
    builder.finish_footnotes();
    (!cancel.load(Ordering::Relaxed)).then_some(builder.root)
}

/// GitHub 给标题算锚点的规则（github-slugger）：转小写，去掉标点和符号（留下字母、数字、`-`、`_`
/// 和空格），空格换成 `-`。重名的由调用方加编号。
fn slug(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter_map(|ch| match ch {
            ' ' => Some('-'),
            '-' | '_' => Some(ch),
            _ if ch.is_alphanumeric() => Some(ch),
            _ => None,
        })
        .collect()
}

/// 给一篇文档里的标题发锚点，和 GitHub 一样重名的依次加 `-1`、`-2`。
#[derive(Default)]
struct Slugger(HashMap<String, usize>);

impl Slugger {
    fn next(&mut self, text: &str) -> String {
        let base = slug(text);
        let mut id = base.clone();
        while self.0.contains_key(&id) {
            let count = self.0.entry(base.clone()).or_default();
            *count += 1;
            id = format!("{base}-{count}");
        }
        self.0.insert(id.clone(), 0);
        id
    }
}

/// 数字换成上标数字，脚注的引用这样画（界面不能单独给一段文字换字号、抬高）。
fn superscript(number: usize) -> String {
    const DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    number.to_string().chars().map(|digit| DIGITS[digit.to_digit(10).unwrap_or(0) as usize]).collect()
}

/// `text` 里 GFM 会自动变成链接的裸网址（`http://`、`https://`、`www.` 开头）：占的字节范围和链接
/// 地址（`www.` 开头的补上 `http://`）。`before` 是 `text` 前面紧挨着的那个字符。网址前面得是开头、
/// 空白或 `*_~(`，域名里至少有一个点；末尾的句读、多出来的右括号和 `&xx;` 不算在网址里。`cancel`
/// 被置上时停下，返回已经找到的。
fn find_autolinks(text: &str, before: Option<char>, cancel: &AtomicBool) -> Vec<(Range<usize>, String)> {
    let mut found = Vec::new();
    let mut prev = before;
    let mut ix = 0;
    let mut steps = 0usize;
    while ix < text.len() {
        steps += 1;
        if steps.is_multiple_of(CANCEL_EVERY) && cancel.load(Ordering::Relaxed) {
            break;
        }
        let rest = &text[ix..];
        let starts = prev.is_none_or(|ch| ch.is_whitespace() || matches!(ch, '*' | '_' | '~' | '('));
        let scheme = ["https://", "http://", "www."]
            .into_iter()
            .find(|scheme| rest.get(..scheme.len()).is_some_and(|head| head.eq_ignore_ascii_case(scheme)));
        if starts
            && let Some(scheme) = scheme
            && let Some(len) = autolink_len(rest, scheme.len())
        {
            let link = &rest[..len];
            let url = if scheme == "www." { format!("http://{link}") } else { link.to_owned() };
            found.push((ix..ix + len, url));
            prev = link.chars().next_back();
            ix += len;
            continue;
        }
        let ch = rest.chars().next().unwrap_or_default();
        prev = Some(ch);
        ix += ch.len_utf8();
    }
    found
}

/// `text` 开头的网址有多长；协议名占 `scheme` 个字节（`www.` 也算在内）。域名不合格时为空。网址到
/// 空白、`<` 或者不是字母数字的非 ASCII 字符（`，`、`。` 这类中文标点）为止，中文域名、路径里的汉字
/// 照算；最长 `MAX_AUTOLINK` 字节。
fn autolink_len(text: &str, scheme: usize) -> Option<usize> {
    let www = text[..scheme].eq_ignore_ascii_case("www.");
    let host_start = if www { 0 } else { scheme };
    let mut end = text.len();
    let mut in_host = true;
    for (ix, ch) in text.char_indices() {
        if ix >= MAX_AUTOLINK || ch.is_whitespace() || ch == '<' || (!ch.is_ascii() && !ch.is_alphanumeric()) {
            end = ix;
            break;
        }
        if ix < host_start || !in_host {
            continue;
        }
        if matches!(ch, '/' | '?' | '#' | ':') {
            in_host = false;
        } else if !ch.is_alphanumeric()
            && !matches!(ch, '-' | '_' | '.' | '!' | ',' | '*' | '~' | '\'' | '"' | ')' | ';' | '&')
        {
            // 域名里有末尾去不掉的字符，不用再往后看（`(www.(www.` 这类长串不必每个候选都扫到底）。
            return None;
        }
    }
    let (open, mut close) = (text[..end].matches('(').count(), text[..end].matches(')').count());
    // 末尾的句读不算；右括号比左括号多时多出来的那个不算；`&lt;` 这类实体引用不算。
    loop {
        let link = &text[..end];
        let Some(last) = link.chars().next_back() else { break };
        if matches!(last, '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"') {
            end -= last.len_utf8();
        } else if last == ')' && close > open {
            end -= 1;
            close -= 1;
        } else if let Some(body) = link.strip_suffix(';')
            && let Some(amp) = body.rfind('&')
            && amp + 1 < body.len()
            && body[amp + 1..].chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            end = amp;
        } else {
            break;
        }
    }
    let host = text.get(host_start..end)?.split(['/', '?', '#', ':']).next().unwrap_or_default();
    let valid = host.contains('.')
        && host.split('.').all(|part| !part.is_empty())
        && host.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    valid.then_some(end)
}

/// 段落里不在链接、行内代码和按键里的文字，把裸网址拆出来变成链接。
fn autolink(inlines: Vec<Inline>, cancel: &AtomicBool) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        let before = out.last().and_then(|last| last.text.chars().next_back());
        if inline.link.is_some() || inline.style.code || inline.style.kbd {
            out.push(inline);
            continue;
        }
        let links = find_autolinks(&inline.text, before, cancel);
        if links.is_empty() {
            out.push(inline);
            continue;
        }
        let mut at = 0;
        for (range, url) in links {
            if range.start > at {
                out.push(Inline { text: inline.text[at..range.start].to_owned(), ..inline.clone() });
            }
            out.push(Inline { text: inline.text[range.clone()].to_owned(), style: inline.style, link: Some(url) });
            at = range.end;
        }
        if at < inline.text.len() {
            out.push(Inline { text: inline.text[at..].to_owned(), ..inline });
        }
    }
    out
}

/// 装着块的容器，嵌套时一层一层压栈。
enum Frame {
    Quote(Option<Alert>, Vec<Block>),
    List {
        start: Option<u64>,
        loose: bool,
        items: Vec<ListItem>,
    },
    Item(ListItem),
    /// 脚注的定义：标签（小写）和内容。
    Footnote(String, Vec<Block>),
}

/// 正在收行内片段的那个块。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    /// `implicit` 是紧凑列表项里没有段落标记的文字，遇到下一个块或者列表项结束时收成段落。
    Paragraph {
        implicit: bool,
    },
    Heading(u8),
    Cell,
}

#[derive(Default)]
struct Table {
    aligns: Vec<Align>,
    head: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
}

#[derive(Default)]
struct Builder<'a> {
    /// 总是有；只为了能 `Default`。
    cancel: Option<&'a AtomicBool>,
    root: Vec<Block>,
    stack: Vec<Frame>,
    /// 套到 `MAX_NESTING` 层以后没有新建的引用、列表、列表项和脚注定义还有几层没结束；它们的结束
    /// 事件只用来抵消这个数。
    flattened: usize,
    open: Option<Open>,
    inlines: Vec<Inline>,
    bold: u32,
    italic: u32,
    strike: u32,
    kbd: u32,
    links: Vec<String>,
    /// 在图片里：地址和收到的替代文字。
    image: Option<(String, String)>,
    /// 在代码块里：语言和收到的代码。
    code: Option<(Option<String>, String)>,
    /// 在 HTML 块里：收到的原文。
    html: Option<String>,
    table: Option<Table>,
    slugs: Slugger,
    /// 被引用过的脚注标签（小写），按第一次引用的先后排，位置加一就是编号。
    footnote_refs: Vec<String>,
    /// 收到的脚注定义，标签（小写）对内容；同名的只要第一个。
    footnote_defs: HashMap<String, Vec<Block>>,
}

impl Builder<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&text);
                } else {
                    self.push_text(&text, false);
                }
            }
            Event::Code(text) => self.push_text(&text, true),
            Event::Html(text) => match &mut self.html {
                Some(html) => html.push_str(&text),
                None => self.push_text(&text, false),
            },
            Event::InlineHtml(text) => {
                let tag = text.trim().to_ascii_lowercase();
                match tag.as_str() {
                    "<kbd>" => self.kbd += 1,
                    "</kbd>" => self.kbd = self.kbd.saturating_sub(1),
                    _ => self.push_text(&text, false),
                }
            }
            Event::FootnoteReference(label) => {
                let label = label.to_lowercase();
                let number = match self.footnote_refs.iter().position(|known| *known == label) {
                    Some(ix) => ix + 1,
                    None => {
                        self.footnote_refs.push(label.clone());
                        self.footnote_refs.len()
                    }
                };
                let style = InlineStyle { footnote: true, ..InlineStyle::default() };
                self.push_inline(&superscript(number), style, Some(format!("#fn-{label}")));
            }
            Event::SoftBreak => self.push_text(" ", false),
            Event::HardBreak => self.push_text("\n", false),
            Event::Rule => {
                self.flush_implicit();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(checked) if self.flattened == 0 => {
                if let Some(Frame::Item(item)) =
                    self.stack.iter_mut().rev().find(|frame| matches!(frame, Frame::Item(_)))
                {
                    item.task = Some(checked);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        // 列表项总跟着建好的列表新建一层，所以栈最多 `MAX_NESTING + 1` 层。
        let nests = matches!(tag, Tag::BlockQuote(_) | Tag::List(_) | Tag::FootnoteDefinition(_));
        if (nests || matches!(tag, Tag::Item)) && (self.flattened > 0 || (nests && self.stack.len() >= MAX_NESTING)) {
            self.flush_implicit();
            self.flattened += 1;
            return;
        }
        match tag {
            Tag::Paragraph => {
                // 列表项里有段落标记的是松散列表。
                if let [.., Frame::List { loose, .. }, Frame::Item(_)] = self.stack.as_mut_slice() {
                    *loose = true;
                }
                self.open_inlines(Open::Paragraph { implicit: false });
            }
            Tag::Heading { level, .. } => self.open_inlines(Open::Heading(level as u8)),
            Tag::BlockQuote(kind) => {
                self.flush_implicit();
                let alert = kind.map(|kind| match kind {
                    BlockQuoteKind::Note => Alert::Note,
                    BlockQuoteKind::Tip => Alert::Tip,
                    BlockQuoteKind::Important => Alert::Important,
                    BlockQuoteKind::Warning => Alert::Warning,
                    BlockQuoteKind::Caution => Alert::Caution,
                });
                self.stack.push(Frame::Quote(alert, Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush_implicit();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().map(str::to_owned),
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::MetadataBlock(_) => {
                self.flush_implicit();
                self.code = Some((Some("yaml".to_owned()), String::new()));
            }
            Tag::HtmlBlock => {
                self.flush_implicit();
                self.html = Some(String::new());
            }
            Tag::List(start) => {
                self.flush_implicit();
                self.stack.push(Frame::List { start, loose: false, items: Vec::new() });
            }
            Tag::Item => self.stack.push(Frame::Item(ListItem::default())),
            Tag::FootnoteDefinition(label) => {
                self.flush_implicit();
                self.stack.push(Frame::Footnote(label.to_lowercase(), Vec::new()));
            }
            Tag::Table(aligns) => {
                self.flush_implicit();
                let aligns = aligns
                    .into_iter()
                    .map(|align| match align {
                        Alignment::Center => Align::Center,
                        Alignment::Right => Align::Right,
                        Alignment::None | Alignment::Left => Align::Left,
                    })
                    .collect();
                self.table = Some(Table { aligns, ..Table::default() });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => self.open_inlines(Open::Cell),
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => self.links.push(dest_url.into_string()),
            Tag::Image { dest_url, .. } => self.image = Some((dest_url.into_string(), String::new())),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        if self.flattened > 0
            && matches!(tag, TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item | TagEnd::FootnoteDefinition)
        {
            self.flush_implicit();
            self.flattened -= 1;
            return;
        }
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.close_inlines(),
            TagEnd::BlockQuote(_) => {
                self.flush_implicit();
                if let Some(Frame::Quote(alert, blocks)) = self.stack.pop() {
                    self.push_block(Block::Quote { alert, blocks });
                }
            }
            TagEnd::FootnoteDefinition => {
                self.flush_implicit();
                if let Some(Frame::Footnote(label, blocks)) = self.stack.pop() {
                    self.footnote_defs.entry(label).or_insert(blocks);
                }
            }
            TagEnd::CodeBlock | TagEnd::MetadataBlock(_) => {
                if let Some((lang, code)) = self.code.take() {
                    let lines: Vec<String> = code.lines().map(str::to_owned).collect();
                    let highlights = lang
                        .as_deref()
                        .zip(self.cancel)
                        .filter(|_| !self.cancelled())
                        .and_then(|(lang, cancel)| highlight_code(lang, &lines, cancel))
                        .unwrap_or_default();
                    self.push_block(Block::Code { lang, lines, highlights });
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(html) = self.html.take() {
                    let text = html.trim_end_matches('\n').to_owned();
                    self.push_block(Block::Paragraph(vec![Inline { text, ..Inline::default() }]));
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List { start, loose, items }) = self.stack.pop() {
                    self.push_block(Block::List { start, loose, items });
                }
            }
            TagEnd::Item => {
                self.flush_implicit();
                if let Some(Frame::Item(item)) = self.stack.pop()
                    && let Some(Frame::List { items, .. }) = self.stack.last_mut()
                {
                    items.push(item);
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.head = mem::take(&mut table.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                self.open = None;
                let cell = self.autolink();
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::Table => {
                if let Some(Table { aligns, head, rows, .. }) = self.table.take() {
                    self.push_block(Block::Table { aligns, head, rows });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                let Some((url, alt)) = self.image.take() else {
                    return;
                };
                match self.open {
                    // 段落从图片处断开：前面的文字收成一段，图片单独成块，后面的文字另起一段。
                    Some(Open::Paragraph { implicit }) => {
                        self.close_inlines();
                        self.push_block(Block::Image { url, alt });
                        self.open = Some(Open::Paragraph { implicit });
                    }
                    _ => self.push_text(&alt, false),
                }
            }
            _ => {}
        }
    }

    fn open_inlines(&mut self, open: Open) {
        self.flush_implicit();
        self.open = Some(open);
        self.inlines.clear();
    }

    /// 收起正在收的段落或标题；只有空白的段落（比如两张图片之间的换行）不要。
    fn close_inlines(&mut self) {
        let inlines = self.autolink();
        match self.open.take() {
            Some(Open::Heading(level)) => {
                let text: String = inlines.iter().map(|inline| inline.text.as_str()).collect();
                let id = self.slugs.next(&text);
                self.push_block(Block::Heading { level, inlines, id });
            }
            Some(Open::Paragraph { .. }) if inlines.iter().any(|inline| !inline.text.trim().is_empty()) => {
                self.push_block(Block::Paragraph(inlines));
            }
            _ => {}
        }
    }

    /// 收到的行内片段拿走，裸网址拆成链接。
    fn autolink(&mut self) -> Vec<Inline> {
        let inlines = mem::take(&mut self.inlines);
        match self.cancel {
            Some(cancel) if !cancel.load(Ordering::Relaxed) => autolink(inlines, cancel),
            _ => inlines,
        }
    }

    /// 紧凑列表项里没有段落标记的文字在这里收成段落。
    fn flush_implicit(&mut self) {
        if self.open == Some(Open::Paragraph { implicit: true }) {
            self.close_inlines();
        }
    }

    fn push_text(&mut self, text: &str, code: bool) {
        let style = InlineStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code,
            kbd: self.kbd > 0,
            footnote: false,
        };
        self.push_inline(text, style, self.links.last().cloned());
    }

    fn push_inline(&mut self, text: &str, style: InlineStyle, link: Option<String>) {
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        if self.open.is_none() {
            self.open = Some(Open::Paragraph { implicit: true });
            self.inlines.clear();
        }
        match self.inlines.last_mut() {
            Some(last) if last.style == style && last.link == link => last.text.push_str(text),
            _ => self.inlines.push(Inline { text: text.to_owned(), style, link }),
        }
    }

    fn push_block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Frame::Quote(_, blocks) | Frame::Item(ListItem { blocks, .. }) | Frame::Footnote(_, blocks)) => {
                blocks.push(block)
            }
            Some(Frame::List { .. }) | None => self.root.push(block),
        }
    }

    /// 被引用过的脚注按编号收到文末，每条最后一段末尾接上指回引用处的箭头。
    fn finish_footnotes(&mut self) {
        let mut footnotes = Vec::new();
        for label in &self.footnote_refs {
            let Some(mut blocks) = self.footnote_defs.remove(label) else {
                continue;
            };
            let back =
                Inline {
                    text: " ↩".to_owned(), style: InlineStyle::default(), link: Some(format!("#fnref-{label}"))
                };
            match blocks.last_mut() {
                Some(Block::Paragraph(inlines)) => inlines.push(back),
                _ => blocks.push(Block::Paragraph(vec![Inline { text: "↩".to_owned(), ..back }])),
            }
            footnotes.push(Footnote { label: label.clone(), blocks });
        }
        if !footnotes.is_empty() {
            self.root.push(Block::Footnotes(footnotes));
        }
    }
}
