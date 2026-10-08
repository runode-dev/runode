//! 文件树上面的搜索框：按名称找文件，或者按内容找文件里的行。搜索词不为空时文件树换成结果，
//! 单击在预览栏里打开（内容结果跳到那一行），双击开成固定标签，回车打开选中的那一条，Esc 清空。
//! 结果可以排成列表，也可以按目录排成树（和 Git 面板共用 `file_tree`），目录能收起。
//!
//! 在后台搜：仓库里按名称找的是 `runode_git::list_files` 列出的文件，不在仓库里时从根目录往下走；
//! 按内容找调 `runode_git::grep`。搜索词全小写时不分大小写。文件树重读过（文件可能变了）、
//! 根目录、搜索词或方式变了时重搜。

use std::{
    collections::HashSet,
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

use super::WindowView;
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, FILTER_ICON, VIEW_LIST_ICON, VIEW_TREE_ICON},
    ui::{
        file_icons::{file_icon, folder_icon},
        hsla,
        text_field::{TextField, TextFieldEvent},
        tooltip::tooltip,
    },
    window::{
        git_panel::{TreeItem, file_tree},
        project::panel_message,
        titlebar::icon_toggle,
    },
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

/// 找到的一个文件；按内容找时带着其中命中的行。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    path: PathBuf,
    /// 相对根目录。
    rel: PathBuf,
    name: SharedString,
    /// 所在目录，相对根目录；列表形式时写在名字后面。
    dir: SharedString,
    lines: Vec<FoundLine>,
}

/// 按内容找到的一行：`line` 从 1 数，`hit` 是 `text` 里命中的那段。
#[derive(Clone, Debug, PartialEq, Eq)]
struct FoundLine {
    line: u32,
    text: SharedString,
    hit: Option<Range<usize>>,
}

/// 结果排成的一行。`file` 是 `FileSearch::found` 的下标，`line` 是那个文件 `lines` 的下标。
#[derive(Clone, Debug, PartialEq, Eq)]
enum SearchRow {
    /// 树形式时的目录，`path` 相对根目录。
    Dir {
        path: PathBuf,
        name: SharedString,
        depth: usize,
        expanded: bool,
    },
    File {
        file: usize,
        depth: usize,
    },
    Line {
        file: usize,
        line: usize,
        depth: usize,
    },
}

/// 把 `found` 排成行：列表形式按搜到的顺序，树形式按目录分层，`collapsed` 里的目录收起；
/// 命中的行跟在各自的文件后面、缩进一层。
fn layout_rows(found: &[Found], tree: bool, collapsed: &HashSet<PathBuf>) -> Vec<SearchRow> {
    let files: Vec<_> = if tree {
        let paths: Vec<_> = found.iter().enumerate().map(|(ix, found)| (ix, found.rel.as_path())).collect();
        file_tree(&paths, |dir| !collapsed.contains(dir))
    } else {
        (0..found.len()).map(|index| TreeItem::File { index, depth: 0 }).collect()
    };
    let mut rows = Vec::new();
    for item in files {
        match item {
            TreeItem::Dir { path, name, depth, expanded } => {
                rows.push(SearchRow::Dir { path, name: name.into(), depth, expanded });
            }
            TreeItem::File { index, depth } => {
                rows.push(SearchRow::File { file: index, depth });
                rows.extend((0..found[index].lines.len()).map(|line| SearchRow::Line {
                    file: index,
                    line,
                    depth: depth + 1,
                }));
            }
        }
    }
    rows
}

/// 一次搜索：在哪个根目录、按什么词、哪种方式。
type SearchKey = (PathBuf, String, SearchMode);

pub(in crate::window) struct FileSearch {
    field: Entity<TextField>,
    mode: SearchMode,
    /// 排成树还是列表，整个窗口一个设置；树形式时收起的目录。
    tree: bool,
    collapsed: HashSet<PathBuf>,
    found: Vec<Found>,
    /// `found` 按 `tree` 和 `collapsed` 排成的行。
    rows: Vec<SearchRow>,
    /// `found` 是哪次搜索的结果；为空时要重搜。
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
            // 没选中时打开第一个文件，跳过树形式开头的目录。
            TextFieldEvent::Next => {
                let rows = &this.file_search.rows;
                let first = rows.iter().position(|row| !matches!(row, SearchRow::Dir { .. }));
                if let Some(ix) = this.file_search.selected.or(first) {
                    this.open_search_row(ix, false, cx);
                }
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
            tree: false,
            collapsed: HashSet::new(),
            found: Vec::new(),
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

fn found(root: &Path, rel: PathBuf, lines: Vec<FoundLine>) -> Found {
    let name = rel.file_name().unwrap_or(rel.as_os_str()).to_string_lossy().into_owned();
    let dir = rel.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
    Found { path: root.join(&rel), rel, name: name.into(), dir: dir.into(), lines }
}

/// 在 `root` 下搜；在后台跑。
fn run_search(root: &Path, query: &str, mode: SearchMode, cancel: &AtomicBool) -> Vec<Found> {
    match mode {
        SearchMode::Name => {
            let files = runode_git::list_files(root).unwrap_or_else(|| walk_files(root));
            match_names(files, query).into_iter().map(|rel| found(root, rel, Vec::new())).collect()
        }
        SearchMode::Content => {
            let matches = runode_git::grep(root, query, ignore_case(query), MAX_LINES, cancel);
            // git grep 按文件一段段地输出，同一个文件的行连在一起。
            let groups = matches.chunk_by(|a, b| a.path == b.path);
            groups
                .map(|group| {
                    let lines = group
                        .iter()
                        .map(|hit| {
                            let (text, range) = snippet(&hit.text, query);
                            FoundLine { line: hit.line, text: text.into(), hit: range }
                        })
                        .collect();
                    found(root, group[0].path.clone(), lines)
                })
                .collect()
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
            search.found.clear();
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
            let found = job.await;
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
                    search.collapsed.clear();
                    search.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                search.found = found;
                search.searched = Some(key);
                search.relayout();
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

    fn toggle_search_tree(&mut self, cx: &mut Context<Self>) {
        let search = &mut self.file_search;
        search.tree = !search.tree;
        search.selected = None;
        search.relayout();
        cx.notify();
    }

    fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.file_search.mode = mode;
        self.sync_file_search(cx);
    }

    /// 在预览栏里打开第 `ix` 行结果，`pin` 时开成固定标签；内容结果跳到那一行，目录展开或收起。
    fn open_search_row(&mut self, ix: usize, pin: bool, cx: &mut Context<Self>) {
        let search = &mut self.file_search;
        match search.rows.get(ix).cloned() {
            Some(SearchRow::Dir { path, .. }) => {
                if !search.collapsed.remove(&path) {
                    search.collapsed.insert(path);
                }
                search.relayout();
            }
            Some(SearchRow::File { file, .. }) => {
                let path = search.found[file].path.clone();
                self.open_preview(&path, pin, cx);
            }
            Some(SearchRow::Line { file, line, .. }) => {
                let found = &search.found[file];
                let (path, line) = (found.path.clone(), found.lines[line].line);
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
        let (icon, text) = if self.file_search.tree {
            (VIEW_LIST_ICON, rust_i18n::t!("files.view_as_list"))
        } else {
            (VIEW_TREE_ICON, rust_i18n::t!("files.view_as_tree"))
        };
        let view_toggle = icon_toggle("search-view", icon, 14., false, fg, bg)
            .flex_none()
            .size(px(26.))
            .tooltip(tooltip(text, None, fg, bg))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_search_tree(cx)));
        let controls = div().flex().gap(px(6.)).child(segments.flex_1()).child(view_toggle);
        div().flex_none().px(px(10.)).pb(px(8.)).flex().flex_col().gap(px(6.)).child(field).child(controls)
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
        // 行号一栏按最大的行号定宽，等宽数字大约 0.6 个字号宽。
        let max_line = search.found.iter().flat_map(|found| &found.lines).map(|line| line.line).max().unwrap_or(0);
        let gutter = (max_line.to_string().len() as f32 * font_size * 0.62).ceil();
        let list = uniform_list(
            "file-search",
            search.rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range.map(|ix| this.render_search_row(ix, font_size, gutter, fg, bg, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&search.scroll)
        .size_full()
        .p(px(4.));
        Some(list.into_any_element())
    }

    fn render_search_row(
        &self,
        ix: usize,
        font_size: f32,
        gutter: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let search = &self.file_search;
        let row = &search.rows[ix];
        let dim = hsla(fg).opacity(0.5);
        // 树形式时文件和目录前面留出箭头的位置，名字对齐。
        let chevron = |expanded: Option<bool>| {
            let icon = expanded.map(|expanded| {
                svg()
                    .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                    .size(px(font_size))
                    .text_color(dim)
            });
            div().flex_none().w(px(font_size)).flex().items_center().children(icon)
        };
        let (depth, tip, content) = match row {
            SearchRow::Dir { path, name, depth, expanded } => {
                let last = path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
                let content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(chevron(Some(*expanded)))
                    .child(img(folder_icon(&last, *expanded)).flex_none().size(px(font_size + 2.)))
                    .child(div().min_w_0().truncate().text_color(hsla(fg).opacity(0.85)).child(name.clone()));
                (*depth, path.display().to_string(), content)
            }
            SearchRow::File { file, depth } => {
                let found = &search.found[*file];
                let hits = found.lines.len();
                let content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .when(search.tree, |item| item.child(chevron(None)))
                    .child(img(file_icon(&found.name)).flex_none().size(px(font_size + 2.)))
                    .child(
                        div()
                            .flex_none()
                            .max_w_full()
                            .truncate()
                            .text_color(hsla(fg).opacity(0.85))
                            .child(found.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl(px(2.))
                            .truncate()
                            .text_size(px(font_size - 1.))
                            .text_color(dim)
                            .when(!search.tree, |item| item.child(found.dir.clone())),
                    )
                    .when(hits > 0, |item| item.child(div().flex_none().text_color(dim).child(hits.to_string())));
                (*depth, found.path.display().to_string(), content)
            }
            SearchRow::Line { file, line, depth } => {
                let found = &search.found[*file];
                let line = &found.lines[*line];
                let highlight = HighlightStyle {
                    color: Some(hsla(fg)),
                    background_color: Some(hsla(bg.mix(fg, 0.25))),
                    ..Default::default()
                };
                let text =
                    StyledText::new(line.text.clone()).with_highlights(line.hit.clone().map(|hit| (hit, highlight)));
                let content = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(gutter))
                            .flex()
                            .justify_end()
                            .text_size(px(font_size - 1.))
                            .text_color(dim)
                            .child(line.line.to_string()),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(hsla(fg).opacity(0.7)).child(text));
                (*depth, format!("{}:{}", found.path.display(), line.line), content)
            }
        };
        let selected = search.selected == Some(ix);
        Self::file_row_shell(("file-search", ix), depth, font_size, fg)
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
            .tooltip(tooltip(tip, None, fg, bg))
            .into_any_element()
    }
}

impl FileSearch {
    /// 搜到了新结果、换了排法或者收起展开了目录，重排 `rows`。
    fn relayout(&mut self) {
        self.rows = layout_rows(&self.found, self.tree, &self.collapsed);
        self.selected = self.selected.filter(|&ix| ix < self.rows.len());
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
    fn lays_out_rows_as_a_list_or_a_tree() {
        let line = FoundLine { line: 3, text: "x".into(), hit: None };
        let found = vec![
            found(Path::new("/r"), "src/b.rs".into(), vec![line.clone(), line]),
            found(Path::new("/r"), "a.rs".into(), Vec::new()),
        ];
        let none = HashSet::new();
        let list = layout_rows(&found, false, &none);
        assert_eq!(
            list,
            [
                SearchRow::File { file: 0, depth: 0 },
                SearchRow::Line { file: 0, line: 0, depth: 1 },
                SearchRow::Line { file: 0, line: 1, depth: 1 },
                SearchRow::File { file: 1, depth: 0 },
            ]
        );
        let dir = |expanded| SearchRow::Dir { path: "src".into(), name: "src".into(), depth: 0, expanded };
        assert_eq!(
            layout_rows(&found, true, &none),
            [
                dir(true),
                SearchRow::File { file: 0, depth: 1 },
                SearchRow::Line { file: 0, line: 0, depth: 2 },
                SearchRow::Line { file: 0, line: 1, depth: 2 },
                SearchRow::File { file: 1, depth: 0 },
            ]
        );
        let collapsed = HashSet::from([PathBuf::from("src")]);
        assert_eq!(layout_rows(&found, true, &collapsed), [dir(false), SearchRow::File { file: 1, depth: 0 }]);
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
