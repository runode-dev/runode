//! 文件树上面的搜索框：按名称找文件，或者按内容找文件里的行。搜索词不为空时文件树换成结果，
//! 单击在预览栏里打开（内容结果跳到那一行），双击开成固定标签，回车打开选中的那一条，Esc 清空。
//! 结果可以排成列表，也可以按目录排成树（和 Git 面板共用 `file_tree`），目录能收起。
//!
//! 在后台搜：仓库里按名称找的是 `runode_git::list_files` 列出的文件，不在仓库里时从根目录往下走；
//! 按内容找调 `runode_git::grep`。按名称找时搜索词全小写就不分大小写；按内容找时可以区分大小写、
//! 全字匹配、用正则，还能只找或不找某些文件（逗号隔开的 glob，写法和 VS Code 一样）。文件树
//! 重读过（文件可能变了）、根目录、搜索词、方式或选项变了时重搜。

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
use regex::{Regex, RegexBuilder};
use runode_git::GrepQuery;
use runode_shared_types::color::Rgb;

use super::WindowView;
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CLOSE_ICON, FILTER_ICON},
    ui::{
        file_icons::{file_icon, folder_icon},
        hsla,
        text_field::{TextField, TextFieldEvent},
        tooltip::tooltip,
    },
    window::{
        git_panel::{TreeItem, file_tree},
        model::WorkspaceId,
        persist::format::SavedSearchOptions as ContentOptions,
        project::panel_message,
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

/// 一次搜索：在哪个根目录、按什么词、哪种方式和选项。
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchKey {
    root: PathBuf,
    query: String,
    mode: SearchMode,
    options: ContentOptions,
}

pub(in crate::window) struct FileSearch {
    field: Entity<TextField>,
    mode: SearchMode,
    /// 按内容找时的区分大小写、全字匹配和正则开关，以及要包含、要排除的文件。
    match_case: bool,
    whole_word: bool,
    regex: bool,
    include: Entity<TextField>,
    exclude: Entity<TextField>,
    /// 上次搜的正则写得不对。
    bad_regex: bool,
    /// 排成树还是列表，整个窗口一个设置，存进窗口存档；树形式时收起的目录。
    pub(in crate::window) tree: bool,
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
    /// 框里的搜索词是哪个 workspace 的。
    workspace: Option<WorkspaceId>,
    _events: [Subscription; 3],
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
        let filter = |placeholder: &str, cx: &mut Context<WindowView>| {
            let placeholder = rust_i18n::t!(placeholder).into_owned();
            let field = cx.new(|cx| TextField::new(String::new(), cx).with_placeholder(placeholder));
            let events = cx.subscribe(&field, |this, _, event: &TextFieldEvent, cx| {
                if let TextFieldEvent::Changed(_) = event {
                    this.sync_file_search(cx);
                    this.save(cx);
                }
            });
            (field, events)
        };
        let (include, include_events) = filter("files.include_placeholder", cx);
        let (exclude, exclude_events) = filter("files.exclude_placeholder", cx);
        Self {
            field,
            mode: SearchMode::Name,
            match_case: false,
            whole_word: false,
            regex: false,
            include,
            exclude,
            bad_regex: false,
            tree: false,
            collapsed: HashSet::new(),
            found: Vec::new(),
            rows: Vec::new(),
            searched: None,
            pending: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            workspace: None,
            _events: [events, include_events, exclude_events],
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

/// 内容结果里标出命中处用的正则，和交给 git grep 的条件一致；正则写得不对时是错误。
/// shortcut: git grep 用的是 POSIX 扩展正则，`\d` 这类 Rust 正则才有的写法这里认、git 不认，
/// 搜不到时再考虑换成 `git grep -P`。
fn matcher(query: &str, options: &ContentOptions) -> Result<Regex, regex::Error> {
    let pattern = if options.regex { query.to_owned() } else { regex::escape(query) };
    let pattern = if options.whole_word { format!(r"\b(?:{pattern})\b") } else { pattern };
    RegexBuilder::new(&pattern).case_insensitive(!options.match_case).build()
}

/// 「要包含的文件」「要排除的文件」里逗号隔开的 glob 换成 git 的 pathspec。和 VS Code 一样，
/// 不以 `**/` 或 `./` 开头的在任何一层都算（`*.ts` 是 `**/*.ts`），写的是目录时也算它里面的文件。
fn pathspecs(include: &str, exclude: &str) -> Vec<String> {
    let specs = |text: &str, magic: &'static str| {
        let globs = text.split(',').map(str::trim).filter(|glob| !glob.is_empty());
        globs
            .flat_map(move |glob| {
                let glob = match glob.strip_prefix("./") {
                    Some(rel) => rel.to_owned(),
                    None if glob.starts_with("**/") => glob.to_owned(),
                    None => format!("**/{glob}"),
                };
                let glob = glob.trim_end_matches('/');
                [format!(":({magic}){glob}"), format!(":({magic}){glob}/**")]
            })
            .collect::<Vec<_>>()
    };
    [specs(include, "glob"), specs(exclude, "exclude,glob")].concat()
}

/// 内容结果里显示的一行：去掉开头的缩进，命中处太靠后时把前面截掉，太长时截短；返回显示的
/// 文字和其中按 `matcher` 命中的第一段。
fn snippet(text: &str, matcher: &Regex) -> (String, Option<Range<usize>>) {
    let text = text.trim_start();
    let at = matcher.find(text).map(|hit| hit.range()).filter(|hit| !hit.is_empty());
    let start = match &at {
        Some(at) if at.start > SNIPPET_LEAD => text.floor_char_boundary(at.start - SNIPPET_LEAD),
        _ => 0,
    };
    let end = text.floor_char_boundary((start + SNIPPET_MAX).min(text.len()));
    let prefix = if start > 0 { "…" } else { "" };
    let shift = |at: usize| at - start + prefix.len();
    let hit = at.filter(|at| at.end <= end).map(|at| shift(at.start)..shift(at.end));
    (format!("{prefix}{}", &text[start..end]), hit)
}

fn found(root: &Path, rel: PathBuf, lines: Vec<FoundLine>) -> Found {
    let name = rel.file_name().unwrap_or(rel.as_os_str()).to_string_lossy().into_owned();
    let dir = rel.parent().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default();
    Found { path: root.join(&rel), rel, name: name.into(), dir: dir.into(), lines }
}

/// 按 `key` 搜；在后台跑。正则写得不对时为空。
fn run_search(key: &SearchKey, cancel: &AtomicBool) -> Option<Vec<Found>> {
    let SearchKey { root, query, options, .. } = key;
    match key.mode {
        SearchMode::Name => {
            let files = runode_git::list_files(root).unwrap_or_else(|| walk_files(root));
            Some(match_names(files, query).into_iter().map(|rel| found(root, rel, Vec::new())).collect())
        }
        SearchMode::Content => {
            let matcher = matcher(query, options).ok()?;
            let pathspecs = pathspecs(&options.include, &options.exclude);
            let grep = GrepQuery {
                pattern: query,
                ignore_case: !options.match_case,
                whole_word: options.whole_word,
                regex: options.regex,
                pathspecs: &pathspecs,
            };
            let matches = runode_git::grep(root, &grep, MAX_LINES, cancel);
            // git grep 按文件一段段地输出，同一个文件的行连在一起。
            let groups = matches.chunk_by(|a, b| a.path == b.path);
            let found = groups
                .map(|group| {
                    let lines = group
                        .iter()
                        .map(|hit| {
                            let (text, range) = snippet(&hit.text, &matcher);
                            FoundLine { line: hit.line, text: text.into(), hit: range }
                        })
                        .collect();
                    found(root, group[0].path.clone(), lines)
                })
                .collect();
            Some(found)
        }
    }
}

impl WindowView {
    /// 正在搜：搜索词不为空。新建、改名的输入框在文件树里，那时照样显示文件树。
    pub(super) fn searching(&self, cx: &gpui::App) -> bool {
        self.file_edit.is_none() && !self.file_search.field.read(cx).query().trim().is_empty()
    }

    /// 根目录、搜索词或方式和上次搜的不一样时在后台重搜，停下还在跑的上一次。
    fn sync_file_search(&mut self, cx: &mut Context<Self>) {
        let search = &self.file_search;
        let options = match search.mode {
            SearchMode::Name => ContentOptions::default(),
            SearchMode::Content => search.options(cx),
        };
        let query = search.field.read(cx).query().trim().to_owned();
        let key = SearchKey { root: self.files_root(), query, mode: search.mode, options };
        let search = &mut self.file_search;
        cx.notify();
        let current = search.pending.as_ref().map(|(key, _)| key).or(search.searched.as_ref());
        if current == Some(&key) {
            return;
        }
        if let Some((_, cancel)) = search.pending.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        if key.query.is_empty() {
            search.found.clear();
            search.rows.clear();
            search.searched = None;
            search.selected = None;
            search.bad_regex = false;
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        search.pending = Some((key.clone(), cancel.clone()));
        let job = cx.background_spawn({
            let key = key.clone();
            let cancel = cancel.clone();
            async move { run_search(&key, &cancel) }
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
                // 只是重读后重搜（词、方式和选项都没变）时留着选中的行和滚动位置。
                let same = search
                    .searched
                    .as_ref()
                    .is_some_and(|old| (&old.query, old.mode, &old.options) == (&key.query, key.mode, &key.options));
                if !same {
                    search.selected = None;
                    search.collapsed.clear();
                    search.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                search.bad_regex = found.is_none();
                search.found = found.unwrap_or_default();
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

    /// 切到了 workspace `id`：框里的词存回原来那个 workspace（它还在的话），换上这个 workspace 的词。
    pub(in crate::window) fn follow_workspace_in_file_search(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        let Some(old) = self.file_search.workspace.replace(id).filter(|&old| old != id) else {
            return;
        };
        let query = self.file_search.field.read(cx).query().to_owned();
        if let Some(workspace) = self.workspaces.iter_mut().find(|workspace| workspace.id == old) {
            workspace.project.file_query = query;
        }
        let query = self.workspace().project.file_query.clone();
        self.file_search.field.update(cx, |field, cx| field.set_query(query, cx));
        self.sync_file_search(cx);
    }

    pub(super) fn toggle_search_tree(&mut self, cx: &mut Context<Self>) {
        let tree = !self.file_search.tree;
        self.file_search.set_tree(tree);
        self.save(cx);
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

    /// 搜索框，下面是切换按名称、按内容找的两段按钮；按内容找时搜索框里多出区分大小写、全字
    /// 匹配和正则三个开关，下面多出要包含、要排除的文件两个输入框。
    pub(super) fn render_file_search_box(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let search = &self.file_search;
        let content = search.mode == SearchMode::Content;
        // 输入框外框；`leading` 画在文字前面。
        let input_box = |field: &Entity<TextField>, leading: Option<gpui::Svg>| {
            div()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(hsla(fg).opacity(0.12))
                .bg(hsla(bg.mix(fg, 0.06)))
                .children(leading)
                .child(div().flex_1().min_w_0().h_full().text_color(hsla(fg)).child(field.clone()))
        };
        // 搜索框里的小按钮：开着的开关垫一层底色。
        let small_button = |id: &'static str, tip: String, on: bool| {
            div()
                .id(id)
                .flex_none()
                .h(px(20.))
                .min_w(px(20.))
                .px(px(3.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .map(|item| {
                    if on {
                        item.bg(hsla(bg.mix(fg, 0.18))).text_color(hsla(fg))
                    } else {
                        item.text_color(hsla(fg).opacity(0.55)).hover(|item| item.text_color(hsla(fg)))
                    }
                })
                .tooltip(tooltip(tip, None, fg, bg))
        };
        type Flip = fn(&mut FileSearch);
        let option = |id, label: &'static str, tip: &str, on: bool, flip: Flip, cx: &mut Context<Self>| {
            small_button(id, rust_i18n::t!(tip).into_owned(), on).child(label).on_click(cx.listener(
                move |this, _, _, cx| {
                    flip(&mut this.file_search);
                    this.sync_file_search(cx);
                    this.save(cx);
                },
            ))
        };
        let has_query = !search.field.read(cx).query().is_empty();
        let clear = has_query.then(|| {
            small_button("search-clear", rust_i18n::t!("files.clear_search").into_owned(), false)
                .child(svg().path(CLOSE_ICON).size(px(12.)).text_color(hsla(fg).opacity(0.6)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.file_search.field.update(cx, |field, cx| field.set_query(String::new(), cx));
                    this.sync_file_search(cx);
                }))
        });
        let options = content.then(|| {
            [
                option("search-case", "Aa", "files.match_case", search.match_case, |s| s.match_case ^= true, cx),
                option("search-word", "ab", "files.whole_word", search.whole_word, |s| s.whole_word ^= true, cx)
                    .underline(),
                option("search-regex", ".*", "files.use_regex", search.regex, |s| s.regex ^= true, cx),
            ]
        });
        let filter_icon = svg().path(FILTER_ICON).flex_none().size(px(14.)).text_color(hsla(fg).opacity(0.5));
        let field = input_box(&search.field, Some(filter_icon)).children(clear).children(options.into_iter().flatten());
        let mode = search.mode;
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
        let labeled = |label: &str, field: &Entity<TextField>| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div().text_size(px(12.)).text_color(hsla(fg).opacity(0.7)).child(rust_i18n::t!(label).into_owned()),
                )
                .child(input_box(field, None))
        };
        let filters = content.then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(labeled("files.include", &search.include))
                .child(labeled("files.exclude", &search.exclude))
        });
        div()
            .flex_none()
            .px(px(10.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(field)
            .child(segments)
            .children(filters)
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
        let fresh = search.searched.as_ref().is_some_and(|key| key.root == self.files_root());
        if !fresh || search.rows.is_empty() {
            let text = if search.pending.is_some() {
                String::new()
            } else if search.bad_regex {
                rust_i18n::t!("files.bad_regex").into_owned()
            } else {
                rust_i18n::t!("files.search_empty").into_owned()
            };
            return Some(panel_message(text, fg).into_any_element());
        }
        // 结果统计：按内容找时是命中的行数和文件数，找够上限时说只列出了前面这些。
        let files = search.found.len();
        let lines: usize = search.found.iter().map(|found| found.lines.len()).sum();
        let mut summary = match search.mode {
            SearchMode::Name => rust_i18n::t!("files.search_files", files = files).into_owned(),
            SearchMode::Content => rust_i18n::t!("files.search_summary", count = lines, files = files).into_owned(),
        };
        let limit = match search.mode {
            SearchMode::Name => (files >= MAX_FILES).then_some(MAX_FILES),
            SearchMode::Content => (lines >= MAX_LINES).then_some(MAX_LINES),
        };
        if let Some(limit) = limit {
            summary.push_str(&rust_i18n::t!("files.search_truncated", limit = limit));
        }
        let summary_text = summary;
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
        let summary = div().flex_none().px(px(10.)).pb(px(4.)).text_size(px(12.)).text_color(hsla(fg).opacity(0.5));
        Some(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(summary.child(summary_text))
                .child(div().flex_1().min_h_0().child(list))
                .into_any_element(),
        )
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
    /// 按内容找时的选项，存进窗口存档。
    pub(in crate::window) fn options(&self, cx: &gpui::App) -> ContentOptions {
        ContentOptions {
            match_case: self.match_case,
            whole_word: self.whole_word,
            regex: self.regex,
            include: self.include.read(cx).query().to_owned(),
            exclude: self.exclude.read(cx).query().to_owned(),
        }
    }

    /// 恢复窗口时换上存档里的选项；这时还没有搜索词，不用重搜。
    pub(in crate::window) fn set_options(&mut self, options: ContentOptions, cx: &mut gpui::App) {
        self.match_case = options.match_case;
        self.whole_word = options.whole_word;
        self.regex = options.regex;
        self.include.update(cx, |field, cx| field.set_query(options.include, cx));
        self.exclude.update(cx, |field, cx| field.set_query(options.exclude, cx));
    }

    pub(in crate::window) fn set_tree(&mut self, tree: bool) {
        self.tree = tree;
        self.selected = None;
        self.relayout();
    }

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

    fn literal(query: &str) -> Regex {
        matcher(query, &ContentOptions::default()).unwrap()
    }

    #[test]
    fn snippets_trim_indent_and_keep_the_hit_in_view() {
        assert_eq!(snippet("    let foo = 1;", &literal("foo")), ("let foo = 1;".to_owned(), Some(4..7)));
        assert_eq!(snippet("Foo", &literal("foo")), ("Foo".to_owned(), Some(0..3)));
        assert_eq!(snippet("Ärger", &literal("ä")), ("Ärger".to_owned(), Some(0..2)));
        let case = ContentOptions { match_case: true, ..ContentOptions::default() };
        assert_eq!(snippet("foo", &matcher("Foo", &case).unwrap()), ("foo".to_owned(), None));
        let long = format!("{}needle", "x".repeat(100));
        let (text, hit) = snippet(&long, &literal("needle"));
        assert!(text.starts_with('…'));
        assert_eq!(&text[hit.unwrap()], "needle");
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    #[test]
    fn matchers_follow_the_word_and_regex_switches() {
        let word = ContentOptions { whole_word: true, ..ContentOptions::default() };
        assert!(matcher("foo", &word).unwrap().find("foobar").is_none());
        assert!(matcher("a.c", &ContentOptions::default()).unwrap().find("abc").is_none());
        let regex = ContentOptions { regex: true, ..ContentOptions::default() };
        assert!(matcher("a.c", &regex).unwrap().find("abc").is_some());
        assert!(matcher("(", &regex).is_err());
    }

    #[test]
    fn globs_become_pathspecs_matching_at_any_depth() {
        assert_eq!(
            pathspecs("*.ts, ./src/", "dist/**"),
            [
                ":(glob)**/*.ts",
                ":(glob)**/*.ts/**",
                ":(glob)src",
                ":(glob)src/**",
                ":(exclude,glob)**/dist/**",
                ":(exclude,glob)**/dist/**/**",
            ]
        );
        assert!(pathspecs(" , ", "").is_empty());
    }
}
