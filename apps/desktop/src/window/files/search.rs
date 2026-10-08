//! 文件树上面的搜索框：按名称找文件，或者按内容找文件里的行。搜索词不为空时文件树换成结果，
//! 单击在预览栏里打开（内容结果跳到那一行），双击开成固定标签，回车打开选中的那一条，Esc 清空。
//!
//! 在后台搜：仓库里按名称找的是 `runode_git::list_files` 列出的文件，不在仓库里时从根目录往下走；
//! 按内容找调 `runode_git::grep`。搜索词全小写时不分大小写。文件树重读过（文件可能变了）、
//! 根目录、搜索词或方式变了时重搜。

use std::{
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, Focusable as _, HighlightStyle, ScrollStrategy, SharedString,
    StyledText, Subscription, UniformListScrollHandle, Window, div, img, prelude::*, px, svg, uniform_list,
};
use runode_shared_types::color::Rgb;

use super::{ROW_EXTRA_HEIGHT, WindowView};
use crate::{
    assets::FILTER_ICON,
    ui::{
        file_icons::file_icon,
        hsla,
        text_field::{TextField, TextFieldEvent},
        tooltip::tooltip,
    },
    window::project::panel_message,
};

/// 按名称最多列出的文件数。
const MAX_FILES: usize = 500;
/// 按内容最多列出的行数。
const MAX_LINES: usize = 2000;
/// 不在仓库里时从根目录往下最多看这么多个文件，免得在家目录这种地方走个没完。
const MAX_WALK: usize = 50_000;
/// 内容结果里命中处前面最多留的字节数，再多就从前面截掉，命中的词才露得出来。
const SNIPPET_LEAD: usize = 24;
/// 内容结果一行最多留的字节数，压缩过的长行不整行排版。
const SNIPPET_MAX: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum SearchMode {
    Name,
    Content,
}

/// 结果里的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SearchRow {
    /// 找到的文件；按内容找时是下面几行所在的文件，`hits` 是其中命中的行数。
    File { path: PathBuf, name: SharedString, dir: SharedString, hits: usize },
    /// 按内容找到的一行：`line` 从 1 数，`hit` 是 `text` 里命中的那段。
    Line { path: PathBuf, line: u32, text: SharedString, hit: Option<Range<usize>> },
}

/// 一次搜索：在哪个根目录、按什么词、哪种方式。
type SearchKey = (PathBuf, String, SearchMode);

pub(in crate::window) struct FileSearch {
    field: Entity<TextField>,
    mode: SearchMode,
    rows: Vec<SearchRow>,
    /// `rows` 是哪次搜索的结果；为空时要重搜。
    searched: Option<SearchKey>,
    /// 后台正在跑的那次搜索和取消它的标志；换了搜索词时置位，停下上一次。
    pending: Option<(SearchKey, Arc<AtomicBool>)>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    _events: Subscription,
}

impl FileSearch {
    pub(in crate::window) fn new(window: &mut Window, cx: &mut Context<WindowView>) -> Self {
        let placeholder = rust_i18n::t!("files.search_placeholder").into_owned();
        let field = cx.new(|cx| TextField::new(String::new(), cx).with_placeholder(placeholder));
        let events = cx.subscribe_in(&field, window, |this, field, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Changed(_) => this.sync_file_search(cx),
            TextFieldEvent::Next => {
                let ix = this.file_search.selected.unwrap_or(0);
                this.open_search_row(ix, false, cx);
            }
            TextFieldEvent::Previous => {}
            // 有搜索词时清空，已经空着时把焦点交回终端。
            TextFieldEvent::Dismiss => {
                if field.read(cx).query().is_empty() {
                    window.focus(&this.focus_handle(cx), cx);
                } else {
                    field.update(cx, |field, cx| field.set_query(String::new(), cx));
                    this.sync_file_search(cx);
                }
            }
        });
        Self {
            field,
            mode: SearchMode::Name,
            rows: Vec::new(),
            searched: None,
            pending: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            _events: events,
        }
    }
}

/// 搜索词全小写时不分大小写。
fn ignore_case(query: &str) -> bool {
    !query.chars().any(char::is_uppercase)
}

/// 不在仓库里时按名称找的文件：从 `root` 往下走，不跟符号链接，跳过 `.git`，最多 `MAX_WALK` 个，
/// 相对 `root`。
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == ".git" || name == ".DS_Store" {
                continue;
            }
            let path = dir.join(name);
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                dirs.push(path);
            } else {
                files.push(path);
                if files.len() >= MAX_WALK {
                    return files;
                }
            }
        }
    }
    files
}

/// `files` 里名字含有 `query` 的，搜索词带 `/` 时比整个相对路径。名字以它开头的排在前面，
/// 其余保持原来的顺序；最多 `MAX_FILES` 个。
fn match_names(files: Vec<PathBuf>, query: &str) -> Vec<PathBuf> {
    let fold = |text: &str| if ignore_case(query) { text.to_lowercase() } else { text.to_owned() };
    let needle = fold(query);
    let mut hits: Vec<_> = files
        .into_iter()
        .filter_map(|rel| {
            let haystack = if query.contains('/') { rel.to_string_lossy() } else { rel.file_name()?.to_string_lossy() };
            let at = fold(&haystack).find(&needle)?;
            Some((at != 0, rel))
        })
        .collect();
    hits.sort_by_key(|(later, _)| *later);
    hits.into_iter().take(MAX_FILES).map(|(_, rel)| rel).collect()
}

/// 内容结果里显示的一行：去掉开头的缩进，命中处太靠后时把前面截掉，太长时截短；返回显示的
/// 文字和其中命中的那段。只按 ASCII 不分大小写定位，其他字符大小写不同时不标出命中的那段。
fn snippet(text: &str, query: &str) -> (String, Option<Range<usize>>) {
    let text = text.trim_start();
    let at =
        if ignore_case(query) { text.to_ascii_lowercase().find(&query.to_ascii_lowercase()) } else { text.find(query) };
    let start = match at {
        Some(at) if at > SNIPPET_LEAD => text.floor_char_boundary(at - SNIPPET_LEAD),
        _ => 0,
    };
    let end = text.floor_char_boundary((start + SNIPPET_MAX).min(text.len()));
    let prefix = if start > 0 { "…" } else { "" };
    let hit = at
        .filter(|&at| at + query.len() <= end)
        .map(|at| at - start + prefix.len()..at - start + prefix.len() + query.len());
    (format!("{prefix}{}", &text[start..end]), hit)
}

fn file_row(root: &Path, rel: &Path, hits: usize) -> SearchRow {
    let name = rel.file_name().unwrap_or(rel.as_os_str()).to_string_lossy().into_owned();
    let dir = rel.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
    SearchRow::File { path: root.join(rel), name: name.into(), dir: dir.into(), hits }
}

/// 在 `root` 下搜，排成结果的行；在后台跑。
fn run_search(root: &Path, query: &str, mode: SearchMode, cancel: &AtomicBool) -> Vec<SearchRow> {
    match mode {
        SearchMode::Name => {
            let files = runode_git::list_files(root).unwrap_or_else(|| walk_files(root));
            match_names(files, query).iter().map(|rel| file_row(root, rel, 0)).collect()
        }
        SearchMode::Content => {
            let matches = runode_git::grep(root, query, ignore_case(query), MAX_LINES, cancel);
            let mut rows = Vec::new();
            // git grep 按文件一段段地输出，同一个文件的行连在一起。
            for group in matches.chunk_by(|a, b| a.path == b.path) {
                rows.push(file_row(root, &group[0].path, group.len()));
                rows.extend(group.iter().map(|hit| {
                    let (text, range) = snippet(&hit.text, query);
                    SearchRow::Line { path: root.join(&hit.path), line: hit.line, text: text.into(), hit: range }
                }));
            }
            rows
        }
    }
}

impl WindowView {
    /// 正在搜：搜索词不为空。新建、改名的输入框在文件树里，那时照样显示文件树。
    fn searching(&self, cx: &gpui::App) -> bool {
        self.file_edit.is_none() && !self.file_search.field.read(cx).query().trim().is_empty()
    }

    /// 根目录、搜索词或方式和上次搜的不一样时在后台重搜，停下还在跑的上一次。
    fn sync_file_search(&mut self, cx: &mut Context<Self>) {
        let query = self.file_search.field.read(cx).query().trim().to_owned();
        let key = (self.files_root(), query, self.file_search.mode);
        let search = &mut self.file_search;
        cx.notify();
        let wanted = match &search.pending {
            Some((pending, _)) => pending,
            None => match &search.searched {
                Some(searched) => searched,
                None => &(PathBuf::new(), String::new(), search.mode),
            },
        };
        if *wanted == key {
            return;
        }
        if let Some((_, cancel)) = search.pending.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if key.1.is_empty() {
            search.rows.clear();
            search.searched = None;
            search.selected = None;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        search.pending = Some((key.clone(), cancel.clone()));
        let job = cx.background_spawn({
            let (root, query, mode) = key.clone();
            let cancel = cancel.clone();
            async move { run_search(&root, &query, mode, &cancel) }
        });
        cx.spawn(async move |this, cx| {
            let rows = job.await;
            this.update(cx, |this, cx| {
                // 搜的时候换了词，结果作废。
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let search = &mut this.file_search;
                search.pending = None;
                // 只是重读后重搜（词和方式没变）时留着选中的行和滚动位置。
                let same = search.searched.as_ref().is_some_and(|old| old.1 == key.1 && old.2 == key.2);
                if !same {
                    search.selected = None;
                    search.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                search.selected = search.selected.filter(|&ix| ix < rows.len());
                search.rows = rows;
                search.searched = Some(key);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 文件树重读过，文件可能变了：有搜索词时重搜。
    pub(in crate::window) fn refresh_file_search(&mut self, cx: &mut Context<Self>) {
        if let Some((_, cancel)) = self.file_search.pending.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.file_search.searched = None;
        self.sync_file_search(cx);
    }

    fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.file_search.mode = mode;
        self.sync_file_search(cx);
    }

    /// 在预览栏里打开第 `ix` 行结果，`pin` 时开成固定标签；内容结果跳到那一行。
    fn open_search_row(&mut self, ix: usize, pin: bool, cx: &mut Context<Self>) {
        match self.file_search.rows.get(ix).cloned() {
            Some(SearchRow::File { path, .. }) => self.open_preview(&path, pin, cx),
            Some(SearchRow::Line { path, line, .. }) => {
                self.open_preview_at(&path, line.saturating_sub(1) as usize, pin, cx);
            }
            None => return,
        }
        self.file_search.selected = Some(ix);
        cx.notify();
    }

    /// 搜索框，下面是切换按名称、按内容找的两段按钮。
    pub(super) fn render_file_search_box(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let field = div()
            .h(px(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .border_1()
            .border_color(hsla(fg).opacity(0.12))
            .bg(hsla(bg.mix(fg, 0.06)))
            .child(svg().path(FILTER_ICON).flex_none().size(px(14.)).text_color(hsla(fg).opacity(0.5)))
            .child(div().flex_1().min_w_0().h_full().text_color(hsla(fg)).child(self.file_search.field.clone()));
        let mode = self.file_search.mode;
        let segment = |id, label: SharedString, value: SearchMode, cx: &mut Context<Self>| {
            let on = mode == value;
            div()
                .id(id)
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .text_size(px(12.))
                .map(|item| {
                    if on {
                        item.bg(hsla(bg)).text_color(hsla(fg))
                    } else {
                        item.text_color(hsla(fg).opacity(0.6)).hover(|item| item.text_color(hsla(fg)))
                    }
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_search_mode(value, cx)))
        };
        let segments = div()
            .h(px(26.))
            .p(px(2.))
            .flex()
            .rounded(px(7.))
            .bg(hsla(bg.mix(fg, 0.08)))
            .child(segment("search-name", rust_i18n::t!("files.search_name").into_owned().into(), SearchMode::Name, cx))
            .child(segment(
                "search-content",
                rust_i18n::t!("files.search_content").into_owned().into(),
                SearchMode::Content,
                cx,
            ));
        div().flex_none().px(px(10.)).pb(px(8.)).flex().flex_col().gap(px(6.)).child(field).child(segments)
    }

    /// 搜索结果，没有正在搜时为空（显示文件树）。
    pub(super) fn render_file_search_results(
        &self,
        font_size: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.searching(cx) {
            return None;
        }
        let search = &self.file_search;
        // 换了 workspace 后、重搜完之前，不显示另一个根目录的结果。
        let fresh = search.searched.as_ref().is_some_and(|(root, ..)| *root == self.files_root());
        if !fresh || search.rows.is_empty() {
            let text =
                if search.pending.is_some() { String::new() } else { rust_i18n::t!("files.search_empty").into_owned() };
            return Some(panel_message(text, fg).into_any_element());
        }
        let list = uniform_list(
            "file-search",
            search.rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range.map(|ix| this.render_search_row(ix, font_size, fg, bg, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&search.scroll)
        .size_full()
        .p(px(4.));
        Some(list.into_any_element())
    }

    fn render_search_row(&self, ix: usize, font_size: f32, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.file_search.rows[ix];
        let selected = self.file_search.selected == Some(ix);
        let dim = hsla(fg).opacity(0.5);
        let content = match row {
            SearchRow::File { name, dir, hits, .. } => div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(img(file_icon(name)).flex_none().size(px(font_size + 2.)))
                .child(div().flex_none().max_w_full().truncate().text_color(hsla(fg).opacity(0.85)).child(name.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(font_size - 1.))
                        .text_color(dim)
                        .child(dir.clone()),
                )
                .when(*hits > 0, |item| item.child(div().flex_none().text_color(dim).child(hits.to_string()))),
            SearchRow::Line { text, hit, .. } => {
                let highlight = HighlightStyle {
                    color: Some(hsla(fg)),
                    background_color: Some(hsla(bg.mix(fg, 0.25))),
                    ..Default::default()
                };
                let text = StyledText::new(text.clone()).with_highlights(hit.clone().map(|hit| (hit, highlight)));
                // 缩进到文件名下面。
                div().flex_1().min_w_0().pl(px(font_size + 8.)).truncate().text_color(hsla(fg).opacity(0.7)).child(text)
            }
        };
        let path = match row {
            SearchRow::File { path, .. } => path.display().to_string(),
            SearchRow::Line { path, line, .. } => format!("{}:{line}", path.display()),
        };
        div()
            .id(("file-search", ix))
            .h(px(font_size + ROW_EXTRA_HEIGHT))
            .w_full()
            .px(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .overflow_hidden()
            .map(|item| {
                if selected {
                    item.bg(hsla(bg.mix(fg, 0.12)))
                } else {
                    item.hover(|item| item.bg(hsla(bg.mix(fg, 0.06))))
                }
            })
            .child(content)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.open_search_row(ix, event.click_count() >= 2, cx);
            }))
            .tooltip(tooltip(path, None, fg, bg))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_names_prefix_first_and_paths_with_a_slash() {
        let files = ["src/main.rs", "src/domain.rs", "README.md", "lib/main/x.rs"].map(PathBuf::from).to_vec();
        assert_eq!(match_names(files.clone(), "main"), [PathBuf::from("src/main.rs"), PathBuf::from("src/domain.rs")]);
        // 带大写字母时区分大小写。
        assert_eq!(match_names(files.clone(), "Main"), Vec::<PathBuf>::new());
        assert_eq!(match_names(files.clone(), "readme"), [PathBuf::from("README.md")]);
        assert_eq!(match_names(files, "main/"), [PathBuf::from("lib/main/x.rs")]);
    }

    #[test]
    fn snippets_trim_indent_and_keep_the_hit_in_view() {
        assert_eq!(snippet("    let foo = 1;", "foo"), ("let foo = 1;".to_owned(), Some(4..7)));
        assert_eq!(snippet("Foo", "foo"), ("Foo".to_owned(), Some(0..3)));
        assert_eq!(snippet("Foo", "Foo"), ("Foo".to_owned(), Some(0..3)));
        assert_eq!(snippet("foo", "Foo"), ("foo".to_owned(), None));
        let long = format!("{}needle", "x".repeat(100));
        let (text, hit) = snippet(&long, "needle");
        assert!(text.starts_with('…'));
        assert_eq!(&text[hit.unwrap()], "needle");
    }
}
